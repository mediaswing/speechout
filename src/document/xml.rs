//! Reading the XML parts out of zipped documents (DOCX, ODT, ODS, PPTX and EPUB),
//! with a limit against zip bombs, and small helpers for reading XML.

use anyhow::{Context, bail};
use std::io::Read;
use std::path::Path;

/// Limit on the uncompressed size of a DOCX, ODT or ODS body, or of all the
/// slides of a PPTX file or chapters of an EPUB book together, which guards
/// against zip bombs.
pub(super) const MAX_XML_BYTES: u64 = 100 * 1024 * 1024;
/// Reads the XML body `name` out of a zipped office document. `format` names
/// the file type in error messages.
pub(super) fn read_zip_xml(path: &Path, name: &str, format: &str) -> anyhow::Result<String> {
    read_zip_entry(&mut open_zip(path, format)?, name, format, &mut { MAX_XML_BYTES })
}

pub(super) fn open_zip(path: &Path, format: &str) -> anyhow::Result<zip::ZipArchive<std::fs::File>> {
    let file = std::fs::File::open(path)?;
    zip::ZipArchive::new(file).with_context(|| format!("this is not a valid {format} file"))
}

/// Reads the XML part `name` out of an open archive, taking its size off
/// `budget`, the uncompressed bytes still allowed for this document.
pub(super) fn read_zip_entry(
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
pub(super) fn attr(e: &quick_xml::events::BytesStart, name: &str) -> Option<String> {
    let a = e.try_get_attribute(name).ok()??;
    Some(a.normalized_value(quick_xml::XmlVersion::Implicit1_0).ok()?.into_owned())
}

/// Appends the character an entity or character reference stands for. Only
/// the five predefined XML entities are expanded, so there is no XXE risk.
pub(super) fn push_ref(out: &mut String, r: &quick_xml::events::BytesRef) {
    push_entity(out, r, quick_xml::escape::resolve_xml_entity);
}

/// As `push_ref`, but with HTML's named entities too, such as `&nbsp;`,
/// which older EPUB books use.
pub(super) fn push_html_ref(out: &mut String, r: &quick_xml::events::BytesRef) {
    push_entity(out, r, quick_xml::escape::resolve_html5_entity);
}

/// Appends what a character reference, or an entity named in `entities`,
/// stands for. Nothing is read from a document's own DTD, and an entity
/// `entities` doesn't name is left out.
fn push_entity(out: &mut String, r: &quick_xml::events::BytesRef, entities: fn(&str) -> Option<&'static str>) {
    if let Ok(Some(c)) = r.resolve_char_ref() {
        out.push(c);
    } else if let Some(s) = entities(r.as_ref()) {
        out.push_str(s);
    }
}

/// The value of the attribute with a prefix, such as `r:id`, whose local
/// name is `local`. Relationship ids are found this way, since the prefix is
/// up to the program that wrote the file.
pub(super) fn prefixed_attr(e: &quick_xml::events::BytesStart, local: &str) -> Option<String> {
    let a = e.attributes().flatten().find(|a| a.key.local_name().as_ref() == local && a.key.prefix().is_some())?;
    Some(a.normalized_value(quick_xml::XmlVersion::Implicit1_0).ok()?.into_owned())
}

/// Resolves a relationship target against the folder `base` of the part
/// that refers to it, giving a name in the archive.
pub(super) fn resolve_part(base: &str, target: &str) -> String {
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

#[cfg(test)]
mod tests {
    use super::*;
    use quick_xml::events::Event;

    /// The text of `xml`, with entities expanded by `push`.
    fn expanded(xml: &str, push: fn(&mut String, &quick_xml::events::BytesRef)) -> String {
        let mut reader = quick_xml::Reader::from_str(xml);
        let mut out = String::new();
        loop {
            match reader.read_event().unwrap() {
                Event::Text(t) => out.push_str(&t.xml10_content()),
                Event::GeneralRef(r) => push(&mut out, &r),
                Event::Eof => return out,
                _ => {}
            }
        }
    }

    #[test]
    fn office_documents_expand_xml_entities_and_epub_html_ones_too() {
        let xml = "<t>Fish &amp; chips&nbsp;&#233;t&#xE9;</t>";
        assert_eq!(expanded(xml, push_ref), "Fish & chipsété");
        assert_eq!(expanded(xml, push_html_ref), "Fish & chips\u{a0}été");
    }

    #[test]
    fn pptx_part_names() {
        assert_eq!(resolve_part("ppt", "slides/slide1.xml"), "ppt/slides/slide1.xml");
        assert_eq!(resolve_part("ppt", "/ppt/slides/slide1.xml"), "ppt/slides/slide1.xml");
        assert_eq!(resolve_part("ppt/slides", "../media/a.png"), "ppt/media/a.png");
    }
}
