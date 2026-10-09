//! macOS: system voices come from the `say` command (backed by the same
//! speech synthesiser VoiceOver uses) and HEIC conversion uses `sips`
//! (backed by ImageIO). Both tools ship with every copy of macOS. They are
//! started directly by absolute path, never through a shell, so file names and
//! voice names cannot be interpreted as commands.
//!
//! This file also holds the release packaging step for macOS: `speechout
//! --package-macos <dir>` wraps the running binary in an `.app` bundle, ad hoc
//! signs it and zips it for the release page. Keeping it here means the
//! release workflow needs no shell script.

use super::SecretStore;
use crate::speech::Voice;
use anyhow::{Context, bail};
use std::io::Write;
use std::path::Path;
use std::process::{Command, Stdio};

const SAY: &str = "/usr/bin/say";
const SIPS: &str = "/usr/bin/sips";
const CODESIGN: &str = "/usr/bin/codesign";
const DITTO: &str = "/usr/bin/ditto";
const OPEN: &str = "/usr/bin/open";

const APP_NAME: &str = "Speech Output Engine";
const BUNDLE_ID: &str = "io.github.mediaswing.speechout";

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

pub fn secret_get(_name: &str) -> SecretStore<Option<String>> {
    SecretStore::Unsupported
}

pub fn secret_set(_name: &str, _value: &str) -> SecretStore<()> {
    SecretStore::Unsupported
}

pub fn secret_delete(_name: &str) -> SecretStore<()> {
    SecretStore::Unsupported
}

/// Builds `Speech Output Engine.app` from the running executable, ad hoc signs
/// it, and writes a zip of the bundle next to it. Returns the zip's path.
pub fn package(out_dir: &Path) -> anyhow::Result<std::path::PathBuf> {
    let exe = std::env::current_exe().context("could not locate the running executable")?;
    std::fs::create_dir_all(out_dir)?;
    let app = out_dir.join(format!("{APP_NAME}.app"));
    if app.exists() {
        std::fs::remove_dir_all(&app).context("could not remove the previous bundle")?;
    }
    let contents = app.join("Contents");
    let macos_dir = contents.join("MacOS");
    std::fs::create_dir_all(&macos_dir)?;
    std::fs::create_dir_all(contents.join("Resources"))?;

    let bundled_exe = macos_dir.join("speechout");
    std::fs::copy(&exe, &bundled_exe).context("could not copy the executable into the bundle")?;
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&bundled_exe, std::fs::Permissions::from_mode(0o755))?;
    }
    std::fs::write(contents.join("Info.plist"), info_plist())?;
    std::fs::write(contents.join("PkgInfo"), b"APPL????")?;
    // The embedded fonts' licence must travel with them.
    std::fs::write(contents.join("Resources/fonts-OFL.txt"), include_bytes!("../../assets/fonts/OFL.txt"))?;

    // Ad hoc signature ("-" identity). Apple Silicon refuses to run unsigned
    // arm64 code, and the hardened runtime is enabled for good measure.
    let status = Command::new(CODESIGN)
        .args(["--force", "--sign", "-", "--timestamp=none", "--options", "runtime"])
        .arg(&app)
        .status()
        .context("could not run codesign")?;
    if !status.success() {
        bail!("codesign could not sign the bundle");
    }
    let status = Command::new(CODESIGN)
        .args(["--verify", "--strict", "--verbose=2"])
        .arg(&app)
        .status()
        .context("could not run codesign")?;
    if !status.success() {
        bail!("the bundle's signature did not verify");
    }

    let zip = out_dir.join(format!(
        "speechout-{}-macos-{}.zip",
        env!("CARGO_PKG_VERSION"),
        std::env::consts::ARCH
    ));
    if zip.exists() {
        std::fs::remove_file(&zip)?;
    }
    // ditto preserves the signature, permissions and extended attributes.
    let status = Command::new(DITTO)
        .args(["-c", "-k", "--sequesterRsrc", "--keepParent"])
        .arg(&app)
        .arg(&zip)
        .status()
        .context("could not run ditto")?;
    if !status.success() {
        bail!("could not zip the bundle");
    }
    Ok(zip)
}

fn info_plist() -> String {
    let version = env!("CARGO_PKG_VERSION");
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>CFBundleDevelopmentRegion</key><string>en</string>
    <key>CFBundleDisplayName</key><string>{APP_NAME}</string>
    <key>CFBundleName</key><string>{APP_NAME}</string>
    <key>CFBundleExecutable</key><string>speechout</string>
    <key>CFBundleIdentifier</key><string>{BUNDLE_ID}</string>
    <key>CFBundleInfoDictionaryVersion</key><string>6.0</string>
    <key>CFBundlePackageType</key><string>APPL</string>
    <key>CFBundleShortVersionString</key><string>{version}</string>
    <key>CFBundleVersion</key><string>{version}</string>
    <key>LSApplicationCategoryType</key><string>public.app-category.utilities</string>
    <key>LSArchitecturePriority</key><array><string>arm64</string></array>
    <key>LSMinimumSystemVersion</key><string>11.0</string>
    <key>NSHighResolutionCapable</key><true/>
    <key>NSSupportsAutomaticGraphicsSwitching</key><true/>
</dict>
</plist>
"#
    )
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
