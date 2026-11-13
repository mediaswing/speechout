//! The Windows installer: `speechout --package-msi <dir>` builds an `.msi`
//! that installs the running executable, and any DLLs beside it, into
//! `C:\Program Files\Common Speech Output` and adds a Start menu shortcut.
//! Opened by double-clicking, it shows the standard installer dialogs
//! (WixUI_InstallDir): welcome, licence, folder, install and finish, with a
//! box on the last one to open the app. `msiexec /qn` still installs silently.
//!
//! The installer is described in WiX source (.wxs) written here, and built with
//! the WiX Toolset's `wix` command, which must be on the PATH, with its UI and
//! Util extensions (`dotnet tool install --global wix --version 6.0.2`, then
//! `wix extension add --global WixToolset.UI.wixext/6.0.2
//! WixToolset.Util.wixext/6.0.2`). The MSI is built for
//! the architecture the running executable was built for, so x64 and Arm each
//! package their own. Neither the program nor the installer is signed, since
//! that needs a code signing certificate.

use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, bail};

const APP_NAME: &str = crate::APP_TITLE;
const MANUFACTURER: &str = "mediaswing";

/// Ties every version of the installer together, so installing a newer one
/// replaces the old one instead of installing beside it. Never change it.
const UPGRADE_CODE: &str = "7C0F4C55-3E0B-4A3D-9B55-6E7D5F2A1C84";

/// WiX's name for the architecture this binary was built for.
fn wix_arch() -> &'static str {
    match std::env::consts::ARCH {
        "x86_64" => "x64",
        "aarch64" => "arm64",
        "x86" => "x86",
        other => other,
    }
}

fn xml_escape(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;")
}

/// Plain text as an RTF document, for the licence dialog, which only shows RTF.
/// Each line break ends a paragraph, and anything beyond ASCII is written as
/// a Unicode escape.
fn rtf(text: &str) -> String {
    let mut out = String::from("{\\rtf1\\ansi\\deff0{\\fonttbl{\\f0\\fswiss Segoe UI;}}\\fs18\n");
    for line in text.lines() {
        for c in line.chars() {
            match c {
                '\\' | '{' | '}' => {
                    out.push('\\');
                    out.push(c);
                }
                '\t' => out.push_str("\\tab "),
                ' '..='~' => out.push(c),
                _ => {
                    let mut units = [0u16; 2];
                    for unit in c.encode_utf16(&mut units) {
                        // RTF numbers are signed 16-bit; `?` is what older readers show.
                        out.push_str(&format!("\\u{}?", *unit as i16));
                    }
                }
            }
        }
        out.push_str("\\par\n");
    }
    out.push('}');
    out
}

/// The DLLs beside the executable: the Visual C++ runtime, which the release
/// workflow copies there so the app runs on PCs without the Redistributable.
fn dlls_beside(exe: &Path) -> anyhow::Result<Vec<PathBuf>> {
    let dir = exe.parent().context("the executable has no folder")?;
    let mut dlls: Vec<PathBuf> = std::fs::read_dir(dir)?
        .filter_map(Result::ok)
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|e| e.eq_ignore_ascii_case("dll")))
        .collect();
    dlls.sort();
    Ok(dlls)
}

/// The installer's WiX source, installing the executable at `exe` and `dlls`,
/// with the plain text `licence` installed and `licence_rtf` shown in a dialog.
fn wxs(exe: &Path, dlls: &[PathBuf], licence: &Path, licence_rtf: &Path) -> String {
    let version = env!("CARGO_PKG_VERSION");
    let exe_name = format!("{}.exe", crate::BIN_NAME);
    let source = xml_escape(&exe.display().to_string());
    let licence = xml_escape(&licence.display().to_string());
    let licence_rtf = xml_escape(&licence_rtf.display().to_string());
    let program_files = if wix_arch() == "x86" { "ProgramFilesFolder" } else { "ProgramFiles64Folder" };
    let dll_components: String = dlls
        .iter()
        .map(|dll| format!("        <Component><File Source=\"{}\" /></Component>\n", xml_escape(&dll.display().to_string())))
        .collect();
    // The shortcut isn't advertised, because an advertised one must name an icon
    // from the Icon table, which would store a second, uncompressed copy of the
    // program. Windows shows the installed executable's own icon for it. A
    // shortcut that isn't advertised needs a component of its own whose key path
    // is a per-user registry value (ICE43).
    //
    // The finish dialog's box opens the app as the person installing it, not
    // as the administrator the installation runs as, and not on uninstalling.
    format!(
        r#"<?xml version="1.0" encoding="utf-8"?>
<Wix xmlns="http://wixtoolset.org/schemas/v4/wxs" xmlns:ui="http://wixtoolset.org/schemas/v4/wxs/ui">
  <Package Name="{APP_NAME}" Manufacturer="{MANUFACTURER}" Version="{version}"
           UpgradeCode="{UPGRADE_CODE}" Scope="perMachine" Compressed="yes">
    <MajorUpgrade DowngradeErrorMessage="A newer version of {APP_NAME} is already installed." />
    <MediaTemplate EmbedCab="yes" />
    <Property Id="ARPURLINFOABOUT" Value="https://github.com/mediaswing/speechout" />
    <UI>
      <ui:WixUI Id="WixUI_InstallDir" InstallDirectory="INSTALLFOLDER" />
      <Publish Dialog="ExitDialog" Control="Finish" Event="DoAction" Value="LaunchApplication"
               Condition="WIXUI_EXITDIALOGOPTIONALCHECKBOX = 1 AND NOT Installed" />
    </UI>
    <WixVariable Id="WixUILicenseRtf" Value="{licence_rtf}" />
    <Property Id="WIXUI_EXITDIALOGOPTIONALCHECKBOXTEXT" Value="Open {APP_NAME}" />
    <Property Id="WIXUI_EXITDIALOGOPTIONALCHECKBOX" Value="1" />
    <Property Id="WixShellExecTarget" Value="[#MainExe]" />
    <CustomAction Id="LaunchApplication" BinaryRef="Wix4UtilCA_$(sys.BUILDARCHSHORT)" DllEntry="WixShellExec"
                  Impersonate="yes" />
    <StandardDirectory Id="{program_files}">
      <Directory Id="INSTALLFOLDER" Name="{APP_NAME}">
        <Component>
          <File Id="MainExe" Source="{source}" Name="{exe_name}" KeyPath="yes" />
        </Component>
        <Component>
          <File Id="Licences" Source="{licence}" Name="LICENSE.txt" />
        </Component>
{dll_components}      </Directory>
    </StandardDirectory>
    <StandardDirectory Id="ProgramMenuFolder">
      <Component Id="StartMenuShortcut">
        <Shortcut Id="StartMenuShortcut" Name="{APP_NAME}"
                  Description="Read documents aloud and describe photos"
                  Target="[INSTALLFOLDER]{exe_name}" WorkingDirectory="INSTALLFOLDER" />
        <RegistryValue Root="HKCU" Key="Software\{MANUFACTURER}\{APP_NAME}"
                       Name="StartMenuShortcut" Type="integer" Value="1" KeyPath="yes" />
      </Component>
    </StandardDirectory>
  </Package>
</Wix>
"#
    )
}

/// Builds `speechout-<version>-windows-<arch>.msi` in `out_dir` from the
/// running executable and returns its path.
pub fn build(out_dir: &Path) -> anyhow::Result<PathBuf> {
    let exe = std::env::current_exe().context("could not locate the running executable")?;
    std::fs::create_dir_all(out_dir)?;
    // wix writes a .wixpdb beside its output, so build in a work folder and move
    // only the .msi out.
    let work = out_dir.join("msi-work");
    if work.exists() {
        std::fs::remove_dir_all(&work)?;
    }
    std::fs::create_dir_all(&work)?;
    let licence = work.join("LICENSE.txt");
    let notices = [&include_bytes!("../../LICENSE")[..], b"\r\n\r\n", include_bytes!("../../assets/fonts/UFL.txt")].concat();
    std::fs::write(&licence, &notices)?;
    let licence_rtf = work.join("LICENSE.rtf");
    std::fs::write(&licence_rtf, rtf(&String::from_utf8_lossy(&notices)))?;
    let source = work.join("commonspeech.wxs");
    std::fs::write(&source, wxs(&exe, &dlls_beside(&exe)?, &licence, &licence_rtf))?;

    let name = format!(
        "{}-{}-windows-{}.msi",
        crate::BIN_NAME,
        env!("CARGO_PKG_VERSION"),
        std::env::consts::ARCH
    );
    let built = work.join(&name);
    let status = Command::new("wix")
        .args(["build", "-arch", wix_arch(), "-pdbtype", "none"])
        .args(["-ext", "WixToolset.UI.wixext", "-ext", "WixToolset.Util.wixext", "-o"])
        .arg(&built)
        .arg(&source)
        .status()
        .context("could not run wix; install the WiX Toolset with `dotnet tool install --global wix --version 6.0.2`")?;
    if !status.success() {
        bail!(
            "wix build exited with {status}; it needs `wix extension add --global WixToolset.UI.wixext/6.0.2 \
             WixToolset.Util.wixext/6.0.2`"
        );
    }

    let msi = out_dir.join(&name);
    if msi.exists() {
        std::fs::remove_file(&msi)?;
    }
    std::fs::rename(&built, &msi).context("could not move the installer out of the work folder")?;
    std::fs::remove_dir_all(&work)?;
    Ok(msi)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wxs_escapes_paths_and_lists_dlls() {
        let source = wxs(
            Path::new(r"C:\a & b\speechout.exe"),
            &[PathBuf::from(r"C:\a & b\vcruntime140.dll")],
            Path::new(r"C:\w\LICENSE.txt"),
            Path::new(r"C:\w\LICENSE.rtf"),
        );
        assert!(source.contains(r#"Source="C:\a &amp; b\speechout.exe""#));
        assert!(source.contains(r#"Name="speechout.exe""#));
        assert!(source.contains(r#"Target="[INSTALLFOLDER]speechout.exe""#));
        assert!(source.contains(r#"Source="C:\a &amp; b\vcruntime140.dll""#));
        assert!(source.contains(&format!(r#"Version="{}""#, env!("CARGO_PKG_VERSION"))));
        assert!(source.contains(r#"Name="Common Speech Output""#));
        assert!(!source.contains("<Icon"), "an Icon would store a second copy of the program");
        assert!(source.contains(r#"<ui:WixUI Id="WixUI_InstallDir" InstallDirectory="INSTALLFOLDER" />"#));
        assert!(source.contains(r#"<WixVariable Id="WixUILicenseRtf" Value="C:\w\LICENSE.rtf" />"#));
        assert!(source.contains(r#"Value="Open Common Speech Output""#));
        assert!(source.contains("NOT Installed"), "uninstalling doesn't open the app");
    }

    #[test]
    fn licence_is_shown_as_rtf() {
        let text = rtf("MIT {1}\\path\nCopyright \u{a9} Łukasz \u{1F600}");
        assert!(text.starts_with("{\\rtf1"));
        assert!(text.ends_with("\\par\n}"));
        assert!(text.contains("MIT \\{1\\}\\\\path\\par\n"), "{text}");
        assert!(text.contains("Copyright \\u169? \\u321?ukasz \\u-10179?\\u-8704?"), "{text}");
        assert!(text.is_ascii());
        let real = rtf(&String::from_utf8_lossy(include_bytes!("../../LICENSE")));
        assert!(real.contains("MIT License") || real.contains("Permission is hereby granted"));
    }

    /// Packages the test binary when WiX is installed (CI installs it).
    #[test]
    fn builds_msi() {
        if Command::new("wix").arg("--version").output().is_err() {
            eprintln!("WiX not found; skipping the .msi build");
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let msi = build(dir.path()).unwrap();
        // An .msi is an OLE compound file.
        assert!(std::fs::read(&msi).unwrap().starts_with(&[0xD0, 0xCF, 0x11, 0xE0, 0xA1, 0xB1, 0x1A, 0xE1]));
    }
}
