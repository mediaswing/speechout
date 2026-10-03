//! Debug log written to a folder the user chooses in Settings.
//!
//! The log records what the app did and any errors. It never records API
//! keys or the text being read, so it is safe to attach to a bug report.
//!
//! Each session starts a fresh `speechout.log`. The one before it is kept as
//! `speechout.previous.log`, so a crash is still on disk after a restart.

use log::{LevelFilter, Log, Metadata, Record};
use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

const LOG_FILE: &str = "speechout.log";
const PREVIOUS_LOG_FILE: &str = "speechout.previous.log";

struct OpenLog {
    path: PathBuf,
    file: File,
}

struct FileLogger {
    log: Mutex<Option<OpenLog>>,
}

static LOGGER: FileLogger = FileLogger { log: Mutex::new(None) };

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
        if let Ok(mut guard) = self.log.lock()
            && let Some(open) = guard.as_mut() {
                let _ = open.file.write_all(line.as_bytes());
            }
        if cfg!(debug_assertions) {
            eprint!("{line}");
        }
    }

    fn flush(&self) {
        if let Ok(mut guard) = self.log.lock()
            && let Some(open) = guard.as_mut() {
                let _ = open.file.flush();
            }
    }
}

pub fn init() {
    let _ = log::set_logger(&LOGGER);
    log::set_max_level(LevelFilter::Debug);
}

/// Points the log at `dir`, creating it if needed. Returns the log file path.
///
/// If this session is already writing to that folder, nothing changes.
/// Otherwise any log already there belongs to an earlier session, so it is
/// moved aside to `speechout.previous.log` and a fresh one is started.
pub fn set_directory(dir: &Path) -> std::io::Result<PathBuf> {
    crate::paths::create_private_dir(dir)?;
    let path = dir.join(LOG_FILE);
    let mut guard = LOGGER.log.lock().unwrap_or_else(|e| e.into_inner());
    if guard.as_ref().is_some_and(|open| same_file(&open.path, &path)) {
        return Ok(path);
    }
    if path.exists() {
        let _ = std::fs::rename(&path, dir.join(PREVIOUS_LOG_FILE));
    }
    let mut options = OpenOptions::new();
    options.create(true).write(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let file = options.open(&path)?;
    *guard = Some(OpenLog { path: path.clone(), file });
    Ok(path)
}

fn same_file(a: &Path, b: &Path) -> bool {
    match (a.canonicalize(), b.canonicalize()) {
        (Ok(a), Ok(b)) => a == b,
        _ => a == b,
    }
}
