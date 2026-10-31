//! Code macOS and Linux share.
//!
//! Only one file is read aloud in the background at a time. The process
//! reading one locks `paths::background_reader_file()` and writes a token of
//! its own in it, starting with its process id. A later one asks it to stop
//! by changing what the file says, which the reader looks at ten times a
//! second, then waits for the lock to go before it starts reading, so the
//! two are never heard at once. The lock goes when the reader's process
//! ends, however it ends, so one that crashed is never waited for, and a
//! reused process id can't be mistaken for it.
//!
//! Some network home folders can't lock files. There the process id in the
//! token is used instead, to tell whether the reader is still running.

use anyhow::Context;
use std::fs::{File, OpenOptions, TryLockError};
use std::io::Write;
use std::os::unix::fs::OpenOptionsExt;
use std::path::Path;
use std::time::{Duration, Instant};

const POLL: Duration = Duration::from_millis(100);

/// How long a new reader waits for the one it asked to stop to finish. One
/// that takes longer has probably hung, so reading starts anyway.
const WAIT: Duration = Duration::from_secs(10);

/// The process id at the start of the reader's token, or after `stop` once
/// it has been asked to stop, if the file has one.
fn pid_in(path: &Path) -> Option<libc::pid_t> {
    let text = std::fs::read_to_string(path).ok()?;
    text.strip_prefix(STOP).unwrap_or(&text).split_whitespace().next()?.parse().ok()
}

/// What the file says once the reader has been asked to stop, followed by
/// its process id, so the next reader can still wait for it.
const STOP: &str = "stop ";

/// Whether the process `pid`, other than this one, is running.
fn alive(pid: libc::pid_t) -> bool {
    if pid <= 0 || pid == std::process::id() as libc::pid_t {
        return false;
    }
    // SAFETY: signal 0 sends nothing; it only checks the process exists.
    let sent = unsafe { libc::kill(pid, 0) } == 0;
    sent || std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
}

/// Whether another process is reading in the background: it holds the lock
/// on `file`, or, where files can't be locked, `pid` is still running.
fn running(file: &File, pid: Option<libc::pid_t>) -> bool {
    match file.try_lock_shared() {
        Ok(()) => {
            let _ = file.unlock();
            false
        }
        Err(TryLockError::WouldBlock) => true,
        Err(TryLockError::Error(e)) => {
            log::debug!("could not lock the background reader's file, so checking its process instead: {e}");
            pid.is_some_and(alive)
        }
    }
}

pub fn stop_background_reader() -> bool {
    let path = crate::paths::background_reader_file();
    let Ok(file) = File::open(&path) else { return false };
    let pid = pid_in(&path);
    if !running(&file, pid) {
        return false;
    }
    // Written in place, as the reader's lock is on this file, not its name.
    let stop = format!("{STOP}{}", pid.map(|p| p.to_string()).unwrap_or_default());
    if let Err(e) = std::fs::write(&path, stop) {
        log::warn!("could not ask the reader in the background to stop: {e}");
    }
    for _ in 0..30 {
        std::thread::sleep(POLL);
        if !running(&file, pid) {
            break;
        }
    }
    true
}

pub fn become_background_reader(on_stop: impl FnOnce() + Send + 'static) -> anyhow::Result<()> {
    let path = crate::paths::background_reader_file();
    let setting_up = "could not set up reading in the background";
    if let Some(dir) = path.parent() {
        crate::paths::create_private_dir(dir).context(setting_up)?;
    }
    // The reader before, if any, for where files can't be locked.
    let previous = pid_in(&path);
    let mut file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .mode(0o600)
        .open(&path)
        .context(setting_up)?;
    // Written straight away, so a reader still running is asked to stop, and
    // a later one can ask this one to stop while it waits.
    let token = format!("{} {:?}", std::process::id(), std::time::SystemTime::now());
    file.set_len(0).context(setting_up)?;
    file.write_all(token.as_bytes()).context(setting_up)?;
    let ours = move |path: &Path| std::fs::read_to_string(path).ok().as_deref() == Some(token.as_str());

    // A reader that was asked to stop but hasn't exited yet still has the
    // lock, so wait for it to go before reading.
    let deadline = Instant::now() + WAIT;
    let mut locked = loop {
        match file.try_lock() {
            Ok(()) => break true,
            Err(TryLockError::WouldBlock) => {}
            Err(TryLockError::Error(_)) if !previous.is_some_and(alive) => break false,
            Err(TryLockError::Error(_)) => {}
        }
        if !ours(&path) {
            // Asked to stop while waiting; the thread below stops at once.
            break false;
        }
        if Instant::now() >= deadline {
            log::warn!("the reader in the background that was asked to stop is still running; reading anyway");
            break false;
        }
        std::thread::sleep(POLL);
    };
    std::thread::spawn(move || {
        while ours(&path) {
            std::thread::sleep(POLL);
            if !locked {
                locked = file.try_lock().is_ok();
            }
        }
        on_stop();
        // The lock is kept until the process exits, which is what the one
        // that asked is waiting for.
        std::mem::forget(file);
    });
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tells_whether_a_process_is_running() {
        assert!(!alive(std::process::id() as libc::pid_t), "this process is never the other reader");
        assert!(!alive(0));
        assert!(!alive(-1));
        // A child that has exited and been waited for has gone.
        let mut child = std::process::Command::new("/bin/sleep").arg("5").spawn().unwrap();
        let pid = child.id() as libc::pid_t;
        assert!(alive(pid));
        child.kill().unwrap();
        child.wait().unwrap();
        assert!(!alive(pid));
    }

    #[test]
    fn reads_the_process_id_from_the_token() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("reader.lock");
        std::fs::write(&path, "1234 SystemTime { tv_sec: 1, tv_nsec: 2 }").unwrap();
        assert_eq!(pid_in(&path), Some(1234));
        std::fs::write(&path, "stop 1234").unwrap();
        assert_eq!(pid_in(&path), Some(1234));
        std::fs::write(&path, "stop ").unwrap();
        assert_eq!(pid_in(&path), None);
        assert_eq!(pid_in(&dir.path().join("missing")), None);
    }
}
