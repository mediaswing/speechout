//! The macOS installer package: `speechout --package-macos <dir>` wraps the
//! running binary in an `.app` bundle, ad hoc signs it and builds an installer
//! package (.pkg) for the release page that puts the bundle in /Applications.
//! Keeping it here means the release workflow needs no shell script.

use anyhow::{Context, bail};
use std::path::Path;
use std::process::Command;

const CODESIGN: &str = "/usr/bin/codesign";
const PKGBUILD: &str = "/usr/bin/pkgbuild";
const PRODUCTBUILD: &str = "/usr/bin/productbuild";

const APP_NAME: &str = "Speech Output Engine";
const BUNDLE_ID: &str = "io.github.mediaswing.speechout";

/// Builds `Speech Output Engine.app` from the running executable, ad hoc signs
/// it, and writes an installer package for it to `out_dir`. Returns the
/// package's path. The bundle is left in `out_dir/root` for trying it out.
pub fn build(out_dir: &Path) -> anyhow::Result<std::path::PathBuf> {
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

    let bundled_exe = macos_dir.join("speechout");
    std::fs::copy(&exe, &bundled_exe).context("could not copy the executable into the bundle")?;
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&bundled_exe, std::fs::Permissions::from_mode(0o755))?;
    }
    std::fs::write(contents.join("Info.plist"), info_plist())?;
    std::fs::write(contents.join("PkgInfo"), b"APPL????")?;
    std::fs::write(contents.join("Resources/speechout.icns"), include_bytes!("../../assets/icon/speechout.icns"))?;
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

    // The component package installs the bundle; the product package around
    // it gives the installer its title and refuses Intel Macs and macOS
    // versions the app can't run on.
    let work = out_dir.join("pkg-work");
    if work.exists() {
        std::fs::remove_dir_all(&work)?;
    }
    std::fs::create_dir_all(&work)?;
    let component_plist = work.join("component.plist");
    std::fs::write(&component_plist, component_plist_xml())?;
    let distribution = work.join("distribution.xml");
    std::fs::write(&distribution, distribution_xml())?;

    let status = Command::new(PKGBUILD)
        .arg("--root")
        .arg(&root)
        .arg("--component-plist")
        .arg(&component_plist)
        .args(["--identifier", BUNDLE_ID, "--version", env!("CARGO_PKG_VERSION")])
        .args(["--install-location", "/Applications"])
        .arg(work.join(COMPONENT_PKG))
        .status()
        .context("could not run pkgbuild")?;
    if !status.success() {
        bail!("pkgbuild could not build the component package");
    }

    let pkg = out_dir.join(format!(
        "speechout-{}-macos-{}.pkg",
        env!("CARGO_PKG_VERSION"),
        std::env::consts::ARCH
    ));
    if pkg.exists() {
        std::fs::remove_file(&pkg)?;
    }
    let status = Command::new(PRODUCTBUILD)
        .arg("--distribution")
        .arg(&distribution)
        .arg("--package-path")
        .arg(&work)
        .arg(&pkg)
        .status()
        .context("could not run productbuild")?;
    if !status.success() {
        bail!("productbuild could not build the installer package");
    }
    std::fs::remove_dir_all(&work)?;
    Ok(pkg)
}

const COMPONENT_PKG: &str = "speechout-app.pkg";

/// Without this, pkgbuild marks the bundle relocatable, and the installer
/// would update a copy found anywhere else on the Mac instead of putting it in
/// /Applications.
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
    <key>CFBundleExecutable</key><string>speechout</string>
    <key>CFBundleIconFile</key><string>speechout</string>
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
