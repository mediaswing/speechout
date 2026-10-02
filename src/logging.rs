//! Debug log written to a folder the user chooses in Settings.
//!
//! The log records what the app did and any errors. It never records API
//! keys or the text being read, so it is safe to attach to a bug report.

use log::{LevelFilter, Log, Metadata, Record};
use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

const LOG_FILE: &str = "speechout.log";
const MAX_LOG_BYTES: u64 = 5 * 1024 * 1024;

struct FileLogger {
    file: Mutex<Option<File>>,
}

static LOGGER: FileLogger = FileLogger { file: Mutex::new(None) };

impl Log for FileLogger {
    fn enabled(&self, metadata: &Metadata) -> bool {
        // Only our own messages, not the chatter of every dependency.
        metadata.target().starts_with("speechout") || metadata.level() <= log::Level::Warn
    }

    fn log(&self, record: &Record) {
        if !self.enabled(record.metadata()) {
            return;
        }
        let secs = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        let line = format!("{secs} {:<5} {}: {}\n", record.level(), record.target(), record.args());
        if let Ok(mut guard) = self.file.lock()
            && let Some(file) = guard.as_mut() {
                let _ = file.write_all(line.as_bytes());
            }
        if cfg!(debug_assertions) {
            eprint!("{line}");
        }
    }

    fn flush(&self) {
        if let Ok(mut guard) = self.file.lock()
            && let Some(file) = guard.as_mut() {
                let _ = file.flush();
            }
    }
}

pub fn init() {
    let _ = log::set_logger(&LOGGER);
    log::set_max_level(LevelFilter::Debug);
}

/// Points the log at `dir`, creating it if needed. Returns the log file path.
pub fn set_directory(dir: &Path) -> std::io::Result<PathBuf> {
    crate::paths::create_private_dir(dir)?;
    let path = dir.join(LOG_FILE);
    if std::fs::metadata(&path).map(|m| m.len() > MAX_LOG_BYTES).unwrap_or(false) {
        let _ = std::fs::rename(&path, dir.join(format!("{LOG_FILE}.old")));
    }
    let mut options = OpenOptions::new();
    options.create(true).append(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let file = options.open(&path)?;
    if let Ok(mut guard) = LOGGER.file.lock() {
        *guard = Some(file);
    }
    Ok(path)
}
