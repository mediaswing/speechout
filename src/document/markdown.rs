//! Markdown files.

use super::DocumentReading;
use super::narration::{BOLD, ITALIC, Marks, NarratedParagraph, Narration, STRIKE, list_item_words};

/// Reads Markdown as its text, leaving out the marks that format it. With
/// `DocumentReading::Format`, its formatting is said in the same words as a
/// DOCX or ODT file's. Raw HTML, front matter and footnotes are left out.
pub(super) fn markdown_to_text(markdown: &str, reading: DocumentReading) -> String {
    use pulldown_cmark::{Event as Md, Options, Parser, Tag, TagEnd};
    let format = reading == DocumentReading::Format;
    let options = Options::ENABLE_TABLES
        | Options::ENABLE_STRIKETHROUGH
        | Options::ENABLE_TASKLISTS
        | Options::ENABLE_FOOTNOTES
        | Options::ENABLE_YAML_STYLE_METADATA_BLOCKS
        | Options::ENABLE_PLUSES_DELIMITED_METADATA_BLOCKS;
    let mut spoken = Narration::default();
    // Markdown text is never inside more than one paragraph at a time, so
    // a paragraph is said as soon as the next block starts.
    let mut paragraph: Option<NarratedParagraph> = None;
    // Words for the start of the next paragraph, such as "List item."
    let mut intro: Vec<String> = Vec::new();
    // The formatting turned on by each emphasis being read, innermost last.
    let mut marks: Vec<u8> = Vec::new();
    let mut tables: Vec<usize> = Vec::new();
    let mut skipped = 0usize;
    let mut links = 0usize;
    let mut lists = 0usize;
    let flush = |spoken: &mut Narration, paragraph: &mut Option<NarratedParagraph>| {
        if let Some(p) = paragraph.take() {
            spoken.paragraph(p);
        }
    };
    for event in Parser::new_ext(markdown, options) {
        let text = match event {
            Md::Start(Tag::MetadataBlock(_) | Tag::FootnoteDefinition(_) | Tag::HtmlBlock) => {
                skipped += 1;
                continue;
            }
            Md::End(TagEnd::MetadataBlock(_) | TagEnd::FootnoteDefinition | TagEnd::HtmlBlock) => {
                skipped = skipped.saturating_sub(1);
                continue;
            }
            _ if skipped > 0 => continue,
            Md::Start(tag) => {
                match tag {
                    Tag::Emphasis => marks.push(ITALIC),
                    Tag::Strong => marks.push(BOLD),
                    Tag::Strikethrough => marks.push(STRIKE),
                    Tag::Link { .. } => links += 1,
                    Tag::Paragraph | Tag::TableCell => flush(&mut spoken, &mut paragraph),
                    Tag::Heading { level, .. } => {
                        flush(&mut spoken, &mut paragraph);
                        intro.push(format!("Heading level {}.", level as usize));
                    }
                    Tag::List(_) => {
                        flush(&mut spoken, &mut paragraph);
                        lists += 1;
                    }
                    Tag::Item => {
                        flush(&mut spoken, &mut paragraph);
                        intro.push(list_item_words(lists));
                    }
                    Tag::Table(_) => {
                        flush(&mut spoken, &mut paragraph);
                        tables.push(0);
                        if format {
                            spoken.line("Table.");
                        }
                    }
                    Tag::TableHead | Tag::TableRow => {
                        flush(&mut spoken, &mut paragraph);
                        if let Some(rows) = tables.last_mut() {
                            *rows += 1;
                            if format {
                                spoken.line(&format!("Row {rows}."));
                            }
                        }
                    }
                    Tag::CodeBlock(_) => {
                        flush(&mut spoken, &mut paragraph);
                        intro.push("Code.".to_owned());
                    }
                    Tag::BlockQuote(_) => {
                        flush(&mut spoken, &mut paragraph);
                        intro.push("Quote.".to_owned());
                    }
                    _ => {}
                }
                continue;
            }
            Md::End(tag) => {
                match tag {
                    TagEnd::Emphasis | TagEnd::Strong | TagEnd::Strikethrough => {
                        marks.pop();
                    }
                    TagEnd::Link => links = links.saturating_sub(1),
                    TagEnd::List(_) => {
                        flush(&mut spoken, &mut paragraph);
                        lists = lists.saturating_sub(1);
                    }
                    TagEnd::Table => {
                        flush(&mut spoken, &mut paragraph);
                        if tables.pop().is_some() && format {
                            spoken.line("End of table.");
                        }
                    }
                    TagEnd::Paragraph
                    | TagEnd::Heading(_)
                    | TagEnd::Item
                    | TagEnd::TableCell
                    | TagEnd::CodeBlock
                    | TagEnd::BlockQuote(_) => {
                        flush(&mut spoken, &mut paragraph);
                        // An empty heading or list item says nothing, and
                        // its words don't carry over to what comes next.
                        intro.clear();
                    }
                    _ => {}
                }
                continue;
            }
            Md::Text(t) | Md::Code(t) => t,
            Md::SoftBreak => " ".into(),
            Md::HardBreak => "\n".into(),
            Md::TaskListMarker(done) => {
                intro.push(if done { "Done." } else { "Not done." }.to_owned());
                continue;
            }
            _ => continue,
        };
        let flags = marks.iter().fold(0, |all, m| all | m);
        let m = Marks { flags, colour: None }.in_link(links);
        let p = paragraph.get_or_insert_with(|| {
            let words = std::mem::take(&mut intro);
            let intro = if format && !words.is_empty() { words.join(" ") + " " } else { String::new() };
            NarratedParagraph { intro, ..NarratedParagraph::default() }
        });
        p.push(&text, if format { m } else { Marks::default() });
    }
    flush(&mut spoken, &mut paragraph);
    spoken.out
}

#[cfg(test)]
mod tests {
    use super::*;
    use super::super::tidy;

    const MARKDOWN: &str = "---\ntitle: Menu\n---\n\
        # Fish & chips\n\n\
        This is **very *hot*** and ~~off~~.[^1]\n\
        On a new line.\n\n\
        - One\n  - See [the menu](https://example.org)\n\n\
        - [x] Order\n\n\
        > Quoted `code`\n\n\
        <div>Hidden</div>\n\n\
        ```\nlet x = 1;\n```\n\n\
        | A | B |\n|---|---|\n| 1 | 2 |\n\n\
        ![A plate of chips](chips.jpg)\n\n\
        [^1]: A footnote\n";

    #[test]
    fn markdown_says_its_formatting() {
        let text = tidy(&markdown_to_text(MARKDOWN, DocumentReading::Format));
        assert_eq!(
            text.split("\n\n").collect::<Vec<_>>(),
            [
                "Heading level 1. Fish & chips",
                "This is, bold, very, italic, hot, end bold, end italic, and, crossed out, off. End crossed out, On a new line.",
                "List item. One",
                "List item, level 2. See, link, the menu, end link.",
                "List item. Done. Order",
                "Quote. Quoted code",
                "Code. let x = 1;",
                "Table.",
                "Row 1.",
                "A",
                "B",
                "Row 2.",
                "1",
                "2",
                "End of table.",
                "A plate of chips",
            ]
        );
    }

    #[test]
    fn markdown_content_leaves_formatting_out() {
        let text = tidy(&markdown_to_text(MARKDOWN, DocumentReading::Content));
        assert_eq!(
            text.split("\n\n").collect::<Vec<_>>(),
            [
                "Fish & chips",
                "This is very hot and off. On a new line.",
                "One",
                "See the menu",
                "Order",
                "Quoted code",
                "let x = 1;",
                "A",
                "B",
                "1",
                "2",
                "A plate of chips",
            ]
        );
    }

    #[test]
    fn markdown_empty_heading_says_nothing() {
        let text = tidy(&markdown_to_text("#\n\nText\n", DocumentReading::Format));
        assert_eq!(text, "Text");
    }
}
