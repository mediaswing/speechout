//! Getting the text out of PDF, TXT, DOCX and CSV files. Files are untrusted: sizes
//! are limited before anything is read or unpacked, so a small file can't
//! expand to fill the memory, and a crash in the PDF parser is contained.

use std::io::Read;
use std::path::Path;

use anyhow::{Context, bail};
use quick_xml::events::Event;

const MAX_FILE_BYTES: u64 = 200 * 1024 * 1024;
/// The most a DOCX's main part may unpack to, and so the size of the largest
/// zip bomb that can do any harm.
const MAX_DOCX_XML_BYTES: u64 = 256 * 1024 * 1024;

/// What a chosen file holds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileKind {
    Pdf,
    Txt,
    Docx,
    /// A table, read a row at a time by `crate::table`.
    Csv,
    Jpeg,
    Heif,
}

impl FileKind {
    /// The kind of file, from its extension. `None` if it isn't supported.
    pub fn of(path: &Path) -> Option<Self> {
        let ext = path.extension()?.to_str()?.to_ascii_lowercase();
        match ext.as_str() {
            "pdf" => Some(FileKind::Pdf),
            "txt" => Some(FileKind::Txt),
            "docx" => Some(FileKind::Docx),
            "csv" => Some(FileKind::Csv),
            "jpg" | "jpeg" => Some(FileKind::Jpeg),
            "heic" | "heif" => Some(FileKind::Heif),
            _ => None,
        }
    }

    pub fn is_image(self) -> bool {
        matches!(self, FileKind::Jpeg | FileKind::Heif)
    }
}

/// The extensions offered in the file picker.
pub const EXTENSIONS: &[&str] = &["pdf", "txt", "docx", "csv", "jpg", "jpeg", "heic", "heif"];

/// Reads the text of a PDF, TXT or DOCX file, or a CSV file's table as
/// sentences to read aloud.
pub fn read_text(path: &Path, kind: FileKind) -> anyhow::Result<String> {
    if std::fs::metadata(path).context("the file could not be opened")?.len() > MAX_FILE_BYTES {
        bail!("the file is larger than 200 MB");
    }
    let text = match kind {
        FileKind::Pdf => read_pdf(path)?,
        FileKind::Txt => decode_text(&std::fs::read(path).context("the file could not be read")?),
        FileKind::Docx => read_docx(path)?,
        FileKind::Csv => crate::table::speakable(&decode_text(&std::fs::read(path).context("the file could not be read")?))?,
        FileKind::Jpeg | FileKind::Heif => bail!("this is an image, not a document"),
    };
    let text = tidy(&text);
    if text.is_empty() {
        bail!("no text was found in this file. If it is a scanned document, it holds only pictures of text");
    }
    Ok(text)
}

fn read_pdf(path: &Path) -> anyhow::Result<String> {
    let bytes = std::fs::read(path).context("the file could not be read")?;
    // The PDF parser can panic on malformed files; contain that to this call.
    match std::panic::catch_unwind(|| pdf_extract::extract_text_from_mem(&bytes)) {
        Ok(Ok(text)) => Ok(text),
        Ok(Err(e)) => {
            log::warn!("PDF extraction failed: {e}");
            bail!("the PDF could not be read. It may be damaged or password protected")
        }
        Err(_) => bail!("the PDF could not be read because it is malformed"),
    }
}

/// Text files come in many encodings: a byte order mark settles it, then
/// UTF-8 if it is valid, and otherwise the usual Windows encoding.
fn decode_text(bytes: &[u8]) -> String {
    if let Some((encoding, bom)) = encoding_rs::Encoding::for_bom(bytes) {
        return encoding.decode_without_bom_handling(&bytes[bom..]).0.into_owned();
    }
    match std::str::from_utf8(bytes) {
        Ok(text) => text.to_owned(),
        Err(_) => encoding_rs::WINDOWS_1252.decode(bytes).0.into_owned(),
    }
}

fn read_docx(path: &Path) -> anyhow::Result<String> {
    let file = std::fs::File::open(path).context("the file could not be opened")?;
    let mut archive = zip::ZipArchive::new(file).context("this DOCX file is damaged")?;
    let part = archive.by_name("word/document.xml").context("this DOCX file has no document in it")?;
    let mut xml = Vec::new();
    part.take(MAX_DOCX_XML_BYTES + 1).read_to_end(&mut xml).context("this DOCX file is damaged")?;
    if xml.len() as u64 > MAX_DOCX_XML_BYTES {
        bail!("this DOCX file is too large to read");
    }
    docx_text(&String::from_utf8_lossy(&xml))
}

/// The document's words, with a line for each paragraph. Words deleted with
/// tracked changes are in `delText` elements, so they aren't read.
fn docx_text(xml: &str) -> anyhow::Result<String> {
    let mut reader = quick_xml::Reader::from_str(xml);
    let mut out = String::new();
    let mut in_text = false;
    loop {
        match reader.read_event().context("this DOCX file is damaged")? {
            Event::DocType(_) => bail!("this DOCX file is damaged"),
            Event::Start(e) if e.local_name().as_ref() == "t" => in_text = true,
            Event::End(e) => match e.local_name().as_ref() {
                "t" => in_text = false,
                "p" => out.push('\n'),
                _ => {}
            },
            Event::Empty(e) => match e.local_name().as_ref() {
                "tab" => out.push(' '),
                "br" | "p" => out.push('\n'),
                _ => {}
            },
            Event::Text(t) if in_text => out.push_str(&t.xml10_content()),
            Event::GeneralRef(r) if in_text => {
                let name: &str = &r;
                let entity = match name {
                    "amp" => Some('&'),
                    "lt" => Some('<'),
                    "gt" => Some('>'),
                    "apos" => Some('\''),
                    "quot" => Some('"'),
                    _ => r.resolve_char_ref().ok().flatten(),
                };
                out.extend(entity);
            }
            Event::Eof => break,
            _ => {}
        }
    }
    Ok(out)
}

/// Normalises line endings, removes control characters and trims.
fn tidy(text: &str) -> String {
    let text = text.replace("\r\n", "\n").replace('\r', "\n");
    let cleaned: String = text.chars().filter(|c| !c.is_control() || matches!(c, '\n' | '\t')).collect();
    cleaned.trim().to_owned()
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use std::io::Write;

    /// A small DOCX file holding `document_xml`.
    pub(crate) fn docx(path: &Path, document_xml: &str) {
        let mut zip = zip::ZipWriter::new(std::fs::File::create(path).unwrap());
        zip.start_file("word/document.xml", zip::write::SimpleFileOptions::default()).unwrap();
        zip.write_all(document_xml.as_bytes()).unwrap();
        zip.finish().unwrap();
    }

    pub(crate) const DOCX_XML: &str = r#"<?xml version="1.0"?><w:document xmlns:w="w"><w:body>
        <w:p><w:r><w:t>Hello &amp; welcome</w:t></w:r><w:r><w:tab/><w:t xml:space="preserve">to Leicester.</w:t></w:r></w:p>
        <w:p><w:r><w:delText>Deleted words.</w:delText></w:r><w:r><w:t>Second&#8217;s line.</w:t></w:r></w:p>
        </w:body></w:document>"#;

    #[test]
    fn knows_file_kinds_whatever_their_case() {
        assert_eq!(FileKind::of(Path::new("a.PDF")), Some(FileKind::Pdf));
        assert_eq!(FileKind::of(Path::new("a.JpG")), Some(FileKind::Jpeg));
        assert_eq!(FileKind::of(Path::new("a.heif")), Some(FileKind::Heif));
        assert_eq!(FileKind::of(Path::new("a.Csv")), Some(FileKind::Csv));
        assert_eq!(FileKind::of(Path::new("a.exe")), None);
        assert_eq!(FileKind::of(Path::new("noextension")), None);
        for ext in EXTENSIONS {
            assert!(FileKind::of(Path::new(&format!("a.{ext}"))).is_some());
        }
    }

    #[test]
    fn reads_docx() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("a.docx");
        docx(&path, DOCX_XML);
        assert_eq!(read_text(&path, FileKind::Docx).unwrap(), "Hello & welcome to Leicester.\nSecond\u{2019}s line.");
    }

    #[test]
    fn reads_csv_as_sentences() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("a.csv");
        // Saved by Excel on Windows: Windows-1252, with a pound sign.
        std::fs::write(&path, b"Item,Price\r\nTea,\xA31.50\r\nCake,\xA32.00\r\n").unwrap();
        let text = read_text(&path, FileKind::Csv).unwrap();
        assert_eq!(
            text,
            "This table has 2 rows, with the columns item and price.\nItem Tea: price 1 pound 50 pence.\nItem Cake: price 2 pounds."
        );
    }

    /// Prints what a real CSV file sounds like:
    /// `E2E_CSV=table.csv cargo test real_csv -- --ignored --nocapture`.
    #[test]
    #[ignore]
    fn real_csv() {
        let path = std::env::var("E2E_CSV").expect("set E2E_CSV to a CSV file");
        println!("{}", read_text(Path::new(&path), FileKind::Csv).unwrap());
    }

    #[test]
    fn reads_pdf() {
        let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/simple.pdf");
        assert!(!read_text(&path, FileKind::Pdf).unwrap().is_empty());
    }

    #[test]
    fn decodes_text_encodings() {
        assert_eq!(decode_text(b"\xEF\xBB\xBFcaf\xC3\xA9"), "café");
        assert_eq!(decode_text(b"caf\xC3\xA9"), "café");
        assert_eq!(decode_text(b"caf\xE9"), "café");
        assert_eq!(decode_text(b"\xFF\xFEh\0i\0"), "hi");
    }

    #[test]
    fn tidies_text() {
        assert_eq!(tidy("  a\r\nb\rc\u{7}\u{0}\n"), "a\nb\nc");
    }

    #[test]
    fn damaged_files_are_errors_not_crashes() {
        let dir = tempfile::tempdir().unwrap();
        for (name, kind) in [("a.pdf", FileKind::Pdf), ("a.docx", FileKind::Docx)] {
            let path = dir.path().join(name);
            std::fs::write(&path, b"this is not what it says it is").unwrap();
            assert!(read_text(&path, kind).is_err(), "{name}");
        }
        let empty = dir.path().join("empty.txt");
        std::fs::write(&empty, b"   \n").unwrap();
        assert!(read_text(&empty, FileKind::Txt).unwrap_err().to_string().contains("no text"));
        assert!(read_text(&dir.path().join("missing.txt"), FileKind::Txt).is_err());
    }

    #[test]
    fn docx_without_a_document_is_an_error() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("a.docx");
        let mut zip = zip::ZipWriter::new(std::fs::File::create(&path).unwrap());
        zip.start_file("other.txt", zip::write::SimpleFileOptions::default()).unwrap();
        zip.finish().unwrap();
        assert!(read_text(&path, FileKind::Docx).is_err());
    }
}
