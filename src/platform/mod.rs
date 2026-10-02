//! Platform-specific code lives in this module, one file per operating system,
//! so that each port can be maintained without touching the rest of the app.
//!
//! Every platform file provides the same set of functions:
//!
//! * `system_voices()` – list the built-in voices.
//! * `synthesize(text, voice_id)` – render text with a built-in voice and return WAV bytes.
//! * `heic_to_jpeg(path)` – convert a HEIF/HEIC photo into JPEG bytes.
//! * `open_url(url)` – open a web page in the default browser.
//! * `ollama_installed/package_manager/install_ollama/start_ollama` – find,
//!   install and start Ollama, which describes photos.
//! * `secret_get/secret_set/secret_delete` – the native secret store, if the
//!   platform has one that the app uses (the Windows registry). Returning
//!   `Unsupported` makes the caller fall back to a file in the config folder.

use crate::speech::Voice;

#[cfg(target_os = "windows")]
mod windows;
#[cfg(target_os = "windows")]
use windows as imp;

#[cfg(target_os = "macos")]
mod macos;
#[cfg(target_os = "macos")]
use macos as imp;

#[cfg(all(unix, not(target_os = "macos")))]
mod linux;
#[cfg(all(unix, not(target_os = "macos")))]
use linux as imp;

/// Result of talking to a native secret store.
#[cfg_attr(not(windows), allow(dead_code))]
pub enum SecretStore<T> {
    /// The operation succeeded.
    Ok(T),
    /// The platform has no native store, or it failed; use the file fallback.
    Unsupported,
}

pub fn system_voices() -> anyhow::Result<Vec<Voice>> {
    imp::system_voices()
}

pub fn synthesize(text: &str, voice_id: &str) -> anyhow::Result<Vec<u8>> {
    imp::synthesize(text, voice_id)
}

pub fn heic_to_jpeg(path: &std::path::Path) -> anyhow::Result<Vec<u8>> {
    imp::heic_to_jpeg(path)
}

/// Opens a web page in the default browser. Only `https://` URLs are allowed.
pub fn open_url(url: &str) -> anyhow::Result<()> {
    if !url.starts_with("https://") || url.chars().any(|c| c.is_whitespace() || c.is_control()) {
        anyhow::bail!("refusing to open an unexpected address");
    }
    imp::open_url(url)
}

/// Whether Ollama, which describes photos, is installed (running or not).
pub fn ollama_installed() -> bool {
    imp::ollama_installed()
}

/// The name of the package manager that can install Ollama here, if any:
/// winget on Windows, Homebrew on macOS, Snap on Linux.
pub fn package_manager() -> Option<&'static str> {
    imp::package_manager()
}

/// Installs Ollama with the package manager. Blocks until it has finished.
pub fn install_ollama() -> anyhow::Result<()> {
    imp::install_ollama()
}

/// Starts Ollama in the background. Returns once it has been launched, which
/// may be before it is ready to answer.
pub fn start_ollama() -> anyhow::Result<()> {
    imp::start_ollama()
}

pub fn secret_get(name: &str) -> SecretStore<Option<String>> {
    imp::secret_get(name)
}

pub fn secret_set(name: &str, value: &str) -> SecretStore<()> {
    imp::secret_set(name, value)
}

pub fn secret_delete(name: &str) -> SecretStore<()> {
    imp::secret_delete(name)
}

/// Where the native secret store keeps keys, for display in the UI.
pub fn secret_store_description() -> &'static str {
    imp::SECRET_STORE_DESCRIPTION
}

/// Release packaging: an ad hoc signed `.app` on macOS, a `.deb` on Linux.
#[cfg(not(target_os = "windows"))]
pub fn package(out_dir: &std::path::Path) -> anyhow::Result<std::path::PathBuf> {
    imp::package(out_dir)
}
