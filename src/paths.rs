//! Locations of the app's own files.

use std::path::PathBuf;

#[cfg(any(target_os = "windows", target_os = "macos"))]
const APP_DIR: &str = "SpeechOut";
#[cfg(not(any(target_os = "windows", target_os = "macos")))]
const APP_DIR: &str = "speechout";

/// Settings, API key fallback file and wordlists. On Windows this is
/// `%APPDATA%\SpeechOut`.
pub fn config_dir() -> PathBuf {
    dirs::config_dir().unwrap_or_else(std::env::temp_dir).join(APP_DIR)
}

pub fn settings_file() -> PathBuf {
    config_dir().join("settings.json")
}

pub fn wordlist_dir() -> PathBuf {
    config_dir().join("wordlists")
}

/// Translations of the interface, one JSON file per language.
pub fn languages_dir() -> PathBuf {
    config_dir().join("languages")
}

pub fn default_log_dir() -> PathBuf {
    dirs::data_local_dir().unwrap_or_else(std::env::temp_dir).join(APP_DIR).join("logs")
}

/// Creates a directory that only the current user can read, where the
/// platform supports it.
pub fn create_private_dir(dir: &std::path::Path) -> std::io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        std::fs::DirBuilder::new().recursive(true).mode(0o700).create(dir)
    }
    #[cfg(not(unix))]
    {
        std::fs::create_dir_all(dir)
    }
}

/// Writes a file atomically (write to a temporary file, then rename) with
/// permissions restricted to the current user on Unix.
pub fn write_private_file(path: &std::path::Path, contents: &[u8]) -> std::io::Result<()> {
    use std::io::Write;
    let dir = path.parent().ok_or_else(|| std::io::Error::other("file has no parent folder"))?;
    create_private_dir(dir)?;
    let mut tmp = tempfile::NamedTempFile::new_in(dir)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        tmp.as_file().set_permissions(std::fs::Permissions::from_mode(0o600))?;
    }
    tmp.write_all(contents)?;
    tmp.as_file().sync_all()?;
    tmp.persist(path).map_err(|e| e.error)?;
    Ok(())
}
