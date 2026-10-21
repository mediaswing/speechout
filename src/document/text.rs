//! Plain text files, in UTF-8, UTF-16 or Windows-1252.


/// Decodes UTF-8 or UTF-16 (with byte order mark), falling back to Windows-1252
/// for legacy files.
pub(super) fn read_txt(bytes: &[u8]) -> String {
    match bytes {
        [0xEF, 0xBB, 0xBF, rest @ ..] => String::from_utf8_lossy(rest).into_owned(),
        [0xFF, 0xFE, rest @ ..] => decode_utf16(rest, u16::from_le_bytes),
        [0xFE, 0xFF, rest @ ..] => decode_utf16(rest, u16::from_be_bytes),
        _ => match std::str::from_utf8(bytes) {
            Ok(s) => s.to_owned(),
            Err(_) => bytes.iter().map(|&b| windows_1252(b)).collect(),
        },
    }
}

/// Windows-1252 matches Latin-1 except for 0x80 to 0x9F, where Latin-1 has
/// invisible control characters and Windows-1252 has curly quotes, dashes,
/// the euro sign and so on. The five bytes it leaves unassigned stay control
/// characters, which `tidy` drops.
fn windows_1252(b: u8) -> char {
    const HIGH: [char; 32] = [
        '€', '\u{81}', '‚', 'ƒ', '„', '…', '†', '‡', 'ˆ', '‰', 'Š', '‹', 'Œ', '\u{8D}', 'Ž', '\u{8F}',
        '\u{90}', '‘', '’', '“', '”', '•', '–', '—', '˜', '™', 'š', '›', 'œ', '\u{9D}', 'ž', 'Ÿ',
    ];
    match b {
        0x80..=0x9F => HIGH[usize::from(b - 0x80)],
        _ => char::from(b),
    }
}

pub(super) fn decode_utf16(bytes: &[u8], f: fn([u8; 2]) -> u16) -> String {
    let units: Vec<u16> = bytes.as_chunks::<2>().0.iter().map(|c| f([c[0], c[1]])).collect();
    String::from_utf16_lossy(&units)
}

#[cfg(test)]
mod tests {
    use super::*;
    use super::super::tidy;

    #[test]
    fn txt_encodings() {
        assert_eq!(read_txt(&[0xEF, 0xBB, 0xBF, b'h', b'i']), "hi");
        assert_eq!(read_txt(&[0xFF, 0xFE, b'h', 0, b'i', 0]), "hi");
        assert_eq!(read_txt(&[b'c', 0xE9]), "c\u{e9}");
    }

    #[test]
    fn txt_windows_1252_punctuation_survives() {
        // don’t “hi” £5–£10 €3… as saved by older Windows programs.
        let bytes = b"don\x92t \x93hi\x94 \xA35\x96\xA310 \x803\x85";
        assert_eq!(tidy(&read_txt(bytes)), "don’t “hi” £5–£10 €3…");
        // Bytes Windows-1252 leaves unassigned are dropped, not kept as junk.
        assert_eq!(tidy(&read_txt(b"a\x81b\x9Dc\xE9")), "abcé");
    }

    #[test]
    fn windows_line_endings_are_single_line_breaks() {
        let wrapped = "The quick brown fox\r\njumps over the dog.\r\n\r\nNext paragraph.\r\n";
        assert_eq!(tidy(wrapped), "The quick brown fox jumps over the dog.\n\nNext paragraph.");
        // Old Mac line endings and Unix ones read the same.
        assert_eq!(tidy("a\rb\r\rc"), "a b\n\nc");
        assert_eq!(tidy("a\nb\n\nc"), "a b\n\nc");
    }
}
