//! Code that differs between operating systems lives here, one file per
//! system, so that each can be maintained without touching the rest of the app.
//!
//! Every platform file provides the same functions:
//!
//! * `system_voices()`: list the built-in voices.
//! * `synthesize(text, voice_id)`: speak text with a built-in voice, as WAV bytes.
//! * `heic_to_jpeg(path)`: convert a HEIF/HEIC photo into JPEG bytes.
//! * `start_ollama()`: start an installed Ollama, returning whether it tried.
//! * `whisper_programs()`, `whisper_installer()` and `WHISPER_HELP`: where
//!   whisper.cpp is usually installed, how to install it if the system has a
//!   way (only ever used once the user agrees), and what to tell them if not.
//! * `restrict_dir_to_owner(dir)` and `create_private_file(path)`: keep the
//!   database, which holds API keys, away from other users of the computer.
//!
//! Programs are always started directly, by absolute path where the system
//! allows, and never through a shell, so file names and voice names can't be
//! interpreted as commands. Text goes through standard input, so it never
//! appears in the process list.
//!
//! The release packaging for each system is in `crate::package`.

#[cfg(target_os = "windows")]
#[path = "windows.rs"]
mod imp;

#[cfg(target_os = "macos")]
#[path = "macos.rs"]
mod imp;

#[cfg(all(unix, not(target_os = "macos")))]
#[path = "linux.rs"]
mod imp;

#[cfg(unix)]
mod unix;

pub use imp::{
    WHISPER_HELP, create_private_file, heic_to_jpeg, restrict_dir_to_owner, start_ollama, synthesize, system_voices,
    whisper_installer, whisper_programs,
};
