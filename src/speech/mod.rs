//! Speech providers: the built-in system voices and the cloud services.

mod cloud;

use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Voice {
    pub id: String,
    pub name: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize, Default)]
pub enum Provider {
    #[default]
    System,
    ElevenLabs,
    OpenAi,
    Deepgram,
    Speechify,
}

impl Provider {
    pub const ALL: [Provider; 5] = [
        Provider::System,
        Provider::ElevenLabs,
        Provider::OpenAi,
        Provider::Deepgram,
        Provider::Speechify,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Provider::System => "System voices (built in)",
            Provider::ElevenLabs => "ElevenLabs",
            Provider::OpenAi => "OpenAI text to speech",
            Provider::Deepgram => "Deepgram Aura",
            Provider::Speechify => "Speechify",
        }
    }

    /// Name of the stored API key, or `None` for providers that need no key.
    pub fn key_name(self) -> Option<&'static str> {
        match self {
            Provider::System => None,
            Provider::ElevenLabs => Some("elevenlabs_api_key"),
            Provider::OpenAi => Some("openai_api_key"),
            Provider::Deepgram => Some("deepgram_api_key"),
            Provider::Speechify => Some("speechify_api_key"),
        }
    }

    /// Largest piece of text sent in one request.
    fn max_chunk_chars(self) -> usize {
        match self {
            Provider::System => 1000,
            Provider::ElevenLabs => 2500,
            Provider::OpenAi => 4000,
            Provider::Deepgram => 1900,
            Provider::Speechify => 1900,
        }
    }
}

pub fn list_voices(provider: Provider, api_key: Option<&str>) -> anyhow::Result<Vec<Voice>> {
    let mut voices = match provider {
        Provider::System => crate::platform::system_voices()?,
        _ => cloud::list_voices(provider, require_key(api_key)?)?,
    };
    voices.sort_by_key(|a| a.name.to_lowercase());
    voices.dedup_by(|a, b| a.id == b.id);
    if provider == Provider::System {
        // An empty ID means "whatever voice the operating system is set to use".
        voices.insert(0, Voice { id: String::new(), name: "Default system voice".into() });
    }
    Ok(voices)
}

/// Renders one chunk of text and returns encoded audio (WAV or MP3).
pub fn synthesize(
    provider: Provider,
    api_key: Option<&str>,
    voice_id: &str,
    text: &str,
) -> anyhow::Result<Vec<u8>> {
    match provider {
        Provider::System => crate::platform::synthesize(text, voice_id),
        _ => cloud::synthesize(provider, require_key(api_key)?, voice_id, text),
    }
}

fn require_key(api_key: Option<&str>) -> anyhow::Result<&str> {
    api_key
        .filter(|k| !k.is_empty())
        .ok_or_else(|| anyhow::anyhow!("no API key is saved for this service; add one in Settings"))
}

/// Splits text into pieces no longer than the provider's limit, preferring to
/// break at the end of a sentence, then at a line break, then at a space.
pub fn chunk_text(provider: Provider, text: &str) -> Vec<String> {
    split_text(text, provider.max_chunk_chars())
}

fn split_text(text: &str, max_chars: usize) -> Vec<String> {
    let mut chunks = Vec::new();
    let mut rest = text.trim();
    while !rest.is_empty() {
        if rest.chars().count() <= max_chars {
            chunks.push(rest.to_owned());
            break;
        }
        // Byte offset of the character just past the limit.
        let limit = rest.char_indices().nth(max_chars).map(|(i, _)| i).unwrap_or(rest.len());
        let window = &rest[..limit];
        let cut = window
            .rfind(['.', '!', '?', '\n'])
            .map(|i| i + 1)
            .filter(|&i| i > limit / 3)
            .or_else(|| window.rfind(char::is_whitespace).filter(|&i| i > 0))
            .unwrap_or(limit);
        // Every candidate is a character boundary: the punctuation marks are
        // one byte long and `rfind` returns the start of a whitespace character.
        let (head, tail) = rest.split_at(cut);
        let head = head.trim();
        if !head.is_empty() {
            chunks.push(head.to_owned());
        }
        rest = tail.trim_start();
    }
    chunks
}

#[cfg(test)]
mod tests {
    use super::split_text;

    #[test]
    fn splits_at_sentences() {
        let chunks = split_text("One two. Three four. Five six.", 12);
        assert_eq!(chunks, vec!["One two.", "Three four.", "Five six."]);
    }

    #[test]
    fn handles_long_words_and_unicode() {
        let chunks = split_text("ééééééééééé", 4);
        assert_eq!(chunks.concat(), "ééééééééééé");
        assert!(chunks.iter().all(|c| c.chars().count() <= 4));
    }

    #[test]
    fn empty_text_has_no_chunks() {
        assert!(split_text("   ", 10).is_empty());
    }
}
