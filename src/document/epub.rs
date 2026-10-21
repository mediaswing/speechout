//! EPUB books.

use anyhow::{Context, bail};
use quick_xml::events::Event;
use std::collections::HashMap;
use std::path::Path;
use super::DocumentReading;
use super::narration::{BOLD, ITALIC, Marks, NarratedParagraph, Narration, STRIKE, UNDERLINE, list_item_words, with_stop};
use super::xml::{MAX_XML_BYTES, attr, open_zip, prefixed_attr, read_zip_entry, resolve_part};

/// Reads an EPUB book: each XHTML document in its spine, the order it is
/// meant to be read in. Parts marked as not in the main reading order, such
/// as pop-up notes, are left out, as footnotes are for DOCX.
pub(super) fn read_epub(path: &Path, reading: DocumentReading) -> anyhow::Result<String> {
    let mut archive = open_zip(path, "EPUB")?;
    let mut budget = MAX_XML_BYTES;
    let container = read_zip_entry(&mut archive, "META-INF/container.xml", "EPUB", &mut budget)?;
    let package_name = epub_package_name(&container)?.context("this EPUB file has no list of its contents")?;
    let package = read_zip_entry(&mut archive, &package_name, "EPUB", &mut budget)?;
    let folder = package_name.rsplit_once('/').map_or("", |(folder, _)| folder);
    let parts: Vec<String> =
        epub_spine(&package)?.iter().map(|href| resolve_part(folder, &percent_decode(href))).collect();
    // Fonts are often scrambled in a way listed here, which is no bar to
    // reading. Encrypted text means the book is copy protected.
    if archive.index_for_name("META-INF/encryption.xml").is_some() {
        let encryption = read_zip_entry(&mut archive, "META-INF/encryption.xml", "EPUB", &mut budget)?;
        let encrypted = epub_encrypted_parts(&encryption)?;
        if parts.iter().any(|part| encrypted.contains(part)) {
            bail!("the book is copy protected (DRM), so its text can't be read");
        }
    }
    let mut out = String::new();
    for name in parts {
        if archive.index_for_name(&name).is_none() {
            log::warn!("EPUB part {name} is missing");
            continue;
        }
        let xml = read_zip_entry(&mut archive, &name, "EPUB", &mut budget)?;
        out.push_str(&xhtml_to_text(&xml, reading)?);
        out.push_str("\n\n");
    }
    // A book with nothing to read is reported as such, not read as just its
    // title.
    if out.trim().is_empty() {
        return Ok(out);
    }
    Ok(epub_announcement(&package)? + &out)
}

/// The name in the archive of the package document, which lists the book's
/// parts, from `META-INF/container.xml`.
fn epub_package_name(xml: &str) -> anyhow::Result<Option<String>> {
    let mut reader = quick_xml::Reader::from_str(xml);
    loop {
        match reader.read_event().context("the EPUB book is damaged")? {
            Event::Start(e) | Event::Empty(e) if e.local_name().as_ref() == "rootfile" => {
                if let Some(name) = attr(&e, "full-path") {
                    return Ok(Some(resolve_part("", &percent_decode(&name))));
                }
            }
            Event::Eof => return Ok(None),
            _ => {}
        }
    }
}

/// The links, relative to the package document, of the XHTML documents in
/// its spine, in reading order.
fn epub_spine(xml: &str) -> anyhow::Result<Vec<String>> {
    let mut reader = quick_xml::Reader::from_str(xml);
    let mut manifest = HashMap::new();
    let mut spine = Vec::new();
    loop {
        match reader.read_event().context("the EPUB book is damaged")? {
            Event::Start(e) | Event::Empty(e) => match e.local_name().as_ref() {
                "item" if attr(&e, "media-type").as_deref() == Some("application/xhtml+xml") => {
                    if let (Some(id), Some(href)) = (attr(&e, "id"), attr(&e, "href")) {
                        manifest.insert(id, href);
                    }
                }
                "itemref" if attr(&e, "linear").as_deref() != Some("no") => spine.extend(attr(&e, "idref")),
                _ => {}
            },
            Event::Eof => break,
            _ => {}
        }
    }
    Ok(spine.iter().filter_map(|id| manifest.get(id).cloned()).collect())
}

/// Said before the book, from its package document: its title and authors,
/// such as "The City of the Blind, by Murray Leinster." Like "Heading level
/// 1.", these words go to the voice, so they stay in English. Empty when the
/// book names neither.
fn epub_announcement(xml: &str) -> anyhow::Result<String> {
    let (title, authors) = epub_title_and_authors(xml)?;
    let title = title.trim_end_matches(['.', ',', ';', ':']).trim_end();
    let authors = join_names(&authors);
    let said = match (title.is_empty(), authors.is_empty()) {
        (true, true) => return Ok(String::new()),
        (false, true) => title.to_owned(),
        (true, false) => format!("By {authors}"),
        (false, false) if title.ends_with(['?', '!']) => format!("{title} By {authors}"),
        (false, false) => format!("{title}, by {authors}"),
    };
    Ok(with_stop(&said) + "\n\n")
}

/// The book's first title, and the names of its authors, in the order given
/// in its package document. Other contributors, such as illustrators, are
/// left out; so is anyone without a name.
fn epub_title_and_authors(xml: &str) -> anyhow::Result<(String, Vec<String>)> {
    /// The metadata element being read.
    enum Field {
        Title,
        /// A creator, with its id and the role given on the element itself,
        /// as in EPUB 2.
        Creator(Option<String>, Option<String>),
        /// A role given for the creator with this id, as in EPUB 3.
        Role(String),
    }
    let mut reader = quick_xml::Reader::from_str(xml);
    let mut title: Option<String> = None;
    let mut creators: Vec<(Option<String>, Option<String>, String)> = Vec::new();
    let mut roles: HashMap<String, String> = HashMap::new();
    let mut field: Option<Field> = None;
    let mut text = String::new();
    loop {
        match reader.read_event().context("the EPUB book is damaged")? {
            Event::Start(e) => {
                text.clear();
                field = match e.local_name().as_ref() {
                    "title" => Some(Field::Title),
                    "creator" => Some(Field::Creator(attr(&e, "id"), prefixed_attr(&e, "role"))),
                    "meta" if attr(&e, "property").as_deref() == Some("role") => {
                        attr(&e, "refines").map(|r| Field::Role(r.trim_start_matches('#').to_owned()))
                    }
                    _ => None,
                };
            }
            Event::Text(t) if field.is_some() => text.push_str(&t.xml10_content()),
            Event::GeneralRef(r) if field.is_some() => push_html_ref(&mut text, &r),
            Event::End(_) => {
                let value = collapse_space(&text).trim().to_owned();
                match field.take() {
                    Some(Field::Title) if title.is_none() && !value.is_empty() => title = Some(value),
                    Some(Field::Creator(id, role)) if !value.is_empty() => creators.push((id, role, value)),
                    Some(Field::Role(id)) => {
                        roles.insert(id, value);
                    }
                    _ => {}
                }
            }
            Event::Eof => break,
            _ => {}
        }
    }
    let authors = creators
        .into_iter()
        .filter(|(id, role, _)| {
            let role = role.as_ref().or_else(|| roles.get(id.as_ref()?));
            role.is_none_or(|r| r.eq_ignore_ascii_case("aut"))
        })
        .map(|(.., name)| name)
        .collect();
    Ok((title.unwrap_or_default(), authors))
}

/// Such as "Ann", "Ann and Bob" or "Ann, Bob and Cy".
fn join_names(names: &[String]) -> String {
    match names {
        [] => String::new(),
        [only] => only.clone(),
        [rest @ .., last] => format!("{} and {last}", rest.join(", ")),
    }
}

/// The names in the archive of the parts listed as encrypted in
/// `META-INF/encryption.xml`.
fn epub_encrypted_parts(xml: &str) -> anyhow::Result<Vec<String>> {
    let mut reader = quick_xml::Reader::from_str(xml);
    let mut parts = Vec::new();
    loop {
        match reader.read_event().context("the EPUB book is damaged")? {
            Event::Start(e) | Event::Empty(e) if e.local_name().as_ref() == "CipherReference" => {
                parts.extend(attr(&e, "URI").map(|uri| resolve_part("", &percent_decode(&uri))));
            }
            Event::Eof => break,
            _ => {}
        }
    }
    Ok(parts)
}

/// Decodes the %20 and so on in a link. Anything that isn't valid UTF-8 once
/// decoded is replaced, so the part is reported missing rather than misread.
fn percent_decode(link: &str) -> String {
    let bytes = link.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        let hex = bytes.get(i + 1..i + 3).and_then(|h| u8::from_str_radix(std::str::from_utf8(h).ok()?, 16).ok());
        match hex {
            Some(b) if bytes[i] == b'%' => {
                out.push(b);
                i += 3;
            }
            _ => {
                out.push(bytes[i]);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// XHTML elements whose text is left out: the page's head, scripts, styles
/// and drawings.
const XHTML_SKIPPED: [&str; 5] = ["head", "script", "style", "svg", "template"];

/// Parts of a book, named in `epub:type` or `role`, whose text is left out:
/// footnotes and endnotes and the numbers that refer to them, as for DOCX,
/// and the page numbers of the printed book.
const EPUB_SKIPPED_TYPES: [&str; 12] = [
    "footnote",
    "footnotes",
    "endnote",
    "endnotes",
    "rearnote",
    "rearnotes",
    "noteref",
    "pagebreak",
    "doc-footnote",
    "doc-endnotes",
    "doc-noteref",
    "doc-pagebreak",
];

/// XHTML elements that start a new paragraph.
const XHTML_BLOCKS: [&str; 26] = [
    "address", "article", "aside", "blockquote", "body", "caption", "dd", "div", "dl", "dt", "figcaption", "figure",
    "footer", "header", "hr", "li", "main", "nav", "ol", "p", "pre", "section", "table", "td", "th", "ul",
];

/// Whether the XHTML element `e` is left out, with everything in it.
fn xhtml_skipped(e: &quick_xml::events::BytesStart) -> bool {
    let name = e.local_name();
    XHTML_SKIPPED.contains(&name.as_ref())
        || attr(e, "hidden").is_some()
        || [attr(e, "epub:type"), attr(e, "role")]
            .iter()
            .flatten()
            .any(|types| types.split_whitespace().any(|t| EPUB_SKIPPED_TYPES.contains(&t)))
}

/// Appends the character an entity or character reference stands for, as
/// `push_ref` does, but with HTML's named entities, such as `&nbsp;`, which
/// older EPUB books use. Nothing is read from the document's own DTD.
fn push_html_ref(out: &mut String, r: &quick_xml::events::BytesRef) {
    if let Ok(Some(c)) = r.resolve_char_ref() {
        out.push(c);
    } else if let Some(s) = quick_xml::escape::resolve_html5_entity(r.as_ref()) {
        out.push_str(s);
    }
}

/// Pulls the text out of an XHTML document from an EPUB book, one paragraph
/// per block, such as a paragraph, heading or list item. Read with its
/// format, it says what the markup shows, as a Markdown file's is said:
/// headings, lists, tables, quotations, code, bold, italic, underlined,
/// crossed-out and linked text. An image's description (its alt text) is
/// read as "Image: " and the description, after the paragraph it is in, so
/// it doesn't break up a sentence.
fn xhtml_to_text(xml: &str, reading: DocumentReading) -> anyhow::Result<String> {
    let format = reading == DocumentReading::Format;
    let mut reader = quick_xml::Reader::from_str(xml);
    let mut spoken = Narration::default();
    let mut paragraph: Option<NarratedParagraph> = None;
    // Words for the start of the next paragraph, such as "List item."
    let mut intro: Vec<String> = Vec::new();
    // The formatting turned on by each inline element being read, innermost
    // last, and whether each `a` element being read is a link.
    let mut marks: Vec<u8> = Vec::new();
    let mut anchors: Vec<bool> = Vec::new();
    let mut tables: Vec<usize> = Vec::new();
    // How deep the reader is inside an element that is left out, and how
    // many lists and preformatted blocks it is inside.
    let mut skipped = 0usize;
    let mut lists = 0usize;
    let mut pre = 0usize;
    // Descriptions of images in the paragraph being read, said after it.
    let mut images: Vec<String> = Vec::new();
    let flush = |spoken: &mut Narration, paragraph: &mut Option<NarratedParagraph>, images: &mut Vec<String>| {
        if let Some(p) = paragraph.take() {
            spoken.paragraph(p);
        }
        for alt in images.drain(..) {
            spoken.line(&image_words(&alt));
        }
    };
    loop {
        let text = match reader.read_event().context("the EPUB book is damaged")? {
            Event::Eof => break,
            Event::Start(_) if skipped > 0 => {
                skipped += 1;
                continue;
            }
            Event::End(_) if skipped > 0 => {
                skipped -= 1;
                continue;
            }
            _ if skipped > 0 => continue,
            Event::Start(e) if xhtml_skipped(&e) => {
                skipped = 1;
                continue;
            }
            Event::Start(e) | Event::Empty(e) if e.local_name().as_ref() == "img" => match image_alt(&e) {
                // A picture of a letter starting a chapter, a drop capital,
                // is read as part of the word it starts.
                Some(alt) if alt.chars().count() == 1 && alt.chars().all(char::is_alphabetic) => alt,
                Some(alt) => {
                    if paragraph.is_some() {
                        images.push(alt);
                    } else {
                        spoken.line(&image_words(&alt));
                    }
                    continue;
                }
                None => continue,
            },
            Event::Start(e) => {
                let name = e.local_name();
                let name = name.as_ref();
                if XHTML_BLOCKS.contains(&name) || heading_level(name).is_some() || name == "tr" {
                    flush(&mut spoken, &mut paragraph, &mut images);
                }
                match name {
                    "b" | "strong" => marks.push(BOLD),
                    "i" | "em" | "cite" => marks.push(ITALIC),
                    "u" | "ins" => marks.push(UNDERLINE),
                    "s" | "strike" | "del" => marks.push(STRIKE),
                    "a" => anchors.push(attr(&e, "href").is_some()),
                    "ul" | "ol" => lists += 1,
                    "li" => intro.push(list_item_words(lists)),
                    "blockquote" => intro.push("Quote.".to_owned()),
                    "pre" => {
                        pre += 1;
                        intro.push("Code.".to_owned());
                    }
                    "table" => {
                        tables.push(0);
                        if format {
                            spoken.line("Table.");
                        }
                    }
                    "tr" => {
                        if let Some(rows) = tables.last_mut() {
                            *rows += 1;
                            if format {
                                spoken.line(&format!("Row {rows}."));
                            }
                        }
                    }
                    _ => {
                        if let Some(level) = heading_level(name) {
                            intro.push(format!("Heading level {level}."));
                        }
                    }
                }
                continue;
            }
            Event::End(e) => {
                let name = e.local_name();
                let name = name.as_ref();
                match name {
                    "b" | "strong" | "i" | "em" | "cite" | "u" | "ins" | "s" | "strike" | "del" => {
                        marks.pop();
                    }
                    "a" => {
                        anchors.pop();
                    }
                    "ul" | "ol" => lists = lists.saturating_sub(1),
                    "pre" => pre = pre.saturating_sub(1),
                    "table" => {
                        flush(&mut spoken, &mut paragraph, &mut images);
                        if tables.pop().is_some() && format {
                            spoken.line("End of table.");
                        }
                    }
                    _ => {}
                }
                if XHTML_BLOCKS.contains(&name) || heading_level(name).is_some() {
                    flush(&mut spoken, &mut paragraph, &mut images);
                    // An empty heading or list item says nothing, and its
                    // words don't carry over to what comes next.
                    intro.clear();
                }
                continue;
            }
            Event::Empty(e) => {
                match e.local_name().as_ref() {
                    "br" => {
                        if let Some(p) = paragraph.as_mut() {
                            p.push("\n", Marks::default());
                        }
                    }
                    "hr" => flush(&mut spoken, &mut paragraph, &mut images),
                    _ => {}
                }
                continue;
            }
            Event::Text(t) => t.xml10_content().into_owned(),
            Event::GeneralRef(r) => {
                let mut s = String::new();
                push_html_ref(&mut s, &r);
                s
            }
            _ => continue,
        };
        // Line breaks and indents in the markup are just layout, except in
        // preformatted text. Space between blocks starts no paragraph, so
        // it doesn't use up the next one's words.
        let text = if pre > 0 { text } else { collapse_space(&text) };
        if pre == 0 && text.trim().is_empty() && paragraph.is_none() {
            continue;
        }
        let flags = marks.iter().fold(0, |all, m| all | m);
        let m = Marks { flags, colour: None }.in_link(anchors.iter().filter(|&&link| link).count());
        let p = paragraph.get_or_insert_with(|| {
            let words = std::mem::take(&mut intro);
            let intro = if format && !words.is_empty() { words.join(" ") + " " } else { String::new() };
            NarratedParagraph { intro, ..NarratedParagraph::default() }
        });
        p.push(&text, if format { m } else { Marks::default() });
    }
    flush(&mut spoken, &mut paragraph, &mut images);
    Ok(spoken.out)
}

/// The description of the image `e`, its alt text. Images marked as there
/// only for decoration, with no alt text or as hidden from screen readers,
/// have none.
fn image_alt(e: &quick_xml::events::BytesStart) -> Option<String> {
    let decorative = matches!(attr(e, "role").as_deref(), Some("presentation" | "none"))
        || attr(e, "aria-hidden").as_deref() == Some("true")
        || xhtml_skipped(e);
    let alt = collapse_space(&attr(e, "alt")?).trim().to_owned();
    (!decorative && !alt.is_empty()).then_some(alt)
}

/// Such as "Image: A man with a lantern." Like "Heading level 1.", these
/// words go to the voice, so they stay in English.
fn image_words(alt: &str) -> String {
    with_stop(&format!("Image: {alt}"))
}

/// Turns each run of whitespace into one space, keeping one at either end so
/// words either side of an inline element stay apart.
fn collapse_space(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut space = false;
    for c in text.chars() {
        if c.is_whitespace() {
            space = true;
        } else {
            if space {
                out.push(' ');
            }
            space = false;
            out.push(c);
        }
    }
    if space {
        out.push(' ');
    }
    out
}

/// The level of a heading element, such as 2 for `h2`.
fn heading_level(name: &str) -> Option<usize> {
    match name.as_bytes() {
        [b'h', level @ b'1'..=b'6'] => Some(usize::from(level - b'0')),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use super::super::{DocumentReading, extract_text, tidy};

    const XHTML: &str = r##"<?xml version="1.0" encoding="utf-8"?>
        <!DOCTYPE html>
        <html xmlns="http://www.w3.org/1999/xhtml" xmlns:epub="http://www.idpf.org/2007/ops">
        <head><title>Not read</title><style>p { color: red }</style></head>
        <body>
          <h2>CHAPTER&nbsp;I.</h2>
          <p>Fish &amp; <i>chips</i>
             today<span epub:type="pagebreak" title="2">2</span>—at
             <b>noon</b>.<a epub:type="noteref" href="#n1">1</a></p>
          <ul>
            <li>One</li>
            <li><p>Two</p></li>
          </ul>
          <blockquote><p>Said <a href="https://example.com">here</a>.</p></blockquote>
          <table><tr><td>A</td><td>B</td></tr></table>
          <aside epub:type="footnote" id="n1"><p>A note.</p></aside>
          <p><img src="a.jpg" alt="A picture"/>Last line<br/>and <img src="b.jpg" alt=" A
             map "/>more.</p>
          <p><img src="t.jpg" alt="T"/>he end.<img src="rule.png" alt=""/><img src="x.png" alt="Flourish" role="presentation"/></p>
        </body></html>"##;

    #[test]
    fn xhtml_content_reads_the_text_alone() {
        assert_eq!(
            tidy(&xhtml_to_text(XHTML, DocumentReading::Content).unwrap()),
            "CHAPTER I.\n\nFish & chips today—at noon.\n\nOne\n\nTwo\n\nSaid here.\n\nA\n\nB\n\nImage: A picture.\n\nLast line and more.\n\nImage: A map.\n\nThe end."
        );
    }

    #[test]
    fn xhtml_says_its_formatting() {
        assert_eq!(
            tidy(&xhtml_to_text(XHTML, DocumentReading::Format).unwrap()),
            "Heading level 2. CHAPTER I.\n\n\
             Fish &, italic, chips, end italic, today—at, bold, noon. End bold.\n\n\
             List item. One\n\nList item. Two\n\n\
             Quote. Said, link, here. End link.\n\n\
             Table.\n\nRow 1.\n\nA\n\nB\n\nEnd of table.\n\n\
             Image: A picture.\n\nLast line and more.\n\nImage: A map.\n\nThe end."
        );
    }

    #[test]
    fn image_descriptions_keep_html_entities() {
        let xhtml = r#"<html><body><p><img alt="Caf&eacute; &amp; bar&nbsp;at&#160;night"/></p></body></html>"#;
        assert_eq!(tidy(&xhtml_to_text(xhtml, DocumentReading::Content).unwrap()), "Image: Café & bar at night.");
    }

    #[test]
    fn epub_spine_is_read_in_order() {
        let opf = r#"<package xmlns="http://www.idpf.org/2007/opf"><manifest>
            <item id="c2" href="text/two%20b.xhtml" media-type="application/xhtml+xml"/>
            <item id="c1" href="text/one.xhtml" media-type="application/xhtml+xml"/>
            <item id="notes" href="notes.xhtml" media-type="application/xhtml+xml"/>
            <item id="cover" href="cover.jpg" media-type="image/jpeg"/>
            </manifest><spine><itemref idref="cover"/><itemref idref="c1"/><itemref idref="notes" linear="no"/><itemref idref="c2"/><itemref idref="gone"/></spine></package>"#;
        assert_eq!(epub_spine(opf).unwrap(), ["text/one.xhtml", "text/two%20b.xhtml"]);
        assert_eq!(percent_decode("two%20b%C3%A9.xhtml%2"), "two bé.xhtml%2");
    }

    #[test]
    fn epub_announces_its_title_and_authors() {
        let opf = r##"<package xmlns="http://www.idpf.org/2007/opf" xmlns:dc="dc" xmlns:opf="opf"><metadata>
            <dc:title>The City
               of the Blind.</dc:title>
            <dc:title>A Subtitle</dc:title>
            <dc:creator id="a1">Murray Leinster</dc:creator>
            <meta property="role" refines="#a1">aut</meta>
            <dc:creator id="i1">An Illustrator</dc:creator>
            <meta property="role" refines="#i1">ill</meta>
            <dc:creator opf:role="aut">Ann &amp; Co</dc:creator>
            <dc:creator opf:role="edt">An Editor</dc:creator>
            <dc:creator>Séamus Ó Dubhghaill</dc:creator>
            </metadata></package>"##;
        assert_eq!(
            epub_announcement(opf).unwrap(),
            "The City of the Blind, by Murray Leinster, Ann & Co and Séamus Ó Dubhghaill.\n\n"
        );
        assert_eq!(epub_announcement("<package><metadata><dc:title>Why?</dc:title></metadata></package>").unwrap(), "Why?\n\n");
        assert_eq!(
            epub_announcement("<package><metadata><dc:title>Why?</dc:title><dc:creator>Ann</dc:creator></metadata></package>")
                .unwrap(),
            "Why? By Ann.\n\n"
        );
        assert_eq!(epub_announcement("<package><metadata><dc:creator>Ann</dc:creator></metadata></package>").unwrap(), "By Ann.\n\n");
        assert_eq!(epub_announcement("<package><metadata/></package>").unwrap(), "");
    }

    /// An EPUB file holding `parts`, with a package that lists `chapters`.
    fn epub_file(chapters: &[&str], parts: &[(&str, &str)]) -> tempfile::NamedTempFile {
        use std::io::Write;
        let file = tempfile::Builder::new().suffix(".epub").tempfile().unwrap();
        let mut zip = zip::ZipWriter::new(file.reopen().unwrap());
        let items: String = chapters
            .iter()
            .enumerate()
            .map(|(i, href)| format!(r#"<item id="c{i}" href="{href}" media-type="application/xhtml+xml"/>"#))
            .collect();
        let spine: String = (0..chapters.len()).map(|i| format!(r#"<itemref idref="c{i}"/>"#)).collect();
        let opf = format!("<package><metadata><dc:title>Book</dc:title></metadata><manifest>{items}</manifest><spine>{spine}</spine></package>");
        let container = r#"<container><rootfiles><rootfile full-path="OEBPS/content.opf"/></rootfiles></container>"#;
        for (name, xml) in [("META-INF/container.xml", container), ("OEBPS/content.opf", &opf)].iter().chain(parts) {
            zip.start_file(*name, zip::write::SimpleFileOptions::default()).unwrap();
            zip.write_all(xml.as_bytes()).unwrap();
        }
        zip.finish().unwrap();
        file
    }

    #[test]
    fn epub_reads_each_chapter() {
        let file = epub_file(
            &["text/one.xhtml", "missing.xhtml", "../OEBPS/text/two%20b.xhtml"],
            &[
                ("OEBPS/text/one.xhtml", "<html><body><h1>One</h1><p>First.</p></body></html>"),
                ("OEBPS/text/two b.xhtml", "<html><body><p>Second.</p></body></html>"),
                // Scrambled fonts don't stop the book being read.
                (
                    "META-INF/encryption.xml",
                    r#"<encryption><EncryptedData><CipherData><CipherReference URI="OEBPS/font.otf"/></CipherData></EncryptedData></encryption>"#,
                ),
            ],
        );
        assert_eq!(extract_text(file.path(), DocumentReading::Content).unwrap(), "Book.\n\nOne\n\nFirst.\n\nSecond.");
        assert_eq!(
            extract_text(file.path(), DocumentReading::Format).unwrap(),
            "Book.\n\nHeading level 1. One\n\nFirst.\n\nSecond."
        );
    }

    #[test]
    fn copy_protected_epub_says_so() {
        let file = epub_file(
            &["one.xhtml"],
            &[
                ("OEBPS/one.xhtml", "scrambled"),
                (
                    "META-INF/encryption.xml",
                    r#"<encryption><EncryptedData><CipherData><CipherReference URI="OEBPS/one.xhtml"/></CipherData></EncryptedData></encryption>"#,
                ),
            ],
        );
        let error = extract_text(file.path(), DocumentReading::Content).unwrap_err();
        assert!(error.to_string().contains("copy protected"), "{error}");
    }

    #[test]
    fn epub_with_nothing_to_read_says_so() {
        let file = epub_file(&["one.xhtml"], &[("OEBPS/one.xhtml", r#"<html><body><img src="a.jpg" alt=""/></body></html>"#)]);
        let error = extract_text(file.path(), DocumentReading::Content).unwrap_err();
        assert!(error.to_string().contains("no readable text"), "{error}");
    }
}
