//! The Ubuntu package: `speechout --package-deb <dir>` writes a `.deb`
//! containing the running binary, assembled in Rust (ar + tar + xz), so that
//! releasing needs no shell script or extra packaging tools.

use std::path::{Path, PathBuf};

use anyhow::Context;

const DEB_DEPENDS: &str = "libc6 (>= 2.35), libgcc-s1, libstdc++6, libasound2t64 | libasound2, \
libgl1, libxkbcommon0, libxkbcommon-x11-0, libwayland-client0, espeak-ng";
const DEB_RECOMMENDS: &str = "libheif-examples, xdg-desktop-portal, at-spi2-core, orca";

/// `Exec`, `StartupWMClass` and the file's own name are the executable's name,
/// `crate::BIN_NAME`, which the window gives as its app id (src/main.rs), so
/// Wayland desktops can match the window to this entry and its icon.
const DESKTOP_ENTRY: &str = "[Desktop Entry]
Type=Application
Name=Common Speech Output
GenericName=Text to speech reader
Comment=Read documents aloud and describe photos
Exec=speechout
Icon=commonspeech
Terminal=false
Categories=Utility;Accessibility;AudioVideo;Audio;
Keywords=speech;tts;screen reader;accessibility;pdf;docx;csv;mp3;
StartupWMClass=speechout
";

const COPYRIGHT: &str = "Format: https://www.debian.org/doc/packaging-manuals/copyright-format/1.0/
Upstream-Name: commonspeech
Source: https://github.com/mediaswing/speechout

Files: *
Copyright: 2026 mediaswing
License: MIT
 See /usr/share/doc/commonspeech/LICENSE.

Files: usr/share/icons/hicolor/*/apps/commonspeech.*
Copyright: SVG Repo, collection Seo Online Marketing 5
License: CC0-1.0

Files: embedded fonts (Ubuntu Medium and Bold)
Copyright: Canonical Ltd
License: Ubuntu-Font-Licence-1.0
 See /usr/share/doc/commonspeech/fonts-UFL.txt.
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

/// Where the program is installed.
const BIN_PATH: &str = "./usr/bin/speechout";

/// One file to place in the package: (path inside the package, mode, bytes).
type DebFile = (&'static str, u32, Vec<u8>);

/// xz rather than gzip makes the package much smaller. dpkg has read xz since 1.17.
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
        format!("{:<16}{:<12}{:<6}{:<6}{:<8}{:<10}`\n", name, mtime, 0, 0, "100644", data.len()).as_bytes(),
    );
    out.extend_from_slice(data);
    if data.len() % 2 == 1 {
        out.push(b'\n');
    }
}

/// Builds `speechout-<version>-ubuntu-<arch>.deb` in `out_dir` from the
/// running binary and returns its path.
pub fn build(out_dir: &Path) -> anyhow::Result<PathBuf> {
    let exe = std::env::current_exe().context("could not locate the running executable")?;
    let binary = std::fs::read(&exe)?;
    let version = env!("CARGO_PKG_VERSION");
    let mtime = std::env::var("SOURCE_DATE_EPOCH").ok().and_then(|v| v.parse().ok()).unwrap_or_else(|| {
        std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
    });

    let files: Vec<DebFile> = vec![
        (BIN_PATH, 0o755, binary),
        ("./usr/share/applications/speechout.desktop", 0o644, DESKTOP_ENTRY.as_bytes().to_vec()),
        (
            "./usr/share/icons/hicolor/256x256/apps/commonspeech.png",
            0o644,
            include_bytes!("../../assets/icon/commonspeech.png").to_vec(),
        ),
        (
            "./usr/share/icons/hicolor/scalable/apps/commonspeech.svg",
            0o644,
            include_bytes!("../../assets/icon/commonspeech.svg").to_vec(),
        ),
        ("./usr/share/doc/commonspeech/copyright", 0o644, COPYRIGHT.as_bytes().to_vec()),
        ("./usr/share/doc/commonspeech/LICENSE", 0o644, include_bytes!("../../LICENSE").to_vec()),
        ("./usr/share/doc/commonspeech/fonts-UFL.txt", 0o644, include_bytes!("../../assets/fonts/UFL.txt").to_vec()),
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
        "./usr/share/doc/commonspeech/",
    ];
    let installed_kib = files.iter().map(|f| f.2.len() as u64).sum::<u64>().div_ceil(1024);

    let control = format!(
        "Package: commonspeech\n\
         Version: {version}\n\
         Architecture: {arch}\n\
         Maintainer: wrichards <mediaswing@outlook.com>\n\
         Installed-Size: {installed_kib}\n\
         Depends: {DEB_DEPENDS}\n\
         Recommends: {DEB_RECOMMENDS}\n\
         Section: sound\n\
         Priority: optional\n\
         Homepage: https://github.com/mediaswing/speechout\n\
         Description: Common Speech Output, an accessible text-to-speech reader\n \
         Reads PDF, TXT, DOCX and CSV files aloud with system or cloud voices, saves the\n \
         speech as MP3, describes photos with a local AI model and applies XML\n \
         pronunciation wordlists. Fully usable by keyboard and screen reader.\n",
        arch = deb_arch()
    );

    let control_tar = tar_xz(&["./"], &[("./control", 0o644, control.into_bytes())], mtime)?;
    let data_tar = tar_xz(&dirs, &files, mtime)?;

    let mut deb = b"!<arch>\n".to_vec();
    ar_member(&mut deb, "debian-binary", b"2.0\n", mtime);
    ar_member(&mut deb, "control.tar.xz", &control_tar, mtime);
    ar_member(&mut deb, "data.tar.xz", &data_tar, mtime);

    std::fs::create_dir_all(out_dir)?;
    let path = out_dir.join(format!("{}-{version}-ubuntu-{}.deb", crate::BIN_NAME, deb_arch()));
    std::fs::write(&path, deb)?;
    Ok(path)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Command;

    /// Packages the test binary and, where dpkg is installed (as on the release
    /// runner), checks that dpkg accepts the result.
    #[test]
    fn builds_valid_deb() {
        let dir = tempfile::tempdir().unwrap();
        let deb = build(dir.path()).unwrap();
        assert!(deb.file_name().unwrap().to_str().unwrap().starts_with("speechout-"), "{deb:?}");
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

    #[test]
    fn installs_and_starts_the_program_by_its_name() {
        assert_eq!(BIN_PATH, format!("./usr/bin/{}", crate::BIN_NAME));
        assert!(DESKTOP_ENTRY.contains(&format!("\nExec={}\n", crate::BIN_NAME)));
        assert!(DESKTOP_ENTRY.contains(&format!("\nStartupWMClass={}\n", crate::BIN_NAME)));
        assert!(DESKTOP_ENTRY.contains("\nName=Common Speech Output\n"));
    }
}
