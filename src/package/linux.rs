//! The Debian and Ubuntu package: `speechout --package-deb <dir>` writes a
//! `.deb` containing the running binary, assembled in Rust (ar + tar + xz) so
//! that the release workflow needs no shell script or extra packaging tools.

use anyhow::Context;
use std::path::{Path, PathBuf};

const DEB_DEPENDS: &str = "libc6 (>= 2.35), libgcc-s1, libstdc++6, libasound2t64 | libasound2, \
libgl1, libxkbcommon0, libxkbcommon-x11-0, libwayland-client0, espeak-ng";
const DEB_RECOMMENDS: &str = "libheif-examples, xdg-desktop-portal, at-spi2-core, orca";

const DESKTOP_ENTRY: &str = "[Desktop Entry]
Type=Application
Name=Speech Output Engine
GenericName=Text to speech reader
Comment=Read documents and describe photos aloud
Exec=speechout
Icon=speechout
Terminal=false
Categories=Utility;Accessibility;AudioVideo;Audio;
Keywords=speech;tts;screen reader;accessibility;pdf;markdown;docx;odt;csv;ods;pptx;ppt;powerpoint;epub;ebook;
";

const COPYRIGHT: &str = "Format: https://www.debian.org/doc/packaging-manuals/copyright-format/1.0/
Upstream-Name: speechout
Source: https://github.com/mediaswing/speechout

Files: *
Copyright: 2026 The Speech Output Engine contributors
License: GPL-3.0-or-later
 On Debian systems, the full text of the GNU General Public License
 version 3 can be found in /usr/share/common-licenses/GPL-3.

Files: usr/share/icons/hicolor/*/apps/speechout.*
Copyright: Laura Reen (Xomo: basics icon set)
License: other
 From https://icon-icons.com/icon/chat-bubbles-speech-conversation-writing/121951

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

/// xz rather than gzip makes the package about a third smaller. dpkg has read
/// xz since 1.17, long before Ubuntu 22.04.
fn tar_xz(dirs: &[&str], files: &[DebFile], mtime: u64) -> anyhow::Result<Vec<u8>> {
    let xz = xz2::write::XzEncoder::new(Vec::new(), 9);
    let mut tar = tar::Builder::new(xz);
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
pub fn build(out_dir: &Path) -> anyhow::Result<PathBuf> {
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
        (
            "./usr/share/icons/hicolor/256x256/apps/speechout.png",
            0o644,
            include_bytes!("../../assets/icon/speechout.png").to_vec(),
        ),
        (
            "./usr/share/icons/hicolor/scalable/apps/speechout.svg",
            0o644,
            include_bytes!("../../assets/icon/speechout.svg").to_vec(),
        ),
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
        "./usr/share/icons/",
        "./usr/share/icons/hicolor/",
        "./usr/share/icons/hicolor/256x256/",
        "./usr/share/icons/hicolor/256x256/apps/",
        "./usr/share/icons/hicolor/scalable/",
        "./usr/share/icons/hicolor/scalable/apps/",
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
         Reads PDF, TXT, Markdown, DOCX, ODT, CSV, ODS, PPTX, PPT and EPUB files\n \
         aloud with system or cloud voices, saves speech as WAV or MP3, describes\n \
         photos with a local AI model, plays WAV and MP3 files and transcribes the\n \
         speech in them with Whisper, and applies XML pronunciation wordlists.\n \
         Fully usable by keyboard and screen reader.\n",
        arch = deb_arch()
    );

    let control_tar = tar_xz(&["./"], &[("./control", 0o644, control.into_bytes())], mtime)?;
    let data_tar = tar_xz(&dirs, &files, mtime)?;

    let mut deb = b"!<arch>\n".to_vec();
    ar_member(&mut deb, "debian-binary", b"2.0\n", mtime);
    ar_member(&mut deb, "control.tar.xz", &control_tar, mtime);
    ar_member(&mut deb, "data.tar.xz", &data_tar, mtime);

    std::fs::create_dir_all(out_dir)?;
    let path = out_dir.join(format!("speechout_{version}_{}.deb", deb_arch()));
    std::fs::write(&path, deb)?;
    Ok(path)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Command;

    /// Packages the test binary and, where dpkg is installed (as on the
    /// release runner), checks that dpkg accepts the result.
    #[test]
    fn builds_valid_deb() {
        let dir = tempfile::tempdir().unwrap();
        let deb = build(dir.path()).unwrap();
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
