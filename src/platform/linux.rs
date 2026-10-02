//! Linux and other Unix systems: system voices come from eSpeak NG, which is
//! packaged by every major distribution and is what Orca users usually have
//! installed. HEIC conversion uses `heif-convert` from libheif. Programs are
//! started directly, never through a shell.
//!
//! This file also holds the release packaging step for Debian and Ubuntu:
//! `speechout --package-deb <dir>` writes a `.deb` containing the running
//! binary, assembled in Rust (ar + tar + gzip) so that the release workflow
//! needs no shell script or extra packaging tools.

use super::SecretStore;
use crate::speech::Voice;
use anyhow::{Context, bail};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

pub const SECRET_STORE_DESCRIPTION: &str = "a private file in your configuration folder";

/// Prefer the copy in /usr/bin so a writable directory early in PATH cannot
/// substitute a different program.
fn find_program(names: &[&str]) -> Option<PathBuf> {
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

pub fn secret_get(_name: &str) -> SecretStore<Option<String>> {
    SecretStore::Unsupported
}

pub fn secret_set(_name: &str, _value: &str) -> SecretStore<()> {
    SecretStore::Unsupported
}

pub fn secret_delete(_name: &str) -> SecretStore<()> {
    SecretStore::Unsupported
}

const DEB_DEPENDS: &str = "libc6 (>= 2.35), libgcc-s1, libasound2t64 | libasound2, \
libgl1, libxkbcommon0, libxkbcommon-x11-0, libwayland-client0, espeak-ng";
const DEB_RECOMMENDS: &str = "libheif-examples, xdg-desktop-portal, at-spi2-core, orca";

const DESKTOP_ENTRY: &str = "[Desktop Entry]
Type=Application
Name=Speech Output Engine
GenericName=Text to speech reader
Comment=Read documents and describe photos aloud
Exec=speechout
Icon=audio-speakers
Terminal=false
Categories=Utility;Accessibility;AudioVideo;Audio;
Keywords=speech;tts;screen reader;accessibility;pdf;docx;csv;
";

const COPYRIGHT: &str = "Format: https://www.debian.org/doc/packaging-manuals/copyright-format/1.0/
Upstream-Name: speechout
Source: https://github.com/mediaswing/speechout

Files: *
Copyright: 2026 The Speech Output Engine contributors
License: GPL-3.0-or-later
 On Debian systems, the full text of the GNU General Public License
 version 3 can be found in /usr/share/common-licenses/GPL-3.

Files: embedded fonts (Google Sans Medium and Bold)
Copyright: Google LLC
License: OFL-1.1
 See /usr/share/doc/speechout/fonts-OFL.txt.
";

fn deb_arch() -> &'static str {
    match std::env::consts::ARCH {
        "x86_64" => "amd64",
        "aarch64" => "arm64",
        "x86" => "i386",
        "arm" => "armhf",
        other => other,
    }
}

/// One file to place in the package: (path inside the package, mode, bytes).
type DebFile = (&'static str, u32, Vec<u8>);

fn tar_gz(dirs: &[&str], files: &[DebFile], mtime: u64) -> anyhow::Result<Vec<u8>> {
    let gz = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::best());
    let mut tar = tar::Builder::new(gz);
    for dir in dirs {
        let mut h = tar::Header::new_gnu();
        h.set_entry_type(tar::EntryType::Directory);
        h.set_mode(0o755);
        h.set_uid(0);
        h.set_gid(0);
        h.set_mtime(mtime);
        h.set_size(0);
        tar.append_data(&mut h, dir, std::io::empty())?;
    }
    for (path, mode, data) in files {
        let mut h = tar::Header::new_gnu();
        h.set_entry_type(tar::EntryType::Regular);
        h.set_mode(*mode);
        h.set_uid(0);
        h.set_gid(0);
        h.set_mtime(mtime);
        h.set_size(data.len() as u64);
        tar.append_data(&mut h, path, data.as_slice())?;
    }
    Ok(tar.into_inner()?.finish()?)
}

fn ar_member(out: &mut Vec<u8>, name: &str, data: &[u8], mtime: u64) {
    out.extend_from_slice(
        format!("{:<16}{:<12}{:<6}{:<6}{:<8}{:<10}`\n", name, mtime, 0, 0, "100644", data.len())
            .as_bytes(),
    );
    out.extend_from_slice(data);
    if data.len() % 2 == 1 {
        out.push(b'\n');
    }
}

/// Builds `speechout_<version>_<arch>.deb` in `out_dir` from the running binary.
pub fn package(out_dir: &Path) -> anyhow::Result<PathBuf> {
    let exe = std::env::current_exe().context("could not locate the running executable")?;
    let binary = std::fs::read(&exe)?;
    let version = env!("CARGO_PKG_VERSION");
    let mtime = std::env::var("SOURCE_DATE_EPOCH")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or_else(|| {
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_secs())
                .unwrap_or(0)
        });

    let files: Vec<DebFile> = vec![
        ("./usr/bin/speechout", 0o755, binary),
        ("./usr/share/applications/speechout.desktop", 0o644, DESKTOP_ENTRY.as_bytes().to_vec()),
        ("./usr/share/doc/speechout/copyright", 0o644, COPYRIGHT.as_bytes().to_vec()),
        (
            "./usr/share/doc/speechout/fonts-OFL.txt",
            0o644,
            include_bytes!("../../assets/fonts/OFL.txt").to_vec(),
        ),
    ];
    let dirs = [
        "./",
        "./usr/",
        "./usr/bin/",
        "./usr/share/",
        "./usr/share/applications/",
        "./usr/share/doc/",
        "./usr/share/doc/speechout/",
    ];
    let installed_kib = files.iter().map(|f| f.2.len() as u64).sum::<u64>().div_ceil(1024);

    let control = format!(
        "Package: speechout\n\
         Version: {version}\n\
         Architecture: {arch}\n\
         Maintainer: wrichards <mediaswing@outlook.com>\n\
         Installed-Size: {installed_kib}\n\
         Depends: {DEB_DEPENDS}\n\
         Recommends: {DEB_RECOMMENDS}\n\
         Section: sound\n\
         Priority: optional\n\
         Homepage: https://github.com/mediaswing/speechout\n\
         Description: Speech Output Engine, an accessible text-to-speech reader\n \
         Reads PDF, TXT, DOCX and CSV files aloud with system or cloud voices, saves\n \
         speech as WAV or MP3, describes photos with a local AI model and applies\n \
         XML pronunciation wordlists. Fully usable by keyboard and screen reader.\n",
        arch = deb_arch()
    );

    let control_tar = tar_gz(&["./"], &[("./control", 0o644, control.into_bytes())], mtime)?;
    let data_tar = tar_gz(&dirs, &files, mtime)?;

    let mut deb = b"!<arch>\n".to_vec();
    ar_member(&mut deb, "debian-binary", b"2.0\n", mtime);
    ar_member(&mut deb, "control.tar.gz", &control_tar, mtime);
    ar_member(&mut deb, "data.tar.gz", &data_tar, mtime);

    std::fs::create_dir_all(out_dir)?;
    let path = out_dir.join(format!("speechout_{version}_{}.deb", deb_arch()));
    std::fs::write(&path, deb)?;
    Ok(path)
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

    /// Packages the test binary and, where dpkg is installed (as on the
    /// release runner), checks that dpkg accepts the result.
    #[test]
    fn builds_valid_deb() {
        let dir = tempfile::tempdir().unwrap();
        let deb = package(dir.path()).unwrap();
        let bytes = std::fs::read(&deb).unwrap();
        assert!(bytes.starts_with(b"!<arch>\ndebian-binary   "));
        let dpkg = Path::new("/usr/bin/dpkg-deb");
        if dpkg.exists() {
            let info = Command::new(dpkg).arg("--info").arg(&deb).output().unwrap();
            assert!(info.status.success(), "{}", String::from_utf8_lossy(&info.stderr));
            let contents = Command::new(dpkg).arg("--contents").arg(&deb).output().unwrap();
            assert!(String::from_utf8_lossy(&contents.stdout).contains("usr/bin/speechout"));
        }
    }
}
