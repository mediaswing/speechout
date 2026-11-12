//! Linux: system voices come from eSpeak NG, which every major distribution
//! packages and which is what Orca users usually have. HEIC conversion uses
//! `heif-convert` from libheif.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use anyhow::{Context, bail};

pub use super::unix::{create_private_file, restrict_dir_to_owner};
use super::unix::without_embedded_commands;
use crate::speech::Voice;

/// Only look in the system's own folders, so a writable folder early in PATH
/// can't substitute a different program.
fn find_program(names: &[&str]) -> Option<PathBuf> {
    names.iter().find_map(|name| {
        ["/usr/bin", "/usr/local/bin", "/bin"].iter().map(|dir| Path::new(dir).join(name)).find(|p| p.is_file())
    })
}

fn espeak() -> anyhow::Result<PathBuf> {
    find_program(&["espeak-ng", "espeak"])
        .context("eSpeak NG is not installed. Install the espeak-ng package to use system voices")
}

pub fn system_voices() -> anyhow::Result<Vec<Voice>> {
    let output = Command::new(espeak()?)
        .arg("--voices")
        .stdin(Stdio::null())
        .output()
        .context("could not run eSpeak NG")?;
    if !output.status.success() {
        bail!("eSpeak NG could not list voices");
    }
    Ok(parse_voice_list(&String::from_utf8_lossy(&output.stdout)))
}

/// Parses the table printed by `espeak-ng --voices`:
/// `Pty Language       Age/Gender VoiceName          File                 Other Languages`
fn parse_voice_list(listing: &str) -> Vec<Voice> {
    listing
        .lines()
        .skip(1)
        .filter_map(|line| {
            let columns: Vec<&str> = line.split_whitespace().collect();
            let language = *columns.get(1)?;
            let name = columns.get(3)?.replace('_', " ");
            Some(Voice { id: language.to_owned(), name: format!("{name} ({language})") })
        })
        .collect()
}

pub fn synthesize(text: &str, voice_id: &str) -> anyhow::Result<Vec<u8>> {
    let out = tempfile::Builder::new()
        .prefix("commonspeech-")
        .suffix(".wav")
        .tempfile()
        .context("could not create a temporary audio file")?;
    let mut command = Command::new(espeak()?);
    if !voice_id.is_empty() {
        command.arg("-v").arg(voice_id);
    }
    command
        .arg("-w")
        .arg(out.path())
        .arg("--stdin")
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::piped());
    let mut child = command.spawn().context("could not run eSpeak NG")?;
    // Without -m, eSpeak NG reads no SSML, but it still takes [[phonemes]].
    child
        .stdin
        .take()
        .context("could not send text to eSpeak NG")?
        .write_all(without_embedded_commands(text).as_bytes())?;
    let result = child.wait_with_output()?;
    if !result.status.success() {
        log::warn!("espeak-ng failed: {}", String::from_utf8_lossy(&result.stderr).trim());
        bail!("eSpeak NG could not speak the text");
    }
    Ok(std::fs::read(out.path())?)
}

pub fn heic_to_jpeg(path: &Path) -> anyhow::Result<Vec<u8>> {
    let converter = find_program(&["heif-convert", "heif-dec"])
        .context("HEIC photos need libheif. Install the libheif-examples package")?;
    let out = tempfile::Builder::new()
        .prefix("commonspeech-")
        .suffix(".jpg")
        .tempfile()
        .context("could not create a temporary image file")?;
    let status = Command::new(converter)
        .arg("--")
        .arg(path)
        .arg(out.path())
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .context("could not run the HEIC converter")?;
    if !status.success() {
        bail!("libheif could not convert this HEIC image");
    }
    Ok(std::fs::read(out.path())?)
}

/// Starts the server for this session, if Ollama is installed. The snap runs
/// it as a service by itself, so there is nothing to start.
pub fn start_ollama() -> bool {
    let Some(ollama) = find_program(&["ollama"]) else { return false };
    Command::new(ollama)
        .arg("serve")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .is_ok()
}

pub fn whisper_programs() -> Vec<PathBuf> {
    crate::transcribe::whisper::PROGRAM_NAMES.iter().filter_map(|n| find_program(&[*n])).collect()
}

/// Ubuntu doesn't package whisper.cpp, so there's nothing to offer.
pub fn whisper_installer() -> Option<crate::transcribe::whisper::Installer> {
    None
}

pub const WHISPER_HELP: &str = "whisper.cpp isn't installed. Build it from github.com/ggml-org/whisper.cpp and \
     put whisper-cli in /usr/local/bin, or choose the whisper-cli program on the Settings tab";

#[cfg(test)]
mod tests {
    use super::*;

    /// The real eSpeak NG, which CI installs and the .deb depends on.
    #[test]
    fn real_voices_speak() {
        let voices = system_voices().unwrap();
        assert!(voices.iter().any(|v| v.id.starts_with("en")), "{voices:?}");
        let pcm = crate::audio::decode(synthesize("Hello there.", "en-gb").unwrap()).unwrap();
        assert!(pcm.samples.len() > 1000);
    }

    #[test]
    fn parses_voice_table() {
        let voices = parse_voice_list(
            "Pty Language       Age/Gender VoiceName          File                 Other Languages\n \
             5  en-gb           --/M      English_(Great_Britain) gmw/en             (en 2)\n",
        );
        assert_eq!(voices, [Voice { id: "en-gb".into(), name: "English (Great Britain) (en-gb)".into() }]);
    }
}
