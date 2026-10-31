//! "Read aloud" on the right-click menu in the file manager, for the kinds of
//! file the app can read.
//!
//! Each file manager has its own way of adding to the menu, so the Settings
//! tab writes one small file for each, all in the user's own data folder
//! (~/.local/share), and removes them all again:
//!
//! * Dolphin (KDE): a service menu, at the top of the menu.
//! * Nemo (Cinnamon): an action, at the top of the menu.
//! * Files (Nautilus, GNOME): a script, under Scripts. It is offered for
//!   every file, as Nautilus can't limit scripts to some kinds.
//! * Any other file manager: an application under Open With, which the
//!   app launcher doesn't show.
//!
//! Each runs `speechout --read <file>`, which reads the file in the
//! background. The files name the copy of the app they were added from, and
//! the label is in the interface language, so the app updates the label each
//! time it starts, and the copy if that one has gone.

use crate::i18n::t;
use anyhow::{Context, bail};
use std::path::{Path, PathBuf};

/// Names every file written, and marks the Nautilus script, which is named
/// after the label.
const ID: &str = "speechout-read-aloud";

/// The kinds of file the app reads, as MIME types: the type each ending in
/// the file dialog on the General tab belongs to.
fn mime_types() -> Vec<&'static str> {
    let mut types: Vec<_> =
        crate::document::FileKind::extensions(|_| true).into_iter().filter_map(mime_type).collect();
    types.sort_unstable();
    types.dedup();
    types
}

fn mime_type(ext: &str) -> Option<&'static str> {
    Some(match ext {
        "pdf" => "application/pdf",
        "txt" => "text/plain",
        "md" | "markdown" => "text/markdown",
        "docx" => "application/vnd.openxmlformats-officedocument.wordprocessingml.document",
        "odt" => "application/vnd.oasis.opendocument.text",
        "csv" => "text/csv",
        "ods" => "application/vnd.oasis.opendocument.spreadsheet",
        "pptx" => "application/vnd.openxmlformats-officedocument.presentationml.presentation",
        "ppt" => "application/vnd.ms-powerpoint",
        "epub" => "application/epub+zip",
        "jpg" | "jpeg" => "image/jpeg",
        "png" => "image/png",
        "jxl" => "image/jxl",
        "heic" => "image/heic",
        "heif" => "image/heif",
        _ => return None,
    })
}

/// One file to write: where, what, and whether it must be executable.
struct MenuFile {
    path: PathBuf,
    contents: String,
    executable: bool,
}

fn data_dir() -> anyhow::Result<PathBuf> {
    dirs::data_dir().context("could not find your data folder")
}

fn open_with_entry(data: &Path) -> PathBuf {
    data.join("applications").join(format!("{ID}.desktop"))
}

fn nautilus_scripts(data: &Path) -> PathBuf {
    data.join("nautilus/scripts")
}

pub fn is_installed() -> bool {
    data_dir().is_ok_and(|data| open_with_entry(&data).is_file())
}

pub fn add() -> anyhow::Result<()> {
    let exe = std::env::current_exe().context("could not locate the running executable")?;
    write(&exe).context("could not add Read aloud to the right-click menu")?;
    log::info!("added Read aloud to the right-click menu");
    Ok(())
}

pub fn remove() -> anyhow::Result<()> {
    let data = data_dir()?;
    // The Nautilus script is found by its marker, as it is named after the
    // label it was written with.
    let scripts = nautilus_scripts(&data);
    let mut paths: Vec<_> =
        files(&data, "", "").into_iter().map(|f| f.path).filter(|p| !p.starts_with(&scripts)).collect();
    paths.extend(nautilus_scripts_written(&data));
    for path in paths {
        match std::fs::remove_file(&path) {
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => {
                return Err(anyhow::Error::new(e).context("could not remove Read aloud from the right-click menu"));
            }
            _ => {}
        }
    }
    applications_changed(&data);
    log::info!("removed Read aloud from the right-click menu");
    Ok(())
}

/// Puts the menu item's label in the interface language, if the menu item is
/// installed. It keeps the copy of the app it was added from, so starting a
/// build from source doesn't take it over, unless that copy has gone.
pub fn refresh() {
    let Ok(data) = data_dir() else { return };
    let Ok(entry) = std::fs::read_to_string(open_with_entry(&data)) else { return };
    let exe = match app_in(&entry).map(PathBuf::from).filter(|app| app.is_file()) {
        Some(app) => app,
        None => match std::env::current_exe() {
            Ok(exe) => exe,
            Err(_) => return,
        },
    };
    let Ok((_, wanted)) = wanted(&exe) else { return };
    let current = wanted.iter().all(|f| std::fs::read_to_string(&f.path).is_ok_and(|c| c == f.contents))
        && nautilus_scripts_written(&data).len() == 1;
    if current {
        return;
    }
    match write(&exe) {
        Ok(()) => log::info!("updated the right-click menu"),
        Err(e) => log::warn!("could not update the right-click menu: {e:#}"),
    }
}

fn write(exe: &Path) -> anyhow::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    let (data, wanted) = wanted(exe)?;
    let ours = nautilus_scripts_written(&data);
    // A script of the user's own, with the same name as the label, is left
    // alone.
    let scripts = nautilus_scripts(&data);
    if let Some(theirs) = wanted.iter().find(|f| f.path.starts_with(&scripts) && f.path.exists() && !ours.contains(&f.path)) {
        bail!("{} is already a script in Files", theirs.path.display());
    }
    // A script under an earlier label would be a second menu item.
    for old in ours {
        if !wanted.iter().any(|f| f.path == old) {
            let _ = std::fs::remove_file(old);
        }
    }
    for file in &wanted {
        if let Some(dir) = file.path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        std::fs::write(&file.path, &file.contents)?;
        let mode = if file.executable { 0o755 } else { 0o644 };
        std::fs::set_permissions(&file.path, std::fs::Permissions::from_mode(mode))?;
    }
    applications_changed(&data);
    Ok(())
}

/// The files for the app at `exe`, in the interface language.
fn wanted(exe: &Path) -> anyhow::Result<(PathBuf, Vec<MenuFile>)> {
    let data = data_dir()?;
    let Some(exe) = exe.to_str().filter(|e| !e.contains(['\n', '\r'])) else {
        bail!("the app's folder has a name the file managers can't use");
    };
    let files = files(&data, exe, &t("context_menu.verb"));
    Ok((data, files))
}

fn files(data: &Path, exe: &str, label: &str) -> Vec<MenuFile> {
    let mime = mime_types().join(";") + ";";
    let label = one_line(label);
    let exec = desktop_quote(exe);
    // A file name can't hold a slash, or be only dots.
    let script_name = match label.replace('/', "-").trim() {
        "" | "." | ".." => ID.to_owned(),
        name => name.to_owned(),
    };
    vec![
        MenuFile {
            path: data.join("kio/servicemenus").join(format!("{ID}.desktop")),
            contents: format!(
                "[Desktop Entry]\nType=Service\nMimeType={mime}\nActions=readAloud\nX-KDE-Priority=TopLevel\n\n\
                 [Desktop Action readAloud]\nName={label}\nIcon=speechout\nExec={exec} --read %f\n"
            ),
            // Dolphin ignores a service menu in the user's folder otherwise.
            executable: true,
        },
        MenuFile {
            path: data.join("nemo/actions").join(format!("{ID}.nemo_action")),
            contents: format!(
                "[Nemo Action]\nName={label}\nComment={label}\nIcon-Name=speechout\nExec={exec} --read %F\n\
                 Selection=s\nMimetypes={mime}\n"
            ),
            executable: false,
        },
        MenuFile {
            path: open_with_entry(data),
            contents: format!(
                "[Desktop Entry]\nType=Application\nName={label}\nIcon=speechout\nExec={exec} --read %f\n\
                 MimeType={mime}\nNoDisplay=true\nTerminal=false\n{APP_KEY}={}\n",
                exe.replace('\\', r"\\")
            ),
            executable: false,
        },
        MenuFile {
            path: nautilus_scripts(data).join(script_name),
            // Nautilus gives the chosen files as arguments, from their folder.
            contents: format!("#!/bin/sh\n# {ID}\nexec {} --read \"$1\"\n", shell_quote(exe)),
            executable: true,
        },
    ]
}

/// The key in the Open With entry that names the copy of the app the menu
/// runs.
const APP_KEY: &str = "X-SpeechOut-App";

/// The copy of the app named in the Open With entry.
fn app_in(entry: &str) -> Option<String> {
    let line = entry.lines().find_map(|l| l.strip_prefix(APP_KEY)?.strip_prefix('='))?;
    Some(line.replace(r"\\", "\\"))
}

/// The Nautilus scripts the app has written, whatever their label.
fn nautilus_scripts_written(data: &Path) -> Vec<PathBuf> {
    let marker = format!("\n# {ID}\n");
    let Ok(entries) = std::fs::read_dir(nautilus_scripts(data)) else { return Vec::new() };
    entries
        .flatten()
        .map(|e| e.path())
        .filter(|p| std::fs::read_to_string(p).is_ok_and(|c| c.contains(&marker)))
        .collect()
}

/// Lets the file managers see a change to the Open With entry straight away.
fn applications_changed(data: &Path) {
    if let Some(update) = super::linux::find_program(&["update-desktop-database"]) {
        let _ = std::process::Command::new(update)
            .arg(data.join("applications"))
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status();
    }
}

/// A value in a desktop entry is one line.
fn one_line(text: &str) -> String {
    text.replace(['\n', '\r'], " ")
}

/// Quotes a program for the Exec line of a desktop entry. Inside the
/// quotes, `"`, `` ` ``, `$` and `\` are escaped with a backslash, and then
/// every backslash is doubled again, as the file's own escaping comes first.
/// `%` becomes `%%`. Nemo's actions are read the same way: Nemo reads Exec
/// with GLib's key file reader, which undoes the backslashes, turns `%%` back
/// into `%` (its TOKEN_EXEC_LITERAL_PERCENT), and parses the result as a
/// shell would, so one quoting does for every file.
fn desktop_quote(arg: &str) -> String {
    let mut quoted = String::from("\"");
    for c in arg.chars() {
        match c {
            '"' | '`' | '$' | '\\' => {
                quoted.push('\\');
                quoted.push(c);
            }
            '%' => quoted.push_str("%%"),
            _ => quoted.push(c),
        }
    }
    quoted.push('"');
    quoted.replace('\\', r"\\")
}

fn shell_quote(arg: &str) -> String {
    format!("'{}'", arg.replace('\'', r"'\''"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_kind_of_file_has_a_mime_type() {
        for ext in crate::document::FileKind::extensions(|_| true) {
            assert!(mime_type(ext).is_some(), "no MIME type for .{ext}");
        }
    }

    #[test]
    fn quotes_the_app_for_desktop_entries() {
        assert_eq!(desktop_quote("/usr/bin/speechout"), r#""/usr/bin/speechout""#);
        assert_eq!(desktop_quote(r#"/a "b" $c\d 5%"#), r#""/a \\"b\\" \\$c\\\\d 5%%""#);
        assert_eq!(shell_quote("/it's"), r"'/it'\''s'");
    }

    #[test]
    fn writes_and_removes_every_file() {
        let dir = tempfile::tempdir().unwrap();
        let files = files(dir.path(), "/usr/bin/speechout", "Read aloud with Speech Output Engine");
        assert_eq!(files.len(), 4);
        let script = dir.path().join("nautilus/scripts/Read aloud with Speech Output Engine");
        assert!(files.iter().any(|f| f.path == script && f.executable));
        std::fs::create_dir_all(script.parent().unwrap()).unwrap();
        let contents = &files.iter().find(|f| f.path == script).unwrap().contents;
        std::fs::write(&script, contents).unwrap();
        std::fs::write(script.with_file_name("Someone else's script"), "#!/bin/sh\n").unwrap();
        assert_eq!(nautilus_scripts_written(dir.path()), [script]);
        assert!(files[0].contents.contains("MimeType=application/epub+zip;application/pdf;"));
        assert!(files[0].contents.contains("Exec=\"/usr/bin/speechout\" --read %f\n"));
        let odd = files(dir.path(), "/opt/50%Fun/speechout", "Read aloud");
        assert!(odd[1].contents.contains("Exec=\"/opt/50%%Fun/speechout\" --read %F\n"), "{}", odd[1].contents);
    }

    #[test]
    fn finds_the_app_it_was_added_from() {
        let files = files(Path::new("/d"), r"/opt/a\b/speechout", "Read aloud");
        assert_eq!(app_in(&files[2].contents).as_deref(), Some(r"/opt/a\b/speechout"));
        assert_eq!(app_in("[Desktop Entry]\n"), None);
    }

    #[test]
    fn the_script_is_never_named_after_a_folder() {
        for label in ["..", " . ", ""] {
            let script = &files(Path::new("/d"), "/usr/bin/speechout", label)[3].path;
            assert_eq!(*script, Path::new("/d/nautilus/scripts").join(ID));
        }
    }
}
