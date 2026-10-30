//! "Read aloud" on the right-click menu in Finder, for the kinds of file the
//! app can read.
//!
//! It is a Quick Action: a small Automator workflow in ~/Library/Services
//! that runs `speechout --read <file>`, which reads the file in the
//! background. The Settings tab adds and removes it, as the installer can't
//! put anything in the user's own Library. The workflow names the copy of the
//! app it was added from, and its label is in the interface language, so the
//! app updates the label each time it starts, and the copy if that one has
//! gone.
//!
//! It is per user, so no administrator password is needed. Finder shows it
//! under Quick Actions, and it can be turned off in System Settings, under
//! Keyboard, Keyboard Shortcuts, Services.

use crate::i18n::t;
use anyhow::Context;
use std::path::{Path, PathBuf};

/// The workflow's folder name, which Finder doesn't show.
const WORKFLOW: &str = "SpeechOut Read Aloud.workflow";
const PBS: &str = "/System/Library/CoreServices/pbs";

/// The kinds of file the app reads, as Finder knows them: the type each
/// ending in the file dialog on the General tab belongs to.
fn file_types() -> Vec<&'static str> {
    let mut types: Vec<_> = crate::document::FileKind::extensions(|_| true).into_iter().filter_map(file_type).collect();
    types.sort_unstable();
    types.dedup();
    types
}

fn file_type(ext: &str) -> Option<&'static str> {
    Some(match ext {
        "pdf" => "com.adobe.pdf",
        "txt" => "public.plain-text",
        "md" | "markdown" => "net.daringfireball.markdown",
        "docx" => "org.openxmlformats.wordprocessingml.document",
        "odt" => "org.oasis-open.opendocument.text",
        "csv" => "public.comma-separated-values-text",
        "ods" => "org.oasis-open.opendocument.spreadsheet",
        "pptx" => "org.openxmlformats.presentationml.presentation",
        "ppt" => "com.microsoft.powerpoint.ppt",
        "epub" => "org.idpf.epub-container",
        "jpg" | "jpeg" => "public.jpeg",
        "png" => "public.png",
        "jxl" => "public.jpeg-xl",
        "heic" => "public.heic",
        "heif" => "public.heif",
        _ => return None,
    })
}

fn workflow() -> Option<PathBuf> {
    dirs::home_dir().map(|home| home.join("Library/Services").join(WORKFLOW))
}

pub fn is_installed() -> bool {
    workflow().is_some_and(|w| w.join("Contents/Info.plist").is_file())
}

pub fn add() -> anyhow::Result<()> {
    let workflow = workflow().context("could not find your home folder")?;
    let exe = std::env::current_exe().context("could not locate the running executable")?;
    write(&workflow, &exe.to_string_lossy()).context("could not add Read aloud to the right-click menu")?;
    services_changed();
    log::info!("added Read aloud to the right-click menu");
    Ok(())
}

pub fn remove() -> anyhow::Result<()> {
    if let Some(workflow) = workflow().filter(|w| w.exists()) {
        std::fs::remove_dir_all(&workflow).context("could not remove Read aloud from the right-click menu")?;
        services_changed();
    }
    log::info!("removed Read aloud from the right-click menu");
    Ok(())
}

/// Puts the menu item's label in the interface language, if the menu item is
/// installed. It keeps the copy of the app it was added from, so starting a
/// build from source, or a copy macOS runs from a temporary folder, doesn't
/// take it over, unless that copy has gone.
pub fn refresh() {
    let Some(workflow) = workflow().filter(|_| is_installed()) else { return };
    let current = |name: &str| std::fs::read_to_string(workflow.join("Contents").join(name)).unwrap_or_default();
    let info = current("Info.plist");
    let exe = match app_in(&info).filter(|app| Path::new(app).is_file()) {
        Some(app) => app,
        None => match std::env::current_exe() {
            Ok(exe) => exe.to_string_lossy().into_owned(),
            Err(_) => return,
        },
    };
    let (new_info, new_document) = contents(&exe);
    if info == new_info && current("document.wflow") == new_document {
        return;
    }
    match write(&workflow, &exe) {
        Ok(()) => {
            services_changed();
            log::info!("updated the right-click menu");
        }
        Err(e) => log::warn!("could not update the right-click menu: {e:#}"),
    }
}

fn write(workflow: &Path, exe: &str) -> anyhow::Result<()> {
    let (info, document) = contents(exe);
    let dir = workflow.join("Contents");
    std::fs::create_dir_all(&dir)?;
    std::fs::write(dir.join("Info.plist"), info)?;
    std::fs::write(dir.join("document.wflow"), document)?;
    Ok(())
}

/// The workflow's Info.plist and document.wflow, for the app at `exe`.
fn contents(exe: &str) -> (String, String) {
    (info_plist(&t("context_menu.verb"), exe), document(&command(exe)))
}

/// The key in Info.plist that names the copy of the app the workflow runs.
const APP_KEY: &str = "SpeechOutApp";

/// The copy of the app named in a workflow's Info.plist.
fn app_in(info: &str) -> Option<String> {
    let start = format!("<key>{APP_KEY}</key><string>");
    let rest = &info[info.find(&start)? + start.len()..];
    let value = &rest[..rest.find("</string>")?];
    quick_xml::escape::unescape(value).ok().map(|v| v.into_owned())
}

/// The shell command the workflow runs, with the file as `$1`. It starts the
/// reader and returns, so Finder doesn't show the Quick Action as running
/// until reading ends, and choosing it again can stop it.
fn command(exe: &str) -> String {
    let quoted = format!("'{}'", exe.replace('\'', r"'\''"));
    format!(r#"{quoted} --read "$1" >/dev/null 2>&1 &"#)
}

/// Tells the system the Services menu has changed, so Finder shows the new
/// one without logging out.
fn services_changed() {
    let _ = std::process::Command::new(PBS)
        .arg("-update")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status();
}

fn info_plist(label: &str, exe: &str) -> String {
    let label = quick_xml::escape::escape(label);
    let exe = quick_xml::escape::escape(exe);
    let types: String = file_types().iter().map(|t| format!("\n                <string>{t}</string>")).collect();
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>CFBundleName</key><string>{label}</string>
    <key>{APP_KEY}</key><string>{exe}</string>
    <key>NSServices</key>
    <array>
        <dict>
            <key>NSMenuItem</key>
            <dict><key>default</key><string>{label}</string></dict>
            <key>NSMessage</key><string>runWorkflowAsService</string>
            <key>NSRequiredContext</key>
            <dict><key>NSApplicationIdentifier</key><string>com.apple.finder</string></dict>
            <key>NSSendFileTypes</key>
            <array>{types}
            </array>
        </dict>
    </array>
</dict>
</plist>
"#
    )
}

/// An Automator workflow with one Run Shell Script action, which is given
/// the chosen files as arguments.
fn document(command: &str) -> String {
    let command = quick_xml::escape::escape(command);
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>AMApplicationBuild</key><string>523</string>
    <key>AMApplicationVersion</key><string>2.10</string>
    <key>AMDocumentVersion</key><string>2</string>
    <key>actions</key>
    <array>
        <dict>
            <key>action</key>
            <dict>
                <key>AMAccepts</key>
                <dict>
                    <key>Container</key><string>List</string>
                    <key>Optional</key><true/>
                    <key>Types</key><array><string>com.apple.cocoa.string</string></array>
                </dict>
                <key>AMActionVersion</key><string>2.0.3</string>
                <key>AMApplication</key><array><string>Automator</string></array>
                <key>AMParameterProperties</key>
                <dict>
                    <key>COMMAND_STRING</key><dict/>
                    <key>CheckedForUserDefaultShell</key><dict/>
                    <key>inputMethod</key><dict/>
                    <key>shell</key><dict/>
                    <key>source</key><dict/>
                </dict>
                <key>AMProvides</key>
                <dict>
                    <key>Container</key><string>List</string>
                    <key>Types</key><array><string>com.apple.cocoa.string</string></array>
                </dict>
                <key>ActionBundlePath</key><string>/System/Library/Automator/Run Shell Script.action</string>
                <key>ActionName</key><string>Run Shell Script</string>
                <key>ActionParameters</key>
                <dict>
                    <key>COMMAND_STRING</key><string>{command}</string>
                    <key>CheckedForUserDefaultShell</key><true/>
                    <key>inputMethod</key><integer>1</integer>
                    <key>shell</key><string>/bin/sh</string>
                    <key>source</key><string></string>
                </dict>
                <key>BundleIdentifier</key><string>com.apple.RunShellScript</string>
                <key>CFBundleVersion</key><string>2.0.3</string>
                <key>CanShowSelectedItemsWhenRun</key><false/>
                <key>CanShowWhenRun</key><true/>
                <key>Category</key><array><string>AMCategoryUtilities</string></array>
                <key>Class Name</key><string>RunShellScriptAction</string>
                <key>InputUUID</key><string>5E3A8E5C-4C1B-4F0A-9D1E-6B8E2C9A7F01</string>
                <key>Keywords</key><array><string>Shell</string><string>Script</string></array>
                <key>OutputUUID</key><string>5E3A8E5C-4C1B-4F0A-9D1E-6B8E2C9A7F02</string>
                <key>UUID</key><string>5E3A8E5C-4C1B-4F0A-9D1E-6B8E2C9A7F03</string>
                <key>UnlocalizedApplications</key><array><string>Automator</string></array>
                <key>arguments</key>
                <dict>
                    <key>0</key>
                    <dict>
                        <key>default value</key><integer>0</integer>
                        <key>name</key><string>inputMethod</string>
                        <key>required</key><string>0</string>
                        <key>type</key><string>0</string>
                        <key>uuid</key><string>0</string>
                    </dict>
                    <key>1</key>
                    <dict>
                        <key>default value</key><false/>
                        <key>name</key><string>CheckedForUserDefaultShell</string>
                        <key>required</key><string>0</string>
                        <key>type</key><string>0</string>
                        <key>uuid</key><string>1</string>
                    </dict>
                    <key>2</key>
                    <dict>
                        <key>default value</key><string></string>
                        <key>name</key><string>source</string>
                        <key>required</key><string>0</string>
                        <key>type</key><string>0</string>
                        <key>uuid</key><string>2</string>
                    </dict>
                    <key>3</key>
                    <dict>
                        <key>default value</key><string></string>
                        <key>name</key><string>COMMAND_STRING</string>
                        <key>required</key><string>0</string>
                        <key>type</key><string>0</string>
                        <key>uuid</key><string>3</string>
                    </dict>
                    <key>4</key>
                    <dict>
                        <key>default value</key><string>/bin/sh</string>
                        <key>name</key><string>shell</string>
                        <key>required</key><string>0</string>
                        <key>type</key><string>0</string>
                        <key>uuid</key><string>4</string>
                    </dict>
                </dict>
                <key>isViewVisible</key><integer>1</integer>
                <key>location</key><string>309.000000:368.000000</string>
                <key>nibPath</key><string>/System/Library/Automator/Run Shell Script.action/Contents/Resources/Base.lproj/main.nib</string>
            </dict>
            <key>isViewVisible</key><integer>1</integer>
        </dict>
    </array>
    <key>connectors</key><dict/>
    <key>workflowMetaData</key>
    <dict>
        <key>applicationBundleID</key><string>com.apple.finder</string>
        <key>applicationBundleIDsByPath</key>
        <dict><key>/System/Library/CoreServices/Finder.app</key><string>com.apple.finder</string></dict>
        <key>applicationPath</key><string>/System/Library/CoreServices/Finder.app</string>
        <key>applicationPaths</key><array><string>/System/Library/CoreServices/Finder.app</string></array>
        <key>inputTypeIdentifier</key><string>com.apple.Automator.fileSystemObject</string>
        <key>outputTypeIdentifier</key><string>com.apple.Automator.nothing</string>
        <key>presentationMode</key><integer>15</integer>
        <key>processesInput</key><false/>
        <key>serviceApplicationBundleID</key><string>com.apple.finder</string>
        <key>serviceApplicationPath</key><string>/System/Library/CoreServices/Finder.app</string>
        <key>serviceInputTypeIdentifier</key><string>com.apple.Automator.fileSystemObject</string>
        <key>serviceOutputTypeIdentifier</key><string>com.apple.Automator.nothing</string>
        <key>serviceProcessesInput</key><false/>
        <key>systemImageName</key><string>NSActionTemplate</string>
        <key>useAutomaticInputType</key><false/>
        <key>workflowTypeIdentifier</key><string>com.apple.Automator.servicesMenu</string>
    </dict>
</dict>
</plist>
"#
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_kind_of_file_has_a_type() {
        for ext in crate::document::FileKind::extensions(|_| true) {
            assert!(file_type(ext).is_some(), "no file type for .{ext}");
        }
    }

    #[test]
    fn the_app_is_quoted_for_the_shell() {
        assert_eq!(command("/Apps/it's here/speechout"), r#"'/Apps/it'\''s here/speechout' --read "$1" >/dev/null 2>&1 &"#);
    }

    #[test]
    fn finds_the_app_it_was_added_from() {
        let info = info_plist("Read aloud", "/Apps/A & B's <app>/speechout");
        assert_eq!(app_in(&info).as_deref(), Some("/Apps/A & B's <app>/speechout"));
        assert_eq!(app_in("<plist/>"), None);
    }

    /// plutil reads both files, as Finder will.
    #[test]
    fn writes_valid_property_lists() {
        let dir = tempfile::tempdir().unwrap();
        let workflow = dir.path().join(WORKFLOW);
        write(&workflow, "/Apps/A & B/speechout").unwrap();
        for name in ["Info.plist", "document.wflow"] {
            let status = std::process::Command::new("/usr/bin/plutil")
                .arg("-lint")
                .arg(workflow.join("Contents").join(name))
                .stdout(std::process::Stdio::null())
                .status()
                .unwrap();
            assert!(status.success(), "{name} is not a valid property list");
        }
    }
}
