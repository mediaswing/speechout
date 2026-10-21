//! "Read aloud" on the right-click menu in File Explorer, for the kinds of
//! file the app can read.
//!
//! The Windows Installer adds the menu item, unless the user leaves it out,
//! and removes it when the app is uninstalled (see src/installer/speechout.wxs).
//! The item runs `speechout.exe --read <file>`, which reads the file in the
//! background. The installer can only write the item's label in English, so
//! the app puts it in the interface language each time it starts.
//!
//! Without the installer, as when running a build from source, there is no
//! menu to add. A menu item added by the app itself before version 2.8, which
//! may run a copy of the app that has since moved or gone, can be removed.
//!
//! Everything is per user (HKEY_CURRENT_USER), so no administrator rights are
//! needed. On Windows 11 the item is under "Show more options".

use crate::i18n::t;
use winreg::RegKey;
use winreg::enums::{HKEY_CURRENT_USER, KEY_READ, KEY_SET_VALUE};

/// The menu item's key under each file type.
const VERB: &str = "SpeechOut.ReadAloud";

/// The kinds of file the app reads, as in the file dialog on the General tab.
/// The installer's ContextMenuExtensions list must match.
const EXTENSIONS: [&str; 17] = ["pdf", "txt", "md", "markdown", "docx", "odt", "csv", "ods", "pptx", "ppt", "epub", "jpg", "jpeg", "png", "jxl", "heic", "heif"];

/// The menu item's key for files ending in `.ext`, under HKEY_CURRENT_USER.
fn verb_key(ext: &str) -> String {
    format!(r"Software\Classes\SystemFileAssociations\.{ext}\shell\{VERB}")
}

pub fn is_installed() -> bool {
    RegKey::predef(HKEY_CURRENT_USER).open_subkey(verb_key(EXTENSIONS[0])).is_ok()
}

/// Whether the Windows Installer has installed the app for this user, and so
/// adds and removes the menu item. The installer keeps this key until the app
/// is uninstalled.
pub fn managed_by_installer() -> bool {
    RegKey::predef(HKEY_CURRENT_USER).open_subkey(r"Software\SpeechOut\Installer").is_ok()
}

/// Removes a menu item the installer doesn't manage.
pub fn remove() -> anyhow::Result<()> {
    let hkcu = RegKey::predef(HKEY_CURRENT_USER);
    for ext in EXTENSIONS {
        match hkcu.delete_subkey_all(verb_key(ext)) {
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => {
                return Err(anyhow::Error::new(e).context("could not remove Read aloud from the right-click menu"));
            }
            _ => {}
        }
    }
    associations_changed();
    log::info!("removed Read aloud from the right-click menu");
    Ok(())
}

/// Puts the menu item's label in the interface language, if the menu item is
/// installed.
pub fn refresh() {
    let label = t("context_menu.verb");
    let hkcu = RegKey::predef(HKEY_CURRENT_USER);
    let mut changed = false;
    for ext in EXTENSIONS {
        let Ok(key) = hkcu.open_subkey_with_flags(verb_key(ext), KEY_READ | KEY_SET_VALUE) else { continue };
        if key.get_value::<String, _>("MUIVerb").is_ok_and(|v| v == label) {
            continue;
        }
        match key.set_value("MUIVerb", &label) {
            Ok(()) => changed = true,
            Err(e) => log::warn!("could not update the right-click menu for .{ext}: {e}"),
        }
    }
    if changed {
        associations_changed();
        log::info!("updated the right-click menu's label");
    }
}

/// Tells Explorer the menus have changed, so it doesn't show an old one.
fn associations_changed() {
    use windows::Win32::UI::Shell::{SHCNE_ASSOCCHANGED, SHCNF_IDLIST, SHChangeNotify};
    // SAFETY: SHCNE_ASSOCCHANGED takes no items.
    unsafe { SHChangeNotify(SHCNE_ASSOCCHANGED, SHCNF_IDLIST, None, None) };
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn covers_the_files_the_app_reads() {
        for ext in EXTENSIONS {
            let path = std::path::PathBuf::from(format!("a.{ext}"));
            assert!(crate::document::FileKind::from_path(&path).is_some(), "{ext}");
        }
    }

    /// The ContextMenuExtensions list in the installer's source.
    fn installer_extensions(wxs: &str) -> Vec<&str> {
        let list = wxs
            .split("<?define ContextMenuExtensions = ")
            .nth(1)
            .and_then(|rest| rest.split("?>").next())
            .expect("the installer has no ContextMenuExtensions list");
        list.trim().split(';').collect()
    }

    #[test]
    fn the_installer_adds_the_menu_for_the_same_files() {
        assert_eq!(installer_extensions(include_str!("../installer/speechout.wxs")), EXTENSIONS);
    }
}
