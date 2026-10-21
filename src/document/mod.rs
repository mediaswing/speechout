//! Text extraction from PDF, TXT, Markdown, DOCX, ODT, CSV, ODS, PPTX, PPT
//! and EPUB files. Each format is read in a file of its own; this one says
//! which kind of file is which, and tidies the text they give.

mod xml;
mod narration;
mod text;
mod pdf;
mod csv;
mod markdown;
mod docx;
mod odf;
mod pptx;
mod ppt;
mod epub;

use anyhow::{Context, bail};
use csv::read_csv;
use docx::read_docx;
use epub::read_epub;
use markdown::markdown_to_text;
use odf::{ods_xml_to_text, read_odt};
use pdf::read_pdf;
use ppt::read_ppt;
use pptx::read_pptx;
use std::path::Path;
use text::read_txt;
use xml::read_zip_xml;

/// Files larger than this are refused, to keep memory use predictable.
const MAX_FILE_BYTES: u64 = 200 * 1024 * 1024;
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FileKind {
    Pdf,
    Text,
    Markdown,
    Docx,
    Odt,
    Csv,
    Ods,
    Pptx,
    Ppt,
    Epub,
    Image,
}

/// Every file name ending the app reads, and the kind of file it marks. The
/// file dialog and the right-click menu on Windows offer these endings, and a
/// test checks the installer's list matches, so a new kind of file is added
/// here alone.
const EXTENSIONS: [(&str, FileKind); 17] = [
    ("pdf", FileKind::Pdf),
    ("txt", FileKind::Text),
    ("md", FileKind::Markdown),
    ("markdown", FileKind::Markdown),
    ("docx", FileKind::Docx),
    ("odt", FileKind::Odt),
    ("csv", FileKind::Csv),
    ("ods", FileKind::Ods),
    ("pptx", FileKind::Pptx),
    ("ppt", FileKind::Ppt),
    ("epub", FileKind::Epub),
    ("jpg", FileKind::Image),
    ("jpeg", FileKind::Image),
    ("png", FileKind::Image),
    ("jxl", FileKind::Image),
    ("heic", FileKind::Image),
    ("heif", FileKind::Image),
];

impl FileKind {
    pub fn from_path(path: &Path) -> Option<Self> {
        let ext = path.extension()?.to_str()?.to_ascii_lowercase();
        // ".text" is read too, but not offered: it is rare, and the file
        // dialog's list is long enough.
        if ext == "text" {
            return Some(FileKind::Text);
        }
        EXTENSIONS.iter().find(|(e, _)| *e == ext).map(|(_, kind)| *kind)
    }

    /// The endings of the kinds of file `which` picks, such as
    /// `FileKind::extensions(|k| k == FileKind::Image)` for images.
    pub fn extensions(which: impl Fn(FileKind) -> bool) -> Vec<&'static str> {
        EXTENSIONS.iter().filter(|(_, kind)| which(*kind)).map(|(e, _)| *e).collect()
    }

    /// Whether the Content or Format setting changes how this kind of file
    /// is read.
    pub fn reads_formatting(self) -> bool {
        matches!(self, FileKind::Docx | FileKind::Odt | FileKind::Markdown | FileKind::Epub)
    }
}

/// Whether a file must be read with the system voices, never a cloud voice
/// service. PNG images are most often screenshots, which can show private
/// details such as messages, emails and account numbers, so their
/// descriptions stay on the computer. This goes by the name; a PNG named as
/// something else is caught when it is decoded (see `worker::read_file`).
pub fn system_voices_only(path: &Path) -> bool {
    path.extension().is_some_and(|e| e.eq_ignore_ascii_case("png"))
}

/// How DOCX, ODT, Markdown and EPUB files are read: the text alone, or the
/// text with what its formatting shows, such as headings, lists, tables, bold
/// and links.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum DocumentReading {
    #[default]
    Content,
    Format,
}

pub fn extract_text(path: &Path, reading: DocumentReading) -> anyhow::Result<String> {
    let size = std::fs::metadata(path).context("the file could not be opened")?.len();
    if size > MAX_FILE_BYTES {
        bail!("the file is too large (the limit is 200 MB)");
    }
    let text = match FileKind::from_path(path) {
        Some(FileKind::Text) => read_txt(&std::fs::read(path)?),
        Some(FileKind::Markdown) => markdown_to_text(&read_txt(&std::fs::read(path)?), reading),
        Some(FileKind::Pdf) => read_pdf(path)?,
        Some(FileKind::Docx) => read_docx(path, reading)?,
        Some(FileKind::Odt) => read_odt(path, reading)?,
        Some(FileKind::Csv) => read_csv(&std::fs::read(path)?),
        Some(FileKind::Ods) => ods_xml_to_text(&read_zip_xml(path, "content.xml", "ODS")?)?,
        Some(FileKind::Pptx) => read_pptx(path)?,
        Some(FileKind::Ppt) => read_ppt(path)?,
        Some(FileKind::Epub) => read_epub(path, reading)?,
        _ => bail!("this type of file is not supported"),
    };
    let text = tidy(&text);
    if text.is_empty() {
        bail!("no readable text was found in this file. If it is a scanned PDF, it contains only pictures of text.");
    }
    Ok(text)
}

/// Most characters that may be typed or pasted into the text box, about 30,000
/// words. Keeps the box responsive and cloud costs bounded.
pub const MAX_PASTED_CHARS: usize = 200_000;

/// Cleans text typed or pasted into the app before it is read aloud: cut to
/// `MAX_PASTED_CHARS`, stripped of invisible characters that can hide words or
/// reorder how text is shown, then tidied like text from a file.
pub fn clean_pasted(text: &str) -> String {
    let visible: String = text.chars().take(MAX_PASTED_CHARS).filter(|c| !is_hidden_format(*c)).collect();
    tidy(&visible)
}

/// Zero-width spaces, byte order marks, bidirectional embeddings, overrides
/// and isolates, and interlinear annotations. Joiners are kept, because emoji
/// and some scripts need them.
fn is_hidden_format(c: char) -> bool {
    matches!(
        c,
        '\u{200B}' | '\u{2060}' | '\u{FEFF}' | '\u{202A}'..='\u{202E}' | '\u{2066}'..='\u{2069}' | '\u{FFF9}'..='\u{FFFB}'
    )
}

/// Normalises whitespace: joins lines broken mid-sentence by PDF layout while
/// keeping paragraph breaks, and drops control characters. A Windows line
/// ending ("\r\n") is one line break, not two, so it is not taken as a
/// paragraph break.
pub(super) fn tidy(text: &str) -> String {
    let cleaned: String = text
        .replace("\r\n", "\n")
        .chars()
        .map(|c| if c == '\r' || c == '\u{c}' { '\n' } else { c })
        .filter(|c| !c.is_control() || *c == '\n' || *c == '\t')
        .collect();
    let mut paragraphs = Vec::new();
    for para in cleaned.split("\n\n") {
        let joined = para.split_whitespace().collect::<Vec<_>>().join(" ");
        if !joined.is_empty() {
            paragraphs.push(joined);
        }
    }
    paragraphs.join("\n\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_kinds() {
        assert_eq!(FileKind::from_path(Path::new("a.PDF")), Some(FileKind::Pdf));
        assert_eq!(FileKind::from_path(Path::new("a.md")), Some(FileKind::Markdown));
        assert_eq!(FileKind::from_path(Path::new("a.Markdown")), Some(FileKind::Markdown));
        assert_eq!(FileKind::from_path(Path::new("a.Csv")), Some(FileKind::Csv));
        assert_eq!(FileKind::from_path(Path::new("a.ODT")), Some(FileKind::Odt));
        assert_eq!(FileKind::from_path(Path::new("a.ods")), Some(FileKind::Ods));
        assert_eq!(FileKind::from_path(Path::new("a.PPTX")), Some(FileKind::Pptx));
        assert_eq!(FileKind::from_path(Path::new("a.ppt")), Some(FileKind::Ppt));
        assert_eq!(FileKind::from_path(Path::new("a.EPUB")), Some(FileKind::Epub));
        assert_eq!(FileKind::from_path(Path::new("a.heic")), Some(FileKind::Image));
        assert_eq!(FileKind::from_path(Path::new("a.PNG")), Some(FileKind::Image));
        assert_eq!(FileKind::from_path(Path::new("a.jxl")), Some(FileKind::Image));
        assert_eq!(FileKind::from_path(Path::new("a.text")), Some(FileKind::Text));
        assert_eq!(FileKind::from_path(Path::new("a.exe")), None);
    }

    #[test]
    fn every_ending_offered_is_read_as_its_kind() {
        for (ext, kind) in EXTENSIONS {
            assert_eq!(FileKind::from_path(Path::new(&format!("a.{ext}"))), Some(kind), "{ext}");
        }
        let documents = FileKind::extensions(|k| k != FileKind::Image);
        let images = FileKind::extensions(|k| k == FileKind::Image);
        assert_eq!(documents.len() + images.len(), EXTENSIONS.len());
        assert!(documents.contains(&"epub") && images.contains(&"heic"));
    }

    /// The ContextMenuExtensions list in the Windows installer's source.
    fn installer_extensions(wxs: &str) -> Vec<&str> {
        let list = wxs
            .split("<?define ContextMenuExtensions = ")
            .nth(1)
            .and_then(|rest| rest.split("?>").next())
            .expect("the installer has no ContextMenuExtensions list");
        list.trim().split(';').collect()
    }

    /// Checked on every platform, so a change to the list shows up wherever
    /// the tests are run, not only in the Windows build.
    #[test]
    fn the_installer_adds_the_menu_for_the_same_files() {
        assert_eq!(installer_extensions(include_str!("../installer/speechout.wxs")), FileKind::extensions(|_| true));
    }

    #[test]
    fn formatting_setting_applies_to_documents_with_formatting() {
        let formatted: Vec<_> =
            EXTENSIONS.iter().filter(|(_, k)| k.reads_formatting()).map(|(e, _)| *e).collect();
        assert_eq!(formatted, ["md", "markdown", "docx", "odt", "epub"]);
    }

    #[test]
    fn png_images_use_system_voices_only() {
        assert!(system_voices_only(Path::new("Screenshot 2026-10-15.png")));
        assert!(system_voices_only(Path::new("a.PNG")));
        for other in ["a.jpg", "a.jxl", "a.heic", "a.pdf", "png"] {
            assert!(!system_voices_only(Path::new(other)), "{other}");
        }
    }

    #[test]
    fn pasted_text_is_cleaned() {
        let pasted = "\u{FEFF}Fish\u{200B} &\u{0}\u{7} chips\r\nare \u{202E}tsaf\u{202C}\r\n\r\n\u{1b}[31mnext\tpara";
        assert_eq!(clean_pasted(pasted), "Fish & chips are tsaf\n\n[31mnext para");
        // Joiners in emoji survive.
        assert_eq!(clean_pasted("a \u{1F469}\u{200D}\u{1F4BB} b"), "a \u{1F469}\u{200D}\u{1F4BB} b");
        assert_eq!(clean_pasted(" \u{200B}\n\n "), "");
    }

    #[test]
    fn pasted_text_is_limited() {
        let long = "ab ".repeat(MAX_PASTED_CHARS);
        assert!(clean_pasted(&long).chars().count() <= MAX_PASTED_CHARS);
    }
}
