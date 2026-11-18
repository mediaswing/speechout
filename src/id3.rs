//! ID3 tags on the MP3s the app saves: the artist is always "Common Speech
//! Output" and the title comes from the file's name. The artist also marks a
//! file as the app's own, so the Audio Player knows it holds speech made from
//! text, which doesn't need writing down again.

use std::io::Read;
use std::path::Path;

/// The artist on every MP3 the app saves.
pub const ARTIST: &str = "Common Speech Output";

/// A file's title, from its name: `my_talk-notes.mp3` is "My Talk Notes".
/// Underscores and hyphens become spaces, and each word starts with a capital,
/// with the rest of it left as typed, so "BBC" stays "BBC".
pub fn title_for(path: &Path) -> String {
    let stem = path.file_stem().map(|s| s.to_string_lossy()).unwrap_or_default();
    stem.replace(['_', '-'], " ").split_whitespace().map(capitalise).collect::<Vec<_>>().join(" ")
}

/// Capitalises the first letter or number of a word, if it is a letter, so
/// "(part" becomes "(Part" but "2nd" stays as it is.
fn capitalise(word: &str) -> String {
    match word.char_indices().find(|(_, c)| c.is_alphanumeric()) {
        Some((at, c)) if c.is_alphabetic() => {
            format!("{}{}{}", &word[..at], c.to_uppercase(), &word[at + c.len_utf8()..])
        }
        _ => word.to_owned(),
    }
}

/// An ID3v2.3 tag, the version most players read, giving the artist and
/// `title`. It goes at the very start of the MP3.
pub fn tag(title: &str) -> Vec<u8> {
    let mut frames = Vec::new();
    for (id, text) in [(b"TPE1", ARTIST), (b"TIT2", title)] {
        // UTF-16 with a byte order mark, which ID3v2.3 needs for anything
        // beyond Latin-1.
        let mut body = vec![1u8, 0xFF, 0xFE];
        body.extend(text.encode_utf16().flat_map(u16::to_le_bytes));
        frames.extend_from_slice(id);
        frames.extend_from_slice(&(body.len() as u32).to_be_bytes());
        frames.extend_from_slice(&[0, 0]);
        frames.extend(body);
    }
    let mut tag = b"ID3\x03\x00\x00".to_vec();
    tag.extend(syncsafe(frames.len() as u32));
    tag.extend(frames);
    tag
}

fn syncsafe(n: u32) -> [u8; 4] {
    [(n >> 21) as u8 & 0x7F, (n >> 14) as u8 & 0x7F, (n >> 7) as u8 & 0x7F, n as u8 & 0x7F]
}

fn unsyncsafe(b: &[u8]) -> usize {
    b.iter().fold(0, |n, &b| (n << 7) | usize::from(b & 0x7F))
}

/// The most of a tag that is read. Ours is tiny, but another program may have
/// added cover art before the artist.
const MAX_TAG: usize = 16 << 20;

/// Whether the file at `path` was saved by this app, going by the artist in
/// its ID3 tag. Anything unreadable is taken as not.
pub fn made_here(path: &Path) -> bool {
    let Ok(mut file) = std::fs::File::open(path) else { return false };
    let mut header = [0u8; 10];
    if file.read_exact(&mut header).is_err() || &header[..3] != b"ID3" {
        return false;
    }
    let size = unsyncsafe(&header[6..10]);
    if size > MAX_TAG {
        return false;
    }
    let mut body = vec![0u8; size];
    if file.read_exact(&mut body).is_err() {
        return false;
    }
    artists(header[3], header[5], body).iter().any(|a| a == ARTIST)
}

/// The artists named in the body of an ID3v2.3 or v2.4 tag.
fn artists(version: u8, flags: u8, mut body: Vec<u8>) -> Vec<String> {
    if !matches!(version, 3 | 4) {
        return Vec::new();
    }
    // Unsynchronisation puts a 0 after each 0xFF byte, taken out here. In v2.4
    // it is marked on each frame instead, and our artist never needs it.
    if version == 3 && flags & 0x80 != 0 {
        let mut kept = Vec::with_capacity(body.len());
        for (i, &b) in body.iter().enumerate() {
            if !(b == 0 && i > 0 && body[i - 1] == 0xFF) {
                kept.push(b);
            }
        }
        body = kept;
    }
    let mut at = 0;
    if flags & 0x40 != 0 && body.len() >= 4 {
        // An extended header: in v2.3 its size leaves out its own four bytes.
        at = if version == 3 { 4 + u32::from_be_bytes(body[..4].try_into().unwrap()) as usize } else { unsyncsafe(&body[..4]) };
    }
    let mut found = Vec::new();
    while at + 10 <= body.len() && body[at] != 0 {
        let id = &body[at..at + 4];
        let size = if version == 4 {
            unsyncsafe(&body[at + 4..at + 8])
        } else {
            u32::from_be_bytes(body[at + 4..at + 8].try_into().unwrap()) as usize
        };
        let format_flags = body[at + 9];
        let start = at + 10;
        let Some(end) = start.checked_add(size).filter(|&e| e <= body.len()) else { break };
        // Compressed, encrypted or (in v2.4) separately unsynchronised frames
        // aren't worth undoing for this.
        let plain = if version == 4 { format_flags & 0x0F == 0 } else { format_flags & 0xC0 == 0 };
        if id == b"TPE1" && plain {
            found.extend(text(&body[start..end]));
        }
        at = end;
    }
    found
}

/// The values of a text frame. v2.4 can hold several, split by nulls.
fn text(frame: &[u8]) -> Vec<String> {
    let Some((&encoding, bytes)) = frame.split_first() else { return Vec::new() };
    let joined = match encoding {
        0 => bytes.iter().map(|&b| char::from(b)).collect(),
        1 | 2 => {
            let mut units: Vec<u16> = bytes.as_chunks::<2>().0.iter().map(|&c| u16::from_be_bytes(c)).collect();
            let little = encoding == 1 && units.first() == Some(&0xFFFE);
            if little {
                units.iter_mut().for_each(|u| *u = u.swap_bytes());
            }
            String::from_utf16_lossy(&units)
        }
        3 => String::from_utf8_lossy(bytes).into_owned(),
        _ => return Vec::new(),
    };
    joined
        .split('\0')
        .map(|s| s.trim_start_matches('\u{FEFF}').trim().to_owned())
        .filter(|s| !s.is_empty())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn titles_come_from_the_file_name() {
        let title = |name: &str| title_for(Path::new(name));
        assert_eq!(title("/somewhere/my_talk-notes.mp3"), "My Talk Notes");
        assert_eq!(title("BBC news__today.MP3"), "BBC News Today");
        assert_eq!(title("speech (part 2 of 3).mp3"), "Speech (Part 2 Of 3)");
        assert_eq!(title("2nd-draft.mp3"), "2nd Draft");
        assert_eq!(title("élan_vital.mp3"), "Élan Vital");
        assert_eq!(title("-_-.mp3"), "");
    }

    fn file_with(bytes: &[u8]) -> (tempfile::TempDir, std::path::PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("a.mp3");
        std::fs::write(&path, bytes).unwrap();
        (dir, path)
    }

    #[test]
    fn our_tag_is_recognised() {
        let mut bytes = tag("Ünïcode Title");
        bytes.extend_from_slice(b"audio would follow");
        let (_dir, path) = file_with(&bytes);
        assert!(made_here(&path));
        assert!(!made_here(&path.with_file_name("missing.mp3")));
    }

    #[test]
    fn other_files_are_not_ours() {
        let (_d, untagged) = file_with(b"\xFF\xFB\x90\x00 an MP3 frame");
        assert!(!made_here(&untagged));
        let (_d, cut_short) = file_with(&tag("Title")[..20]);
        assert!(!made_here(&cut_short));

        // Someone else's v2.4 tag, in UTF-8.
        let v24 = |artist: &str| {
            let mut frame = b"TPE1".to_vec();
            frame.extend(syncsafe(artist.len() as u32 + 1));
            frame.extend_from_slice(&[0, 0, 3]);
            frame.extend_from_slice(artist.as_bytes());
            let mut bytes = b"ID3\x04\x00\x00".to_vec();
            bytes.extend(syncsafe(frame.len() as u32 + 20));
            bytes.extend(frame);
            bytes.extend([0; 20]);
            bytes
        };
        let (_d, theirs) = file_with(&v24("A Band"));
        assert!(!made_here(&theirs));
        let (_d, retagged) = file_with(&v24("Someone\0Common Speech Output"));
        assert!(made_here(&retagged), "another program kept our artist, in v2.4");
    }
}
