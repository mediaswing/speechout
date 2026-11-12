//! Release packaging, which only the release workflow runs: an installer package
//! (.pkg) on macOS, a .deb on Linux and an .msi on Windows. Keeping it here means
//! the workflow needs no shell script to build them, and it is kept apart from
//! the code that runs when the app is used.

#[cfg(target_os = "macos")]
#[path = "macos.rs"]
mod imp;

#[cfg(all(unix, not(target_os = "macos")))]
#[path = "linux.rs"]
mod imp;

#[cfg(windows)]
#[path = "windows.rs"]
mod imp;

pub use imp::build;
