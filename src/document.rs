//! Text extraction from PDF, TXT, Markdown, DOCX, ODT, CSV, ODS, PPTX and PPT files.

use anyhow::{Context, bail};
use quick_xml::events::Event;
use std::collections::HashMap;
use std::io::Read;
use std::path::Path;

/// Files larger than this are refused, to keep memory use predictable.
const MAX_FILE_BYTES: u64 = 200 * 1024 * 1024;
/// Limit on the uncompressed size of a DOCX, ODT or ODS body, or of all the
/// slides of a PPTX file together, which guards against zip bombs.
const MAX_XML_BYTES: u64 = 100 * 1024 * 1024;
/// Limit on the cells an ODS file may expand to. Spreadsheets mark runs of
/// identical cells and rows with a repeat count rather than writing them out,
/// so a small file could otherwise ask for billions of cells.
const MAX_ODS_CELLS: usize = 2_000_000;
/// Limit on the text read from a PPT file. Its slides can refer to the same
/// drawing or text over and over, so a small file could otherwise expand to
/// more text than fits in memory.
const MAX_PPT_TEXT_BYTES: usize = 100 * 1024 * 1024;

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
    Image,
}

impl FileKind {
    pub fn from_path(path: &Path) -> Option<Self> {
        let ext = path.extension()?.to_str()?.to_ascii_lowercase();
        match ext.as_str() {
            "pdf" => Some(FileKind::Pdf),
            "txt" | "text" => Some(FileKind::Text),
            "md" | "markdown" => Some(FileKind::Markdown),
            "docx" => Some(FileKind::Docx),
            "odt" => Some(FileKind::Odt),
            "csv" => Some(FileKind::Csv),
            "ods" => Some(FileKind::Ods),
            "pptx" => Some(FileKind::Pptx),
            "ppt" => Some(FileKind::Ppt),
            "jpg" | "jpeg" | "png" | "jxl" | "heic" | "heif" => Some(FileKind::Image),
            _ => None,
        }
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

/// How DOCX, ODT and Markdown files are read: the text alone, or the text
/// with what its formatting shows, such as headings, lists, tables, bold and
/// links.
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
        _ => bail!("this type of file is not supported"),
    };
    let text = tidy(&text);
    if text.is_empty() {
        bail!("no readable text was found in this file. If it is a scanned PDF, it contains only pictures of text.");
    }
    Ok(text)
}

/// Decodes UTF-8 or UTF-16 (with byte order mark), falling back to Windows-1252
/// style lossy decoding for legacy files.
fn read_txt(bytes: &[u8]) -> String {
    match bytes {
        [0xEF, 0xBB, 0xBF, rest @ ..] => String::from_utf8_lossy(rest).into_owned(),
        [0xFF, 0xFE, rest @ ..] => decode_utf16(rest, u16::from_le_bytes),
        [0xFE, 0xFF, rest @ ..] => decode_utf16(rest, u16::from_be_bytes),
        _ => match std::str::from_utf8(bytes) {
            Ok(s) => s.to_owned(),
            // Latin-1 maps every byte to a character, so nothing is lost silently.
            Err(_) => bytes.iter().map(|&b| char::from(b)).collect(),
        },
    }
}

fn decode_utf16(bytes: &[u8], f: fn([u8; 2]) -> u16) -> String {
    let units: Vec<u16> = bytes.as_chunks::<2>().0.iter().map(|c| f([c[0], c[1]])).collect();
    String::from_utf16_lossy(&units)
}

fn read_pdf(path: &Path) -> anyhow::Result<String> {
    let bytes = std::fs::read(path)?;
    // The PDF parser can panic on malformed files; contain that to this call.
    match std::panic::catch_unwind(|| pdf_extract::extract_text_from_mem(&bytes)) {
        Ok(Ok(text)) => Ok(text),
        Ok(Err(e)) => {
            log::warn!("PDF extraction failed: {e}");
            bail!("the PDF could not be read. It may be damaged or password protected.")
        }
        Err(_) => bail!("the PDF could not be read because it is malformed"),
    }
}

fn read_docx(path: &Path, reading: DocumentReading) -> anyhow::Result<String> {
    if reading == DocumentReading::Content {
        return docx_xml_to_text(&read_zip_xml(path, "word/document.xml", "DOCX")?);
    }
    let mut archive = open_zip(path, "DOCX")?;
    let mut budget = MAX_XML_BYTES;
    let body = read_zip_entry(&mut archive, "word/document.xml", "DOCX", &mut budget)?;
    // Without its styles, a document is still read, just without its headings.
    let styles = read_zip_entry(&mut archive, "word/styles.xml", "DOCX", &mut budget)
        .and_then(|xml| parse_docx_styles(&xml))
        .unwrap_or_else(|e| {
            log::warn!("DOCX styles could not be read: {e:#}");
            HashMap::new()
        });
    docx_xml_to_spoken(&body, &styles)
}

fn read_odt(path: &Path, reading: DocumentReading) -> anyhow::Result<String> {
    if reading == DocumentReading::Content {
        return odt_xml_to_text(&read_zip_xml(path, "content.xml", "ODT")?);
    }
    let mut archive = open_zip(path, "ODT")?;
    let mut budget = MAX_XML_BYTES;
    let body = read_zip_entry(&mut archive, "content.xml", "ODT", &mut budget)?;
    let mut styles = OdfStyles::default();
    // The named styles, such as "Strong Emphasis", are in styles.xml, and the
    // ones made for this document's own formatting are in content.xml.
    if let Err(e) = read_zip_entry(&mut archive, "styles.xml", "ODT", &mut budget)
        .and_then(|xml| styles.add(&xml))
    {
        log::warn!("ODT styles could not be read: {e:#}");
    }
    styles.add(&body)?;
    odt_xml_to_spoken(&body, &styles)
}

/// Reads the XML body `name` out of a zipped office document. `format` names
/// the file type in error messages.
fn read_zip_xml(path: &Path, name: &str, format: &str) -> anyhow::Result<String> {
    read_zip_entry(&mut open_zip(path, format)?, name, format, &mut { MAX_XML_BYTES })
}

fn open_zip(path: &Path, format: &str) -> anyhow::Result<zip::ZipArchive<std::fs::File>> {
    let file = std::fs::File::open(path)?;
    zip::ZipArchive::new(file).with_context(|| format!("this is not a valid {format} file"))
}

/// Reads the XML part `name` out of an open archive, taking its size off
/// `budget`, the uncompressed bytes still allowed for this document.
fn read_zip_entry(
    archive: &mut zip::ZipArchive<std::fs::File>,
    name: &str,
    format: &str,
    budget: &mut u64,
) -> anyhow::Result<String> {
    let entry = archive.by_name(name).with_context(|| format!("this {format} file has no document body"))?;
    let mut xml = String::new();
    entry
        .take(*budget + 1)
        .read_to_string(&mut xml)
        .with_context(|| format!("the {format} document body is not valid text"))?;
    if xml.len() as u64 > *budget {
        bail!("the {format} document is too large");
    }
    *budget -= xml.len() as u64;
    Ok(xml)
}

/// The value of attribute `name` (as written, with its prefix), if present.
fn attr(e: &quick_xml::events::BytesStart, name: &str) -> Option<String> {
    let a = e.try_get_attribute(name).ok()??;
    Some(a.normalized_value(quick_xml::XmlVersion::Implicit1_0).ok()?.into_owned())
}

/// Appends the character an entity or character reference stands for. Only
/// the five predefined XML entities are expanded, so there is no XXE risk.
fn push_ref(out: &mut String, r: &quick_xml::events::BytesRef) {
    if let Ok(Some(c)) = r.resolve_char_ref() {
        out.push(c);
    } else {
        match r.as_ref() {
            "amp" => out.push('&'),
            "lt" => out.push('<'),
            "gt" => out.push('>'),
            "quot" => out.push('"'),
            "apos" => out.push('\''),
            _ => {}
        }
    }
}

/// Pulls the visible text out of WordprocessingML. Entities other than the
/// five predefined XML ones are never expanded, so there is no XXE risk.
fn docx_xml_to_text(xml: &str) -> anyhow::Result<String> {
    let mut reader = quick_xml::Reader::from_str(xml);
    let mut out = String::new();
    let mut in_text = false;
    loop {
        match reader.read_event().context("the DOCX document body is damaged")? {
            Event::Start(e) => match e.local_name().as_ref() {
                "t" => in_text = true,
                "tab" => out.push('\t'),
                _ => {}
            },
            Event::Empty(e) => match e.local_name().as_ref() {
                "tab" => out.push('\t'),
                "br" | "cr" => out.push('\n'),
                _ => {}
            },
            Event::End(e) => match e.local_name().as_ref() {
                "t" => in_text = false,
                "p" => out.push_str("\n\n"),
                _ => {}
            },
            Event::Text(t) if in_text => {
                out.push_str(&t.xml10_content());
            }
            Event::GeneralRef(r) if in_text => push_ref(&mut out, &r),
            Event::Eof => break,
            _ => {}
        }
    }
    Ok(out)
}

/// OpenDocument elements whose text is left out, as it is for DOCX: comments,
/// footnotes and endnotes, and deleted text kept for tracked changes.
const ODF_SKIPPED: [&str; 3] = ["annotation", "note", "tracked-changes"];

/// Pulls the visible text out of an ODT `content.xml`. Headings and
/// paragraphs each become a paragraph.
fn odt_xml_to_text(xml: &str) -> anyhow::Result<String> {
    let mut reader = quick_xml::Reader::from_str(xml);
    let mut out = String::new();
    // How many paragraphs or headings the reader is inside, and how many
    // skipped elements.
    let mut paragraphs = 0usize;
    let mut skipped = 0usize;
    loop {
        match reader.read_event().context("the ODT document body is damaged")? {
            Event::Start(e) => match e.local_name().as_ref() {
                name if ODF_SKIPPED.contains(&name) => skipped += 1,
                "p" | "h" => paragraphs += 1,
                _ => {}
            },
            Event::Empty(e) if skipped == 0 => match e.local_name().as_ref() {
                "s" => out.push(' '),
                "tab" => out.push('\t'),
                "line-break" => out.push('\n'),
                _ => {}
            },
            Event::End(e) => match e.local_name().as_ref() {
                name if ODF_SKIPPED.contains(&name) => skipped = skipped.saturating_sub(1),
                "p" | "h" => {
                    paragraphs = paragraphs.saturating_sub(1);
                    if skipped == 0 {
                        out.push_str("\n\n");
                    }
                }
                _ => {}
            },
            Event::Text(t) if paragraphs > 0 && skipped == 0 => out.push_str(&t.xml10_content()),
            Event::GeneralRef(r) if paragraphs > 0 && skipped == 0 => push_ref(&mut out, &r),
            Event::Eof => break,
            _ => {}
        }
    }
    Ok(out)
}

// Formatting, as said when a DOCX, ODT or Markdown file is read with its
// format. Like "This is part 1 of 3", these words go to the voice, so they
// stay in English.

const BOLD: u8 = 1;
const ITALIC: u8 = 2;
const UNDERLINE: u8 = 4;
const STRIKE: u8 = 8;
const LINK: u8 = 16;

/// What is said as each kind of formatting starts and ends.
const MARK_WORDS: [(u8, &str, &str); 5] = [
    (BOLD, "bold", "end bold"),
    (ITALIC, "italic", "end italic"),
    (UNDERLINE, "underlined", "end underline"),
    (STRIKE, "crossed out", "end crossed out"),
    (LINK, "link", "end link"),
];

/// Text colours are said as the nearest of these.
const COLOURS: [(&str, [u8; 3]); 16] = [
    ("black", [0, 0, 0]),
    ("dark grey", [64, 64, 64]),
    ("grey", [128, 128, 128]),
    ("light grey", [192, 192, 192]),
    ("white", [255, 255, 255]),
    ("red", [220, 30, 30]),
    ("dark red", [128, 0, 0]),
    ("orange", [255, 140, 0]),
    ("yellow", [255, 220, 0]),
    ("green", [0, 160, 60]),
    ("dark green", [0, 90, 30]),
    ("light blue", [80, 170, 240]),
    ("blue", [0, 80, 220]),
    ("dark blue", [0, 0, 128]),
    ("purple", [120, 40, 160]),
    ("pink", [240, 110, 170]),
];

/// The name of a colour written as six hex digits, such as "FF0000" or
/// "#ff0000". Black is the usual colour of text, so it isn't worth saying.
fn colour_name(hex: &str) -> Option<&'static str> {
    let hex = hex.trim_start_matches('#');
    if hex.len() != 6 || !hex.is_ascii() {
        return None;
    }
    let rgb: Vec<i32> = (0..3).map(|i| i32::from_str_radix(&hex[i * 2..i * 2 + 2], 16)).collect::<Result<_, _>>().ok()?;
    let (name, _) = COLOURS.iter().min_by_key(|(_, c)| {
        c.iter().zip(&rgb).map(|(a, b)| (i32::from(*a) - b).pow(2)).sum::<i32>()
    })?;
    (*name != "black").then_some(*name)
}

/// How deep a chain of styles, each based on the one before, is followed.
/// Real documents need a few; a damaged one could go round in a circle.
const MAX_STYLE_DEPTH: usize = 20;

/// The formatting of a piece of text.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct Marks {
    flags: u8,
    colour: Option<&'static str>,
}

impl Marks {
    /// Inside a link, its underlining and colour say nothing more.
    fn in_link(mut self, links: usize) -> Self {
        if links > 0 {
            self.flags = (self.flags | LINK) & !UNDERLINE;
            self.colour = None;
        }
        self
    }

    /// Without the formatting in `unsaid`.
    fn without(mut self, unsaid: Marks) -> Self {
        self.flags &= !unsaid.flags;
        if self.colour == unsaid.colour {
            self.colour = None;
        }
        self
    }
}

/// Formatting a style turns on and off, leaving the rest as it was.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct MarkChange {
    on: u8,
    off: u8,
    /// `Some(None)` sets the colour back to the usual one.
    colour: Option<Option<&'static str>>,
}

impl MarkChange {
    fn set(&mut self, mark: u8, on: bool) {
        if on {
            self.on |= mark;
            self.off &= !mark;
        } else {
            self.off |= mark;
            self.on &= !mark;
        }
    }

    fn apply(self, marks: Marks) -> Marks {
        Marks { flags: (marks.flags | self.on) & !self.off, colour: self.colour.unwrap_or(marks.colour) }
    }
}

/// One paragraph, gathered before it is said, since what it is (a heading,
/// say) is only known once its properties have been read.
#[derive(Default)]
struct SpokenParagraph {
    /// Said first, such as "Heading level 1."
    intro: String,
    /// Formatting that comes with the kind of paragraph, such as a heading's
    /// bold, which would only get in the way if it were said.
    unsaid: Marks,
    /// Formatting of the whole paragraph, from its style.
    marks: Marks,
    runs: Vec<(String, Marks)>,
}

impl SpokenParagraph {
    fn push(&mut self, text: &str, marks: Marks) {
        match self.runs.last_mut() {
            Some((last, m)) if *m == marks => last.push_str(text),
            _ => self.runs.push((text.to_owned(), marks)),
        }
    }
}

/// The text being put together, with formatting said in words.
#[derive(Default)]
struct Spoken {
    out: String,
}

impl Spoken {
    /// A line of its own, such as "Table."
    fn line(&mut self, text: &str) {
        self.out.push_str(text);
        self.out.push_str("\n\n");
    }

    fn paragraph(&mut self, p: SpokenParagraph) {
        // An empty paragraph says nothing, not even "List item."
        if p.runs.iter().all(|(text, _)| text.trim().is_empty()) {
            return;
        }
        let mut said = p.intro.clone();
        let mut open = Marks::default();
        let mut started = false;
        for (text, marks) in &p.runs {
            let marks = marks.without(p.unsaid);
            // Formatting on spaces or punctuation alone is not worth saying.
            let words = text.find(char::is_alphanumeric);
            let mut text = text.as_str();
            if let Some(first) = words.filter(|_| marks != open) {
                // Punctuation before the first word, such as the full stop in
                // ". See", ends what came before.
                said.push_str(&text[..first]);
                text = &text[first..];
                say_marks(&mut said, &mark_change_words(open, marks), !started);
                open = marks;
            }
            started |= words.is_some();
            said.push_str(text);
        }
        if open != Marks::default() {
            let words = mark_change_words(open, Marks::default());
            let end = said.trim_end_matches([' ', '\t']).len();
            said.truncate(end);
            if said.ends_with(['.', '!', '?']) {
                said.push(' ');
                said.push_str(&capitalise(&words));
            } else {
                said.push_str(", ");
                said.push_str(&words);
            }
            said.push('.');
        }
        self.out.push_str(&said);
        self.out.push_str("\n\n");
    }
}

/// Such as "end bold, italic" when bold text is followed by italic text, or
/// "colour red" when the text turns red.
fn mark_change_words(from: Marks, to: Marks) -> String {
    let (from_flags, to_flags) = (from.flags, to.flags);
    let mut ended: Vec<String> = MARK_WORDS
        .iter()
        .filter(|(m, ..)| from_flags & m != 0 && to_flags & m == 0)
        .map(|(.., end)| (*end).to_owned())
        .collect();
    let mut started: Vec<String> = MARK_WORDS
        .iter()
        .filter(|(m, ..)| to_flags & m != 0 && from_flags & m == 0)
        .map(|(_, start, _)| (*start).to_owned())
        .collect();
    if from.colour != to.colour {
        match to.colour {
            Some(colour) => started.push(format!("colour {colour}")),
            None => ended.push("end colour".to_owned()),
        }
    }
    ended.append(&mut started);
    ended.join(", ")
}

/// Adds `words` where formatting changes, set off with commas so the voice
/// pauses, or as the start of a sentence at the start of one.
fn say_marks(said: &mut String, words: &str, at_start: bool) {
    let end = said.trim_end_matches([' ', '\t']).len();
    said.truncate(end);
    if at_start || said.ends_with(['.', '!', '?']) {
        if !said.is_empty() && !said.ends_with(char::is_whitespace) {
            said.push(' ');
        }
        said.push_str(&capitalise(words));
        said.push_str(", ");
    } else {
        // After a comma, say, another would be one too many.
        said.push_str(if said.ends_with([',', ';', ':']) { " " } else { ", " });
        said.push_str(words);
        said.push_str(", ");
    }
}

fn capitalise(words: &str) -> String {
    let mut chars = words.chars();
    chars.next().map(|c| c.to_uppercase().chain(chars).collect()).unwrap_or_default()
}

/// Such as "List item." or, inside another list, "List item, level 2."
fn list_item_words(level: usize) -> String {
    if level > 1 { format!("List item, level {level}.") } else { "List item.".to_owned() }
}

/// Whether a WordprocessingML on/off property, such as `<w:b/>` for bold,
/// is on. Leaving out `w:val` means on.
fn docx_on(e: &quick_xml::events::BytesStart) -> bool {
    !matches!(attr(e, "w:val").as_deref(), Some("0" | "false" | "off" | "none"))
}

/// The colour a `<w:color>` sets. "auto" is the usual colour of text.
fn docx_colour(e: &quick_xml::events::BytesStart) -> Option<&'static str> {
    attr(e, "w:val").and_then(|v| colour_name(&v))
}

#[derive(Default)]
struct DocxStyle {
    based_on: Option<String>,
    /// From an outline level, or the built-in names "heading 1" and so on.
    heading: Option<usize>,
    list: bool,
    marks: MarkChange,
}

/// The paragraph and character styles of a DOCX file, by style ID.
fn parse_docx_styles(xml: &str) -> anyhow::Result<HashMap<String, DocxStyle>> {
    let mut reader = quick_xml::Reader::from_str(xml);
    let mut styles = HashMap::new();
    let mut current: Option<(String, DocxStyle)> = None;
    loop {
        let event = reader.read_event().context("the DOCX styles are damaged")?;
        let (e, empty) = match &event {
            Event::Start(e) => (e, false),
            Event::Empty(e) => (e, true),
            Event::End(e) if e.local_name().as_ref() == "style" => {
                if let Some((id, style)) = current.take() {
                    styles.insert(id, style);
                }
                continue;
            }
            Event::Eof => break,
            _ => continue,
        };
        let name = e.local_name();
        if name.as_ref() == "style" {
            if !empty {
                current = attr(e, "w:styleId").map(|id| (id, DocxStyle::default()));
            }
            continue;
        }
        let Some((_, style)) = current.as_mut() else { continue };
        match name.as_ref() {
            "basedOn" => style.based_on = attr(e, "w:val"),
            "name" => {
                let name = attr(e, "w:val").unwrap_or_default().to_ascii_lowercase();
                if let Some(level) = name.strip_prefix("heading ").and_then(|n| n.parse().ok()) {
                    style.heading = Some(level);
                }
            }
            "outlineLvl" => {
                // 0 is the top level; 9 means body text.
                if let Some(level) = attr(e, "w:val").and_then(|v| v.parse::<usize>().ok()) {
                    style.heading = (level < 9).then_some(level + 1);
                }
            }
            "numPr" => style.list = true,
            "b" => style.marks.set(BOLD, docx_on(e)),
            "i" => style.marks.set(ITALIC, docx_on(e)),
            "u" => style.marks.set(UNDERLINE, docx_on(e)),
            "strike" | "dstrike" => style.marks.set(STRIKE, docx_on(e)),
            "color" => style.marks.colour = Some(docx_colour(e)),
            _ => {}
        }
    }
    Ok(styles)
}

/// The style `id` and the styles it is based on, the style itself first.
fn docx_style_chain<'a>(styles: &'a HashMap<String, DocxStyle>, id: Option<&str>) -> Vec<&'a DocxStyle> {
    let mut chain = Vec::new();
    let mut next = id;
    while let Some(style) = next.and_then(|id| styles.get(id)) {
        if chain.len() == MAX_STYLE_DEPTH {
            break;
        }
        chain.push(style);
        next = style.based_on.as_deref();
    }
    chain
}

fn docx_style_marks(styles: &HashMap<String, DocxStyle>, id: Option<&str>, marks: Marks) -> Marks {
    docx_style_chain(styles, id).iter().rev().fold(marks, |m, style| style.marks.apply(m))
}

/// A paragraph being read from a DOCX file.
#[derive(Default)]
struct DocxParagraph {
    style: Option<String>,
    list_level: Option<usize>,
    spoken: SpokenParagraph,
}

/// Pulls the text out of WordprocessingML as `docx_xml_to_text` does, and
/// says what its formatting shows: headings, list items, tables, and bold,
/// italic, underlined, crossed-out, coloured and linked text.
fn docx_xml_to_spoken(xml: &str, styles: &HashMap<String, DocxStyle>) -> anyhow::Result<String> {
    let mut reader = quick_xml::Reader::from_str(xml);
    let mut spoken = Spoken::default();
    // Paragraphs can hold others, in text boxes.
    let mut paragraphs: Vec<DocxParagraph> = Vec::new();
    // The rows so far in each table being read, which can be inside others.
    let mut tables: Vec<usize> = Vec::new();
    let (mut in_ppr, mut in_rpr, mut in_text) = (false, false, false);
    let mut run_style: Option<String> = None;
    let mut run_marks = MarkChange::default();
    let mut links = 0usize;
    loop {
        let event = reader.read_event().context("the DOCX document body is damaged")?;
        // The formatting of the run being read.
        let marks = |p: &DocxParagraph, run_style: &Option<String>, run_marks: MarkChange| {
            run_marks.apply(docx_style_marks(styles, run_style.as_deref(), p.spoken.marks)).in_link(links)
        };
        match event {
            Event::Start(ref e) | Event::Empty(ref e) => {
                let empty = matches!(event, Event::Empty(_));
                match e.local_name().as_ref() {
                    "p" if !empty => paragraphs.push(DocxParagraph::default()),
                    "pPr" if !empty => in_ppr = true,
                    "rPr" if !empty && !in_ppr => in_rpr = true,
                    "r" => {
                        run_style = None;
                        run_marks = MarkChange::default();
                    }
                    "pStyle" if in_ppr => {
                        if let Some(p) = paragraphs.last_mut() {
                            p.style = attr(e, "w:val");
                        }
                    }
                    "ilvl" if in_ppr => {
                        if let Some(p) = paragraphs.last_mut() {
                            p.list_level = attr(e, "w:val").and_then(|v| v.parse::<usize>().ok()).map(|l| l + 1);
                        }
                    }
                    // Numbering 0 takes a paragraph out of a list.
                    "numId" if in_ppr => {
                        if let Some(p) = paragraphs.last_mut() {
                            p.list_level = match attr(e, "w:val").as_deref() {
                                Some("0") => Some(0),
                                _ => p.list_level.or(Some(1)),
                            };
                        }
                    }
                    "rStyle" if in_rpr => run_style = attr(e, "w:val"),
                    "b" if in_rpr => run_marks.set(BOLD, docx_on(e)),
                    "i" if in_rpr => run_marks.set(ITALIC, docx_on(e)),
                    "u" if in_rpr => run_marks.set(UNDERLINE, docx_on(e)),
                    "strike" | "dstrike" if in_rpr => run_marks.set(STRIKE, docx_on(e)),
                    "color" if in_rpr => run_marks.colour = Some(docx_colour(e)),
                    "hyperlink" if !empty => links += 1,
                    "t" if !empty => in_text = true,
                    name @ ("tab" | "br" | "cr") if !in_ppr => {
                        let text = if name == "tab" { "\t" } else { "\n" };
                        if let Some(p) = paragraphs.last_mut() {
                            let m = marks(p, &run_style, run_marks);
                            p.spoken.push(text, m);
                        }
                    }
                    "tbl" if !empty => {
                        tables.push(0);
                        spoken.line("Table.");
                    }
                    "tr" if !empty => {
                        if let Some(rows) = tables.last_mut() {
                            *rows += 1;
                            spoken.line(&format!("Row {rows}."));
                        }
                    }
                    _ => {}
                }
            }
            Event::End(ref e) => match e.local_name().as_ref() {
                "pPr" => {
                    in_ppr = false;
                    // The paragraph's style is known now, so its formatting is.
                    if let Some(p) = paragraphs.last_mut() {
                        let chain = docx_style_chain(styles, p.style.as_deref());
                        let heading = chain.iter().find_map(|s| s.heading);
                        if p.list_level.is_none() && chain.iter().any(|s| s.list) {
                            p.list_level = Some(1);
                        }
                        p.spoken.marks = docx_style_marks(styles, p.style.as_deref(), Marks::default());
                        let mut intro = Vec::new();
                        if let Some(level) = p.list_level.filter(|l| *l > 0) {
                            intro.push(list_item_words(level));
                        }
                        if let Some(level) = heading {
                            intro.push(format!("Heading level {level}."));
                            p.spoken.unsaid = p.spoken.marks;
                        }
                        if !intro.is_empty() {
                            p.spoken.intro = intro.join(" ") + " ";
                        }
                    }
                }
                "rPr" => in_rpr = false,
                "t" => in_text = false,
                "hyperlink" => links = links.saturating_sub(1),
                "p" => {
                    if let Some(p) = paragraphs.pop() {
                        spoken.paragraph(p.spoken);
                    }
                }
                "tbl" if tables.pop().is_some() => spoken.line("End of table."),
                _ => {}
            },
            Event::Text(ref t) if in_text => {
                if let Some(p) = paragraphs.last_mut() {
                    let m = marks(p, &run_style, run_marks);
                    p.spoken.push(&t.xml10_content(), m);
                }
            }
            Event::GeneralRef(ref r) if in_text => {
                if let Some(p) = paragraphs.last_mut() {
                    let m = marks(p, &run_style, run_marks);
                    let mut c = String::new();
                    push_ref(&mut c, r);
                    p.spoken.push(&c, m);
                }
            }
            Event::Eof => break,
            _ => {}
        }
    }
    Ok(spoken.out)
}

#[derive(Default)]
struct OdfStyle {
    parent: Option<String>,
    marks: MarkChange,
}

/// The paragraph and text styles of an OpenDocument file, by name. The two
/// kinds can share names, so they are kept apart.
#[derive(Default)]
struct OdfStyles {
    paragraph: HashMap<String, OdfStyle>,
    text: HashMap<String, OdfStyle>,
}

impl OdfStyles {
    /// Adds the styles in `xml`, which is `styles.xml` or `content.xml`.
    fn add(&mut self, xml: &str) -> anyhow::Result<()> {
        let mut reader = quick_xml::Reader::from_str(xml);
        let mut current: Option<(bool, String, OdfStyle)> = None;
        loop {
            let event = reader.read_event().context("the ODT styles are damaged")?;
            let (e, empty) = match &event {
                Event::Start(e) => (e, false),
                Event::Empty(e) => (e, true),
                Event::End(e) if e.local_name().as_ref() == "style" => {
                    if let Some((paragraph, name, style)) = current.take() {
                        let map = if paragraph { &mut self.paragraph } else { &mut self.text };
                        map.insert(name, style);
                    }
                    continue;
                }
                Event::Eof => break,
                _ => continue,
            };
            match e.local_name().as_ref() {
                "style" if !empty => {
                    let family = attr(e, "style:family");
                    current = match (family.as_deref(), attr(e, "style:name")) {
                        (Some(family @ ("paragraph" | "text")), Some(name)) => {
                            let style = OdfStyle { parent: attr(e, "style:parent-style-name"), ..OdfStyle::default() };
                            Some((family == "paragraph", name, style))
                        }
                        _ => None,
                    };
                }
                "text-properties" => {
                    let Some((_, _, style)) = current.as_mut() else { continue };
                    if let Some(weight) = attr(e, "fo:font-weight") {
                        let bold = weight == "bold" || weight.parse::<u32>().is_ok_and(|w| w >= 600);
                        style.marks.set(BOLD, bold);
                    }
                    if let Some(slant) = attr(e, "fo:font-style") {
                        style.marks.set(ITALIC, slant != "normal");
                    }
                    if let Some(line) = attr(e, "style:text-underline-style") {
                        style.marks.set(UNDERLINE, line != "none");
                    }
                    if let Some(line) = attr(e, "style:text-line-through-style") {
                        style.marks.set(STRIKE, line != "none");
                    }
                    if let Some(colour) = attr(e, "fo:color") {
                        style.marks.colour = Some(colour_name(&colour));
                    }
                }
                _ => {}
            }
        }
        Ok(())
    }

    /// `marks` with the formatting of the named style, and the styles it is
    /// based on, applied.
    fn marks(&self, paragraph: bool, name: Option<&str>, marks: Marks) -> Marks {
        let map = if paragraph { &self.paragraph } else { &self.text };
        let mut chain = Vec::new();
        let mut next = name;
        while let Some(style) = next.and_then(|n| map.get(n)) {
            if chain.len() == MAX_STYLE_DEPTH {
                break;
            }
            chain.push(style);
            next = style.parent.as_deref();
        }
        chain.iter().rev().fold(marks, |m, style| style.marks.apply(m))
    }
}

/// Pulls the text out of an ODT `content.xml` as `odt_xml_to_text` does, and
/// says what its formatting shows, as `docx_xml_to_spoken` does.
fn odt_xml_to_spoken(xml: &str, styles: &OdfStyles) -> anyhow::Result<String> {
    let mut reader = quick_xml::Reader::from_str(xml);
    let mut spoken = Spoken::default();
    let mut paragraphs: Vec<SpokenParagraph> = Vec::new();
    // The formatting inside each span being read, innermost last.
    let mut spans: Vec<Marks> = Vec::new();
    let mut tables: Vec<usize> = Vec::new();
    let mut skipped = 0usize;
    let mut links = 0usize;
    let mut lists = 0usize;
    // A list item has started, and its first paragraph hasn't yet.
    let mut new_item = false;
    loop {
        let event = reader.read_event().context("the ODT document body is damaged")?;
        // The formatting of the text being read, before any link.
        let current = |paragraphs: &[SpokenParagraph], spans: &[Marks]| {
            spans.last().copied().or(paragraphs.last().map(|p| p.marks)).unwrap_or_default()
        };
        match event {
            Event::Start(ref e) => match e.local_name().as_ref() {
                name if ODF_SKIPPED.contains(&name) => skipped += 1,
                _ if skipped > 0 => {}
                name @ ("p" | "h") => {
                    let style = attr(e, "text:style-name");
                    let marks = styles.marks(true, style.as_deref(), Marks::default());
                    let mut p = SpokenParagraph { marks, ..SpokenParagraph::default() };
                    let mut intro = Vec::new();
                    if new_item {
                        intro.push(list_item_words(lists));
                        new_item = false;
                    }
                    if name == "h" {
                        let level = attr(e, "text:outline-level").and_then(|l| l.parse().ok()).unwrap_or(1usize);
                        intro.push(format!("Heading level {level}."));
                        p.unsaid = marks;
                    }
                    if !intro.is_empty() {
                        p.intro = intro.join(" ") + " ";
                    }
                    paragraphs.push(p);
                }
                "span" => {
                    let base = current(&paragraphs, &spans);
                    spans.push(styles.marks(false, attr(e, "text:style-name").as_deref(), base));
                }
                "a" => links += 1,
                "list" => lists += 1,
                "list-item" => new_item = true,
                "table" => {
                    tables.push(0);
                    spoken.line("Table.");
                }
                "table-row" => {
                    if let Some(rows) = tables.last_mut() {
                        *rows += 1;
                        spoken.line(&format!("Row {rows}."));
                    }
                }
                _ => {}
            },
            Event::Empty(ref e) if skipped == 0 => {
                let text = match e.local_name().as_ref() {
                    "s" => " ",
                    "tab" => "\t",
                    "line-break" => "\n",
                    _ => continue,
                };
                let m = current(&paragraphs, &spans).in_link(links);
                if let Some(p) = paragraphs.last_mut() {
                    p.push(text, m);
                }
            }
            Event::End(ref e) => match e.local_name().as_ref() {
                name if ODF_SKIPPED.contains(&name) => skipped = skipped.saturating_sub(1),
                _ if skipped > 0 => {}
                "p" | "h" => {
                    if let Some(p) = paragraphs.pop() {
                        spoken.paragraph(p);
                    }
                }
                "span" => {
                    spans.pop();
                }
                "a" => links = links.saturating_sub(1),
                "list" => lists = lists.saturating_sub(1),
                "list-item" => new_item = false,
                "table" if tables.pop().is_some() => spoken.line("End of table."),
                _ => {}
            },
            Event::Text(ref t) if skipped == 0 => {
                let m = current(&paragraphs, &spans).in_link(links);
                if let Some(p) = paragraphs.last_mut() {
                    p.push(&t.xml10_content(), m);
                }
            }
            Event::GeneralRef(ref r) if skipped == 0 => {
                let m = current(&paragraphs, &spans).in_link(links);
                if let Some(p) = paragraphs.last_mut() {
                    let mut c = String::new();
                    push_ref(&mut c, r);
                    p.push(&c, m);
                }
            }
            Event::Eof => break,
            _ => {}
        }
    }
    Ok(spoken.out)
}

/// Reads Markdown as its text, leaving out the marks that format it. With
/// `DocumentReading::Format`, its formatting is said in the same words as a
/// DOCX or ODT file's. Raw HTML, front matter and footnotes are left out.
fn markdown_to_text(markdown: &str, reading: DocumentReading) -> String {
    use pulldown_cmark::{Event as Md, Options, Parser, Tag, TagEnd};
    let format = reading == DocumentReading::Format;
    let options = Options::ENABLE_TABLES
        | Options::ENABLE_STRIKETHROUGH
        | Options::ENABLE_TASKLISTS
        | Options::ENABLE_FOOTNOTES
        | Options::ENABLE_YAML_STYLE_METADATA_BLOCKS
        | Options::ENABLE_PLUSES_DELIMITED_METADATA_BLOCKS;
    let mut spoken = Spoken::default();
    // Markdown text is never inside more than one paragraph at a time, so
    // a paragraph is said as soon as the next block starts.
    let mut paragraph: Option<SpokenParagraph> = None;
    // Words for the start of the next paragraph, such as "List item."
    let mut intro: Vec<String> = Vec::new();
    // The formatting turned on by each emphasis being read, innermost last.
    let mut marks: Vec<u8> = Vec::new();
    let mut tables: Vec<usize> = Vec::new();
    let mut skipped = 0usize;
    let mut links = 0usize;
    let mut lists = 0usize;
    let flush = |spoken: &mut Spoken, paragraph: &mut Option<SpokenParagraph>| {
        if let Some(p) = paragraph.take() {
            spoken.paragraph(p);
        }
    };
    for event in Parser::new_ext(markdown, options) {
        let text = match event {
            Md::Start(Tag::MetadataBlock(_) | Tag::FootnoteDefinition(_) | Tag::HtmlBlock) => {
                skipped += 1;
                continue;
            }
            Md::End(TagEnd::MetadataBlock(_) | TagEnd::FootnoteDefinition | TagEnd::HtmlBlock) => {
                skipped = skipped.saturating_sub(1);
                continue;
            }
            _ if skipped > 0 => continue,
            Md::Start(tag) => {
                match tag {
                    Tag::Emphasis => marks.push(ITALIC),
                    Tag::Strong => marks.push(BOLD),
                    Tag::Strikethrough => marks.push(STRIKE),
                    Tag::Link { .. } => links += 1,
                    Tag::Paragraph | Tag::TableCell => flush(&mut spoken, &mut paragraph),
                    Tag::Heading { level, .. } => {
                        flush(&mut spoken, &mut paragraph);
                        intro.push(format!("Heading level {}.", level as usize));
                    }
                    Tag::List(_) => {
                        flush(&mut spoken, &mut paragraph);
                        lists += 1;
                    }
                    Tag::Item => {
                        flush(&mut spoken, &mut paragraph);
                        intro.push(list_item_words(lists));
                    }
                    Tag::Table(_) => {
                        flush(&mut spoken, &mut paragraph);
                        tables.push(0);
                        if format {
                            spoken.line("Table.");
                        }
                    }
                    Tag::TableHead | Tag::TableRow => {
                        flush(&mut spoken, &mut paragraph);
                        if let Some(rows) = tables.last_mut() {
                            *rows += 1;
                            if format {
                                spoken.line(&format!("Row {rows}."));
                            }
                        }
                    }
                    Tag::CodeBlock(_) => {
                        flush(&mut spoken, &mut paragraph);
                        intro.push("Code.".to_owned());
                    }
                    Tag::BlockQuote(_) => {
                        flush(&mut spoken, &mut paragraph);
                        intro.push("Quote.".to_owned());
                    }
                    _ => {}
                }
                continue;
            }
            Md::End(tag) => {
                match tag {
                    TagEnd::Emphasis | TagEnd::Strong | TagEnd::Strikethrough => {
                        marks.pop();
                    }
                    TagEnd::Link => links = links.saturating_sub(1),
                    TagEnd::List(_) => {
                        flush(&mut spoken, &mut paragraph);
                        lists = lists.saturating_sub(1);
                    }
                    TagEnd::Table => {
                        flush(&mut spoken, &mut paragraph);
                        if tables.pop().is_some() && format {
                            spoken.line("End of table.");
                        }
                    }
                    TagEnd::Paragraph
                    | TagEnd::Heading(_)
                    | TagEnd::Item
                    | TagEnd::TableCell
                    | TagEnd::CodeBlock
                    | TagEnd::BlockQuote(_) => {
                        flush(&mut spoken, &mut paragraph);
                        // An empty heading or list item says nothing, and
                        // its words don't carry over to what comes next.
                        intro.clear();
                    }
                    _ => {}
                }
                continue;
            }
            Md::Text(t) | Md::Code(t) => t,
            Md::SoftBreak => " ".into(),
            Md::HardBreak => "\n".into(),
            Md::TaskListMarker(done) => {
                intro.push(if done { "Done." } else { "Not done." }.to_owned());
                continue;
            }
            _ => continue,
        };
        let flags = marks.iter().fold(0, |all, m| all | m);
        let m = Marks { flags, colour: None }.in_link(links);
        let p = paragraph.get_or_insert_with(|| {
            let words = std::mem::take(&mut intro);
            let intro = if format && !words.is_empty() { words.join(" ") + " " } else { String::new() };
            SpokenParagraph { intro, ..SpokenParagraph::default() }
        });
        p.push(&text, if format { m } else { Marks::default() });
    }
    flush(&mut spoken, &mut paragraph);
    spoken.out
}

/// Reads each sheet of an ODS `content.xml` as a spoken table, in the same
/// way as a CSV file. When there is more than one sheet with something in it,
/// each table is introduced with its sheet's name.
fn ods_xml_to_text(xml: &str) -> anyhow::Result<String> {
    let sheets = parse_ods(xml)?;
    let sheets: Vec<_> = sheets.into_iter().filter(|(_, rows)| rows.iter().any(|r| !row_is_empty(r))).collect();
    let named = sheets.len() > 1;
    let tables: Vec<String> = sheets
        .into_iter()
        .map(|(name, rows)| {
            let table = speak_table(rows);
            if named { format!("Sheet: {name}.\n\n{table}") } else { table }
        })
        .collect();
    Ok(tables.join("\n\n"))
}

fn row_is_empty(row: &[String]) -> bool {
    row.iter().all(|c| c.trim().is_empty())
}

/// Splits an ODS `content.xml` into sheets of rows of cell text, expanding
/// repeated cells and rows. Runs of empty cells are only written out when a
/// cell with something in it follows, and runs of empty rows are kept as a
/// single row, since a sheet usually ends with an empty run that repeats to
/// the edge of the grid.
fn parse_ods(xml: &str) -> anyhow::Result<Vec<(String, Vec<Vec<String>>)>> {
    fn repeat(e: &quick_xml::events::BytesStart, name: &str) -> usize {
        attr(e, name).and_then(|v| v.trim().parse().ok()).unwrap_or(1).max(1)
    }
    let too_large = || anyhow::anyhow!("the ODS spreadsheet is too large");

    let mut reader = quick_xml::Reader::from_str(xml);
    let mut sheets = Vec::new();
    let mut rows: Vec<Vec<String>> = Vec::new();
    let mut row: Vec<String> = Vec::new();
    let mut row_repeat = 1;
    let mut cell: Option<(String, usize)> = None;
    let mut pending_empty = 0usize;
    let mut cells = 0usize;
    let mut skipped = 0usize;
    let mut sheet_name = String::new();

    // Adds a finished cell, repeated `n` times, to the row, filling any gap
    // of empty cells first.
    let finish_cell = |row: &mut Vec<String>, pending_empty: &mut usize, cells: &mut usize, text: String, n: usize| {
        let text = text.split_whitespace().collect::<Vec<_>>().join(" ");
        if text.is_empty() {
            *pending_empty = pending_empty.saturating_add(n);
            return Ok(());
        }
        *cells = cells.saturating_add(*pending_empty).saturating_add(n);
        if *cells > MAX_ODS_CELLS {
            return Err(too_large());
        }
        row.extend(std::iter::repeat_n(String::new(), *pending_empty));
        row.extend(std::iter::repeat_n(text, n));
        *pending_empty = 0;
        Ok(())
    };

    loop {
        let event = reader.read_event().context("the ODS document body is damaged")?;
        match &event {
            Event::Start(e) | Event::Empty(e) => {
                let is_empty = matches!(event, Event::Empty(_));
                match e.local_name().as_ref() {
                    name if ODF_SKIPPED.contains(&name) => {
                        if !is_empty {
                            skipped += 1;
                        }
                    }
                    _ if skipped > 0 => {}
                    "table" if !is_empty => {
                        sheet_name = attr(e, "table:name").unwrap_or_default();
                        rows.clear();
                    }
                    "table-row" => {
                        row.clear();
                        pending_empty = 0;
                        row_repeat = repeat(e, "table:number-rows-repeated");
                        if is_empty {
                            rows.push(Vec::new());
                        }
                    }
                    "table-cell" | "covered-table-cell" => {
                        let n = repeat(e, "table:number-columns-repeated");
                        if is_empty {
                            finish_cell(&mut row, &mut pending_empty, &mut cells, String::new(), n)?;
                        } else {
                            cell = Some((String::new(), n));
                        }
                    }
                    "p" if !is_empty => {
                        if let Some((text, _)) = &mut cell
                            && !text.is_empty()
                        {
                            text.push(' ');
                        }
                    }
                    "s" | "tab" | "line-break" => {
                        if let Some((text, _)) = &mut cell {
                            text.push(' ');
                        }
                    }
                    _ => {}
                }
            }
            Event::End(e) => match e.local_name().as_ref() {
                name if ODF_SKIPPED.contains(&name) => skipped = skipped.saturating_sub(1),
                _ if skipped > 0 => {}
                "table-cell" | "covered-table-cell" => {
                    if let Some((text, n)) = cell.take() {
                        finish_cell(&mut row, &mut pending_empty, &mut cells, text, n)?;
                    }
                }
                "table-row" => {
                    let row = std::mem::take(&mut row);
                    // Empty rows are dropped when the table is read, so one
                    // copy stands for any number of them.
                    let n = if row.is_empty() { 1 } else { row_repeat };
                    cells = cells.saturating_add(row.len().saturating_mul(n));
                    if cells > MAX_ODS_CELLS {
                        return Err(too_large());
                    }
                    rows.extend(std::iter::repeat_n(row, n));
                }
                "table" => sheets.push((std::mem::take(&mut sheet_name), std::mem::take(&mut rows))),
                _ => {}
            },
            Event::Text(t) if skipped == 0 => {
                if let Some((text, _)) = &mut cell {
                    text.push_str(&t.xml10_content());
                }
            }
            Event::GeneralRef(r) if skipped == 0 => {
                if let Some((text, _)) = &mut cell {
                    push_ref(text, r);
                }
            }
            Event::Eof => break,
            _ => {}
        }
    }
    Ok(sheets)
}

/// Reads each slide of a PowerPoint presentation in turn, in the order they
/// are shown, introducing each one with its number. The text of SmartArt
/// graphics and the data of charts are read where they appear. Speaker notes
/// and comments are left out, as comments are for DOCX.
fn read_pptx(path: &Path) -> anyhow::Result<String> {
    if is_compound_file(path)? {
        // Office keeps a password-protected PPTX inside a compound file; any
        // other compound file is a PPT that has been given a .pptx name.
        let file = cfb::open(path).context("this is not a valid PPTX file")?;
        if file.exists("EncryptedPackage") {
            bail!("the presentation is password protected");
        }
        return read_ppt(path);
    }
    let mut archive = open_zip(path, "PPTX")?;
    let mut budget = MAX_XML_BYTES;
    let presentation = read_zip_entry(&mut archive, "ppt/presentation.xml", "PPTX", &mut budget)?;
    let rels = read_zip_entry(&mut archive, "ppt/_rels/presentation.xml.rels", "PPTX", &mut budget)?;
    let targets = parse_rels(&rels)?;
    let mut slides = Vec::new();
    for id in pptx_slide_ids(&presentation)? {
        // A missing slide still counts, so the slides after it keep their
        // numbers.
        let name = targets.get(&id).map(|t| resolve_part("ppt", t));
        let text = match name {
            Some(name) if archive.index_for_name(&name).is_some() => {
                let xml = read_zip_entry(&mut archive, &name, "PPTX", &mut budget)?;
                read_pptx_slide(&mut archive, &name, &xml, &mut budget)?
            }
            _ => {
                log::warn!("PPTX slide {id} is missing");
                String::new()
            }
        };
        slides.push(text);
    }
    Ok(speak_slides(slides))
}

/// Reads the slide `name`, whose XML is `xml`, following its links to the
/// charts and SmartArt graphics it shows.
fn read_pptx_slide(
    archive: &mut zip::ZipArchive<std::fs::File>,
    name: &str,
    xml: &str,
    budget: &mut u64,
) -> anyhow::Result<String> {
    let (folder, file) = name.rsplit_once('/').unwrap_or(("", name));
    let rels_name = format!("{folder}/_rels/{file}.rels");
    let targets = match archive.index_for_name(&rels_name) {
        Some(_) => parse_rels(&read_zip_entry(archive, &rels_name, "PPTX", budget)?)?,
        None => HashMap::new(),
    };
    pptx_slide_to_text(xml, |embedded| {
        let (id, is_chart) = match &embedded {
            Embedded::Chart(id) => (id, true),
            Embedded::Diagram(id) => (id, false),
        };
        let Some(part) = targets.get(id).map(|t| resolve_part(folder, t)) else { return Ok(String::new()) };
        if archive.index_for_name(&part).is_none() {
            log::warn!("PPTX part {part} is missing");
            return Ok(String::new());
        }
        let xml = read_zip_entry(archive, &part, "PPTX", budget)?;
        if is_chart { chart_xml_to_text(&xml) } else { diagram_xml_to_text(&xml) }
    })
}

/// The value of the attribute with a prefix, such as `r:id`, whose local
/// name is `local`. Relationship ids are found this way, since the prefix is
/// up to the program that wrote the file.
fn prefixed_attr(e: &quick_xml::events::BytesStart, local: &str) -> Option<String> {
    let a = e.attributes().flatten().find(|a| a.key.local_name().as_ref() == local && a.key.prefix().is_some())?;
    Some(a.normalized_value(quick_xml::XmlVersion::Implicit1_0).ok()?.into_owned())
}

/// The relationship ids of the slides listed in `ppt/presentation.xml`, in
/// the order they are shown.
fn pptx_slide_ids(xml: &str) -> anyhow::Result<Vec<String>> {
    let mut reader = quick_xml::Reader::from_str(xml);
    let mut ids = Vec::new();
    loop {
        match reader.read_event().context("the PPTX presentation is damaged")? {
            Event::Start(e) | Event::Empty(e) if e.local_name().as_ref() == "sldId" => {
                // The slide's own `id` has no prefix; the relationship id does.
                if let Some(id) = prefixed_attr(&e, "id") {
                    ids.push(id);
                }
            }
            Event::Eof => break,
            _ => {}
        }
    }
    Ok(ids)
}

/// Maps each relationship id in a `.rels` part to its target.
fn parse_rels(xml: &str) -> anyhow::Result<HashMap<String, String>> {
    let mut reader = quick_xml::Reader::from_str(xml);
    let mut targets = HashMap::new();
    loop {
        match reader.read_event().context("the PPTX presentation is damaged")? {
            Event::Start(e) | Event::Empty(e) if e.local_name().as_ref() == "Relationship" => {
                if let (Some(id), Some(target)) = (attr(&e, "Id"), attr(&e, "Target")) {
                    targets.insert(id, target);
                }
            }
            Event::Eof => break,
            _ => {}
        }
    }
    Ok(targets)
}

/// Resolves a relationship target against the folder `base` of the part
/// that refers to it, giving a name in the archive.
fn resolve_part(base: &str, target: &str) -> String {
    let mut parts: Vec<&str> = match target.strip_prefix('/') {
        Some(_) => Vec::new(),
        None => base.split('/').filter(|p| !p.is_empty()).collect(),
    };
    for segment in target.split('/') {
        match segment {
            "" | "." => {}
            ".." => {
                parts.pop();
            }
            s => parts.push(s),
        }
    }
    parts.join("/")
}

/// A chart or SmartArt graphic on a slide, kept in another part of the file
/// and named by its relationship id.
enum Embedded {
    Chart(String),
    Diagram(String),
}

/// Pulls the text out of a PresentationML slide, one paragraph per text
/// paragraph, in the order the shapes appear on the slide. The text of each
/// chart or SmartArt graphic comes from `embedded`.
fn pptx_slide_to_text(xml: &str, mut embedded: impl FnMut(Embedded) -> anyhow::Result<String>) -> anyhow::Result<String> {
    let mut reader = quick_xml::Reader::from_str(xml);
    let mut out = String::new();
    let mut in_text = false;
    // The shape is a date, footer or slide number placeholder, which repeats
    // on every slide, so its text is left out.
    let mut skip_shape = false;
    // How many `mc:Fallback` elements the reader is inside. Newer content,
    // such as Office 2016 charts, comes with a fallback copy for older apps
    // (for those charts, a box saying the chart can't be shown), which is
    // left out so it isn't read as well.
    let mut fallback = 0usize;
    loop {
        let event = reader.read_event().context("a PPTX slide is damaged")?;
        match &event {
            Event::Start(e) if e.local_name().as_ref() == "Fallback" => fallback += 1,
            Event::End(e) if e.local_name().as_ref() == "Fallback" => fallback = fallback.saturating_sub(1),
            Event::Eof => break,
            _ if fallback > 0 => continue,
            Event::Start(e) | Event::Empty(e) if e.local_name().as_ref() == "ph" => {
                if matches!(attr(e, "type").as_deref(), Some("dt" | "ftr" | "sldNum" | "hdr")) {
                    skip_shape = true;
                }
            }
            Event::Start(e) | Event::Empty(e) if matches!(e.local_name().as_ref(), "chart" | "relIds") => {
                let found = match e.local_name().as_ref() {
                    "chart" => prefixed_attr(e, "id").map(Embedded::Chart),
                    _ => prefixed_attr(e, "dm").map(Embedded::Diagram),
                };
                if let Some(found) = found {
                    out.push_str(&embedded(found)?);
                    out.push_str("\n\n");
                }
            }
            Event::Start(e) if e.local_name().as_ref() == "sp" => skip_shape = false,
            Event::Start(e) if e.local_name().as_ref() == "t" => in_text = true,
            Event::Empty(e) if !skip_shape => match e.local_name().as_ref() {
                "br" => out.push('\n'),
                "tab" => out.push('\t'),
                _ => {}
            },
            Event::End(e) => match e.local_name().as_ref() {
                "t" => in_text = false,
                "p" => out.push_str("\n\n"),
                "sp" => skip_shape = false,
                _ => {}
            },
            Event::Text(t) if in_text && !skip_shape => out.push_str(&t.xml10_content()),
            Event::GeneralRef(r) if in_text && !skip_shape => push_ref(&mut out, r),
            _ => {}
        }
    }
    Ok(out)
}

/// Reads the text of a SmartArt graphic from its data part
/// (`ppt/diagrams/dataN.xml`), one paragraph per box, in the order of the
/// graphic's outline: each box, then the boxes under it.
fn diagram_xml_to_text(xml: &str) -> anyhow::Result<String> {
    let mut reader = quick_xml::Reader::from_str(xml);
    // Each box's id and text, in the order written, and the id of the
    // graphic as a whole.
    let mut points: Vec<(String, String)> = Vec::new();
    let mut root = None;
    // The parent of each box, and its children with their positions.
    let mut children: HashMap<String, Vec<(usize, String)>> = HashMap::new();
    let mut point: Option<(String, String)> = None;
    let mut in_text = false;
    loop {
        match reader.read_event().context("a SmartArt graphic is damaged")? {
            Event::Start(e) => {
                let name = e.local_name();
                in_text = name.as_ref() == "t";
                match name.as_ref() {
                    "pt" => {
                        let id = attr(&e, "modelId").unwrap_or_default();
                        match attr(&e, "type").as_deref() {
                            None | Some("node" | "asst") => point = Some((id, String::new())),
                            Some("doc") => root = Some(id),
                            // Connector shapes and layout points hold no text.
                            _ => {}
                        }
                    }
                    "cxn" => add_connection(&e, &mut children),
                    _ => {}
                }
            }
            Event::Empty(e) => match e.local_name().as_ref() {
                "cxn" => add_connection(&e, &mut children),
                "br" | "tab" => {
                    if let Some((_, text)) = &mut point {
                        text.push(' ');
                    }
                }
                _ => {}
            },
            Event::End(e) => {
                in_text = false;
                match e.local_name().as_ref() {
                    "p" => {
                        if let Some((_, text)) = &mut point {
                            text.push_str("\n\n");
                        }
                    }
                    "pt" => points.extend(point.take()),
                    _ => {}
                }
            }
            Event::Text(t) if in_text => {
                if let Some((_, text)) = &mut point {
                    text.push_str(&t.xml10_content());
                }
            }
            Event::GeneralRef(r) if in_text => {
                if let Some((_, text)) = &mut point {
                    push_ref(text, &r);
                }
            }
            Event::Eof => break,
            _ => {}
        }
    }

    // Walk the outline from the top. Boxes it doesn't reach, which a damaged
    // file might have, are read at the end in the order written.
    for kids in children.values_mut() {
        kids.sort();
    }
    let mut order: Vec<&str> = Vec::new();
    let mut seen = std::collections::HashSet::new();
    let mut stack: Vec<&str> = root.as_deref().into_iter().collect();
    while let Some(id) = stack.pop() {
        if !seen.insert(id) {
            continue;
        }
        order.push(id);
        if let Some(kids) = children.get(id) {
            stack.extend(kids.iter().rev().map(|(_, kid)| kid.as_str()));
        }
    }
    let texts: HashMap<&str, &str> = points.iter().map(|(id, text)| (id.as_str(), text.as_str())).collect();
    let mut out = String::new();
    let unreached = points.iter().map(|(id, _)| id.as_str()).filter(|id| !seen.contains(id));
    for id in order.into_iter().chain(unreached) {
        if let Some(text) = texts.get(id) {
            out.push_str(text);
            out.push_str("\n\n");
        }
    }
    Ok(out)
}

/// Records a SmartArt connection between a box and the box under it. Other
/// kinds of connection link boxes to how they are drawn, and are ignored.
fn add_connection(e: &quick_xml::events::BytesStart, children: &mut HashMap<String, Vec<(usize, String)>>) {
    if attr(e, "type").is_some_and(|t| t != "parOf") {
        return;
    }
    if let (Some(parent), Some(child)) = (attr(e, "srcId"), attr(e, "destId")) {
        let position = attr(e, "srcOrd").and_then(|o| o.trim().parse().ok()).unwrap_or(usize::MAX);
        children.entry(parent).or_default().push((position, child));
    }
}

/// One series of a chart: its name, and its categories and values by index.
#[derive(Default)]
struct ChartSeries {
    name: String,
    categories: std::collections::BTreeMap<usize, String>,
    values: std::collections::BTreeMap<usize, String>,
    /// The number format of the values, such as "0%".
    format: String,
}

/// Reads a chart (`ppt/charts/chartN.xml`) as speech: its title and axis
/// titles, then each series in turn with the value for each category, from
/// the copy of the data the chart keeps with it.
fn chart_xml_to_text(xml: &str) -> anyhow::Result<String> {
    /// Elements whose text is not part of the chart's data or titles: data
    /// labels, trendlines, error bars and extensions.
    const SKIPPED: [&str; 5] = ["extLst", "dLbls", "dLbl", "trendline", "errBars"];
    let mut reader = quick_xml::Reader::from_str(xml);
    // The local names of the elements the reader is inside.
    let mut stack: Vec<String> = Vec::new();
    let mut title = String::new();
    let mut axis_titles: Vec<String> = Vec::new();
    let mut series: Vec<ChartSeries> = Vec::new();
    let mut index = 0usize;
    loop {
        let text = match reader.read_event().context("a chart is damaged")? {
            Event::Start(e) => {
                let name = e.local_name().as_ref().to_owned();
                match name.as_str() {
                    "ser" => series.push(ChartSeries::default()),
                    "title" if stack.last().is_some_and(|p| p != "chart") => axis_titles.push(String::new()),
                    "pt" => index = attr(&e, "idx").and_then(|i| i.trim().parse().ok()).unwrap_or(0),
                    _ => {}
                }
                stack.push(name);
                continue;
            }
            Event::End(_) => {
                // Paragraphs of a title run on as one line.
                if stack.pop().as_deref() == Some("p") && !stack.iter().any(|n| n == "ser") {
                    match stack.iter().rposition(|n| n == "title") {
                        Some(at) if at > 0 && stack[at - 1] == "chart" => title.push(' '),
                        Some(_) => axis_titles.last_mut().into_iter().for_each(|t| t.push(' ')),
                        None => {}
                    }
                }
                continue;
            }
            Event::Text(t) => t.xml10_content().into_owned(),
            Event::GeneralRef(r) => {
                let mut s = String::new();
                push_ref(&mut s, &r);
                s
            }
            Event::Eof => break,
            _ => continue,
        };
        let Some(leaf) = stack.last().map(String::as_str) else { continue };
        if !matches!(leaf, "t" | "v" | "formatCode") || stack.iter().any(|n| SKIPPED.contains(&n.as_str())) {
            continue;
        }
        if let Some(ser) = stack.iter().position(|n| n == "ser") {
            let Some(current) = series.last_mut() else { continue };
            match (stack.get(ser + 1).map(String::as_str), leaf) {
                (Some("tx"), "t" | "v") => current.name.push_str(&text),
                (Some("cat" | "xVal"), "v") => {
                    let category = current.categories.entry(index).or_default();
                    // Categories with more than one level, such as a quarter
                    // within a year, give each level in turn.
                    if !category.is_empty() {
                        category.push_str(", ");
                    }
                    category.push_str(text.trim());
                }
                (Some("val" | "yVal"), "v") => {
                    current.values.insert(index, text.trim().to_owned());
                }
                (Some("val" | "yVal"), "formatCode") => current.format.push_str(&text),
                _ => {}
            }
        } else if leaf != "formatCode"
            && let Some(at) = stack.iter().rposition(|n| n == "title")
        {
            if at > 0 && stack[at - 1] == "chart" {
                title.push_str(&text);
            } else if let Some(axis) = axis_titles.last_mut() {
                axis.push_str(&text);
            }
        }
    }
    Ok(speak_chart(&title, &axis_titles, &series))
}

/// Puts a chart into words: "Chart: Sales by region. Axes: Quarter, Sales
/// (£m)." then a paragraph for each series, such as "North. Q1: 10. Q2: 20."
fn speak_chart(title: &str, axis_titles: &[String], series: &[ChartSeries]) -> String {
    let tidy_line = |s: &str| s.split_whitespace().collect::<Vec<_>>().join(" ");
    let title = tidy_line(title);
    let mut out = if title.is_empty() { "Chart.".to_owned() } else { format!("Chart: {}", with_stop(&title)) };
    let axes: Vec<String> = axis_titles.iter().map(|t| tidy_line(t)).filter(|t| !t.is_empty()).collect();
    if !axes.is_empty() {
        out.push_str(&format!(" {}: {}", if axes.len() == 1 { "Axis" } else { "Axes" }, with_stop(&axes.join(", "))));
    }
    for (n, ser) in series.iter().enumerate() {
        let name = tidy_line(&ser.name);
        let name = if name.is_empty() { format!("Series {}", n + 1) } else { name };
        out.push_str(&format!("\n\n{}", with_stop(&name)));
        for (i, value) in &ser.values {
            let category = ser.categories.get(i).map(|c| tidy_line(c)).filter(|c| !c.is_empty());
            let category = category.unwrap_or_else(|| format!("Point {}", i + 1));
            out.push_str(&format!(" {category}: {}.", speakable_number(value, &ser.format)));
        }
    }
    out
}

/// Ends `text` with a full stop unless it already ends a sentence.
fn with_stop(text: &str) -> String {
    if text.ends_with(['.', '!', '?']) { text.to_owned() } else { format!("{text}.") }
}

/// Writes a chart value as it would be read: "0.35" in a percentage format
/// is "35%", and the rounding noise in values such as "12.300000000000001"
/// is dropped. Values that aren't numbers are kept as they are.
fn speakable_number(value: &str, format: &str) -> String {
    let Ok(mut n) = value.trim().parse::<f64>() else { return value.trim().to_owned() };
    if !n.is_finite() {
        return value.trim().to_owned();
    }
    let percent = format.contains('%');
    if percent {
        n *= 100.0;
    }
    let mut s = format!("{n:.6}");
    if s.contains('.') {
        s = s.trim_end_matches('0').trim_end_matches('.').to_owned();
    }
    if s == "-0" {
        s = "0".to_owned();
    }
    if percent { format!("{s}%") } else { s }
}

/// Joins the text of each slide, introducing each with its number. Slides
/// with no text are left out, but the others keep their own numbers.
fn speak_slides(slides: Vec<String>) -> String {
    let slides: Vec<String> = slides
        .into_iter()
        .enumerate()
        .filter(|(_, text)| !text.trim().is_empty())
        .map(|(n, text)| format!("Slide {}.\n\n{}", n + 1, text.trim()))
        .collect();
    slides.join("\n\n")
}

/// Record types in a PowerPoint 97–2003 `PowerPoint Document` stream.
mod ppt {
    pub const DOCUMENT: u16 = 0x03E8;
    pub const SLIDE: u16 = 0x03EE;
    pub const SLIDE_PERSIST_ATOM: u16 = 0x03F3;
    pub const OUTLINE_TEXT_REF_ATOM: u16 = 0x0F9E;
    pub const TEXT_HEADER_ATOM: u16 = 0x0F9F;
    pub const TEXT_CHARS_ATOM: u16 = 0x0FA0;
    pub const TEXT_BYTES_ATOM: u16 = 0x0FA8;
    pub const SLIDE_LIST_WITH_TEXT: u16 = 0x0FF0;
    pub const PERSIST_DIRECTORY_ATOM: u16 = 0x1772;
    /// Containers hold further records rather than data.
    pub const CONTAINER_VERSION: u16 = 0xF;
    /// How deeply containers may nest, which keeps a damaged file from
    /// exhausting the stack.
    pub const MAX_DEPTH: usize = 64;
}

/// One record of a PowerPoint 97–2003 stream.
struct PptRecord<'a> {
    version: u16,
    instance: u16,
    kind: u16,
    data: &'a [u8],
}

/// Splits `data` into records, stopping at the first one that is cut short.
/// Yields each record with its offset from the start of `data`.
fn ppt_records(data: &[u8]) -> impl Iterator<Item = (usize, PptRecord<'_>)> {
    let mut pos: usize = 0;
    std::iter::from_fn(move || {
        let header = data.get(pos..pos.checked_add(8)?)?;
        let ver_instance = u16::from_le_bytes([header[0], header[1]]);
        let kind = u16::from_le_bytes([header[2], header[3]]);
        let len = u32::from_le_bytes([header[4], header[5], header[6], header[7]]) as usize;
        let body = data.get(pos + 8..(pos + 8).checked_add(len)?)?;
        let record = PptRecord { version: ver_instance & 0xF, instance: ver_instance >> 4, kind, data: body };
        let offset = pos;
        pos += 8 + len;
        Some((offset, record))
    })
}

/// Whether the file starts like an OLE compound file, the container that
/// PowerPoint 97–2003 files and password-protected Office files use.
fn is_compound_file(path: &Path) -> anyhow::Result<bool> {
    let mut magic = [0u8; 8];
    let mut file = std::fs::File::open(path)?;
    Ok(file.read_exact(&mut magic).is_ok() && magic == [0xD0, 0xCF, 0x11, 0xE0, 0xA1, 0xB1, 0x1A, 0xE1])
}

fn read_ppt(path: &Path) -> anyhow::Result<String> {
    let bytes = std::fs::read(path)?;
    // A PPTX file that has been given a .ppt name is a zip archive.
    if bytes.starts_with(b"PK\x03\x04") {
        return read_pptx(path);
    }
    let mut file = cfb::CompoundFile::open(std::io::Cursor::new(bytes)).context("this is not a valid PPT file")?;
    if file.exists("EncryptedSummary") || file.exists("EncryptedPackage") {
        bail!("the presentation is password protected");
    }
    let mut stream = Vec::new();
    file.open_stream("PowerPoint Document")
        .context("this PPT file has no slides. It may be from PowerPoint 95 or earlier, which can't be read.")?
        .read_to_end(&mut stream)?;
    Ok(speak_slides(ppt_stream_to_text(&stream, MAX_PPT_TEXT_BYTES)?))
}

/// Reads the text of each slide in a `PowerPoint Document` stream, in the
/// order they are shown. Text in placeholders, such as titles and bullet
/// points, is kept in a list for each slide, while text in other shapes is
/// kept with the slide's drawing, which refers to the placeholder text where
/// it appears; so each slide's drawing is followed in turn. Fails if the
/// text comes to more than `limit` bytes.
fn ppt_stream_to_text(stream: &[u8], limit: usize) -> anyhow::Result<Vec<String>> {
    let mut budget = limit;
    // Where each persistent object starts. Later directories, written by
    // later saves, replace entries in earlier ones.
    let mut persist: HashMap<u32, usize> = HashMap::new();
    let mut document = None;
    for (_, r) in ppt_records(stream) {
        match r.kind {
            ppt::PERSIST_DIRECTORY_ATOM => {
                let words: Vec<u32> = r.data.as_chunks::<4>().0.iter().map(|w| u32::from_le_bytes(*w)).collect();
                let mut words = words.into_iter();
                while let Some(header) = words.next() {
                    let (first, count) = (header & 0xF_FFFF, header >> 20);
                    for (i, offset) in (0..count).zip(words.by_ref()) {
                        persist.insert(first + i, offset as usize);
                    }
                }
            }
            ppt::DOCUMENT if r.version == ppt::CONTAINER_VERSION => document = Some(r.data),
            _ => {}
        }
    }

    // Each slide's persist id and placeholder text, in the order shown.
    let mut slides: Vec<(u32, Vec<String>)> = Vec::new();
    let slide_list = document.into_iter().flat_map(ppt_records).find(|(_, r)| r.kind == ppt::SLIDE_LIST_WITH_TEXT && r.instance == 0);
    if let Some((_, list)) = slide_list {
        for (_, r) in ppt_records(list.data) {
            match r.kind {
                ppt::SLIDE_PERSIST_ATOM => {
                    let id = r.data.first_chunk::<4>().map_or(0, |w| u32::from_le_bytes(*w));
                    slides.push((id, Vec::new()));
                }
                // References to placeholder text count text headers, since
                // a placeholder with no text has a header and no text atom.
                ppt::TEXT_HEADER_ATOM => {
                    if let Some((_, texts)) = slides.last_mut() {
                        texts.push(String::new());
                    }
                }
                ppt::TEXT_CHARS_ATOM | ppt::TEXT_BYTES_ATOM => {
                    if let Some(text) = slides.last_mut().and_then(|(_, texts)| texts.last_mut()) {
                        *text = ppt_text(&r);
                    }
                }
                _ => {}
            }
        }
    }

    slides
        .into_iter()
        .map(|(id, placeholders)| {
            let drawing = persist
                .get(&id)
                .and_then(|&offset| ppt_records(stream.get(offset..)?).next())
                .filter(|(_, r)| r.kind == ppt::SLIDE && r.version == ppt::CONTAINER_VERSION);
            let mut used = vec![false; placeholders.len()];
            let mut out = String::new();
            if let Some((_, slide)) = drawing {
                ppt_collect_text(slide.data, &placeholders, &mut used, &mut out, &mut budget, 0)?;
            }
            // Placeholder text the drawing didn't refer to is read at the end,
            // so nothing is lost if the drawing couldn't be found.
            for (text, _) in placeholders.iter().zip(&used).filter(|(_, used)| !**used) {
                push_ppt_text(&mut out, text, &mut budget)?;
            }
            Ok(out)
        })
        .collect()
}

/// Adds a piece of a slide's text to `out`, taking its size off `budget`.
fn push_ppt_text(out: &mut String, text: &str, budget: &mut usize) -> anyhow::Result<()> {
    *budget = budget
        .checked_sub(text.len() + 2)
        .context("the presentation has more text than can be read (the limit is 100 MB)")?;
    out.push_str(text);
    out.push_str("\n\n");
    Ok(())
}

/// Appends the text in a slide's records to `out`, in the order the shapes
/// appear, looking up references to placeholder text in `placeholders`.
/// The text's size is taken off `budget`.
fn ppt_collect_text(
    data: &[u8],
    placeholders: &[String],
    used: &mut [bool],
    out: &mut String,
    budget: &mut usize,
    depth: usize,
) -> anyhow::Result<()> {
    if depth > ppt::MAX_DEPTH {
        return Ok(());
    }
    for (_, r) in ppt_records(data) {
        match r.kind {
            ppt::TEXT_CHARS_ATOM | ppt::TEXT_BYTES_ATOM => {
                // A slide number is stored as a "*", which stands for the
                // number; a box holding only that repeats on every slide.
                let text = ppt_text(&r);
                if text.trim() != "*" {
                    push_ppt_text(out, &text, budget)?;
                }
            }
            ppt::OUTLINE_TEXT_REF_ATOM => {
                let index = r.data.first_chunk::<4>().map_or(usize::MAX, |w| u32::from_le_bytes(*w) as usize);
                if let Some(text) = placeholders.get(index) {
                    used[index] = true;
                    push_ppt_text(out, text, budget)?;
                }
            }
            _ if r.version == ppt::CONTAINER_VERSION => {
                ppt_collect_text(r.data, placeholders, used, out, budget, depth + 1)?
            }
            _ => {}
        }
    }
    Ok(())
}

/// Decodes a text atom. Paragraphs end with a carriage return and line
/// breaks are a vertical tab.
fn ppt_text(r: &PptRecord) -> String {
    let text = if r.kind == ppt::TEXT_CHARS_ATOM {
        decode_utf16(r.data, u16::from_le_bytes)
    } else {
        // Each byte is the low byte of a UTF-16 character, so this is Latin-1.
        r.data.iter().map(|&b| char::from(b)).collect()
    };
    text.replace('\r', "\n\n").replace('\u{b}', "\n")
}

/// Reads a CSV file as a spoken table: how many columns and rows it has, then
/// one paragraph per row giving each column's name and value. The first row
/// holds the column names. Empty cells are left out, so sparse tables do not
/// read as a long list of blanks.
fn read_csv(bytes: &[u8]) -> String {
    let text = read_txt(bytes);
    speak_table(parse_csv(&text, detect_delimiter(&text)))
}

/// Reads rows of cells as a spoken table. See `read_csv`.
fn speak_table(records: Vec<Vec<String>>) -> String {
    let mut records = records.into_iter().filter(|r| !row_is_empty(r));
    let Some(header) = records.next() else { return String::new() };
    let rows: Vec<Vec<String>> = records.collect();
    let columns = rows.iter().map(Vec::len).fold(header.len(), usize::max);
    let names: Vec<String> = (0..columns)
        .map(|i| match header.get(i).map(|s| s.trim()) {
            Some(name) if !name.is_empty() => speakable_column(name),
            _ => format!("Column {}", i + 1),
        })
        .collect();

    let mut out = format!("Table with {} and {}.", count(columns, "column"), count(rows.len(), "row"));
    if rows.is_empty() {
        out.push_str(&format!(" The columns are {}.", names.join(", ")));
    }
    for (n, row) in rows.iter().enumerate() {
        out.push_str(&format!("\n\nRow {}.", n + 1));
        for (name, cell) in names.iter().zip(row) {
            // Line breaks inside a cell would otherwise split the row in two.
            let cell = cell.split_whitespace().collect::<Vec<_>>().join(" ");
            if !cell.is_empty() {
                out.push_str(&format!(" {name}: {cell}"));
                if !cell.ends_with(['.', '!', '?']) {
                    out.push('.');
                }
            }
        }
    }
    out
}

/// Makes a column name read naturally: "query_type" becomes "query type" and
/// "Totals.Capital outlay" becomes "Totals, Capital outlay". Names whose dots
/// mark abbreviations, such as "No." or "e.g.", keep them.
fn speakable_column(name: &str) -> String {
    let name = name.replace('_', " ");
    let parts: Vec<&str> = name.split('.').map(str::trim).collect();
    if parts.len() > 1 && parts.iter().all(|p| p.chars().count() >= 2) {
        parts.join(", ")
    } else {
        name.trim().to_owned()
    }
}

/// "1 row", "3 rows" or "no rows".
fn count(n: usize, noun: &str) -> String {
    match n {
        0 => format!("no {noun}s"),
        1 => format!("1 {noun}"),
        _ => format!("{n} {noun}s"),
    }
}

/// Picks whichever of comma, semicolon or tab appears most often in the first
/// line (outside quotes). Spreadsheets saved in many European locales use
/// semicolons. Commas win a tie.
fn detect_delimiter(text: &str) -> char {
    let mut counts = [(',', 0), (';', 0), ('\t', 0)];
    let mut quoted = false;
    for c in text.chars() {
        match c {
            '"' => quoted = !quoted,
            '\n' | '\r' if !quoted => break,
            _ if !quoted => {
                if let Some(entry) = counts.iter_mut().find(|(d, _)| *d == c) {
                    entry.1 += 1;
                }
            }
            _ => {}
        }
    }
    counts.iter().rev().max_by_key(|(_, n)| *n).filter(|(_, n)| *n > 0).map_or(',', |(d, _)| *d)
}

/// Splits CSV text into records of fields. Handles quoted fields containing
/// the delimiter, line breaks or doubled quotes, and any style of line ending.
/// A quote part-way through an unquoted field is kept as an ordinary character.
fn parse_csv(text: &str, delimiter: char) -> Vec<Vec<String>> {
    let mut records = Vec::new();
    let mut record = Vec::new();
    let mut field = String::new();
    let mut quoted = false;
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        if quoted {
            if c != '"' {
                field.push(c);
            } else if chars.next_if_eq(&'"').is_some() {
                field.push('"');
            } else {
                quoted = false;
            }
        } else if c == '"' && field.is_empty() {
            quoted = true;
        } else if c == delimiter {
            record.push(std::mem::take(&mut field));
        } else if c == '\n' || c == '\r' {
            if c == '\r' {
                chars.next_if_eq(&'\n');
            }
            record.push(std::mem::take(&mut field));
            records.push(std::mem::take(&mut record));
        } else {
            field.push(c);
        }
    }
    if !field.is_empty() || !record.is_empty() {
        record.push(field);
        records.push(record);
    }
    records
}

/// Most characters that may be typed or pasted into the text box, about 30,000
/// words. Keeps the box responsive and cloud costs bounded.
pub const MAX_PASTED_CHARS: usize = 200_000;

/// Cleans text typed or pasted into the app before it is read aloud: cut to
/// `MAX_PASTED_CHARS`, stripped of invisible characters that can hide words or
/// reorder how text is shown, then tidied like text from a file.
pub fn clean_pasted(text: &str) -> String {
    // Windows puts "\r\n" at the end of each line it copies, which `tidy`
    // would otherwise take as a paragraph break.
    let visible: String = text
        .chars()
        .take(MAX_PASTED_CHARS)
        .filter(|c| !is_hidden_format(*c))
        .collect::<String>()
        .replace("\r\n", "\n");
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
/// keeping paragraph breaks, and drops control characters.
fn tidy(text: &str) -> String {
    let cleaned: String = text
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
    fn docx_text_and_paragraphs() {
        let xml = r#"<w:document xmlns:w="x"><w:body>
            <w:p><w:r><w:t>Fish &amp; chips</w:t></w:r><w:r><w:tab/><w:t xml:space="preserve"> today</w:t></w:r></w:p>
            <w:p><w:r><w:t>Second &#8211; para</w:t></w:r></w:p>
        </w:body></w:document>"#;
        let text = tidy(&docx_xml_to_text(xml).unwrap());
        assert_eq!(text, "Fish & chips today\n\nSecond \u{2013} para");
    }

    #[test]
    fn docx_ignores_custom_entities() {
        let xml = r#"<!DOCTYPE d [<!ENTITY x "boom">]><w:document xmlns:w="x"><w:p><w:t>a&x;b</w:t></w:p></w:document>"#;
        assert_eq!(tidy(&docx_xml_to_text(xml).unwrap()), "ab");
    }

    #[test]
    fn odt_text_and_paragraphs() {
        let xml = r#"<office:document-content xmlns:office="o" xmlns:text="t"><office:body><office:text>
            <text:tracked-changes><text:changed-region><text:deletion><text:p>gone</text:p></text:deletion></text:changed-region></text:tracked-changes>
            <text:h text:outline-level="1">Fish &amp; chips</text:h>
            <text:p>Two<text:s text:c="3"/>words<text:tab/>here<text:note><text:note-citation>1</text:note-citation><text:note-body><text:p>A footnote</text:p></text:note-body></text:note>.<office:annotation><text:p>A comment</text:p></office:annotation></text:p>
            <text:list><text:list-item><text:p>Line one<text:line-break/>line &#8211; two</text:p></text:list-item></text:list>
        </office:text></office:body></office:document-content>"#;
        assert_eq!(
            tidy(&odt_xml_to_text(xml).unwrap()),
            "Fish & chips\n\nTwo words here.\n\nLine one line \u{2013} two"
        );
    }

    #[test]
    fn docx_says_its_formatting() {
        let styles = r#"<w:styles xmlns:w="x">
            <w:style w:type="paragraph" w:styleId="Heading1"><w:name w:val="heading 1"/><w:rPr><w:b/><w:color w:val="2F5496"/></w:rPr></w:style>
            <w:style w:type="paragraph" w:styleId="ListBullet"><w:name w:val="List Bullet"/><w:pPr><w:numPr><w:numId w:val="3"/></w:numPr></w:pPr></w:style>
            <w:style w:type="character" w:styleId="Strong"><w:name w:val="Strong"/><w:rPr><w:b/></w:rPr></w:style>
            <w:style w:type="character" w:styleId="Hyperlink"><w:name w:val="Hyperlink"/><w:rPr><w:color w:val="0563C1"/><w:u w:val="single"/></w:rPr></w:style>
        </w:styles>"#;
        let xml = r#"<w:document xmlns:w="x"><w:body>
            <w:p><w:pPr><w:pStyle w:val="Heading1"/></w:pPr><w:r><w:t>Fish &amp; chips</w:t></w:r></w:p>
            <w:p><w:r><w:t xml:space="preserve">This is </w:t></w:r><w:r><w:rPr><w:rStyle w:val="Strong"/></w:rPr><w:t>very</w:t></w:r><w:r><w:t xml:space="preserve"> </w:t></w:r><w:r><w:rPr><w:b/><w:i/></w:rPr><w:t>hot</w:t></w:r><w:r><w:t xml:space="preserve"> and </w:t></w:r><w:r><w:rPr><w:color w:val="FF0000"/><w:u w:val="single"/></w:rPr><w:t>red</w:t></w:r><w:r><w:t>.</w:t></w:r></w:p>
            <w:p><w:pPr><w:pStyle w:val="ListBullet"/></w:pPr><w:r><w:t>One</w:t></w:r></w:p>
            <w:p><w:pPr><w:numPr><w:ilvl w:val="1"/><w:numId w:val="3"/></w:numPr></w:pPr><w:r><w:t xml:space="preserve">See </w:t></w:r><w:hyperlink r:id="r1"><w:r><w:rPr><w:rStyle w:val="Hyperlink"/></w:rPr><w:t>the menu</w:t></w:r></w:hyperlink></w:p>
            <w:p><w:r><w:rPr><w:strike/></w:rPr><w:t>Gone.</w:t></w:r></w:p>
            <w:p><w:r><w:rPr><w:b w:val="0"/><w:color w:val="auto"/></w:rPr><w:t>Plain</w:t></w:r></w:p>
            <w:tbl><w:tr><w:tc><w:p><w:r><w:t>A</w:t></w:r></w:p></w:tc></w:tr><w:tr><w:tc><w:p><w:r><w:t>B</w:t></w:r></w:p></w:tc></w:tr></w:tbl>
        </w:body></w:document>"#;
        let text = tidy(&docx_xml_to_spoken(xml, &parse_docx_styles(styles).unwrap()).unwrap());
        assert_eq!(
            text.split("\n\n").collect::<Vec<_>>(),
            [
                "Heading level 1. Fish & chips",
                "This is, bold, very, italic, hot, end bold, end italic, and, underlined, colour red, red. End underline, end colour.",
                "List item. One",
                "List item, level 2. See, link, the menu, end link.",
                "Crossed out, Gone. End crossed out.",
                "Plain",
                "Table.",
                "Row 1.",
                "A",
                "Row 2.",
                "B",
                "End of table.",
            ]
        );
    }

    #[test]
    fn docx_content_leaves_formatting_out() {
        let xml = r#"<w:document xmlns:w="x"><w:body><w:p><w:r><w:rPr><w:b/><w:color w:val="FF0000"/></w:rPr><w:t>Hot</w:t></w:r></w:p></w:body></w:document>"#;
        assert_eq!(tidy(&docx_xml_to_text(xml).unwrap()), "Hot");
        let spoken = tidy(&docx_xml_to_spoken(xml, &HashMap::new()).unwrap());
        assert_eq!(spoken, "Bold, colour red, Hot, end bold, end colour.");
    }

    #[test]
    fn docx_styles_that_loop_are_safe() {
        let styles = r#"<w:styles xmlns:w="x">
            <w:style w:styleId="A"><w:basedOn w:val="B"/><w:rPr><w:i/></w:rPr></w:style>
            <w:style w:styleId="B"><w:basedOn w:val="A"/></w:style>
        </w:styles>"#;
        let xml = r#"<w:document xmlns:w="x"><w:body><w:p><w:pPr><w:pStyle w:val="A"/></w:pPr><w:r><w:t>Round</w:t></w:r></w:p></w:body></w:document>"#;
        let text = tidy(&docx_xml_to_spoken(xml, &parse_docx_styles(styles).unwrap()).unwrap());
        assert_eq!(text, "Italic, Round, end italic.");
    }

    #[test]
    fn odt_says_its_formatting() {
        let styles_xml = r##"<office:document-styles xmlns:office="o" xmlns:style="s" xmlns:fo="f"><office:styles>
            <style:style style:name="Strong_20_Emphasis" style:family="text"><style:text-properties fo:font-weight="bold"/></style:style>
            <style:style style:name="Heading_20_1" style:family="paragraph"><style:text-properties fo:font-weight="bold" fo:color="#1f3864"/></style:style>
        </office:styles></office:document-styles>"##;
        let xml = r##"<office:document-content xmlns:office="o" xmlns:style="s" xmlns:fo="f" xmlns:text="t" xmlns:table="tb" xmlns:xlink="l">
            <office:automatic-styles>
                <style:style style:name="T1" style:family="text"><style:text-properties fo:font-style="italic" fo:color="#00a000"/></style:style>
                <style:style style:name="T2" style:family="text"><style:text-properties style:text-underline-style="solid" style:text-line-through-style="solid"/></style:style>
                <style:style style:name="P1" style:family="paragraph" style:parent-style-name="Heading_20_1"/>
            </office:automatic-styles>
            <office:body><office:text>
            <text:h text:style-name="P1" text:outline-level="2">Fish &amp; chips</text:h>
            <text:p>This is <text:span text:style-name="Strong_20_Emphasis">very <text:span text:style-name="T1">hot</text:span></text:span> and <text:span text:style-name="T2">off</text:span>.<text:note><text:note-body><text:p>A footnote</text:p></text:note-body></text:note></text:p>
            <text:list><text:list-item><text:p>One</text:p><text:list><text:list-item><text:p>See <text:a xlink:href="https://example.org">the menu</text:a></text:p></text:list-item></text:list></text:list-item></text:list>
            <table:table><table:table-row><table:table-cell><text:p>A</text:p></table:table-cell></table:table-row></table:table>
            </office:text></office:body></office:document-content>"##;
        let mut styles = OdfStyles::default();
        styles.add(styles_xml).unwrap();
        styles.add(xml).unwrap();
        let text = tidy(&odt_xml_to_spoken(xml, &styles).unwrap());
        assert_eq!(
            text.split("\n\n").collect::<Vec<_>>(),
            [
                "Heading level 2. Fish & chips",
                "This is, bold, very, italic, colour green, hot, end bold, end italic, end colour, and, underlined, crossed out, off. End underline, end crossed out.",
                "List item. One",
                "List item, level 2. See, link, the menu, end link.",
                "Table.",
                "Row 1.",
                "A",
                "End of table.",
            ]
        );
    }

    const MARKDOWN: &str = "---\ntitle: Menu\n---\n\
        # Fish & chips\n\n\
        This is **very *hot*** and ~~off~~.[^1]\n\
        On a new line.\n\n\
        - One\n  - See [the menu](https://example.org)\n\n\
        - [x] Order\n\n\
        > Quoted `code`\n\n\
        <div>Hidden</div>\n\n\
        ```\nlet x = 1;\n```\n\n\
        | A | B |\n|---|---|\n| 1 | 2 |\n\n\
        ![A plate of chips](chips.jpg)\n\n\
        [^1]: A footnote\n";

    #[test]
    fn markdown_says_its_formatting() {
        let text = tidy(&markdown_to_text(MARKDOWN, DocumentReading::Format));
        assert_eq!(
            text.split("\n\n").collect::<Vec<_>>(),
            [
                "Heading level 1. Fish & chips",
                "This is, bold, very, italic, hot, end bold, end italic, and, crossed out, off. End crossed out, On a new line.",
                "List item. One",
                "List item, level 2. See, link, the menu, end link.",
                "List item. Done. Order",
                "Quote. Quoted code",
                "Code. let x = 1;",
                "Table.",
                "Row 1.",
                "A",
                "B",
                "Row 2.",
                "1",
                "2",
                "End of table.",
                "A plate of chips",
            ]
        );
    }

    #[test]
    fn markdown_content_leaves_formatting_out() {
        let text = tidy(&markdown_to_text(MARKDOWN, DocumentReading::Content));
        assert_eq!(
            text.split("\n\n").collect::<Vec<_>>(),
            [
                "Fish & chips",
                "This is very hot and off. On a new line.",
                "One",
                "See the menu",
                "Order",
                "Quoted code",
                "let x = 1;",
                "A",
                "B",
                "1",
                "2",
                "A plate of chips",
            ]
        );
    }

    #[test]
    fn markdown_empty_heading_says_nothing() {
        let text = tidy(&markdown_to_text("#\n\nText\n", DocumentReading::Format));
        assert_eq!(text, "Text");
    }

    #[test]
    fn colours_have_plain_names() {
        assert_eq!(colour_name("FF0000"), Some("red"));
        assert_eq!(colour_name("#0000ff"), Some("blue"));
        assert_eq!(colour_name("00B050"), Some("green"));
        assert_eq!(colour_name("7030A0"), Some("purple"));
        assert_eq!(colour_name("000000"), None);
        assert_eq!(colour_name("auto"), None);
        assert_eq!(colour_name("ééé"), None);
    }

    #[test]
    fn ods_reads_each_sheet_as_a_table() {
        let xml = r#"<office:document-content xmlns:office="o" xmlns:table="t" xmlns:text="x"><office:body><office:spreadsheet>
            <table:table table:name="Sales">
                <table:table-column table:number-columns-repeated="3"/>
                <table:table-row><table:table-cell><text:p>Name</text:p></table:table-cell><table:table-cell table:number-columns-repeated="2"><text:p>Score</text:p></table:table-cell><table:table-cell table:number-columns-repeated="16381"/></table:table-row>
                <table:table-row table:number-rows-repeated="2"><table:table-cell><text:p>Ann</text:p><text:p>Smith</text:p></table:table-cell><table:table-cell/><table:table-cell office:value-type="float" office:value="5"><text:p>5</text:p><office:annotation><text:p>note</text:p></office:annotation></table:table-cell></table:table-row>
                <table:table-row table:number-rows-repeated="1048573"><table:table-cell table:number-columns-repeated="16384"/></table:table-row>
            </table:table>
            <table:table table:name="Empty"><table:table-row><table:table-cell/></table:table-row></table:table>
            <table:table table:name="Notes"><table:table-row><table:table-cell><text:p>Only &lt;header&gt;</text:p></table:table-cell></table:table-row></table:table>
        </office:spreadsheet></office:body></office:document-content>"#;
        assert_eq!(
            tidy(&ods_xml_to_text(xml).unwrap()),
            "Sheet: Sales.\n\n\
             Table with 3 columns and 2 rows.\n\n\
             Row 1. Name: Ann Smith. Score: 5.\n\n\
             Row 2. Name: Ann Smith. Score: 5.\n\n\
             Sheet: Notes.\n\n\
             Table with 1 column and no rows. The columns are Only <header>."
        );
    }

    #[test]
    fn ods_single_sheet_has_no_name_and_huge_repeats_are_refused() {
        let xml = r#"<office:document-content><table:table table:name="Sheet1"><table:table-row><table:table-cell><text:p>a</text:p></table:table-cell></table:table-row></table:table></office:document-content>"#;
        assert_eq!(ods_xml_to_text(xml).unwrap(), "Table with 1 column and no rows. The columns are a.");
        let xml = r#"<table:table><table:table-row table:number-rows-repeated="1000000"><table:table-cell table:number-columns-repeated="1000"><text:p>x</text:p></table:table-cell></table:table-row></table:table>"#;
        assert!(ods_xml_to_text(xml).is_err());
    }

    #[test]
    fn pptx_reads_slides_in_show_order() {
        use std::io::Write;
        let file = tempfile::Builder::new().suffix(".pptx").tempfile().unwrap();
        let mut zip = zip::ZipWriter::new(file.reopen().unwrap());
        let parts = [
            (
                "ppt/presentation.xml",
                r#"<p:presentation xmlns:p="p" xmlns:r="r"><p:sldIdLst><p:sldId id="256" r:id="rId3"/><p:sldId id="257" r:id="rId2"/><p:sldId id="259" r:id="rId9"/><p:sldId id="258" r:id="rId4"/><p:sldId id="260" r:id="rId5"/></p:sldIdLst></p:presentation>"#,
            ),
            (
                "ppt/_rels/presentation.xml.rels",
                r#"<Relationships><Relationship Id="rId2" Target="slides/slide1.xml"/><Relationship Id="rId3" Target="/ppt/slides/slide2.xml"/><Relationship Id="rId4" Target="slides/../slides/slide3.xml"/><Relationship Id="rId5" Target="slides/slide4.xml"/></Relationships>"#,
            ),
            (
                "ppt/slides/slide1.xml",
                r#"<p:sld xmlns:p="p" xmlns:a="a"><p:cSld><p:spTree><p:sp><p:txBody><a:p><a:r><a:t>Fish &amp; chips</a:t></a:r><a:br/><a:r><a:t>today</a:t></a:r></a:p><a:p><a:r><a:t>Second</a:t></a:r><a:r><a:t> point</a:t></a:r></a:p></p:txBody></p:sp></p:spTree></p:cSld></p:sld>"#,
            ),
            (
                "ppt/slides/slide2.xml",
                r#"<p:sld xmlns:p="p" xmlns:a="a"><p:sp><p:nvSpPr><p:nvPr><p:ph type="title"/></p:nvPr></p:nvSpPr><p:txBody><a:p><a:r><a:t>Welcome</a:t></a:r></a:p></p:txBody></p:sp><p:sp><p:nvSpPr><p:nvPr><p:ph type="sldNum" idx="12"/></p:nvPr></p:nvSpPr><p:txBody><a:p><a:fld type="slidenum"><a:t>1</a:t></a:fld></a:p></p:txBody></p:sp><p:sp><p:txBody><a:p><a:r><a:t>After</a:t></a:r></a:p></p:txBody></p:sp></p:sld>"#,
            ),
            ("ppt/slides/slide3.xml", r#"<p:sld xmlns:p="p" xmlns:a="a"><a:p><a:endParaRPr/></a:p></p:sld>"#),
            (
                "ppt/slides/slide4.xml",
                r#"<p:sld xmlns:p="p" xmlns:a="a" xmlns:r="r"><a:p><a:r><a:t>Last</a:t></a:r></a:p><p:graphicFrame><a:graphic><a:graphicData><c:chart xmlns:c="c" r:id="rId2"/></a:graphicData></a:graphic></p:graphicFrame><p:graphicFrame><a:graphic><a:graphicData><dgm:relIds xmlns:dgm="d" r:dm="rId3" r:lo="rId4"/></a:graphicData></a:graphic></p:graphicFrame><p:graphicFrame><a:graphic><a:graphicData><c:chart xmlns:c="c" r:id="rId8"/></a:graphicData></a:graphic></p:graphicFrame></p:sld>"#,
            ),
            (
                "ppt/slides/_rels/slide4.xml.rels",
                r#"<Relationships><Relationship Id="rId2" Target="../charts/chart1.xml"/><Relationship Id="rId3" Target="../diagrams/data1.xml"/><Relationship Id="rId8" Target="../charts/missing.xml"/></Relationships>"#,
            ),
            (
                "ppt/charts/chart1.xml",
                r#"<c:chartSpace xmlns:c="c" xmlns:a="a"><c:chart><c:title><c:tx><c:rich><a:p><a:r><a:t>Sales</a:t></a:r></a:p></c:rich></c:tx></c:title><c:plotArea><c:pieChart><c:ser><c:tx><c:v>North</c:v></c:tx><c:cat><c:strLit><c:pt idx="0"><c:v>Q1</c:v></c:pt></c:strLit></c:cat><c:val><c:numLit><c:pt idx="0"><c:v>10</c:v></c:pt></c:numLit></c:val></c:ser></c:pieChart></c:plotArea></c:chart></c:chartSpace>"#,
            ),
            (
                "ppt/diagrams/data1.xml",
                r#"<dgm:dataModel xmlns:dgm="d" xmlns:a="a"><dgm:ptLst><dgm:pt modelId="0" type="doc"/><dgm:pt modelId="1"><dgm:t><a:p><a:r><a:t>Step</a:t></a:r></a:p></dgm:t></dgm:pt></dgm:ptLst><dgm:cxnLst><dgm:cxn modelId="9" srcId="0" destId="1" srcOrd="0" destOrd="0"/></dgm:cxnLst></dgm:dataModel>"#,
            ),
        ];
        for (name, xml) in parts {
            zip.start_file(name, zip::write::SimpleFileOptions::default()).unwrap();
            zip.write_all(xml.as_bytes()).unwrap();
        }
        zip.finish().unwrap();
        assert_eq!(
            extract_text(file.path(), DocumentReading::Content).unwrap(),
            "Slide 1.\n\nWelcome\n\nAfter\n\nSlide 2.\n\nFish & chips today\n\nSecond point\n\nSlide 5.\n\nLast\n\nChart: Sales.\n\nNorth. Q1: 10.\n\nStep"
        );
    }

    #[test]
    fn password_protected_presentations_say_so() {
        for (suffix, stream) in [(".pptx", "EncryptedPackage"), (".ppt", "EncryptedSummary")] {
            let file = tempfile::Builder::new().suffix(suffix).tempfile().unwrap();
            let mut cfb = cfb::create(file.path()).unwrap();
            cfb.create_stream(stream).unwrap();
            cfb.flush().unwrap();
            drop(cfb);
            let error = extract_text(file.path(), DocumentReading::Content).unwrap_err().to_string();
            assert!(error.contains("password protected"), "{suffix}: {error}");
        }
    }

    #[test]
    fn chart_reads_titles_and_each_series() {
        let xml = r#"<c:chartSpace xmlns:c="c" xmlns:a="a"><c:chart>
            <c:title><c:tx><c:rich><a:bodyPr/><a:p><a:r><a:t>Sales by</a:t></a:r><a:r><a:t> region</a:t></a:r></a:p><a:p><a:r><a:t>2026</a:t></a:r></a:p></c:rich></c:tx><c:overlay val="0"/></c:title>
            <c:plotArea><c:barChart><c:barDir val="col"/>
                <c:ser><c:idx val="0"/><c:order val="0"/>
                    <c:tx><c:strRef><c:f>Sheet1!$B$1</c:f><c:strCache><c:ptCount val="1"/><c:pt idx="0"><c:v>North &amp; East</c:v></c:pt></c:strCache></c:strRef></c:tx>
                    <c:dLbls><c:dLbl><c:idx val="0"/><c:tx><c:rich><a:p><a:r><a:t>label</a:t></a:r></a:p></c:rich></c:tx></c:dLbl></c:dLbls>
                    <c:cat><c:multiLvlStrRef><c:f>Sheet1!$A$2:$A$3</c:f><c:multiLvlStrCache><c:ptCount val="2"/>
                        <c:lvl><c:pt idx="0"><c:v>Q1</c:v></c:pt><c:pt idx="1"><c:v>Q2</c:v></c:pt></c:lvl>
                        <c:lvl><c:pt idx="0"><c:v>2026</c:v></c:pt></c:lvl>
                    </c:multiLvlStrCache></c:multiLvlStrRef></c:cat>
                    <c:val><c:numRef><c:f>Sheet1!$B$2:$B$3</c:f><c:numCache><c:formatCode>General</c:formatCode><c:ptCount val="2"/><c:pt idx="0"><c:v>12.300000000000001</c:v></c:pt><c:pt idx="1"><c:v>20</c:v></c:pt></c:numCache></c:numRef></c:val>
                    <c:extLst><c:ext><c15:filteredSeriesTitle><c15:tx><c:v>hidden</c:v></c15:tx></c15:filteredSeriesTitle></c:ext></c:extLst>
                </c:ser>
                <c:ser><c:idx val="1"/>
                    <c:val><c:numRef><c:numCache><c:formatCode>0.0%</c:formatCode><c:pt idx="1"><c:v>0.355</c:v></c:pt></c:numCache></c:numRef></c:val>
                </c:ser>
            </c:barChart>
            <c:catAx><c:axId val="1"/><c:title><c:tx><c:rich><a:p><a:r><a:t>Quarter</a:t></a:r></a:p></c:rich></c:tx></c:title></c:catAx>
            <c:valAx><c:axId val="2"/><c:title><c:tx><c:rich><a:p><a:r><a:t>Sales (£m)</a:t></a:r></a:p></c:rich></c:tx></c:title><c:txPr><a:p><a:pPr/></a:p></c:txPr></c:valAx>
            </c:plotArea></c:chart></c:chartSpace>"#;
        assert_eq!(
            tidy(&chart_xml_to_text(xml).unwrap()),
            "Chart: Sales by region 2026. Axes: Quarter, Sales (£m).\n\n\
             North & East. Q1, 2026: 12.3. Q2: 20.\n\n\
             Series 2. Point 2: 35.5%."
        );
        assert_eq!(chart_xml_to_text("<c:chartSpace/>").unwrap(), "Chart.");
    }

    #[test]
    fn chart_numbers_read_naturally() {
        assert_eq!(speakable_number("12.300000000000001", "General"), "12.3");
        assert_eq!(speakable_number("1500", ""), "1500");
        assert_eq!(speakable_number("0.25", "0%"), "25%");
        assert_eq!(speakable_number("-0.0000001", ""), "0");
        assert_eq!(speakable_number(" n/a ", ""), "n/a");
    }

    #[test]
    fn smartart_reads_boxes_in_outline_order() {
        let xml = r#"<dgm:dataModel xmlns:dgm="d" xmlns:a="a"><dgm:ptLst>
            <dgm:pt modelId="{0}" type="doc"><dgm:prSet/><dgm:spPr/><dgm:t><a:bodyPr/><a:lstStyle/><a:p><a:endParaRPr/></a:p></dgm:t></dgm:pt>
            <dgm:pt modelId="{B}"><dgm:prSet phldrT="[Text]"/><dgm:spPr/><dgm:t><a:bodyPr/><a:p><a:r><a:t>Second</a:t></a:r></a:p></dgm:t></dgm:pt>
            <dgm:pt modelId="{A}"><dgm:t><a:p><a:r><a:t>First &amp; foremost</a:t></a:r><a:br/><a:r><a:t>line two</a:t></a:r></a:p></dgm:t></dgm:pt>
            <dgm:pt modelId="{A1}"><dgm:t><a:p><a:r><a:t>Under first</a:t></a:r></a:p></dgm:t></dgm:pt>
            <dgm:pt modelId="{T}" type="parTrans" cxnId="{c1}"><dgm:t><a:p><a:r><a:t>connector</a:t></a:r></a:p></dgm:t></dgm:pt>
            <dgm:pt modelId="{P}" type="pres"><dgm:prSet presName="x"/></dgm:pt>
            <dgm:pt modelId="{O}"><dgm:t><a:p><a:r><a:t>Orphan</a:t></a:r></a:p></dgm:t></dgm:pt>
            <dgm:pt modelId="{E}"><dgm:t><a:p><a:endParaRPr/></a:p></dgm:t></dgm:pt>
        </dgm:ptLst><dgm:cxnLst>
            <dgm:cxn modelId="{c1}" srcId="{0}" destId="{B}" srcOrd="1" destOrd="0"/>
            <dgm:cxn modelId="{c2}" srcId="{0}" destId="{A}" srcOrd="0" destOrd="0"/>
            <dgm:cxn modelId="{c3}" srcId="{A}" destId="{A1}" srcOrd="0" destOrd="0"/>
            <dgm:cxn modelId="{c4}" type="presOf" srcId="{B}" destId="{P}" srcOrd="0" destOrd="0"/>
            <dgm:cxn modelId="{c5}" srcId="{A1}" destId="{A}" srcOrd="0" destOrd="0"/>
        </dgm:cxnLst></dgm:dataModel>"#;
        assert_eq!(
            tidy(&diagram_xml_to_text(xml).unwrap()),
            "First & foremost line two\n\nUnder first\n\nSecond\n\nOrphan"
        );
    }

    #[test]
    fn pptx_slide_skips_fallback_copies() {
        let xml = r#"<p:sld xmlns:p="p" xmlns:a="a" xmlns:mc="mc" xmlns:r="r"><mc:AlternateContent>
            <mc:Choice Requires="cx1"><p:graphicFrame><a:graphic><a:graphicData><cx:chart xmlns:cx="cx" r:id="rId2"/></a:graphicData></a:graphic></p:graphicFrame></mc:Choice>
            <mc:Fallback><p:sp><p:txBody><a:p><a:r><a:t>This chart isn't available in your version of PowerPoint</a:t></a:r></a:p></p:txBody></p:sp></mc:Fallback>
        </mc:AlternateContent><a:p><a:r><a:t>After</a:t></a:r></a:p></p:sld>"#;
        let text = pptx_slide_to_text(xml, |e| Ok(if matches!(e, Embedded::Chart(id) if id == "rId2") { "Chart." } else { "" }.into()));
        assert_eq!(tidy(&text.unwrap()), "Chart.\n\nAfter");
    }

    #[test]
    fn pptx_part_names() {
        assert_eq!(resolve_part("ppt", "slides/slide1.xml"), "ppt/slides/slide1.xml");
        assert_eq!(resolve_part("ppt", "/ppt/slides/slide1.xml"), "ppt/slides/slide1.xml");
        assert_eq!(resolve_part("ppt/slides", "../media/a.png"), "ppt/media/a.png");
    }

    /// A PowerPoint 97–2003 record: version and instance, type, then body.
    fn ppt_record(ver_instance: u16, kind: u16, body: &[u8]) -> Vec<u8> {
        let mut out = ver_instance.to_le_bytes().to_vec();
        out.extend(kind.to_le_bytes());
        out.extend((body.len() as u32).to_le_bytes());
        out.extend(body);
        out
    }

    #[test]
    fn ppt_reads_placeholders_and_text_boxes_in_order() {
        let utf16 = |s: &str| s.encode_utf16().flat_map(u16::to_le_bytes).collect::<Vec<u8>>();
        // Slide 1's drawing: its title, a text box, then its bullet points.
        let textbox = ppt_record(0xF, 0xF00D, &ppt_record(0, ppt::TEXT_CHARS_ATOM, &utf16("Note box")));
        let slide_number = ppt_record(0xF, 0xF00D, &ppt_record(0, ppt::TEXT_BYTES_ATOM, b"*"));
        let drawing = [
            ppt_record(0, ppt::OUTLINE_TEXT_REF_ATOM, &0u32.to_le_bytes()),
            textbox,
            slide_number,
            ppt_record(0, ppt::OUTLINE_TEXT_REF_ATOM, &1u32.to_le_bytes()),
            ppt_record(0, ppt::OUTLINE_TEXT_REF_ATOM, &2u32.to_le_bytes()),
            ppt_record(0, ppt::OUTLINE_TEXT_REF_ATOM, &99u32.to_le_bytes()),
        ]
        .concat();
        let slide = ppt_record(0xF, ppt::SLIDE, &ppt_record(0xF, 0xF002, &drawing));
        // Slide 2 has no drawing in the persist directory, so its
        // placeholder text is read on its own.
        let list = [
            ppt_record(0, ppt::SLIDE_PERSIST_ATOM, &[1, 0, 0, 0, 0, 0, 0, 0]),
            ppt_record(0, ppt::TEXT_HEADER_ATOM, &[0; 4]),
            ppt_record(0, ppt::TEXT_BYTES_ATOM, b"Title"),
            // An empty placeholder: a header with no text after it.
            ppt_record(0, ppt::TEXT_HEADER_ATOM, &[1, 0, 0, 0]),
            ppt_record(0, ppt::TEXT_HEADER_ATOM, &[1, 0, 0, 0]),
            ppt_record(0, ppt::TEXT_CHARS_ATOM, &utf16("Point one\rPoint\u{b}two")),
            ppt_record(0, ppt::SLIDE_PERSIST_ATOM, &[7, 0, 0, 0]),
            ppt_record(0, ppt::TEXT_HEADER_ATOM, &[0; 4]),
            ppt_record(0, ppt::TEXT_BYTES_ATOM, b"Caf\xe9"),
        ]
        .concat();
        let masters = ppt_record(0x1F, ppt::SLIDE_LIST_WITH_TEXT, &ppt_record(0, ppt::TEXT_BYTES_ATOM, b"Master"));
        let document = ppt_record(0xF, ppt::DOCUMENT, &[masters, ppt_record(0xF, ppt::SLIDE_LIST_WITH_TEXT, &list)].concat());
        let directory = ppt_record(0, ppt::PERSIST_DIRECTORY_ATOM, &[(1u32 | 1 << 20).to_le_bytes(), 0u32.to_le_bytes()].concat());
        let stream = [slide, document, directory].concat();
        assert_eq!(
            tidy(&speak_slides(ppt_stream_to_text(&stream, MAX_PPT_TEXT_BYTES).unwrap())),
            "Slide 1.\n\nTitle\n\nNote box\n\nPoint one\n\nPoint two\n\nSlide 2.\n\nCaf\u{e9}"
        );
    }

    #[test]
    fn ppt_survives_truncated_and_deeply_nested_records() {
        assert!(ppt_stream_to_text(&[0x0F, 0, 0xE8, 0x03, 0xFF, 0xFF, 0xFF, 0x7F, 1], 100).unwrap().is_empty());
        let mut nested = ppt_record(0, ppt::TEXT_BYTES_ATOM, b"deep");
        for _ in 0..1000 {
            nested = ppt_record(0xF, 0xF003, &nested);
        }
        let mut out = String::new();
        ppt_collect_text(&nested, &[], &mut [], &mut out, &mut 100, 0).unwrap();
        assert!(out.is_empty());
    }

    #[test]
    fn ppt_text_used_over_and_over_has_a_limit() {
        // Every slide shows the same drawing, and the drawing refers to the
        // same placeholder text many times.
        let refs: Vec<u8> = (0..100).flat_map(|_| ppt_record(0, ppt::OUTLINE_TEXT_REF_ATOM, &[0; 4])).collect();
        let drawing = [ppt_record(0, ppt::TEXT_BYTES_ATOM, &[b'a'; 1000]), refs].concat();
        let slide = ppt_record(0xF, ppt::SLIDE, &drawing);
        let mut list = Vec::new();
        for _ in 0..100 {
            list.extend(ppt_record(0, ppt::SLIDE_PERSIST_ATOM, &1u32.to_le_bytes()));
            list.extend(ppt_record(0, ppt::TEXT_HEADER_ATOM, &[0; 4]));
            list.extend(ppt_record(0, ppt::TEXT_BYTES_ATOM, &[b'b'; 1000]));
        }
        let document = ppt_record(0xF, ppt::DOCUMENT, &ppt_record(0xF, ppt::SLIDE_LIST_WITH_TEXT, &list));
        let directory = ppt_record(0, ppt::PERSIST_DIRECTORY_ATOM, &[(1u32 | 1 << 20).to_le_bytes(), 0u32.to_le_bytes()].concat());
        let stream = [slide, document, directory].concat();
        // About 10 MB of text from about 120 KB of file.
        let error = ppt_stream_to_text(&stream, 1024 * 1024).unwrap_err();
        assert!(error.to_string().contains("more text than can be read"), "{error}");
        let slides = ppt_stream_to_text(&stream, 20 * 1024 * 1024).unwrap();
        assert_eq!(slides.len(), 100);
    }

    #[test]
    fn txt_encodings() {
        assert_eq!(read_txt(&[0xEF, 0xBB, 0xBF, b'h', b'i']), "hi");
        assert_eq!(read_txt(&[0xFF, 0xFE, b'h', 0, b'i', 0]), "hi");
        assert_eq!(read_txt(&[b'c', 0xE9]), "c\u{e9}");
    }

    #[test]
    fn csv_reads_as_a_table() {
        let csv = "Name,Age,Town\r\nAnn,41,Leeds\r\n\"Smith, Bob\",,\"York\"\r\n";
        assert_eq!(
            tidy(&read_csv(csv.as_bytes())),
            "Table with 3 columns and 2 rows.\n\n\
             Row 1. Name: Ann. Age: 41. Town: Leeds.\n\n\
             Row 2. Name: Smith, Bob. Town: York."
        );
    }

    #[test]
    fn csv_semicolons_and_quotes() {
        let csv = "Username;Note\nbooker12;\"Said \"\"hi\"\"\n\nthen left.\"\n\njenkins46;x";
        assert_eq!(
            tidy(&read_csv(csv.as_bytes())),
            "Table with 2 columns and 2 rows.\n\n\
             Row 1. Username: booker12. Note: Said \"hi\" then left.\n\n\
             Row 2. Username: jenkins46. Note: x."
        );
    }

    #[test]
    fn csv_unnamed_and_extra_columns() {
        let csv = "a,\n1,2,3\n";
        assert_eq!(
            tidy(&read_csv(csv.as_bytes())),
            "Table with 3 columns and 1 row.\n\nRow 1. a: 1. Column 2: 2. Column 3: 3."
        );
    }

    #[test]
    fn csv_column_names_read_naturally() {
        assert_eq!(speakable_column("query_type"), "query type");
        assert_eq!(speakable_column("Totals.Capital outlay"), "Totals, Capital outlay");
        assert_eq!(speakable_column("Totals. Debt at end"), "Totals, Debt at end");
        assert_eq!(speakable_column("Ref No."), "Ref No.");
        assert_eq!(speakable_column("e.g. size"), "e.g. size");
    }

    #[test]
    fn csv_header_only_or_empty() {
        assert_eq!(read_csv(b"a,b\n"), "Table with 2 columns and no rows. The columns are a, b.");
        assert_eq!(read_csv(b"\n\n"), "");
    }

    #[test]
    fn csv_delimiter_ignores_quoted_text() {
        assert_eq!(detect_delimiter("\"a;b;c\",d\n"), ',');
        assert_eq!(detect_delimiter("a\tb\tc\n"), '\t');
        assert_eq!(detect_delimiter("single"), ',');
    }

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
        assert_eq!(FileKind::from_path(Path::new("a.heic")), Some(FileKind::Image));
        assert_eq!(FileKind::from_path(Path::new("a.PNG")), Some(FileKind::Image));
        assert_eq!(FileKind::from_path(Path::new("a.jxl")), Some(FileKind::Image));
        assert_eq!(FileKind::from_path(Path::new("a.exe")), None);
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
