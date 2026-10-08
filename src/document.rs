//! Text extraction from PDF, TXT, DOCX, ODT, CSV and ODS files.

use anyhow::{Context, bail};
use quick_xml::events::Event;
use std::io::Read;
use std::path::Path;

/// Files larger than this are refused, to keep memory use predictable.
const MAX_FILE_BYTES: u64 = 200 * 1024 * 1024;
/// Limit on the uncompressed size of a DOCX, ODT or ODS body, which guards
/// against zip bombs.
const MAX_XML_BYTES: u64 = 100 * 1024 * 1024;
/// Limit on the cells an ODS file may expand to. Spreadsheets mark runs of
/// identical cells and rows with a repeat count rather than writing them out,
/// so a small file could otherwise ask for billions of cells.
const MAX_ODS_CELLS: usize = 2_000_000;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FileKind {
    Pdf,
    Text,
    Docx,
    Odt,
    Csv,
    Ods,
    Image,
}

impl FileKind {
    pub fn from_path(path: &Path) -> Option<Self> {
        let ext = path.extension()?.to_str()?.to_ascii_lowercase();
        match ext.as_str() {
            "pdf" => Some(FileKind::Pdf),
            "txt" | "text" => Some(FileKind::Text),
            "docx" => Some(FileKind::Docx),
            "odt" => Some(FileKind::Odt),
            "csv" => Some(FileKind::Csv),
            "ods" => Some(FileKind::Ods),
            "jpg" | "jpeg" | "heic" | "heif" => Some(FileKind::Image),
            _ => None,
        }
    }
}

pub fn extract_text(path: &Path) -> anyhow::Result<String> {
    let size = std::fs::metadata(path).context("the file could not be opened")?.len();
    if size > MAX_FILE_BYTES {
        bail!("the file is too large (the limit is 200 MB)");
    }
    let text = match FileKind::from_path(path) {
        Some(FileKind::Text) => read_txt(&std::fs::read(path)?),
        Some(FileKind::Pdf) => read_pdf(path)?,
        Some(FileKind::Docx) => read_docx(path)?,
        Some(FileKind::Odt) => odt_xml_to_text(&read_zip_xml(path, "content.xml", "ODT")?)?,
        Some(FileKind::Csv) => read_csv(&std::fs::read(path)?),
        Some(FileKind::Ods) => ods_xml_to_text(&read_zip_xml(path, "content.xml", "ODS")?)?,
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

fn read_docx(path: &Path) -> anyhow::Result<String> {
    docx_xml_to_text(&read_zip_xml(path, "word/document.xml", "DOCX")?)
}

/// Reads the XML body `name` out of a zipped office document. `format` names
/// the file type in error messages.
fn read_zip_xml(path: &Path, name: &str, format: &str) -> anyhow::Result<String> {
    let file = std::fs::File::open(path)?;
    let mut archive = zip::ZipArchive::new(file).with_context(|| format!("this is not a valid {format} file"))?;
    let entry = archive.by_name(name).with_context(|| format!("this {format} file has no document body"))?;
    let mut xml = String::new();
    entry
        .take(MAX_XML_BYTES + 1)
        .read_to_string(&mut xml)
        .with_context(|| format!("the {format} document body is not valid text"))?;
    if xml.len() as u64 > MAX_XML_BYTES {
        bail!("the {format} document is too large");
    }
    Ok(xml)
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
    fn attr(e: &quick_xml::events::BytesStart, name: &str) -> Option<String> {
        let a = e.try_get_attribute(name).ok()??;
        Some(a.normalized_value(quick_xml::XmlVersion::Implicit1_0).ok()?.into_owned())
    }
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
        assert_eq!(FileKind::from_path(Path::new("a.Csv")), Some(FileKind::Csv));
        assert_eq!(FileKind::from_path(Path::new("a.ODT")), Some(FileKind::Odt));
        assert_eq!(FileKind::from_path(Path::new("a.ods")), Some(FileKind::Ods));
        assert_eq!(FileKind::from_path(Path::new("a.heic")), Some(FileKind::Image));
        assert_eq!(FileKind::from_path(Path::new("a.exe")), None);
    }
}
