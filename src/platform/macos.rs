//! macOS: system voices come from the `say` command, which uses the same
//! speech synthesiser as VoiceOver, and HEIC conversion uses `sips`. Both ship
//! with every copy of macOS.

use std::io::Write;
use std::path::Path;
use std::process::{Command, Stdio};

use anyhow::{Context, bail};

pub use super::unix::{create_private_file, restrict_dir_to_owner};
use super::unix::without_embedded_commands;
use crate::speech::Voice;

const SAY: &str = "/usr/bin/say";
const SIPS: &str = "/usr/bin/sips";
const OPEN: &str = "/usr/bin/open";
const OLLAMA_APP: &str = "/Applications/Ollama.app";
const OLLAMA_CLI: [&str; 2] = ["/opt/homebrew/bin/ollama", "/usr/local/bin/ollama"];

pub fn system_voices() -> anyhow::Result<Vec<Voice>> {
    let output = Command::new(SAY)
        .arg("--voice=?")
        .stdin(Stdio::null())
        .output()
        .context("could not run the macOS speech command")?;
    if !output.status.success() {
        bail!("the macOS speech command could not list voices");
    }
    Ok(parse_voice_list(&String::from_utf8_lossy(&output.stdout)))
}

/// Parses lines such as `Eddy (English (UK)) en_GB    # Hello! My name is Eddy.`
fn parse_voice_list(listing: &str) -> Vec<Voice> {
    let mut voices = Vec::new();
    for line in listing.lines() {
        let Some((head, _sample)) = line.split_once('#') else { continue };
        let head = head.trim_end();
        let Some((name, locale)) = head.rsplit_once(char::is_whitespace) else { continue };
        let name = name.trim();
        if !name.is_empty() {
            voices.push(Voice { id: name.to_owned(), name: format!("{name} ({})", locale.trim().replace('_', "-")) });
        }
    }
    voices
}

pub fn synthesize(text: &str, voice_id: &str) -> anyhow::Result<Vec<u8>> {
    let out = tempfile::Builder::new()
        .prefix("commonspeech-")
        .suffix(".wav")
        .tempfile()
        .context("could not create a temporary audio file")?;
    let mut command = Command::new(SAY);
    if !voice_id.is_empty() {
        // `--voice=NAME` keeps a name that begins with "-" from being read as an option.
        command.arg(format!("--voice={voice_id}"));
    }
    command
        .arg("--file-format=WAVE")
        .arg("--data-format=LEI16@22050")
        .arg("-o")
        .arg(out.path())
        .arg("--input-file=-")
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::piped());
    let mut child = command.spawn().context("could not run the macOS speech command")?;
    child
        .stdin
        .take()
        .context("could not send text to the speech command")?
        .write_all(without_embedded_commands(text).as_bytes())?;
    let result = child.wait_with_output()?;
    if !result.status.success() {
        log::warn!("say failed: {}", String::from_utf8_lossy(&result.stderr).trim());
        bail!("the macOS speech command failed");
    }
    Ok(std::fs::read(out.path())?)
}

pub fn heic_to_jpeg(path: &Path) -> anyhow::Result<Vec<u8>> {
    let out = tempfile::Builder::new()
        .prefix("commonspeech-")
        .suffix(".jpg")
        .tempfile()
        .context("could not create a temporary image file")?;
    let status = Command::new(SIPS)
        .args(["-s", "format", "jpeg"])
        .arg(path)
        .arg("--out")
        .arg(out.path())
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .context("could not run the macOS image converter")?;
    if !status.success() {
        bail!("macOS could not convert this HEIC image");
    }
    Ok(std::fs::read(out.path())?)
}

/// Opens the Ollama app, which runs its server in the background, or starts the
/// server alone if only the command-line tool is installed.
pub fn start_ollama() -> bool {
    if Path::new(OLLAMA_APP).exists() {
        return Command::new(OPEN).args(["-g", "-a", OLLAMA_APP]).stdin(Stdio::null()).status().is_ok_and(|s| s.success());
    }
    let Some(cli) = OLLAMA_CLI.into_iter().find(|p| Path::new(p).is_file()) else { return false };
    Command::new(cli)
        .arg("serve")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .is_ok()
}

/// Homebrew's two homes: Apple Silicon, then Intel.
const BREW_PREFIXES: [&str; 2] = ["/opt/homebrew", "/usr/local"];

pub fn whisper_programs() -> Vec<std::path::PathBuf> {
    BREW_PREFIXES
        .iter()
        .flat_map(|prefix| crate::transcribe::whisper::PROGRAM_NAMES.iter().map(move |n| Path::new(prefix).join("bin").join(n)))
        .collect()
}

/// Homebrew, if it is installed.
pub fn whisper_installer() -> Option<crate::transcribe::whisper::Installer> {
    let brew = BREW_PREFIXES.iter().map(|p| Path::new(p).join("bin/brew")).find(|p| p.is_file())?;
    Some(crate::transcribe::whisper::Installer {
        program: brew,
        args: &["install", "whisper-cpp"],
        command: "brew install whisper-cpp",
    })
}

pub const WHISPER_HELP: &str = "whisper.cpp isn't installed. Install Homebrew from brew.sh and run \
     \"brew install whisper-cpp\", or choose the whisper-cli program on the Settings tab";

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_voice_listing() {
        let voices = parse_voice_list(
            "Alex                en_US    # Most people recognize me by my voice.\n\
             Eddy (English (UK)) en_GB    # Hello! My name is Eddy.\n",
        );
        assert_eq!(voices.len(), 2);
        assert_eq!(voices[1].id, "Eddy (English (UK))");
        assert_eq!(voices[1].name, "Eddy (English (UK)) (en-GB)");
    }

    #[test]
    fn real_voices_speak() {
        let voices = system_voices().unwrap();
        assert!(!voices.is_empty());
        let pcm = crate::audio::decode(synthesize("Hello there.", "").unwrap()).unwrap();
        assert!(pcm.samples.len() > 1000);
    }

    #[test]
    fn real_heic_converts() {
        // sips can write HEIC, so make one to read back.
        let dir = tempfile::tempdir().unwrap();
        let jpeg = dir.path().join("a.jpg");
        let heic = dir.path().join("a.heic");
        std::fs::write(&jpeg, crate::vision::geotagged_jpeg(None, (64, 64))).unwrap();
        let status = Command::new(SIPS)
            .args(["-s", "format", "heic"])
            .arg(&jpeg)
            .arg("--out")
            .arg(&heic)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .unwrap();
        assert!(status.success());
        let decoded = image::load_from_memory(&heic_to_jpeg(&heic).unwrap()).unwrap();
        assert_eq!((decoded.width(), decoded.height()), (64, 64));
        std::fs::write(&heic, b"not a photo").unwrap();
        assert!(heic_to_jpeg(&heic).is_err());
    }
}
