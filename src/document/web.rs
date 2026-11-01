//! Web pages, for the Web Reader. The article is found with Readability, the
//! method behind Firefox's Reader View, which leaves out menus, adverts,
//! related stories and footers. What it can't tell apart from the article,
//! such as image credits, is tidied away afterwards.

use super::{is_hidden_format, tidy};
use anyhow::bail;
use dom_smoothie::{Config, Readability, TextMode};
use encoding_rs::{Encoding, UTF_8, UTF_16BE, UTF_16LE};
use regex::Regex;
use std::sync::LazyLock;

/// A page with fewer words than this has no article to read: it is a "please
/// enable JavaScript" or "access denied" page, or a page of links.
const MIN_WORDS: usize = 50;
/// Pages with more elements than this are refused, to keep memory use and
/// the time taken predictable.
const MAX_ELEMENTS: usize = 200_000;

/// Social media posts embedded in an article. Their text comes with links and
/// dates, and they usually repeat what the article quotes.
const EMBEDDED_POSTS: &str = "blockquote.twitter-tweet, blockquote.instagram-media, blockquote.tiktok-embed, \
    blockquote.bluesky-embed, blockquote.threads-embed, blockquote.reddit-embed-bq, .fb-post";

/// An image credit run into a caption, such as "Dubai(Image: We Love TV)".
static IMAGE_CREDIT: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\s*\((Image|Photo|Picture): [^)]*\)").unwrap());

/// The article on a web page.
pub struct WebArticle {
    /// The page's headline, or the site's name if it has none.
    pub title: String,
    /// The headline, then the article.
    pub text: String,
}

/// Takes the article out of a downloaded page. `url` is where the page came
/// from, which links in it are relative to, and `charset` the character set
/// the site said it is in, if it did.
pub fn read_web_page(html: &[u8], url: &str, charset: Option<&str>) -> anyhow::Result<WebArticle> {
    let html = decode(html, charset);
    let config = Config { text_mode: TextMode::Formatted, max_elements_to_parse: MAX_ELEMENTS, ..Default::default() };
    let mut readability = Readability::new(html.as_str(), Some(url), Some(config))?;
    readability.doc.select(EMBEDDED_POSTS).remove();
    let Ok(article) = readability.parse() else {
        bail!("no article was found on the page");
    };
    let title = tidy(&visible(&article.title));
    let body = clean(&article.text_content);
    if body.split_whitespace().count() < MIN_WORDS {
        bail!("no article was found on the page. The site may need a web browser to show it");
    }
    let text = if title.is_empty() || body.starts_with(&title) { body } else { format!("{title}\n\n{body}") };
    Ok(WebArticle { title, text })
}

/// The page's text. A byte order mark settles it. Otherwise almost every page
/// is UTF-8, and sites often name the wrong character set, so a page that is
/// valid UTF-8 is read as UTF-8. Any other page is read in the character set
/// it names in its headers or a `<meta>` tag near its start, with the names
/// web browsers know, such as Shift_JIS or Windows-1251. A page that names
/// none is read as UTF-8, where one bad byte spoils only itself.
fn decode(html: &[u8], charset: Option<&str>) -> String {
    if let Some((encoding, bom)) = Encoding::for_bom(html) {
        return encoding.decode_without_bom_handling(&html[bom..]).0.into_owned();
    }
    let from_headers = charset.and_then(|c| Encoding::for_label(c.trim().as_bytes()));
    // UTF-16 text in English is also valid UTF-8, so the headers' word comes
    // first. A `<meta>` tag can't name UTF-16, as it couldn't be found in a
    // UTF-16 page; browsers take it to mean UTF-8.
    if let Some(utf16) = from_headers.filter(|e| *e == UTF_16LE || *e == UTF_16BE) {
        return utf16.decode_without_bom_handling(html).0.into_owned();
    }
    if let Ok(text) = std::str::from_utf8(html) {
        return text.to_owned();
    }
    let declared = from_headers.or_else(|| meta_charset(html).and_then(|c| Encoding::for_label(c.as_bytes())));
    match declared {
        Some(encoding) if encoding != UTF_8 && encoding != UTF_16LE && encoding != UTF_16BE => {
            encoding.decode_without_bom_handling(html).0.into_owned()
        }
        _ => String::from_utf8_lossy(html).into_owned(),
    }
}

/// The character set named in the first part of a page, as in
/// `<meta charset="utf-8">` or `content="text/html; charset=utf-8"`.
fn meta_charset(html: &[u8]) -> Option<String> {
    let start = String::from_utf8_lossy(&html[..html.len().min(2048)]).to_ascii_lowercase();
    let after = &start[start.find("charset=")? + "charset=".len()..];
    let name: String = after
        .trim_start_matches(['"', '\''])
        .chars()
        .take_while(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.' | ':'))
        .collect();
    (!name.is_empty()).then_some(name)
}

/// `text` without invisible characters that can hide or reorder words.
fn visible(text: &str) -> String {
    text.chars().filter(|c| !is_hidden_format(*c)).collect()
}

/// Tidies the article's text, and leaves out what isn't part of it:
/// invisible characters that can hide or reorder words, as in pasted text, image
/// credits, a paragraph repeated straight after itself (a subheading shown
/// twice), and single words before the article starts, such as "Comments".
fn clean(text: &str) -> String {
    let mut paragraphs: Vec<String> = Vec::new();
    for para in tidy(&visible(text)).split("\n\n") {
        let para = IMAGE_CREDIT.replace_all(para, "");
        let para = para.trim();
        let credit = ["Image:", "Photo:", "Picture:"].iter().any(|p| para.starts_with(p))
            && para.split_whitespace().count() <= 15;
        let stray = paragraphs.is_empty() && para.split_whitespace().count() <= 1;
        let repeated = paragraphs.last().is_some_and(|p| p == para);
        if !(para.is_empty() || credit || stray || repeated) {
            paragraphs.push(para.to_owned());
        }
    }
    paragraphs.join("\n\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    const URL: &str = "https://news.example.com/2026/10/story";

    /// A news page with a menu, an article and a footer.
    fn page(article: &str) -> String {
        let paragraphs = [
            "The council said the new library would open in the spring, after two years of building work and a \
             long debate about its cost, which rose by a third.",
            "It will have room for forty thousand books, a children's floor, a café and a hall for talks, and it \
             will stay open until eight in the evening on weekdays.",
            "Local schools have been asked to help choose its name, and the winning entry will be announced at a \
             ceremony in the town square next month.",
        ]
        .map(|p| format!("<p>{p}</p>"))
        .concat();
        format!(
            r#"<!DOCTYPE html><html><head><meta charset="utf-8"><title>Library to open | Example News</title>
            <meta property="og:title" content="Library to open in spring"></head><body>
            <nav><ul><li><a href="/">Home</a></li><li><a href="/news">News</a></li><li><a href="/sport">Sport</a></li></ul></nav>
            <main><article><h1>Library to open in spring</h1>
            {article}
            {paragraphs}</article></main>
            <footer><p>Copyright Example News. All rights reserved.</p><a href="/privacy">Privacy</a></footer>
            </body></html>"#
        )
    }

    #[test]
    fn reads_the_article_alone() {
        let article = read_web_page(page("<p>A new library is coming to the town centre.</p>").as_bytes(), URL, None).unwrap();
        assert_eq!(article.title, "Library to open in spring");
        assert!(article.text.starts_with("Library to open in spring\n\nA new library is coming"), "{}", article.text);
        for chrome in ["Home", "Sport", "Copyright", "Privacy"] {
            assert!(!article.text.contains(chrome), "{chrome} was read: {}", article.text);
        }
    }

    #[test]
    fn leaves_out_embedded_posts() {
        let post = r#"<blockquote class="twitter-tweet"><p>What a day for the town!</p>
            &mdash; Someone (@someone) <a href="https://twitter.com/x">October 28, 2026</a></blockquote>"#;
        let article = read_web_page(page(post).as_bytes(), URL, None).unwrap();
        assert!(!article.text.contains("What a day"), "{}", article.text);
    }

    #[test]
    fn leaves_out_image_credits_and_repeats() {
        let text = "Comments\n\nThe standfirst.\n\nThe standfirst.\u{feff}\n\nImage: Getty Images\n\n\
                    The library in the town centre(Image: Example Council)\n\nImage: a drawing of the library, which \
                    shows its glass front, its garden, its café and the square in front of it, full of people.";
        assert_eq!(
            clean(text),
            "The standfirst.\n\nThe library in the town centre\n\nImage: a drawing of the library, which shows its \
             glass front, its garden, its café and the square in front of it, full of people."
        );
    }

    #[test]
    fn pages_without_an_article_are_refused() {
        let blocked = "<html><head><title>Access Denied</title></head><body><h1>Access Denied</h1>\
                       <p>You don't have permission to access this page on this server.</p></body></html>";
        let e = read_web_page(blocked.as_bytes(), URL, None).err().unwrap();
        assert!(e.to_string().starts_with("no article was found"), "{e}");
        assert!(read_web_page(b"<html><body><p>Please enable JS</p></body></html>", URL, None).is_err());
    }

    /// The page with "\u{80}", written as UTF-8, made a single byte 0x80,
    /// which is the euro sign in Windows-1252 and invalid in UTF-8.
    fn with_byte_0x80(html: String) -> Vec<u8> {
        let mut html = html.into_bytes();
        let at = html.windows(2).position(|w| w == [0xC2, 0x80]).unwrap();
        html.splice(at..at + 2, [0x80]);
        html
    }

    #[test]
    fn reads_windows_1252_pages() {
        let html = with_byte_0x80(page("<p>It costs \u{80}5 million.</p>"));
        let article = read_web_page(&html, URL, Some("windows-1252")).unwrap();
        assert!(article.text.contains("It costs €5 million."), "{}", article.text);
        // Said in a <meta> tag instead of the headers.
        let html = with_byte_0x80(page("<p>It costs \u{80}5 million.</p>").replace("charset=\"utf-8\"", "charset=\"windows-1252\""));
        let article = read_web_page(&html, URL, None).unwrap();
        assert!(article.text.contains("It costs €5 million."), "{}", article.text);
    }

    /// One bad byte in a UTF-8 page doesn't turn its other accents and quotes
    /// into nonsense.
    #[test]
    fn a_bad_byte_spoils_only_itself() {
        let html = with_byte_0x80(page("<p>The café’s owner\u{80} said “yes”.</p>"));
        let article = read_web_page(&html, URL, Some("utf-8")).unwrap();
        assert!(article.text.contains("The café’s owner\u{fffd} said “yes”."), "{}", article.text);
        let article = read_web_page(&html, URL, None).unwrap();
        assert!(article.text.contains("The café’s owner\u{fffd} said “yes”."), "{}", article.text);
    }

    #[test]
    fn reads_pages_in_other_character_sets() {
        for (charset, sentence) in
            [("shift_jis", "図書館は春に開館します。"), ("windows-1251", "Библиотека откроется весной."), ("iso-8859-2", "Knihovna se otevře na jaře.")]
        {
            let encoding = Encoding::for_label(charset.as_bytes()).unwrap();
            let html = page(&format!("<p>{sentence}</p>"));
            let (bytes, _, _) = encoding.encode(&html);
            assert!(decode(&bytes, Some(charset)).contains(sentence), "{charset}");
            // Said in a <meta> tag instead of the headers.
            let html = html.replace("charset=\"utf-8\"", &format!("charset=\"{charset}\""));
            let (bytes, _, _) = encoding.encode(&html);
            assert!(decode(&bytes, None).contains(sentence), "{charset} in <meta>");
        }
        // As a whole article.
        let sentence = "Библиотека откроется весной, после двух лет строительства и долгих споров о её стоимости.";
        let html = page(&format!("<p>{sentence}</p>"));
        let (bytes, _, _) = encoding_rs::WINDOWS_1251.encode(&html);
        let article = read_web_page(&bytes, URL, Some("windows-1251")).unwrap();
        assert!(article.text.contains(sentence), "{}", article.text);
    }

    #[test]
    fn reads_utf_16_pages() {
        let html = page("<p>A new library is coming to the town centre.</p>");
        let utf16: Vec<u8> = html.encode_utf16().flat_map(u16::to_le_bytes).collect();
        let article = read_web_page(&utf16, URL, Some("utf-16le")).unwrap();
        assert!(article.text.contains("A new library is coming"), "{}", article.text);
        let with_bom = [&[0xFF, 0xFE][..], &utf16].concat();
        let article = read_web_page(&with_bom, URL, None).unwrap();
        assert!(article.text.contains("A new library is coming"), "{}", article.text);
    }

    /// A UTF-8 page that says it is in another character set is read as UTF-8.
    #[test]
    fn utf_8_pages_with_the_wrong_charset_are_read_as_utf_8() {
        let html = page("<p>The café’s owner said “yes”.</p>");
        let article = read_web_page(html.as_bytes(), URL, Some("iso-8859-1")).unwrap();
        assert!(article.text.contains("The café’s owner said “yes”."), "{}", article.text);
    }

    /// Invisible characters in a headline don't stop it being matched with
    /// the article's own, so it isn't read twice.
    #[test]
    fn hidden_characters_leave_the_title() {
        let html = page("<p>A new library is coming to the town centre.</p>")
            .replace("content=\"Library to open in spring\"", "content=\"Library\u{200B} to open in spring\"")
            .replace("<title>Library to open", "<title>Library\u{200B} to open");
        let article = read_web_page(html.as_bytes(), URL, None).unwrap();
        assert_eq!(article.title, "Library to open in spring");
        assert_eq!(article.text.matches("Library to open in spring").count(), 1, "{}", article.text);
    }
}
