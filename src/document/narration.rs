//! Formatting said in words, as when a DOCX, ODT, Markdown or EPUB file is read
//! with its format: "bold, very, end bold", "Heading level 1." and so on.


// Formatting, as said when a DOCX, ODT or Markdown file is read with its
// format. Like "This is part 1 of 3", these words go to the voice, so they
// stay in English.

pub(super) const BOLD: u8 = 1;
pub(super) const ITALIC: u8 = 2;
pub(super) const UNDERLINE: u8 = 4;
pub(super) const STRIKE: u8 = 8;
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
pub(super) fn colour_name(hex: &str) -> Option<&'static str> {
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
pub(super) const MAX_STYLE_DEPTH: usize = 20;

/// The formatting of a piece of text.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(super) struct Marks {
    pub(super) flags: u8,
    pub(super) colour: Option<&'static str>,
}

impl Marks {
    /// Inside a link, its underlining and colour say nothing more.
    pub(super) fn in_link(mut self, links: usize) -> Self {
        if links > 0 {
            self.flags = (self.flags | LINK) & !UNDERLINE;
            self.colour = None;
        }
        self
    }

    /// Without the formatting in `unsaid`.
    pub(super) fn without(mut self, unsaid: Marks) -> Self {
        self.flags &= !unsaid.flags;
        if self.colour == unsaid.colour {
            self.colour = None;
        }
        self
    }
}

/// Formatting a style turns on and off, leaving the rest as it was.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(super) struct MarkChange {
    pub(super) on: u8,
    pub(super) off: u8,
    /// `Some(None)` sets the colour back to the usual one.
    pub(super) colour: Option<Option<&'static str>>,
}

impl MarkChange {
    pub(super) fn set(&mut self, mark: u8, on: bool) {
        if on {
            self.on |= mark;
            self.off &= !mark;
        } else {
            self.off |= mark;
            self.on &= !mark;
        }
    }

    pub(super) fn apply(self, marks: Marks) -> Marks {
        Marks { flags: (marks.flags | self.on) & !self.off, colour: self.colour.unwrap_or(marks.colour) }
    }
}

/// One paragraph, gathered before it is said, since what it is (a heading,
/// say) is only known once its properties have been read.
#[derive(Default)]
pub(super) struct NarratedParagraph {
    /// Said first, such as "Heading level 1."
    pub(super) intro: String,
    /// Formatting that comes with the kind of paragraph, such as a heading's
    /// bold, which would only get in the way if it were said.
    pub(super) unsaid: Marks,
    /// Formatting of the whole paragraph, from its style.
    pub(super) marks: Marks,
    pub(super) runs: Vec<(String, Marks)>,
}

impl NarratedParagraph {
    pub(super) fn push(&mut self, text: &str, marks: Marks) {
        match self.runs.last_mut() {
            Some((last, m)) if *m == marks => last.push_str(text),
            _ => self.runs.push((text.to_owned(), marks)),
        }
    }
}

/// The text being put together, with formatting said in words.
#[derive(Default)]
pub(super) struct Narration {
    pub(super) out: String,
}

impl Narration {
    /// A line of its own, such as "Table."
    pub(super) fn line(&mut self, text: &str) {
        self.out.push_str(text);
        self.out.push_str("\n\n");
    }

    pub(super) fn paragraph(&mut self, p: NarratedParagraph) {
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
pub(super) fn list_item_words(level: usize) -> String {
    if level > 1 { format!("List item, level {level}.") } else { "List item.".to_owned() }
}

/// Ends `text` with a full stop unless it already ends a sentence.
pub(super) fn with_stop(text: &str) -> String {
    if text.ends_with(['.', '!', '?']) { text.to_owned() } else { format!("{text}.") }
}

#[cfg(test)]
mod tests {
    use super::*;

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
}
