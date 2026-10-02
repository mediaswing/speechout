//! XML wordlists that change how words are spoken: pronunciation fixes, or
//! substitutions that keep spoken output classroom-safe.
//!
//! ```xml
//! <wordlist name="UK place names" description="..." language="en-GB">
//!   <entry match="Leicester" replace="Lester"/>
//!   <entry match="SQL" replace="sequel" case-sensitive="true"/>
//!   <entry match="C#" replace="C sharp"/>
//! </wordlist>
//! ```
//!
//! For compatibility with earlier wordlists, `find` is accepted in place of
//! `match`, `match-case` in place of `case-sensitive`, and the description may
//! be a `<description>` child element instead of an attribute.

use anyhow::{Context, bail};
use quick_xml::events::{BytesStart, Event};
use regex::{Regex, RegexBuilder};
use std::collections::{BTreeSet, HashMap};
use std::path::{Path, PathBuf};

const MAX_WORDLIST_BYTES: u64 = 5 * 1024 * 1024;
const MAX_ENTRIES: usize = 50_000;
const MAX_FIELD_CHARS: usize = 500;

/// Example wordlists shipped inside the binary and installed on first run.
pub const BUNDLED: &[(&str, &str)] = &[
    ("uk-place-names.xml", include_str!("../wordlists/uk-place-names.xml")),
    ("technology-terms.xml", include_str!("../wordlists/technology-terms.xml")),
    ("classroom-safe.xml", include_str!("../wordlists/classroom-safe.xml")),
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

/// An installed wordlist, as shown on the Wordlists tab.
#[derive(Clone, Debug)]
pub struct Installed {
    pub file_name: String,
    pub list: Result<Wordlist, String>,
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

/// The first of several alternative attribute names that is present.
fn attr_any(e: &BytesStart, names: &[&str]) -> anyhow::Result<Option<String>> {
    for name in names {
        if let Some(value) = attr(e, name)? {
            return Ok(Some(value));
        }
    }
    Ok(None)
}

fn flag(e: &BytesStart, names: &[&str], default: bool) -> anyhow::Result<bool> {
    Ok(match attr_any(e, names)?.as_deref().map(str::trim) {
        None => default,
        Some("true" | "yes" | "1") => true,
        Some("false" | "no" | "0") => false,
        Some(other) => bail!("\"{other}\" is not true or false"),
    })
}

fn clean(value: String, what: &str) -> anyhow::Result<String> {
    let value: String = value.chars().filter(|c| !c.is_control()).collect();
    if value.chars().count() > MAX_FIELD_CHARS {
        bail!("{what} is longer than {MAX_FIELD_CHARS} characters");
    }
    Ok(value.trim().to_owned())
}

/// Parses a wordlist. Custom entities and DTDs are not expanded.
pub fn parse(xml: &str) -> anyhow::Result<Wordlist> {
    let mut reader = quick_xml::Reader::from_str(xml);
    let mut list: Option<Wordlist> = None;
    let mut in_description = false;
    loop {
        let event = reader
            .read_event()
            .with_context(|| format!("XML error near byte {}", reader.buffer_position()))?;
        match &event {
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
                    let line = list.entries.len() + 1;
                    let pattern = clean(
                        attr_any(e, &["match", "find"])?
                            .with_context(|| format!("entry {line} has no match attribute"))?,
                        "a match",
                    )?;
                    if pattern.is_empty() {
                        bail!("entry {line} has an empty match attribute");
                    }
                    let replacement = clean(attr(e, "replace")?.unwrap_or_default(), "a replacement")?;
                    list.entries.push(Entry {
                        pattern,
                        replacement,
                        case_sensitive: flag(e, &["case-sensitive", "match-case"], false)?,
                        whole_word: flag(e, &["whole-word"], true)?,
                    });
                    if list.entries.len() > MAX_ENTRIES {
                        bail!("a wordlist can have at most {MAX_ENTRIES} entries");
                    }
                }
                "description" if matches!(event, Event::Start(_)) => in_description = true,
                _ => {}
            },
            Event::End(e) if e.local_name().as_ref() == "description" => in_description = false,
            Event::Text(t) if in_description => {
                if let Some(list) = list.as_mut() {
                    let text = format!("{} {}", list.description, t.xml10_content());
                    list.description = clean(text, "the description")?;
                }
            }
            Event::Eof => break,
            _ => {}
        }
    }
    list.context("this file has no wordlist element, so it is not a wordlist")
}

fn read_file(path: &Path) -> anyhow::Result<Wordlist> {
    let size = std::fs::metadata(path)?.len();
    if size > MAX_WORDLIST_BYTES {
        bail!("the file is larger than 5 MB");
    }
    let bytes = std::fs::read(path)?;
    let text = std::str::from_utf8(bytes.strip_prefix(b"\xEF\xBB\xBF").unwrap_or(&bytes))
        .context("wordlists must be saved as UTF-8")?;
    parse(text)
}

/// Writes the bundled examples into `dir`, skipping any that already exist.
pub fn install_examples(dir: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(dir)?;
    for (name, contents) in BUNDLED {
        let path = dir.join(name);
        if !path.exists() {
            std::fs::write(path, contents)?;
        }
    }
    Ok(())
}

pub fn list_installed(dir: &Path) -> Vec<Installed> {
    let Ok(read) = std::fs::read_dir(dir) else { return Vec::new() };
    let mut out: Vec<Installed> = read
        .filter_map(Result::ok)
        .filter(|e| e.path().extension().is_some_and(|x| x.eq_ignore_ascii_case("xml")))
        .filter(|e| e.file_type().is_ok_and(|t| t.is_file()))
        .map(|e| Installed {
            file_name: e.file_name().to_string_lossy().into_owned(),
            list: read_file(&e.path()).map_err(|err| format!("{err:#}")),
        })
        .collect();
    out.sort_by_key(|a| a.file_name.to_lowercase());
    out
}

/// Validates a wordlist and copies it into `dir`. Returns the wordlist's name.
pub fn import(source: &Path, dir: &Path) -> anyhow::Result<String> {
    let list = read_file(source)?;
    std::fs::create_dir_all(dir)?;
    let stem: String = source
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("wordlist")
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() || c == '-' || c == '_' { c } else { '-' })
        .take(60)
        .collect();
    let stem = if stem.trim_matches('-').is_empty() { "wordlist".to_owned() } else { stem };
    let mut dest = dir.join(format!("{stem}.xml"));
    let mut n = 2;
    while dest.exists() {
        dest = dir.join(format!("{stem}-{n}.xml"));
        n += 1;
    }
    std::fs::copy(source, &dest).context("could not copy the wordlist")?;
    Ok(list.name)
}

/// Deletes an installed wordlist. `file_name` must be a plain file name.
pub fn remove(dir: &Path, file_name: &str) -> anyhow::Result<()> {
    let path = PathBuf::from(file_name);
    if path.components().count() != 1 || path.file_name().is_none_or(|f| f != file_name) {
        bail!("invalid wordlist name");
    }
    std::fs::remove_file(dir.join(path)).context("could not remove the wordlist")
}

/// All enabled wordlists compiled into a single pass over the text.
#[derive(Default)]
pub struct Substitutions {
    regex: Option<Regex>,
    exact: HashMap<String, String>,
    folded: HashMap<String, String>,
}

impl Substitutions {
    pub fn new(installed: &[Installed], disabled: &BTreeSet<String>) -> Self {
        let mut entries: Vec<&Entry> = installed
            .iter()
            .filter(|i| !disabled.contains(&i.file_name))
            .filter_map(|i| i.list.as_ref().ok())
            .flat_map(|l| &l.entries)
            .collect();
        if entries.is_empty() {
            return Self::default();
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
            .size_limit(256 * 1024 * 1024)
            .build();
        match regex {
            Ok(regex) => Self { regex: Some(regex), exact, folded },
            Err(e) => {
                log::warn!("could not compile wordlists: {e}");
                Self::default()
            }
        }
    }

    #[cfg(test)]
    pub fn is_empty(&self) -> bool {
        self.regex.is_none()
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

/// "Damn" -> "Darn" when the replacement is written in lower case.
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
        let installed = vec![Installed { file_name: "a.xml".into(), list: Ok(parse(xml).unwrap()) }];
        Substitutions::new(&installed, &BTreeSet::new())
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
    fn capitalisation_follows_original() {
        let s = subs(r#"<wordlist name="t"><entry match="damn" replace="darn"/></wordlist>"#);
        assert_eq!(s.apply("Damn, damn."), "Darn, darn.");
    }

    #[test]
    fn disabled_lists_are_ignored() {
        let installed = vec![Installed {
            file_name: "a.xml".into(),
            list: Ok(parse(r#"<wordlist name="t"><entry match="a" replace="b"/></wordlist>"#).unwrap()),
        }];
        let disabled = BTreeSet::from(["a.xml".to_owned()]);
        assert!(Substitutions::new(&installed, &disabled).is_empty());
    }

    #[test]
    fn accepts_earlier_format() {
        let list = parse(
            r#"<wordlist name="t"><description>Old style</description><entry find="Dr." replace="Doctor" match-case="true" whole-word="false"/></wordlist>"#,
        )
        .unwrap();
        assert_eq!(list.description, "Old style");
        assert_eq!(
            list.entries[0],
            Entry { pattern: "Dr.".into(), replacement: "Doctor".into(), case_sensitive: true, whole_word: false }
        );
    }

    #[test]
    fn rejects_bad_files() {
        assert!(parse("<notawordlist/>").is_err());
        assert!(parse(r#"<wordlist name="t"><entry replace="x"/></wordlist>"#).is_err());
        assert!(parse(r#"<wordlist><entry match="x"/></wordlist>"#).is_err());
    }

    #[test]
    fn bundled_examples_parse() {
        for (name, xml) in BUNDLED {
            let list = parse(xml).unwrap_or_else(|e| panic!("{name}: {e}"));
            assert!(!list.entries.is_empty(), "{name}");
        }
    }

    #[test]
    fn remove_rejects_paths() {
        let dir = tempfile::tempdir().unwrap();
        assert!(remove(dir.path(), "../x.xml").is_err());
        assert!(remove(dir.path(), "/etc/passwd").is_err());
    }
}
