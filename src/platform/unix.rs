//! What macOS and Linux share: file permissions, and keeping text from
//! controlling the voice.

use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::Path;

/// Only the owner can enter the folder. A failure is logged and otherwise
/// ignored: the folder is usually already private, and the app should still run.
pub fn restrict_dir_to_owner(dir: &Path) {
    if let Err(e) = std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700)) {
        log::warn!("Could not make {} private: {e}", dir.display());
    }
}

/// Creates the file readable only by its owner if it doesn't exist, and makes
/// it so if it does. Existing contents are kept.
pub fn create_private_file(path: &Path) -> std::io::Result<()> {
    std::fs::OpenOptions::new().create(true).append(true).mode(0o600).open(path)?;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))
}

/// Both system synthesisers obey commands between double brackets: macOS's
/// `say` takes commands such as `[[volm 0]]` (silence) or `[[slnc 5000]]` (a
/// pause), and eSpeak NG takes raw phonemes as `[[...]]`. So a document could
/// change how it is read. Shrinking every run of opening brackets to one means
/// no command can start, including `[[dlim]]`, which would change the brackets
/// themselves.
pub fn without_embedded_commands(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        if !(c == '[' && out.ends_with('[')) {
            out.push(c);
        }
    }
    out
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
}
