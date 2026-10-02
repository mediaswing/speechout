//! API key storage. Keys go into the platform's native store where the app uses
//! one (the HKEY_CURRENT_USER registry hive on Windows); if that is not
//! available or fails, they go into `api-keys.txt` in the app's config folder
//! (`%APPDATA%\SpeechOut` on Windows), readable only by the current user.
//!
//! Keys are never written to the debug log.

use crate::platform::{self, SecretStore};
use std::collections::BTreeMap;
use std::path::PathBuf;

const FALLBACK_FILE: &str = "api-keys.txt";

fn fallback_path() -> PathBuf {
    crate::paths::config_dir().join(FALLBACK_FILE)
}

fn read_fallback() -> BTreeMap<String, String> {
    let Ok(text) = std::fs::read_to_string(fallback_path()) else {
        return BTreeMap::new();
    };
    text.lines()
        .filter_map(|line| line.split_once('='))
        .map(|(k, v)| (k.trim().to_owned(), v.trim().to_owned()))
        .filter(|(k, v)| !k.is_empty() && !v.is_empty())
        .collect()
}

fn write_fallback(map: &BTreeMap<String, String>) -> std::io::Result<()> {
    let mut text = String::from("# Speech Output Engine API keys. Keep this file private.\n");
    for (k, v) in map {
        text.push_str(&format!("{k}={v}\n"));
    }
    crate::paths::write_private_file(&fallback_path(), text.as_bytes())
}

/// Removes characters that could break the line-based fallback file or an HTTP header.
fn sanitize(value: &str) -> String {
    value.trim().chars().filter(|c| !c.is_control()).collect()
}

pub fn get(name: &str) -> Option<String> {
    if let SecretStore::Ok(Some(value)) = platform::secret_get(name)
        && !value.is_empty() {
            return Some(value);
        }
    read_fallback().remove(name)
}

/// Saves a key, or deletes it if `value` is empty. Returns a description of
/// where the key was stored, for the status message.
pub fn set(name: &str, value: &str) -> anyhow::Result<&'static str> {
    let value = sanitize(value);
    if value.is_empty() {
        delete(name)?;
        return Ok("removed");
    }
    if let SecretStore::Ok(()) = platform::secret_set(name, &value) {
        // Do not leave an older copy behind in the fallback file.
        let mut map = read_fallback();
        if map.remove(name).is_some() {
            write_fallback(&map)?;
        }
        return Ok(platform::secret_store_description());
    }
    let mut map = read_fallback();
    map.insert(name.to_owned(), value);
    write_fallback(&map)?;
    Ok("a private file in the app's settings folder")
}

pub fn delete(name: &str) -> anyhow::Result<()> {
    let _ = platform::secret_delete(name);
    let mut map = read_fallback();
    if map.remove(name).is_some() {
        write_fallback(&map)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::sanitize;

    #[test]
    fn strips_newlines_and_spaces() {
        assert_eq!(sanitize("  sk-abc\r\nevil=1 "), "sk-abcevil=1");
    }
}
