//! XML wordlists that change how words are spoken: pronunciation fixes, or
//! substitutions that keep spoken output classroom-safe.
//!
//! ```xml
//! <wordlist name="UK place names" description="...">
//!   <entry match="Leicester" replace="Lester"/>
//!   <entry match="SQL" replace="sequel" case-sensitive="true"/>
//!   <entry match="C#" replace="C sharp"/>
//!   <entry match="Dr." replace="Doctor" whole-word="false"/>
//! </wordlist>
//! ```
//!
//! Wordlists come from other people, so they are treated as untrusted: their
//! size is limited, DTDs are refused (which rules out entity expansion
//! attacks), matches are literal text rather than regular expressions, and
//! the text shown from them has anything that could disguise it removed.

use anyhow::{Context, bail};
use quick_xml::events::{BytesStart, Event};
use regex::{Regex, RegexBuilder};
use std::collections::HashMap;
use std::path::Path;

const MAX_WORDLIST_BYTES: u64 = 5 * 1024 * 1024;
const MAX_ENTRIES: usize = 50_000;
const MAX_FIELD_CHARS: usize = 500;

/// The examples added the first time the app runs, and whether each starts
/// turned on. The classroom list starts off, because it changes the meaning of
/// what is read, which nobody should get without asking for it.
pub const EXAMPLES: &[(&str, bool)] = &[
    (include_str!("../wordlists/uk-place-names.xml"), true),
    (include_str!("../wordlists/technology-terms.xml"), true),
    (include_str!("../wordlists/classroom-safe.xml"), false),
];

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Entry {
    pub pattern: String,
    pub replacement: String,
    pub case_sensitive: bool,
    pub whole_word: bool,
}

#[derive(Clone, Debug)]
pub struct Wordlist {
    pub name: String,
    pub description: String,
    pub entries: Vec<Entry>,
}

/// Removes control characters and the invisible characters that reorder text,
/// which could make a name read differently from what it is.
pub fn clean_display_text(text: &str) -> String {
    text.chars()
        .filter(|c| !c.is_control() && !matches!(c, '\u{200E}'..='\u{200F}' | '\u{202A}'..='\u{202E}' | '\u{2066}'..='\u{2069}'))
        .collect()
}

fn attr(e: &BytesStart, name: &str) -> anyhow::Result<Option<String>> {
    for a in e.attributes() {
        let a = a.context("malformed attribute")?;
        if a.key.local_name().as_ref() == name {
            let value = a.normalized_value(quick_xml::XmlVersion::Implicit1_0).context("malformed attribute value")?;
            return Ok(Some(value.into_owned()));
        }
    }
    Ok(None)
}

fn flag(e: &BytesStart, name: &str, default: bool) -> anyhow::Result<bool> {
    Ok(match attr(e, name)?.as_deref().map(str::trim) {
        None => default,
        Some("true" | "yes" | "1") => true,
        Some("false" | "no" | "0") => false,
        Some(other) => bail!("\"{other}\" is not true or false"),
    })
}

fn clean(value: String, what: &str) -> anyhow::Result<String> {
    let value = clean_display_text(&value);
    if value.chars().count() > MAX_FIELD_CHARS {
        bail!("{what} is longer than {MAX_FIELD_CHARS} characters");
    }
    Ok(value.trim().to_owned())
}

/// Parses a wordlist.
pub fn parse(xml: &str) -> anyhow::Result<Wordlist> {
    let mut reader = quick_xml::Reader::from_str(xml);
    let mut list: Option<Wordlist> = None;
    loop {
        let event = reader
            .read_event()
            .with_context(|| format!("XML error near byte {}", reader.buffer_position()))?;
        match &event {
            Event::DocType(_) => bail!("wordlists can't contain a DOCTYPE"),
            Event::Start(e) | Event::Empty(e) => match e.local_name().as_ref() {
                "wordlist" => {
                    if list.is_some() {
                        bail!("a file can contain only one wordlist element");
                    }
                    let name = clean(attr(e, "name")?.unwrap_or_default(), "the name")?;
                    if name.is_empty() {
                        bail!("the wordlist element needs a name attribute");
                    }
                    let description = clean(attr(e, "description")?.unwrap_or_default(), "the description")?;
                    list = Some(Wordlist { name, description, entries: Vec::new() });
                }
                "entry" => {
                    let list = list.as_mut().context("entry found outside a wordlist element")?;
                    let number = list.entries.len() + 1;
                    let pattern = clean(
                        attr(e, "match")?.with_context(|| format!("entry {number} has no match attribute"))?,
                        "a match",
                    )?;
                    if pattern.is_empty() {
                        bail!("entry {number} has an empty match attribute");
                    }
                    let replacement = clean(attr(e, "replace")?.unwrap_or_default(), "a replacement")?;
                    list.entries.push(Entry {
                        pattern,
                        replacement,
                        case_sensitive: flag(e, "case-sensitive", false)?,
                        whole_word: flag(e, "whole-word", true)?,
                    });
                    if list.entries.len() > MAX_ENTRIES {
                        bail!("a wordlist can have at most {MAX_ENTRIES} entries");
                    }
                }
                _ => {}
            },
            Event::Eof => break,
            _ => {}
        }
    }
    list.context("this file has no wordlist element, so it is not a wordlist")
}

/// Reads a wordlist file's text, ready for `Store::import_wordlist`.
pub fn read_file(path: &Path) -> anyhow::Result<String> {
    let size = std::fs::metadata(path)?.len();
    if size > MAX_WORDLIST_BYTES {
        bail!("the file is larger than 5 MB");
    }
    let bytes = std::fs::read(path)?;
    let text = std::str::from_utf8(bytes.strip_prefix(b"\xEF\xBB\xBF").unwrap_or(&bytes))
        .context("wordlists must be saved as UTF-8")?;
    Ok(text.to_owned())
}

/// All enabled wordlists compiled into a single pass over the text, so one
/// replacement is never itself replaced.
#[derive(Default)]
pub struct Substitutions {
    regex: Option<Regex>,
    exact: HashMap<String, String>,
    folded: HashMap<String, String>,
}

impl Substitutions {
    /// Fails rather than leaving anything out: someone relying on a wordlist to
    /// keep a reading suitable for a class must not get the original words.
    pub fn new(lists: &[Wordlist]) -> anyhow::Result<Self> {
        Self::with_size_limit(lists, 256 * 1024 * 1024)
    }

    fn with_size_limit(lists: &[Wordlist], size_limit: usize) -> anyhow::Result<Self> {
        let mut entries: Vec<&Entry> = lists.iter().flat_map(|l| &l.entries).collect();
        if entries.is_empty() {
            return Ok(Self::default());
        }
        // Longest first, so "New York City" wins over "New York".
        entries.sort_by_key(|e| std::cmp::Reverse(e.pattern.chars().count()));

        let mut exact = HashMap::new();
        let mut folded = HashMap::new();
        let mut alternatives = Vec::with_capacity(entries.len());
        for e in entries {
            let escaped = regex::escape(&e.pattern);
            let starts_word = e.pattern.chars().next().is_some_and(is_word_char);
            let ends_word = e.pattern.chars().last().is_some_and(is_word_char);
            let (pre, post) = if e.whole_word {
                (if starts_word { r"\b" } else { "" }, if ends_word { r"\b" } else { "" })
            } else {
                ("", "")
            };
            let group = if e.case_sensitive { "(?:" } else { "(?i:" };
            alternatives.push(format!("{group}{pre}{escaped}{post})"));
            if e.case_sensitive {
                exact.entry(e.pattern.clone()).or_insert_with(|| e.replacement.clone());
            } else {
                folded.entry(e.pattern.to_lowercase()).or_insert_with(|| e.replacement.clone());
            }
        }
        let regex = RegexBuilder::new(&alternatives.join("|"))
            .size_limit(size_limit)
            .build()
            .context("the wordlists that are turned on are too large to use together. Turn some off on the Wordlist tab")?;
        Ok(Self { regex: Some(regex), exact, folded })
    }

    pub fn apply(&self, text: &str) -> String {
        let Some(regex) = &self.regex else { return text.to_owned() };
        regex
            .replace_all(text, |caps: &regex::Captures| {
                let found = &caps[0];
                if let Some(r) = self.exact.get(found) {
                    return r.clone();
                }
                match self.folded.get(&found.to_lowercase()) {
                    Some(r) => match_capital(found, r),
                    None => found.to_owned(),
                }
            })
            .into_owned()
    }
}

fn is_word_char(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

/// "Damn" becomes "Darn" when the replacement is written in lower case.
fn match_capital(found: &str, replacement: &str) -> String {
    let found_cap = found.chars().next().is_some_and(char::is_uppercase);
    let mut chars = replacement.chars();
    match chars.next() {
        Some(first) if found_cap && first.is_lowercase() => first.to_uppercase().chain(chars).collect(),
        _ => replacement.to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn subs(xml: &str) -> Substitutions {
        Substitutions::new(&[parse(xml).unwrap()]).unwrap()
    }

    #[test]
    fn replaces_whole_words_case_insensitively() {
        let s = subs(r#"<wordlist name="t"><entry match="Leicester" replace="Lester"/><entry match="sql" replace="sequel" case-sensitive="true"/></wordlist>"#);
        assert_eq!(s.apply("leicester, Leicestershire and LEICESTER."), "Lester, Leicestershire and Lester.");
        assert_eq!(s.apply("sql SQL"), "sequel SQL");
    }

    #[test]
    fn handles_symbols_and_longest_match() {
        let s = subs(r#"<wordlist name="t"><entry match="C#" replace="C sharp"/><entry match="New York" replace="NY"/><entry match="New York City" replace="NYC"/></wordlist>"#);
        assert_eq!(s.apply("I like C# in New York City."), "I like C sharp in NYC.");
    }

    #[test]
    fn patterns_are_literal_text() {
        let s = subs(r#"<wordlist name="t"><entry match="(a+)+$" replace="x"/><entry match="." replace="dot" whole-word="false"/></wordlist>"#);
        assert_eq!(s.apply("aaaa"), "aaaa");
        assert_eq!(s.apply("a.b"), "adotb");
    }

    #[test]
    fn replacements_are_not_replaced_again() {
        let s = subs(r#"<wordlist name="t"><entry match="a" replace="b"/><entry match="b" replace="c"/></wordlist>"#);
        assert_eq!(s.apply("a b"), "b c");
    }

    #[test]
    fn capitalisation_follows_original() {
        let s = subs(r#"<wordlist name="t"><entry match="damn" replace="darn"/></wordlist>"#);
        assert_eq!(s.apply("Damn, damn."), "Darn, darn.");
    }

    #[test]
    fn classroom_list_leaves_innocent_words_alone() {
        let s = Substitutions::new(&[parse(EXAMPLES[2].0).unwrap()]).unwrap();
        let innocent = "Scunthorpe class assess Dickens shell hello grass Essex";
        assert_eq!(s.apply(innocent), innocent);
        assert_ne!(s.apply("What the hell"), "What the hell");
    }

    #[test]
    fn a_list_that_cannot_be_used_is_an_error_not_ignored() {
        let list = parse(EXAMPLES[2].0).unwrap();
        assert!(Substitutions::with_size_limit(&[list], 10).is_err());
    }

    #[test]
    fn rejects_bad_files() {
        assert!(parse("<notawordlist/>").is_err());
        assert!(parse(r#"<wordlist name="t"><entry replace="x"/></wordlist>"#).is_err());
        assert!(parse(r#"<wordlist><entry match="x"/></wordlist>"#).is_err());
        assert!(parse(r#"<wordlist name="t"><entry match="" replace="x"/></wordlist>"#).is_err());
        assert!(parse(r#"<entry match="x"/>"#).is_err());
    }

    #[test]
    fn refuses_entity_expansion_attacks() {
        let bomb = r#"<?xml version="1.0"?><!DOCTYPE lolz [<!ENTITY lol "lol">]><wordlist name="&lol;"></wordlist>"#;
        assert!(parse(bomb).unwrap_err().to_string().contains("DOCTYPE"));
    }

    #[test]
    fn names_cannot_be_disguised() {
        let list = parse("<wordlist name=\"safe&#x202E;txt.exe&#10;\"><entry match=\"a\"/></wordlist>").unwrap();
        assert_eq!(list.name, "safetxt.exe");
    }

    #[test]
    fn limits_field_length_and_entries() {
        let long = "x".repeat(MAX_FIELD_CHARS + 1);
        assert!(parse(&format!(r#"<wordlist name="{long}"/>"#)).is_err());
        let many = r#"<entry match="a"/>"#.repeat(MAX_ENTRIES + 1);
        assert!(parse(&format!(r#"<wordlist name="t">{many}</wordlist>"#)).is_err());
    }

    #[test]
    fn examples_parse() {
        for (xml, _) in EXAMPLES {
            assert!(!parse(xml).unwrap().entries.is_empty());
        }
    }
}
