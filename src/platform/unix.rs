//! Code macOS and Linux share.
//!
//! Only one file is read aloud in the background at a time. The process
//! reading one locks `paths::background_reader_file()` and writes a token of
//! its own in it. A later one asks it to stop by changing what the file says,
//! which the reader looks at ten times a second, then waits for the lock to
//! go. The lock goes when the reader's process ends, however it ends, so one
//! that crashed is never waited for, and a reused process id can't be
//! mistaken for it.

use anyhow::Context;
use std::fs::{File, OpenOptions, TryLockError};
use std::io::Write;
use std::os::unix::fs::OpenOptionsExt;
use std::time::Duration;

const POLL: Duration = Duration::from_millis(100);

/// Whether another process holds the reader's lock.
fn held(file: &File) -> bool {
    match file.try_lock_shared() {
        Ok(()) => {
            let _ = file.unlock();
            false
        }
        Err(TryLockError::WouldBlock) => true,
        Err(TryLockError::Error(e)) => {
            log::warn!("could not check for a reader in the background: {e}");
            false
        }
    }
}

pub fn stop_background_reader() -> bool {
    let path = crate::paths::background_reader_file();
    let Ok(file) = File::open(&path) else { return false };
    if !held(&file) {
        return false;
    }
    // Written in place, as the reader's lock is on this file, not its name.
    if let Err(e) = std::fs::write(&path, b"stop") {
        log::warn!("could not ask the reader in the background to stop: {e}");
    }
    for _ in 0..30 {
        std::thread::sleep(POLL);
        if !held(&file) {
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
    let mut file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .mode(0o600)
        .open(&path)
        .context(setting_up)?;
    // A reader that was asked to stop but hasn't exited yet still has the
    // lock, so it is taken as soon as that one has gone.
    let mut locked = file.try_lock().is_ok();
    let token = format!("{} {:?}", std::process::id(), std::time::SystemTime::now());
    file.set_len(0).context(setting_up)?;
    file.write_all(token.as_bytes()).context(setting_up)?;
    std::thread::spawn(move || {
        loop {
            std::thread::sleep(POLL);
            if !locked {
                locked = file.try_lock().is_ok();
            }
            if std::fs::read_to_string(&path).ok().as_deref() != Some(token.as_str()) {
                break;
            }
        }
        on_stop();
        // The lock is kept until the process exits, which is what the one
        // that asked is waiting for.
        std::mem::forget(file);
    });
    Ok(())
}
