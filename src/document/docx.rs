//! Word documents (DOCX).

use anyhow::Context;
use quick_xml::events::Event;
use std::collections::HashMap;
use std::path::Path;
use super::DocumentReading;
use super::narration::{BOLD, ITALIC, MAX_STYLE_DEPTH, MarkChange, Marks, NarratedParagraph, Narration, STRIKE, UNDERLINE, colour_name, list_item_words};
use super::xml::{MAX_XML_BYTES, attr, open_zip, push_ref, read_zip_entry, read_zip_xml};

pub(super) fn read_docx(path: &Path, reading: DocumentReading) -> anyhow::Result<String> {
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
    spoken: NarratedParagraph,
}

/// Pulls the text out of WordprocessingML as `docx_xml_to_text` does, and
/// says what its formatting shows: headings, list items, tables, and bold,
/// italic, underlined, crossed-out, coloured and linked text.
fn docx_xml_to_spoken(xml: &str, styles: &HashMap<String, DocxStyle>) -> anyhow::Result<String> {
    let mut reader = quick_xml::Reader::from_str(xml);
    let mut spoken = Narration::default();
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

#[cfg(test)]
mod tests {
    use super::*;
    use super::super::tidy;

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
}
