//! macOS: system voices come from the `say` command (backed by the same
//! speech synthesiser VoiceOver uses) and HEIC conversion uses `sips`
//! (backed by ImageIO). Both tools ship with every copy of macOS. They are
//! started directly by absolute path, never through a shell, so file names and
//! voice names cannot be interpreted as commands.
//!
//! The release packaging step for macOS is in `crate::package`.

use super::SecretStore;
use crate::speech::Voice;
use anyhow::{Context, bail};
use std::io::Write;
use std::path::Path;
use std::process::{Command, Stdio};

const SAY: &str = "/usr/bin/say";
const SIPS: &str = "/usr/bin/sips";
const OPEN: &str = "/usr/bin/open";

pub const SECRET_STORE_DESCRIPTION: &str = "a private file in your Application Support folder";

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
        if name.is_empty() {
            continue;
        }
        voices.push(Voice {
            id: name.to_owned(),
            name: format!("{name} ({})", locale.trim().replace('_', "-")),
        });
    }
    voices
}

pub fn synthesize(text: &str, voice_id: &str) -> anyhow::Result<Vec<u8>> {
    let out = tempfile::Builder::new()
        .prefix("speechout-")
        .suffix(".wav")
        .tempfile()
        .context("could not create a temporary audio file")?;

    let mut cmd = Command::new(SAY);
    if !voice_id.is_empty() {
        // `--voice=NAME` keeps a name that begins with "-" from being read as an option.
        cmd.arg(format!("--voice={voice_id}"));
    }
    cmd.arg("--file-format=WAVE")
        .arg("--data-format=LEI16@22050")
        .arg("-o")
        .arg(out.path())
        // Text is passed on standard input, so it never appears in the process list.
        .arg("--input-file=-")
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::piped());

    let mut child = cmd.spawn().context("could not run the macOS speech command")?;
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

/// The speech synthesiser obeys commands written between double brackets,
/// such as `[[volm 0]]` (silence) or `[[slnc 5000]]` (a pause), so a document
/// could change how it is read. Shrinking every run of opening brackets to one
/// means no command can start, including `[[dlim]]`, which would change the
/// brackets themselves.
fn without_embedded_commands(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        if !(c == '[' && out.ends_with('[')) {
            out.push(c);
        }
    }
    out
}

pub fn heic_to_jpeg(path: &Path) -> anyhow::Result<Vec<u8>> {
    let out = tempfile::Builder::new()
        .prefix("speechout-")
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

pub fn open_url(url: &str) -> anyhow::Result<()> {
    let status = Command::new(OPEN)
        .arg("--")
        .arg(url)
        .stdin(Stdio::null())
        .status()
        .context("could not open the web browser")?;
    if !status.success() {
        bail!("could not open the web browser");
    }
    Ok(())
}

const OLLAMA_APP: &str = "/Applications/Ollama.app";
const BREW: [&str; 2] = ["/opt/homebrew/bin/brew", "/usr/local/bin/brew"];
const OLLAMA_CLI: [&str; 2] = ["/opt/homebrew/bin/ollama", "/usr/local/bin/ollama"];

pub fn ollama_installed() -> bool {
    Path::new(OLLAMA_APP).exists() || OLLAMA_CLI.iter().any(|p| Path::new(p).is_file())
}

fn brew() -> Option<&'static str> {
    BREW.into_iter().find(|p| Path::new(p).is_file())
}

pub fn package_manager() -> Option<&'static str> {
    brew().map(|_| "Homebrew")
}

/// Installs the Ollama app with Homebrew.
pub fn install_ollama() -> anyhow::Result<()> {
    let brew = brew().context("Homebrew is not installed")?;
    let output = Command::new(brew)
        .args(["install", "--cask", "ollama-app"])
        .env("HOMEBREW_NO_ENV_HINTS", "1")
        .stdin(Stdio::null())
        .output()
        .context("could not run Homebrew")?;
    if !output.status.success() {
        log::warn!("brew failed ({}): {}", output.status, String::from_utf8_lossy(&output.stderr).trim());
        bail!("Homebrew could not install Ollama");
    }
    Ok(())
}

/// Opens the Ollama app, which runs the server in the background, or starts
/// the server alone if only the command-line tool is installed.
pub fn start_ollama() -> anyhow::Result<()> {
    if Path::new(OLLAMA_APP).exists() {
        let status = Command::new(OPEN).args(["-g", "-a", OLLAMA_APP]).stdin(Stdio::null()).status()?;
        if !status.success() {
            bail!("could not open Ollama");
        }
        return Ok(());
    }
    let cli = OLLAMA_CLI.into_iter().find(|p| Path::new(p).is_file()).context("Ollama is not installed")?;
    Command::new(cli)
        .arg("serve")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .context("could not start Ollama")?;
    Ok(())
}

/// Where Homebrew (Apple silicon, then Intel) and MacPorts put ffmpeg. Apps
/// opened from the Finder don't get the PATH a terminal has.
const FFMPEG: [&str; 3] = ["/opt/homebrew/bin/ffmpeg", "/usr/local/bin/ffmpeg", "/opt/local/bin/ffmpeg"];

pub fn ffmpeg() -> Option<std::path::PathBuf> {
    FFMPEG.into_iter().map(std::path::PathBuf::from).find(|p| p.is_file())
}

pub fn ffmpeg_package_manager() -> Option<&'static str> {
    package_manager()
}

/// Installs ffmpeg with Homebrew.
pub fn install_ffmpeg() -> anyhow::Result<()> {
    let brew = brew().context("Homebrew is not installed")?;
    let output = Command::new(brew)
        .args(["install", "ffmpeg"])
        .env("HOMEBREW_NO_ENV_HINTS", "1")
        .stdin(Stdio::null())
        .output()
        .context("could not run Homebrew")?;
    if !output.status.success() {
        log::warn!("brew failed ({}): {}", output.status, String::from_utf8_lossy(&output.stderr).trim());
        bail!("Homebrew could not install ffmpeg");
    }
    Ok(())
}

pub fn secret_get(_name: &str) -> SecretStore<Option<String>> {
    SecretStore::Unsupported
}

pub fn secret_set(_name: &str, _value: &str) -> SecretStore<()> {
    SecretStore::Unsupported
}

pub fn secret_delete(_name: &str) -> SecretStore<()> {
    SecretStore::Unsupported
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn embedded_commands_are_neutralised() {
        assert_eq!(without_embedded_commands("a [[volm 0]] b"), "a [volm 0]] b");
        assert_eq!(without_embedded_commands("[[[slnc 5000]]"), "[slnc 5000]]");
        assert_eq!(without_embedded_commands("[a] [ [b]"), "[a] [ [b]");
    }

    #[test]
    fn parses_voice_listing() {
        let voices = parse_voice_list(
            "Alex                en_US    # Most people recognize me by my voice.\n\
             Eddy (English (UK)) en_GB    # Hello! My name is Eddy.\n",
        );
        assert_eq!(voices.len(), 2);
        assert_eq!(voices[0].id, "Alex");
        assert_eq!(voices[1].id, "Eddy (English (UK))");
        assert_eq!(voices[1].name, "Eddy (English (UK)) (en-GB)");
    }
}

#[cfg(test)]
mod system_tests {
    /// Uses the real speech synthesiser; run with `cargo test -- --ignored`.
    #[test]
    #[ignore]
    fn speaks_to_wav() {
        let voices = super::system_voices().unwrap();
        assert!(!voices.is_empty());
        let wav = super::synthesize("Hello from the Speech Output Engine.", &voices[0].id).unwrap();
        let pcm = crate::audio::decode(wav).unwrap();
        assert!(pcm.samples.len() > 10_000);
    }
}

/// VoiceOver runs as its own process while it is on.
pub fn screen_reader_running() -> bool {
    Command::new("/usr/bin/pgrep")
        .args(["-x", "VoiceOver"])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|s| s.success())
}

pub use super::unix::{become_background_reader, stop_background_reader};
