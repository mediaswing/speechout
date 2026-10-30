//! Linux and other Unix systems: system voices come from eSpeak NG, which is
//! packaged by every major distribution and is what Orca users usually have
//! installed. HEIC conversion uses `heif-convert` from libheif. Programs are
//! started directly, never through a shell.
//!
//! The release packaging step for Debian and Ubuntu is in `crate::package`.

use super::SecretStore;
use crate::speech::Voice;
use anyhow::{Context, bail};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

pub const SECRET_STORE_DESCRIPTION: &str = "a private file in your configuration folder";

/// Prefer the copy in /usr/bin so a writable directory early in PATH cannot
/// substitute a different program.
pub(super) fn find_program(names: &[&str]) -> Option<PathBuf> {
    for name in names {
        for dir in ["/usr/bin", "/usr/local/bin", "/bin"] {
            let candidate = Path::new(dir).join(name);
            if candidate.is_file() {
                return Some(candidate);
            }
        }
    }
    None
}

fn espeak() -> anyhow::Result<PathBuf> {
    find_program(&["espeak-ng", "espeak"])
        .context("eSpeak NG is not installed. Install the espeak-ng package to use system voices.")
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
            let cols: Vec<&str> = line.split_whitespace().collect();
            if cols.len() < 4 {
                return None;
            }
            let language = cols[1];
            let name = cols[3].replace('_', " ");
            Some(Voice { id: language.to_owned(), name: format!("{name} ({language})") })
        })
        .collect()
}

pub fn synthesize(text: &str, voice_id: &str) -> anyhow::Result<Vec<u8>> {
    let out = tempfile::Builder::new()
        .prefix("speechout-")
        .suffix(".wav")
        .tempfile()
        .context("could not create a temporary audio file")?;
    let mut cmd = Command::new(espeak()?);
    if !voice_id.is_empty() {
        cmd.arg("-v").arg(voice_id);
    }
    cmd.arg("-w")
        .arg(out.path())
        .arg("--stdin")
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::piped());
    let mut child = cmd.spawn().context("could not run eSpeak NG")?;
    child
        .stdin
        .take()
        .context("could not send text to eSpeak NG")?
        .write_all(text.as_bytes())?;
    let result = child.wait_with_output()?;
    if !result.status.success() {
        log::warn!("espeak-ng failed: {}", String::from_utf8_lossy(&result.stderr).trim());
        bail!("eSpeak NG failed to speak the text");
    }
    Ok(std::fs::read(out.path())?)
}

pub fn heic_to_jpeg(path: &Path) -> anyhow::Result<Vec<u8>> {
    let converter = find_program(&["heif-convert", "heif-dec"])
        .context("HEIC support needs libheif. Install the libheif-examples package.")?;
    let out = tempfile::Builder::new()
        .prefix("speechout-")
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

pub fn open_url(url: &str) -> anyhow::Result<()> {
    let opener = find_program(&["xdg-open"]).context("xdg-open is not installed")?;
    // xdg-open returns straight away; the browser keeps running on its own.
    Command::new(opener)
        .arg(url)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .context("could not open the web browser")?;
    Ok(())
}

const SNAP_OLLAMA: &str = "/snap/bin/ollama";

fn ollama() -> Option<PathBuf> {
    find_program(&["ollama"]).or_else(|| Some(PathBuf::from(SNAP_OLLAMA)).filter(|p| p.is_file()))
}

pub fn ollama_installed() -> bool {
    ollama().is_some()
}

/// Ollama is installed as a snap, which works on most distributions. pkexec
/// asks for the administrator password in a desktop dialog.
pub fn package_manager() -> Option<&'static str> {
    (find_program(&["snap"]).is_some() && find_program(&["pkexec"]).is_some()).then_some("Snap")
}

pub fn install_ollama() -> anyhow::Result<()> {
    let snap = find_program(&["snap"]).context("Snap is not installed")?;
    let pkexec = find_program(&["pkexec"]).context("pkexec is not installed")?;
    let output = Command::new(pkexec)
        .arg(snap)
        .args(["install", "ollama"])
        .stdin(Stdio::null())
        .output()
        .context("could not run snap")?;
    match output.status.code() {
        Some(0) => Ok(()),
        // pkexec: the password dialog was dismissed or refused.
        Some(126 | 127) => bail!("the administrator password was not given"),
        _ => {
            log::warn!("snap failed ({}): {}", output.status, String::from_utf8_lossy(&output.stderr).trim());
            bail!("Snap could not install Ollama")
        }
    }
}

/// The snap runs Ollama as a service by itself. Otherwise, start the server
/// for this session.
pub fn start_ollama() -> anyhow::Result<()> {
    let ollama = ollama().context("Ollama is not installed")?;
    if ollama.as_path() == Path::new(SNAP_OLLAMA) {
        return Ok(());
    }
    Command::new(ollama)
        .arg("serve")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .context("could not start Ollama")?;
    Ok(())
}

pub fn ffmpeg() -> Option<PathBuf> {
    find_program(&["ffmpeg"])
}

/// The distribution's own package manager, run through pkexec, which asks
/// for the administrator password in a desktop dialog: the program, its
/// name, and the arguments that install ffmpeg. Fedora's package is
/// ffmpeg-free, the build without patented codecs it is allowed to ship.
fn ffmpeg_installer() -> Option<(PathBuf, &'static str, &'static [&'static str])> {
    find_program(&["pkexec"])?;
    if let Some(apt) = find_program(&["apt-get"]) {
        return Some((apt, "APT", &["install", "-y", "ffmpeg"][..]));
    }
    find_program(&["dnf"]).map(|dnf| (dnf, "DNF", &["install", "-y", "ffmpeg-free"][..]))
}

pub fn ffmpeg_package_manager() -> Option<&'static str> {
    ffmpeg_installer().map(|(_, name, _)| name)
}

pub fn install_ffmpeg() -> anyhow::Result<()> {
    let (program, name, args) = ffmpeg_installer().context("no package manager was found")?;
    let pkexec = find_program(&["pkexec"]).context("pkexec is not installed")?;
    let output = Command::new(pkexec)
        .arg(program)
        .args(args)
        .stdin(Stdio::null())
        .output()
        .with_context(|| format!("could not run {name}"))?;
    match output.status.code() {
        Some(0) => Ok(()),
        // pkexec: the password dialog was dismissed or refused.
        Some(126 | 127) => bail!("the administrator password was not given"),
        _ => {
            log::warn!("{name} failed ({}): {}", output.status, String::from_utf8_lossy(&output.stderr).trim());
            bail!("{name} could not install ffmpeg")
        }
    }
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
    fn parses_voice_table() {
        let voices = parse_voice_list(
            "Pty Language       Age/Gender VoiceName          File                 Other Languages\n \
             5  en-gb           --/M      English_(Great_Britain) gmw/en             (en 2)\n",
        );
        assert_eq!(voices.len(), 1);
        assert_eq!(voices[0].id, "en-gb");
        assert_eq!(voices[0].name, "English (Great Britain) (en-gb)");
    }

}

/// Orca is the screen reader on Linux desktops.
pub fn screen_reader_running() -> bool {
    let Some(pgrep) = find_program(&["pgrep"]) else { return false };
    Command::new(pgrep)
        .args(["-x", "orca"])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|s| s.success())
}

pub use super::unix::{become_background_reader, stop_background_reader};
