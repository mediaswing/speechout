//! OpenDocument text documents (ODT) and spreadsheets (ODS).

use anyhow::Context;
use quick_xml::events::Event;
use std::collections::HashMap;
use std::path::Path;
use super::DocumentReading;
use super::csv::speak_table;
use super::narration::{BOLD, ITALIC, MAX_STYLE_DEPTH, MarkChange, Marks, NarratedParagraph, Narration, STRIKE, UNDERLINE, colour_name, list_item_words};
use super::xml::{MAX_XML_BYTES, attr, open_zip, push_ref, read_zip_entry, read_zip_xml};

/// Limit on the cells an ODS file may expand to. Spreadsheets mark runs of
/// identical cells and rows with a repeat count rather than writing them out,
/// so a small file could otherwise ask for billions of cells.
const MAX_ODS_CELLS: usize = 2_000_000;
pub(super) fn read_odt(path: &Path, reading: DocumentReading) -> anyhow::Result<String> {
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
    let mut spoken = Narration::default();
    let mut paragraphs: Vec<NarratedParagraph> = Vec::new();
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
        let current = |paragraphs: &[NarratedParagraph], spans: &[Marks]| {
            spans.last().copied().or(paragraphs.last().map(|p| p.marks)).unwrap_or_default()
        };
        match event {
            Event::Start(ref e) => match e.local_name().as_ref() {
                name if ODF_SKIPPED.contains(&name) => skipped += 1,
                _ if skipped > 0 => {}
                name @ ("p" | "h") => {
                    let style = attr(e, "text:style-name");
                    let marks = styles.marks(true, style.as_deref(), Marks::default());
                    let mut p = NarratedParagraph { marks, ..NarratedParagraph::default() };
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

/// Reads each sheet of an ODS `content.xml` as a spoken table, in the same
/// way as a CSV file. When there is more than one sheet with something in it,
/// each table is introduced with its sheet's name.
pub(super) fn ods_xml_to_text(xml: &str) -> anyhow::Result<String> {
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

pub(super) fn row_is_empty(row: &[String]) -> bool {
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

#[cfg(test)]
mod tests {
    use super::*;
    use super::super::tidy;

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
}
