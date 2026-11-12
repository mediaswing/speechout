//! The macOS installer package: `speechout --package-macos <dir>` wraps the
//! running binary in an `.app` bundle, ad hoc signs it, builds an installer
//! package (.pkg) that puts the bundle in /Applications, and checks the result.
//! All of it is here, so the release workflow needs no shell code.
//!
//! The signature is ad hoc because a real one needs a paid Apple certificate.
//! Apple Silicon refuses to run unsigned code, and an ad hoc signature is enough
//! for that, but Gatekeeper still warns about a download that isn't notarised.

use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, bail};

const CODESIGN: &str = "/usr/bin/codesign";
const PKGBUILD: &str = "/usr/bin/pkgbuild";
const PKGUTIL: &str = "/usr/sbin/pkgutil";
const PRODUCTBUILD: &str = "/usr/bin/productbuild";

const APP_NAME: &str = crate::APP_TITLE;
const EXECUTABLE: &str = crate::BIN_NAME;
const BUNDLE_ID: &str = "io.github.mediaswing.commonspeech";
const COMPONENT_PKG: &str = "commonspeech-app.pkg";

fn run(command: &mut Command, what: &str) -> anyhow::Result<()> {
    let status = command.status().with_context(|| format!("could not run {what}"))?;
    if !status.success() {
        bail!("{what} failed with {status}");
    }
    Ok(())
}

/// Builds `Common Speech Output.app` from the running executable, ad hoc signs
/// it, and writes an installer package for it to `out_dir`. Returns the
/// package's path. The bundle is left in `out_dir/root` for trying it out.
pub fn build(out_dir: &Path) -> anyhow::Result<PathBuf> {
    let exe = std::env::current_exe().context("could not locate the running executable")?;
    // pkgbuild installs everything in the root folder, so it holds only the bundle.
    let root = out_dir.join("root");
    if root.exists() {
        std::fs::remove_dir_all(&root).context("could not remove the previous bundle")?;
    }
    let app = root.join(format!("{APP_NAME}.app"));
    let contents = app.join("Contents");
    let macos_dir = contents.join("MacOS");
    std::fs::create_dir_all(&macos_dir)?;
    std::fs::create_dir_all(contents.join("Resources"))?;

    let bundled_exe = macos_dir.join(EXECUTABLE);
    std::fs::copy(&exe, &bundled_exe).context("could not copy the executable into the bundle")?;
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&bundled_exe, std::fs::Permissions::from_mode(0o755))?;
    }
    std::fs::write(contents.join("Info.plist"), info_plist())?;
    std::fs::write(contents.join("PkgInfo"), b"APPL????")?;
    std::fs::write(contents.join("Resources/commonspeech.icns"), include_bytes!("../../assets/icon/commonspeech.icns"))?;
    // The embedded fonts' licence must travel with them.
    std::fs::write(contents.join("Resources/fonts-UFL.txt"), include_bytes!("../../assets/fonts/UFL.txt"))?;
    std::fs::write(contents.join("Resources/LICENSE.txt"), include_bytes!("../../LICENSE"))?;

    // Ad hoc signature ("-" identity), with the hardened runtime on.
    run(
        Command::new(CODESIGN).args(["--force", "--sign", "-", "--timestamp=none", "--options", "runtime"]).arg(&app),
        "codesign",
    )?;
    run(Command::new(CODESIGN).args(["--verify", "--strict", "--verbose=2"]).arg(&app), "codesign --verify")?;

    // The component package installs the bundle; the product package around it
    // gives the installer its title and refuses Intel Macs and macOS versions
    // the app can't run on.
    let work = out_dir.join("pkg-work");
    if work.exists() {
        std::fs::remove_dir_all(&work)?;
    }
    std::fs::create_dir_all(&work)?;
    let component_plist = work.join("component.plist");
    std::fs::write(&component_plist, component_plist_xml())?;
    let distribution = work.join("distribution.xml");
    std::fs::write(&distribution, distribution_xml())?;

    run(
        Command::new(PKGBUILD)
            .arg("--root")
            .arg(&root)
            .arg("--component-plist")
            .arg(&component_plist)
            .args(["--identifier", BUNDLE_ID, "--version", env!("CARGO_PKG_VERSION")])
            .args(["--install-location", "/Applications"])
            .arg(work.join(COMPONENT_PKG)),
        "pkgbuild",
    )?;

    let pkg = out_dir.join(format!("{EXECUTABLE}-{}-macos-{}.pkg", env!("CARGO_PKG_VERSION"), std::env::consts::ARCH));
    if pkg.exists() {
        std::fs::remove_file(&pkg)?;
    }
    run(
        Command::new(PRODUCTBUILD).arg("--distribution").arg(&distribution).arg("--package-path").arg(&work).arg(&pkg),
        "productbuild",
    )?;
    std::fs::remove_dir_all(&work)?;
    check(&pkg)?;
    Ok(pkg)
}

/// Checks the installer holds the program and that what it would install still
/// has a valid ad hoc signature.
fn check(pkg: &Path) -> anyhow::Result<()> {
    let listing = Command::new(PKGUTIL).arg("--payload-files").arg(pkg).output().context("could not run pkgutil")?;
    let wanted = format!("{APP_NAME}.app/Contents/MacOS/{EXECUTABLE}");
    if !listing.status.success() || !String::from_utf8_lossy(&listing.stdout).contains(&wanted) {
        bail!("the installer package doesn't contain {wanted}");
    }
    let expanded = tempfile::tempdir().context("could not make a temporary folder")?;
    let expanded = expanded.path().join("expanded");
    run(Command::new(PKGUTIL).arg("--expand-full").arg(pkg).arg(&expanded), "pkgutil --expand-full")?;
    let app = expanded.join(COMPONENT_PKG).join("Payload").join(format!("{APP_NAME}.app"));
    run(Command::new(CODESIGN).args(["--verify", "--strict"]).arg(&app), "codesign --verify on the package's contents")?;
    let details = Command::new(CODESIGN).args(["--display", "--verbose=2"]).arg(&app).output().context("could not run codesign")?;
    if !String::from_utf8_lossy(&details.stderr).contains("Signature=adhoc") {
        bail!("the package's bundle isn't ad hoc signed");
    }
    Ok(())
}

/// Without this, pkgbuild marks the bundle relocatable, and the installer would
/// update a copy found anywhere else on the Mac instead of using /Applications.
fn component_plist_xml() -> String {
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<array>
    <dict>
        <key>BundleHasStrictIdentifier</key><true/>
        <key>BundleIsRelocatable</key><false/>
        <key>BundleIsVersionChecked</key><false/>
        <key>BundleOverwriteAction</key><string>upgrade</string>
        <key>RootRelativeBundlePath</key><string>{APP_NAME}.app</string>
    </dict>
</array>
</plist>
"#
    )
}

fn distribution_xml() -> String {
    let version = env!("CARGO_PKG_VERSION");
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<installer-gui-script minSpecVersion="2">
    <title>{APP_NAME}</title>
    <options customize="never" require-scripts="false" hostArchitectures="arm64"/>
    <domains enable_localSystem="true"/>
    <volume-check>
        <allowed-os-versions><os-version min="11.0"/></allowed-os-versions>
    </volume-check>
    <choices-outline>
        <line choice="{BUNDLE_ID}"/>
    </choices-outline>
    <choice id="{BUNDLE_ID}" visible="false">
        <pkg-ref id="{BUNDLE_ID}"/>
    </choice>
    <pkg-ref id="{BUNDLE_ID}" version="{version}" onConclusion="none">{COMPONENT_PKG}</pkg-ref>
</installer-gui-script>
"#
    )
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
    <key>CFBundleExecutable</key><string>{EXECUTABLE}</string>
    <key>CFBundleIconFile</key><string>commonspeech</string>
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

    /// Packages the test binary. `build` does the signature checks itself.
    #[test]
    fn builds_ad_hoc_signed_pkg() {
        let dir = tempfile::tempdir().unwrap();
        let pkg = build(dir.path()).unwrap();
        assert!(pkg.is_file());
        assert!(pkg.file_name().unwrap().to_str().unwrap().starts_with("speechout-"), "{pkg:?}");
        assert!(!dir.path().join("pkg-work").exists());
    }
}
