//! Release packaging, which only the release workflow runs: an installer
//! package (.pkg) on macOS, and a .deb on Linux. It is kept apart from the
//! code that runs when the app is used. Windows' installer is built by WiX
//! Toolset in the release workflow instead (see src/installer/speechout.wxs).

#[cfg(target_os = "macos")]
mod macos;
#[cfg(target_os = "macos")]
pub use macos::build;

#[cfg(all(unix, not(target_os = "macos")))]
mod linux;
#[cfg(all(unix, not(target_os = "macos")))]
pub use linux::build;
