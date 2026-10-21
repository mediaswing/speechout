//! PowerPoint 97-2003 presentations (PPT).

use anyhow::{Context, bail};
use std::collections::HashMap;
use std::io::Read;
use std::path::Path;
use super::pptx::{read_pptx, speak_slides};
use super::text::decode_utf16;

/// Limit on the text read from a PPT file. Its slides can refer to the same
/// drawing or text over and over, so a small file could otherwise expand to
/// more text than fits in memory.
const MAX_PPT_TEXT_BYTES: usize = 100 * 1024 * 1024;

/// Record types in a PowerPoint 97–2003 `PowerPoint Document` stream.
mod record {
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
pub(super) fn is_compound_file(path: &Path) -> anyhow::Result<bool> {
    let mut magic = [0u8; 8];
    let mut file = std::fs::File::open(path)?;
    Ok(file.read_exact(&mut magic).is_ok() && magic == [0xD0, 0xCF, 0x11, 0xE0, 0xA1, 0xB1, 0x1A, 0xE1])
}

pub(super) fn read_ppt(path: &Path) -> anyhow::Result<String> {
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
            record::PERSIST_DIRECTORY_ATOM => {
                let words: Vec<u32> = r.data.as_chunks::<4>().0.iter().map(|w| u32::from_le_bytes(*w)).collect();
                let mut words = words.into_iter();
                while let Some(header) = words.next() {
                    let (first, count) = (header & 0xF_FFFF, header >> 20);
                    for (i, offset) in (0..count).zip(words.by_ref()) {
                        persist.insert(first + i, offset as usize);
                    }
                }
            }
            record::DOCUMENT if r.version == record::CONTAINER_VERSION => document = Some(r.data),
            _ => {}
        }
    }

    // Each slide's persist id and placeholder text, in the order shown.
    let mut slides: Vec<(u32, Vec<String>)> = Vec::new();
    let slide_list = document.into_iter().flat_map(ppt_records).find(|(_, r)| r.kind == record::SLIDE_LIST_WITH_TEXT && r.instance == 0);
    if let Some((_, list)) = slide_list {
        for (_, r) in ppt_records(list.data) {
            match r.kind {
                record::SLIDE_PERSIST_ATOM => {
                    let id = r.data.first_chunk::<4>().map_or(0, |w| u32::from_le_bytes(*w));
                    slides.push((id, Vec::new()));
                }
                // References to placeholder text count text headers, since
                // a placeholder with no text has a header and no text atom.
                record::TEXT_HEADER_ATOM => {
                    if let Some((_, texts)) = slides.last_mut() {
                        texts.push(String::new());
                    }
                }
                record::TEXT_CHARS_ATOM | record::TEXT_BYTES_ATOM => {
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
                .filter(|(_, r)| r.kind == record::SLIDE && r.version == record::CONTAINER_VERSION);
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
    if depth > record::MAX_DEPTH {
        return Ok(());
    }
    for (_, r) in ppt_records(data) {
        match r.kind {
            record::TEXT_CHARS_ATOM | record::TEXT_BYTES_ATOM => {
                // A slide number is stored as a "*", which stands for the
                // number; a box holding only that repeats on every slide.
                let text = ppt_text(&r);
                if text.trim() != "*" {
                    push_ppt_text(out, &text, budget)?;
                }
            }
            record::OUTLINE_TEXT_REF_ATOM => {
                let index = r.data.first_chunk::<4>().map_or(usize::MAX, |w| u32::from_le_bytes(*w) as usize);
                if let Some(text) = placeholders.get(index) {
                    used[index] = true;
                    push_ppt_text(out, text, budget)?;
                }
            }
            _ if r.version == record::CONTAINER_VERSION => {
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
    let text = if r.kind == record::TEXT_CHARS_ATOM {
        decode_utf16(r.data, u16::from_le_bytes)
    } else {
        // Each byte is the low byte of a UTF-16 character, so this is Latin-1.
        r.data.iter().map(|&b| char::from(b)).collect()
    };
    text.replace('\r', "\n\n").replace('\u{b}', "\n")
}

#[cfg(test)]
mod tests {
    use super::*;
    use super::super::pptx::speak_slides;
    use super::super::tidy;

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
        let textbox = ppt_record(0xF, 0xF00D, &ppt_record(0, record::TEXT_CHARS_ATOM, &utf16("Note box")));
        let slide_number = ppt_record(0xF, 0xF00D, &ppt_record(0, record::TEXT_BYTES_ATOM, b"*"));
        let drawing = [
            ppt_record(0, record::OUTLINE_TEXT_REF_ATOM, &0u32.to_le_bytes()),
            textbox,
            slide_number,
            ppt_record(0, record::OUTLINE_TEXT_REF_ATOM, &1u32.to_le_bytes()),
            ppt_record(0, record::OUTLINE_TEXT_REF_ATOM, &2u32.to_le_bytes()),
            ppt_record(0, record::OUTLINE_TEXT_REF_ATOM, &99u32.to_le_bytes()),
        ]
        .concat();
        let slide = ppt_record(0xF, record::SLIDE, &ppt_record(0xF, 0xF002, &drawing));
        // Slide 2 has no drawing in the persist directory, so its
        // placeholder text is read on its own.
        let list = [
            ppt_record(0, record::SLIDE_PERSIST_ATOM, &[1, 0, 0, 0, 0, 0, 0, 0]),
            ppt_record(0, record::TEXT_HEADER_ATOM, &[0; 4]),
            ppt_record(0, record::TEXT_BYTES_ATOM, b"Title"),
            // An empty placeholder: a header with no text after it.
            ppt_record(0, record::TEXT_HEADER_ATOM, &[1, 0, 0, 0]),
            ppt_record(0, record::TEXT_HEADER_ATOM, &[1, 0, 0, 0]),
            ppt_record(0, record::TEXT_CHARS_ATOM, &utf16("Point one\rPoint\u{b}two")),
            ppt_record(0, record::SLIDE_PERSIST_ATOM, &[7, 0, 0, 0]),
            ppt_record(0, record::TEXT_HEADER_ATOM, &[0; 4]),
            ppt_record(0, record::TEXT_BYTES_ATOM, b"Caf\xe9"),
        ]
        .concat();
        let masters = ppt_record(0x1F, record::SLIDE_LIST_WITH_TEXT, &ppt_record(0, record::TEXT_BYTES_ATOM, b"Master"));
        let document = ppt_record(0xF, record::DOCUMENT, &[masters, ppt_record(0xF, record::SLIDE_LIST_WITH_TEXT, &list)].concat());
        let directory = ppt_record(0, record::PERSIST_DIRECTORY_ATOM, &[(1u32 | 1 << 20).to_le_bytes(), 0u32.to_le_bytes()].concat());
        let stream = [slide, document, directory].concat();
        assert_eq!(
            tidy(&speak_slides(ppt_stream_to_text(&stream, MAX_PPT_TEXT_BYTES).unwrap())),
            "Slide 1.\n\nTitle\n\nNote box\n\nPoint one\n\nPoint two\n\nSlide 2.\n\nCaf\u{e9}"
        );
    }

    #[test]
    fn ppt_survives_truncated_and_deeply_nested_records() {
        assert!(ppt_stream_to_text(&[0x0F, 0, 0xE8, 0x03, 0xFF, 0xFF, 0xFF, 0x7F, 1], 100).unwrap().is_empty());
        let mut nested = ppt_record(0, record::TEXT_BYTES_ATOM, b"deep");
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
        let refs: Vec<u8> = (0..100).flat_map(|_| ppt_record(0, record::OUTLINE_TEXT_REF_ATOM, &[0; 4])).collect();
        let drawing = [ppt_record(0, record::TEXT_BYTES_ATOM, &[b'a'; 1000]), refs].concat();
        let slide = ppt_record(0xF, record::SLIDE, &drawing);
        let mut list = Vec::new();
        for _ in 0..100 {
            list.extend(ppt_record(0, record::SLIDE_PERSIST_ATOM, &1u32.to_le_bytes()));
            list.extend(ppt_record(0, record::TEXT_HEADER_ATOM, &[0; 4]));
            list.extend(ppt_record(0, record::TEXT_BYTES_ATOM, &[b'b'; 1000]));
        }
        let document = ppt_record(0xF, record::DOCUMENT, &ppt_record(0xF, record::SLIDE_LIST_WITH_TEXT, &list));
        let directory = ppt_record(0, record::PERSIST_DIRECTORY_ATOM, &[(1u32 | 1 << 20).to_le_bytes(), 0u32.to_le_bytes()].concat());
        let stream = [slide, document, directory].concat();
        // About 10 MB of text from about 120 KB of file.
        let error = ppt_stream_to_text(&stream, 1024 * 1024).unwrap_err();
        assert!(error.to_string().contains("more text than can be read"), "{error}");
        let slides = ppt_stream_to_text(&stream, 20 * 1024 * 1024).unwrap();
        assert_eq!(slides.len(), 100);
    }
}
