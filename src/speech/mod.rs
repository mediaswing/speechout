//! The speech services: the voices built into the operating system, and the
//! cloud services that need an API key.

pub mod cloud;

use serde_json::Value;

/// One voice in a dropdown.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Voice {
    pub id: String,
    pub name: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Provider {
    System,
    ElevenLabs,
    OpenAi,
    Deepgram,
    Speechify,
}

impl Provider {
    pub const ALL: [Provider; 5] =
        [Provider::System, Provider::ElevenLabs, Provider::OpenAi, Provider::Deepgram, Provider::Speechify];
    pub const CLOUD: [Provider; 4] = [Provider::ElevenLabs, Provider::OpenAi, Provider::Deepgram, Provider::Speechify];

    pub fn label(self) -> &'static str {
        match self {
            Provider::System => "System Voices",
            Provider::ElevenLabs => "ElevenLabs",
            Provider::OpenAi => "OpenAI TTS",
            Provider::Deepgram => "Deepgram Aura",
            Provider::Speechify => "Speechify",
        }
    }

    /// The name used for this service in the database. Never change one.
    pub fn key(self) -> &'static str {
        match self {
            Provider::System => "system",
            Provider::ElevenLabs => "elevenlabs",
            Provider::OpenAi => "openai",
            Provider::Deepgram => "deepgram",
            Provider::Speechify => "speechify",
        }
    }

    /// Where to sign up for the service and get an API key.
    pub fn signup_url(self) -> Option<&'static str> {
        match self {
            Provider::System => None,
            Provider::ElevenLabs => Some("https://elevenlabs.io/app/sign-up"),
            Provider::OpenAi => Some("https://platform.openai.com/signup"),
            Provider::Deepgram => Some("https://console.deepgram.com/signup"),
            Provider::Speechify => Some("https://console.sws.speechify.com/signup"),
        }
    }

    /// The most characters to send in one request. The services' own limits
    /// differ, and a long text is sent a piece at a time anyway so that
    /// reading starts soon after pressing the button.
    fn max_chars(self) -> usize {
        match self {
            Provider::System => 3000,
            Provider::ElevenLabs => 5000,
            Provider::OpenAi => 3000,
            Provider::Deepgram | Provider::Speechify => 1900,
        }
    }
}

/// Lists the voices of a service. Blocks, so it runs on a background thread.
pub fn list_voices(provider: Provider, key: &str) -> anyhow::Result<Vec<Voice>> {
    match provider {
        Provider::System => crate::platform::system_voices(),
        _ => cloud::list_voices(provider, key),
    }
}

/// Speaks one piece of text, returning WAV or MP3 audio. Blocks.
pub fn synthesize(provider: Provider, key: &str, voice: &str, text: &str) -> anyhow::Result<Vec<u8>> {
    match provider {
        Provider::System => crate::platform::synthesize(text, voice),
        _ => cloud::synthesize(provider, key, voice, text),
    }
}

/// Splits text into pieces the service will accept, between sentences where
/// possible, then between words, then wherever the limit falls.
pub fn split_text(text: &str, provider: Provider) -> Vec<String> {
    split_at_most(text, provider.max_chars())
}

fn split_at_most(text: &str, limit: usize) -> Vec<String> {
    let mut pieces = Vec::new();
    let mut rest = text.trim();
    while !rest.is_empty() {
        // The byte offset just after `limit` characters, if there are more.
        let Some((end, _)) = rest.char_indices().nth(limit) else {
            pieces.push(rest.to_owned());
            break;
        };
        let head = &rest[..end];
        let cut = sentence_end(head).or_else(|| head.rfind(char::is_whitespace)).filter(|c| *c > 0).unwrap_or(end);
        pieces.push(rest[..cut].trim().to_owned());
        rest = rest[cut..].trim_start();
    }
    pieces.retain(|p| !p.is_empty());
    pieces
}

/// Where the last sentence or line in `text` ends, if there is one.
fn sentence_end(text: &str) -> Option<usize> {
    let mut last = None;
    let mut previous = '\0';
    for (i, c) in text.char_indices() {
        if c == '\n' || (c.is_whitespace() && matches!(previous, '.' | '!' | '?' | '\u{3002}')) {
            last = Some(i);
        }
        previous = c;
    }
    last
}

/// A string from a JSON value, for reading replies from services.
fn text(value: &Value) -> Option<&str> {
    value.as_str().filter(|s| !s.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn short_text_is_one_piece() {
        assert_eq!(split_at_most(" Hello. ", 100), ["Hello."]);
        assert!(split_at_most("   ", 100).is_empty());
    }

    #[test]
    fn splits_between_sentences() {
        assert_eq!(split_at_most("One two. Three four. Five six.", 22), ["One two. Three four.", "Five six."]);
    }

    #[test]
    fn splits_between_words_when_there_are_no_sentences() {
        assert_eq!(split_at_most("aaa bbb ccc ddd", 8), ["aaa bbb", "ccc ddd"]);
    }

    #[test]
    fn splits_long_words_without_breaking_characters() {
        let pieces = split_at_most(&"é".repeat(25), 10);
        assert_eq!(pieces.len(), 3);
        assert!(pieces.iter().all(|p| p.chars().count() <= 10));
        assert_eq!(pieces.concat(), "é".repeat(25));
    }

    #[test]
    fn no_piece_is_over_the_limit_and_nothing_is_lost() {
        let text = "A sentence of some length. ".repeat(500);
        for provider in Provider::ALL {
            let pieces = split_text(&text, provider);
            assert!(pieces.iter().all(|p| p.chars().count() <= provider.max_chars()));
            let words = |s: &str| s.split_whitespace().map(str::to_owned).collect::<Vec<_>>();
            assert_eq!(words(&pieces.join(" ")), words(&text));
        }
    }

    #[test]
    fn database_names_are_unique() {
        let mut keys: Vec<_> = Provider::ALL.map(Provider::key).to_vec();
        keys.sort();
        keys.dedup();
        assert_eq!(keys.len(), Provider::ALL.len());
    }
}
