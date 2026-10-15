//! Speech providers: the built-in system voices and the cloud services.

mod cloud;
pub mod retry;

use crate::i18n::{t, tf};
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

    /// Largest piece of text sent in one request, in Unicode characters.
    ///
    /// These are deliberately well below what each service accepts (checked
    /// October 2026: ElevenLabs multilingual v2 10,000, OpenAI 4,096, Deepgram
    /// Aura-2 2,000, Speechify 2,000 including any SSML). A smaller piece means
    /// the first words are heard sooner, and because reading aloud only works
    /// two pieces ahead, pressing Stop wastes less paid-for text. Pieces that
    /// are too small would mean more requests, more pauses at the joins and a
    /// greater chance of being rate limited, so the limits are not reduced
    /// further either.
    fn max_chunk_chars(self) -> usize {
        match self {
            Provider::System => 1000,
            Provider::ElevenLabs => 2500,
            Provider::OpenAi => 4000,
            Provider::Deepgram => 1900,
            Provider::Speechify => 1900,
        }
    }

    /// The speeds offered for this provider, slowest first, or an empty list
    /// if the provider has no speed setting. Each list stays inside the range
    /// the service accepts (ElevenLabs 0.7–1.2, Deepgram Aura-2 0.7–1.5).
    /// OpenAI's gpt-4o-mini-tts model and Speechify's plain-text requests have
    /// no reliable speed setting, and system voices use the computer's own.
    pub fn speed_choices(self) -> &'static [f32] {
        match self {
            Provider::ElevenLabs => &[0.7, 0.8, 0.9, 1.0, 1.1, 1.2],
            Provider::Deepgram => &[0.7, 0.8, 0.9, 1.0, 1.1, 1.2, 1.3, 1.4, 1.5],
            Provider::System | Provider::OpenAi | Provider::Speechify => &[],
        }
    }

    /// The nearest speed this provider offers, or 1.0 (normal) if it has no
    /// speed setting or `speed` is not a number.
    pub fn clamp_speed(self, speed: f32) -> f32 {
        if !speed.is_finite() {
            return 1.0;
        }
        self.speed_choices()
            .iter()
            .copied()
            .min_by(|a, b| (a - speed).abs().total_cmp(&(b - speed).abs()))
            .unwrap_or(1.0)
    }

    /// One sentence saying where the speech is made, for the General tab.
    /// It states only what the app does, not what the service does with it.
    /// The service's name as shown in the window, in the interface language.
    /// `label` stays in English for the debug log and error messages.
    pub fn name(self) -> String {
        match self {
            Provider::System => t("service.system"),
            Provider::OpenAi => t("service.openai"),
            _ => self.label().to_owned(),
        }
    }

    pub fn privacy_note(self) -> String {
        match self {
            Provider::System => t("service.local_note"),
            _ => tf("service.cloud_note", &[("service", &self.name())]),
        }
    }

    /// The closing sentence added to a saved photo description, saying where
    /// the description and the speech were made.
    pub fn image_description_note(self) -> String {
        match self {
            Provider::System => {
                "This image description was generated locally and voiced using a local voice.".to_owned()
            }
            _ => format!(
                "This image description was generated locally and voiced using a cloud voice from {}.",
                self.short_name()
            ),
        }
    }

    /// The service's name as used in a sentence.
    pub fn short_name(self) -> &'static str {
        match self {
            Provider::System => "system voices",
            Provider::ElevenLabs => "ElevenLabs",
            Provider::OpenAi => "OpenAI",
            Provider::Deepgram => "Deepgram",
            Provider::Speechify => "Speechify",
        }
    }
}

/// How a speed is shown in the speed list and read by screen readers.
pub fn speed_label(speed: f32) -> String {
    let speed_text = format!("{speed:.1}");
    if speed == 1.0 {
        t("speed.normal")
    } else if speed < 1.0 {
        tf("speed.slower", &[("speed", &speed_text)])
    } else {
        tf("speed.faster", &[("speed", &speed_text)])
    }
}

/// The number of characters that will be sent to the provider for `text`,
/// including any part announcements. Services count Unicode characters, and
/// the whitespace trimmed between pieces is not sent.
pub fn billable_chars(provider: Provider, text: &str, announce_parts: bool) -> usize {
    pieces(provider, text, announce_parts).iter().map(|p| p.chars().count()).sum()
}

/// Text longer than this is read in parts, at most this many characters each.
pub const PART_CHARS: usize = 4800;

/// Splits text into parts of at most `PART_CHARS` characters, then each part
/// into pieces the provider accepts, one request each. With `announce_parts`,
/// each part of a text with more than one begins "This is part 2 of 3."
/// Otherwise the parts join like any other pieces.
pub fn pieces(provider: Provider, text: &str, announce_parts: bool) -> Vec<String> {
    let parts = split_text(text, PART_CHARS);
    let total = parts.len();
    parts
        .into_iter()
        .enumerate()
        .flat_map(|(i, part)| {
            let part = if announce_parts && total > 1 { format!("This is part {} of {total}. {part}", i + 1) } else { part };
            chunk_text(provider, &part)
        })
        .collect()
}

/// Writes a count with thousands separators, such as "48,250".
pub fn format_count(n: usize) -> String {
    let digits = n.to_string();
    let mut out = String::with_capacity(digits.len() + digits.len() / 3);
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(c);
    }
    out
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

/// Renders one chunk of text and returns encoded audio (WAV or MP3). `speed`
/// is ignored by providers without a speed setting.
pub fn synthesize(
    provider: Provider,
    api_key: Option<&str>,
    voice_id: &str,
    text: &str,
    speed: f32,
) -> anyhow::Result<Vec<u8>> {
    match provider {
        Provider::System => crate::platform::synthesize(text, voice_id),
        _ => cloud::synthesize(provider, require_key(api_key)?, voice_id, text, speed),
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
        // Byte offset of the character just past the limit. Looking only this
        // far ahead keeps long documents quick to split.
        let Some((limit, _)) = rest.char_indices().nth(max_chars) else {
            chunks.push(rest.to_owned());
            break;
        };
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
    use super::*;

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

    #[test]
    fn chunks_stay_within_each_provider_limit() {
        let sentence = "The quick brown fox jumps over the lazy dog. ";
        let text = sentence.repeat(500);
        // The services' own documented limits.
        let service_limit = |p| match p {
            Provider::System => 1000,
            Provider::ElevenLabs => 10_000,
            Provider::OpenAi => 4096,
            Provider::Deepgram | Provider::Speechify => 2000,
        };
        for provider in Provider::ALL {
            let chunks = chunk_text(provider, &text);
            assert!(chunks.len() > 1);
            for chunk in &chunks {
                let n = chunk.chars().count();
                assert!(n <= provider.max_chunk_chars(), "{provider:?}: {n}");
                assert!(n < service_limit(provider), "{provider:?}: {n}");
                assert!(chunk.ends_with('.'), "{provider:?} split mid-sentence");
            }
        }
    }

    #[test]
    fn long_texts_are_read_in_parts() {
        let text = "A short sentence that is easy to count. ".repeat(250); // 10,000 characters
        let announced = pieces(Provider::OpenAi, &text, true);
        let starts: Vec<&String> = announced.iter().filter(|p| p.starts_with("This is part")).collect();
        assert_eq!(starts.len(), 3);
        assert!(starts[0].starts_with("This is part 1 of 3. A short"));
        assert!(starts[1].starts_with("This is part 2 of 3. A short"));
        assert!(starts[2].starts_with("This is part 3 of 3. A short"));
        assert!(announced.iter().all(|p| p.chars().count() <= Provider::OpenAi.max_chunk_chars()));

        let joined = pieces(Provider::OpenAi, &text, false);
        assert!(joined.iter().all(|p| !p.contains("This is part")));
        assert_eq!(joined.join(" ").split_whitespace().count(), text.split_whitespace().count());
    }

    #[test]
    fn short_texts_have_one_part_and_no_announcement() {
        assert_eq!(pieces(Provider::System, "Hello there.", true), vec!["Hello there.".to_owned()]);
    }

    #[test]
    fn chunks_lose_no_words() {
        let text = "Ünïcödé wörds, çafé and naïve. ".repeat(300);
        let chunks = chunk_text(Provider::Deepgram, &text);
        let rejoined = chunks.join(" ");
        assert_eq!(rejoined.split_whitespace().collect::<Vec<_>>(), text.split_whitespace().collect::<Vec<_>>());
    }

    #[test]
    fn counts_unicode_characters_not_bytes() {
        assert_eq!(billable_chars(Provider::OpenAi, "café", false), 4);
        assert_eq!(billable_chars(Provider::OpenAi, "日本語のテキスト", false), 8);
        assert_eq!(billable_chars(Provider::OpenAi, "  padded  ", false), 6);
        assert_eq!(billable_chars(Provider::OpenAi, "", false), 0);
        // Whitespace dropped at the joins between pieces is not counted.
        let text = "One two. Three four. Five six.";
        assert_eq!(split_text(text, 12).iter().map(|c| c.chars().count()).sum::<usize>(), text.len() - 2);
    }

    #[test]
    fn formats_counts_with_separators() {
        assert_eq!(format_count(0), "0");
        assert_eq!(format_count(999), "999");
        assert_eq!(format_count(1000), "1,000");
        assert_eq!(format_count(48_250), "48,250");
        assert_eq!(format_count(1_234_567), "1,234,567");
    }

    #[test]
    fn speeds_stay_inside_each_provider_range() {
        let range = |p| match p {
            Provider::ElevenLabs => Some((0.7, 1.2)),
            Provider::Deepgram => Some((0.7, 1.5)),
            _ => None,
        };
        for provider in Provider::ALL {
            let choices = provider.speed_choices();
            match range(provider) {
                Some((min, max)) => {
                    assert!(choices.contains(&1.0), "{provider:?} has no normal speed");
                    assert!(choices.iter().all(|s| (min..=max).contains(s)), "{provider:?}");
                    assert!(choices.windows(2).all(|w| w[0] < w[1]));
                }
                None => assert!(choices.is_empty(), "{provider:?} should not offer a speed"),
            }
        }
    }

    #[test]
    fn clamps_speeds() {
        assert_eq!(Provider::ElevenLabs.clamp_speed(1.5), 1.2);
        assert_eq!(Provider::ElevenLabs.clamp_speed(0.1), 0.7);
        assert_eq!(Provider::Deepgram.clamp_speed(1.5), 1.5);
        assert_eq!(Provider::Deepgram.clamp_speed(1.04), 1.0);
        assert_eq!(Provider::Deepgram.clamp_speed(f32::NAN), 1.0);
        for provider in [Provider::System, Provider::OpenAi, Provider::Speechify] {
            assert_eq!(provider.clamp_speed(1.3), 1.0);
        }
    }

    #[test]
    fn labels_speeds() {
        assert_eq!(speed_label(1.0), "Normal speed");
        assert_eq!(speed_label(0.8), "0.8 times normal speed (slower)");
        assert_eq!(speed_label(1.2), "1.2 times normal speed (faster)");
    }

    #[test]
    fn privacy_notes_name_the_service() {
        assert_eq!(Provider::System.privacy_note(), "Speech is made on this computer.");
        assert_eq!(Provider::Deepgram.privacy_note(), "Your text is sent to Deepgram Aura to make the speech.");
    }

    #[test]
    fn image_description_notes_say_where_the_voice_was_made() {
        assert_eq!(
            Provider::System.image_description_note(),
            "This image description was generated locally and voiced using a local voice."
        );
        assert_eq!(
            Provider::ElevenLabs.image_description_note(),
            "This image description was generated locally and voiced using a cloud voice from ElevenLabs."
        );
    }
}
