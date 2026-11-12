//! The debug log: a file in a folder the user can choose on the Settings tab,
//! which can change while the app runs.
//!
//! The log holds what the app did and what went wrong. It never holds API keys
//! or the text being read, and only this app's own messages are recorded at
//! debug level, because libraries may log requests. Other crates' warnings and
//! errors still appear.

use std::fs::File;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use anyhow::{Context, bail};
use log::{Level, LevelFilter, Log, Metadata, Record};

pub const LOG_FILE: &str = "commonspeech.log";
/// When the log reaches this size it is moved aside, so it can't fill the disk.
const MAX_LOG_BYTES: u64 = 5 * 1024 * 1024;

static LOGGER: FileLogger = FileLogger { file: Mutex::new(None) };

struct FileLogger {
    file: Mutex<Option<File>>,
}

impl Log for FileLogger {
    fn enabled(&self, metadata: &Metadata) -> bool {
        metadata.target().starts_with(crate::BIN_NAME) || metadata.level() <= Level::Warn
    }

    fn log(&self, record: &Record) {
        if !self.enabled(record.metadata()) {
            return;
        }
        let Ok(mut file) = self.file.lock() else { return };
        if let Some(file) = file.as_mut() {
            let _ = writeln!(file, "{} {:<5} {}: {}", timestamp(), record.level(), record.target(), record.args());
        }
    }

    fn flush(&self) {
        if let Ok(mut file) = self.file.lock()
            && let Some(file) = file.as_mut()
        {
            let _ = file.flush();
        }
    }
}

/// The time now in UTC, as `2026-11-06 17:04:05`.
fn timestamp() -> String {
    let seconds = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs());
    format_time(seconds)
}

fn format_time(seconds: u64) -> String {
    // Days since 1970 to a calendar date, by Howard Hinnant's algorithm.
    let days = (seconds / 86400) as i64 + 719_468;
    let era = days.div_euclid(146_097);
    let day_of_era = days.rem_euclid(146_097);
    let year_of_era = (day_of_era - day_of_era / 1460 + day_of_era / 36524 - day_of_era / 146_096) / 365;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let shifted_month = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * shifted_month + 2) / 5 + 1;
    let month = if shifted_month < 10 { shifted_month + 3 } else { shifted_month - 9 };
    let year = year_of_era + era * 400 + i64::from(month <= 2);
    let s = seconds % 86400;
    format!("{year:04}-{month:02}-{day:02} {:02}:{:02}:{:02}", s / 3600, s / 60 % 60, s % 60)
}

/// Opens the log in `dir`, creating the folder if needed. The folder is the
/// user's to choose, so its permissions are left alone, but the file is private
/// as document names can be sensitive. A symbolic link in place of the log is
/// refused, so writing can't be redirected onto another file.
fn open_log(dir: &Path) -> anyhow::Result<(File, PathBuf)> {
    if !dir.is_absolute() {
        bail!("the log folder must be a full path, such as {}", crate::store::data_dir().join("logs").display());
    }
    std::fs::create_dir_all(dir).with_context(|| format!("could not create the folder {}", dir.display()))?;
    let path = dir.join(LOG_FILE);
    match std::fs::symlink_metadata(&path) {
        Ok(meta) if !meta.file_type().is_file() => bail!("{} is not an ordinary file", path.display()),
        Ok(meta) if meta.len() > MAX_LOG_BYTES => {
            let _ = std::fs::rename(&path, dir.join(format!("{LOG_FILE}.old")));
        }
        _ => {}
    }
    crate::platform::create_private_file(&path).with_context(|| format!("could not write to {}", path.display()))?;
    let file = std::fs::OpenOptions::new()
        .append(true)
        .open(&path)
        .with_context(|| format!("could not write to {}", path.display()))?;
    Ok((file, path))
}

/// Starts logging to `dir`, and logs panics too.
pub fn init(dir: &Path) -> anyhow::Result<PathBuf> {
    if log::set_logger(&LOGGER).is_ok() {
        log::set_max_level(LevelFilter::Debug);
        let default_hook = std::panic::take_hook();
        std::panic::set_hook(Box::new(move |info| {
            log::error!("PANIC: {info}");
            default_hook(info);
        }));
    }
    set_dir(dir)
}

/// Moves the log to `dir`. If that fails, the log stays where it was.
pub fn set_dir(dir: &Path) -> anyhow::Result<PathBuf> {
    let (file, path) = open_log(dir)?;
    if let Ok(mut current) = LOGGER.file.lock() {
        *current = Some(file);
    }
    log::info!("{} {} logging to {}", crate::APP_TITLE, env!("CARGO_PKG_VERSION"), path.display());
    Ok(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats_times() {
        assert_eq!(format_time(0), "1970-01-01 00:00:00");
        assert_eq!(format_time(1_794_000_245), "2026-11-06 21:24:05");
        assert_eq!(format_time(951_782_400), "2000-02-29 00:00:00");
    }

    #[test]
    fn makes_the_folder_and_a_private_file() {
        let dir = tempfile::tempdir().unwrap();
        let logs = dir.path().join("a").join("b");
        let (_file, path) = open_log(&logs).unwrap();
        assert_eq!(path, logs.join(LOG_FILE));
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(std::fs::metadata(&path).unwrap().permissions().mode() & 0o777, 0o600);
        }
    }

    #[test]
    fn refuses_relative_folders() {
        assert!(open_log(Path::new("logs")).is_err());
    }

    #[test]
    fn moves_a_full_log_aside() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join(LOG_FILE), vec![b'x'; MAX_LOG_BYTES as usize + 1]).unwrap();
        open_log(dir.path()).unwrap();
        assert_eq!(std::fs::metadata(dir.path().join(LOG_FILE)).unwrap().len(), 0);
        assert!(dir.path().join(format!("{LOG_FILE}.old")).exists());
    }

    #[cfg(unix)]
    #[test]
    fn refuses_a_symbolic_link_in_place_of_the_log() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("precious.txt");
        std::fs::write(&target, b"keep").unwrap();
        std::os::unix::fs::symlink(&target, dir.path().join(LOG_FILE)).unwrap();
        assert!(open_log(dir.path()).is_err());
    }

    #[test]
    fn only_our_debug_messages_are_kept() {
        let info = |target, level| log::Metadata::builder().target(target).level(level).build();
        assert!(LOGGER.enabled(&info(module_path!(), Level::Debug)));
        assert!(!LOGGER.enabled(&info("ureq::unversioned", Level::Debug)));
        assert!(!LOGGER.enabled(&info("rustls", Level::Info)));
        assert!(LOGGER.enabled(&info("rustls", Level::Warn)));
    }
}
