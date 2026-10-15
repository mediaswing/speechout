//! User settings, stored as JSON in the app's config folder. API keys are not
//! kept here; see `secrets.rs`.

use crate::audio::AudioFormat;
use crate::document::DocumentReading;
use crate::speakers::SpeakerLabels;
use crate::speech::Provider;
use crate::transcribe::WhisperModel;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    pub provider: Provider,
    /// Chosen voice ID for each provider, keyed by `Provider` debug name.
    pub voices: BTreeMap<String, String>,
    /// Speaking speed for each provider that has one, keyed the same way.
    /// 1.0 is normal speed.
    pub speeds: BTreeMap<String, f32>,
    pub audio_format: AudioFormat,
    pub vision_model: String,
    pub resolve_location: bool,
    pub log_dir: Option<PathBuf>,
    /// File names of installed wordlists the user has turned off.
    pub disabled_wordlists: BTreeSet<String>,
    pub examples_installed: bool,
    /// Ask GitHub for a newer release when the app starts.
    pub check_updates: bool,
    /// Say "This is part 2 of 3" at the start of each part of a long text.
    pub announce_parts: bool,
    /// Language code for the interface, such as "fr". "en" is English.
    pub language: String,
    /// Ollama model used to translate the interface. Empty means the image
    /// description model.
    pub translation_model: String,
    /// Whisper model used to transcribe speech on the Audio Player tab.
    pub whisper_model: WhisperModel,
    /// Whether transcripts say who is speaking.
    pub speaker_labels: SpeakerLabels,
    /// Play a sound when something succeeds or fails.
    pub sounds: bool,
    /// Whether DOCX, ODT and Markdown files are read with their formatting.
    pub document_reading: DocumentReading,
    /// Whether keyboard shortcuts work at all, if the user has chosen.
    /// Otherwise they are on, and a screen reader running as the app starts
    /// gets a note saying where to change them. See `shortcuts_enabled`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub shortcuts_on: Option<bool>,
    /// Version 2.3.0 saved `shortcuts_on` here, always set, so only "off"
    /// shows a choice. Read by `load`, never saved.
    #[serde(rename = "shortcuts_enabled", skip_serializing)]
    old_shortcuts_enabled: Option<bool>,
    /// Shortcuts changed from the defaults, keyed by action, such as
    /// "pause": "Ctrl+J". "off" turns one off. See `shortcuts.rs`.
    pub shortcuts: BTreeMap<String, String>,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            provider: Provider::System,
            voices: BTreeMap::new(),
            speeds: BTreeMap::new(),
            audio_format: AudioFormat::Mp3,
            vision_model: String::new(),
            resolve_location: false,
            log_dir: None,
            disabled_wordlists: BTreeSet::new(),
            examples_installed: false,
            check_updates: true,
            announce_parts: false,
            language: crate::i18n::ENGLISH_CODE.to_owned(),
            translation_model: String::new(),
            whisper_model: WhisperModel::Base,
            speaker_labels: SpeakerLabels::Off,
            sounds: true,
            document_reading: DocumentReading::Content,
            shortcuts_on: None,
            old_shortcuts_enabled: None,
            shortcuts: BTreeMap::new(),
        }
    }
}

impl Settings {
    pub fn load() -> Self {
        match std::fs::read_to_string(crate::paths::settings_file()) {
            Ok(text) => Self::parse(&text).unwrap_or_else(|e| {
                log::warn!("settings file is damaged, using defaults: {e}");
                Self::default()
            }),
            Err(_) => Self::default(),
        }
    }

    fn parse(text: &str) -> serde_json::Result<Self> {
        let mut settings: Self = serde_json::from_str(text)?;
        if settings.shortcuts_on.is_none() && settings.old_shortcuts_enabled == Some(false) {
            settings.shortcuts_on = Some(false);
        }
        Ok(settings)
    }

    /// Whether keyboard shortcuts work: as the user chose, or else on. They
    /// stay on with a screen reader too, since none of them clash with its
    /// keys (see `shortcuts.rs`).
    pub fn shortcuts_enabled(&self) -> bool {
        self.shortcuts_on.unwrap_or(true)
    }

    pub fn save(&self) {
        let result = serde_json::to_vec_pretty(self)
            .map_err(std::io::Error::other)
            .and_then(|json| crate::paths::write_private_file(&crate::paths::settings_file(), &json));
        if let Err(e) = result {
            log::warn!("could not save settings: {e}");
        }
    }

    pub fn log_dir(&self) -> PathBuf {
        self.log_dir.clone().unwrap_or_else(crate::paths::default_log_dir)
    }

    pub fn voice_for(&self, provider: Provider) -> Option<&str> {
        self.voices.get(&format!("{provider:?}")).map(String::as_str)
    }

    pub fn set_voice(&mut self, provider: Provider, voice_id: String) {
        self.voices.insert(format!("{provider:?}"), voice_id);
    }

    /// The saved speed, kept inside the provider's range; 1.0 if none is saved
    /// or the provider has no speed setting.
    pub fn speed_for(&self, provider: Provider) -> f32 {
        self.speeds.get(&format!("{provider:?}")).map_or(1.0, |s| provider.clamp_speed(*s))
    }

    pub fn set_speed(&mut self, provider: Provider, speed: f32) {
        self.speeds.insert(format!("{provider:?}"), provider.clamp_speed(speed));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn remembers_a_voice_for_each_provider() {
        let mut settings = Settings::default();
        settings.set_voice(Provider::OpenAi, "nova".into());
        settings.set_voice(Provider::Deepgram, "aura-2-draco-en".into());
        settings.set_voice(Provider::OpenAi, "sage".into());
        assert_eq!(settings.voice_for(Provider::OpenAi), Some("sage"));
        assert_eq!(settings.voice_for(Provider::Deepgram), Some("aura-2-draco-en"));
        assert_eq!(settings.voice_for(Provider::ElevenLabs), None);
    }

    #[test]
    fn speeds_are_per_provider_and_in_range() {
        let mut settings = Settings::default();
        assert_eq!(settings.speed_for(Provider::ElevenLabs), 1.0);
        settings.set_speed(Provider::Deepgram, 1.4);
        settings.set_speed(Provider::ElevenLabs, 1.4);
        assert_eq!(settings.speed_for(Provider::Deepgram), 1.4);
        assert_eq!(settings.speed_for(Provider::ElevenLabs), 1.2);
        // A hand-edited or out-of-date value is pulled back into range.
        settings.speeds.insert("ElevenLabs".into(), 9.0);
        assert_eq!(settings.speed_for(Provider::ElevenLabs), 1.2);
        // Providers without a speed setting always use normal speed.
        settings.set_speed(Provider::OpenAi, 1.4);
        assert_eq!(settings.speed_for(Provider::OpenAi), 1.0);
        assert_eq!(settings.speed_for(Provider::System), 1.0);
    }

    #[test]
    fn older_settings_files_still_load() {
        let old = r#"{"provider":"Deepgram","voices":{"Deepgram":"aura-2-luna-en"},"check_updates":false}"#;
        let settings: Settings = serde_json::from_str(old).unwrap();
        assert_eq!(settings.provider, Provider::Deepgram);
        assert_eq!(settings.voice_for(Provider::Deepgram), Some("aura-2-luna-en"));
        assert_eq!(settings.speed_for(Provider::Deepgram), 1.0);
        assert!(!settings.check_updates);
    }

    #[test]
    fn shortcuts_are_on_unless_turned_off() {
        assert!(Settings::default().shortcuts_enabled());
        assert!(Settings { shortcuts_on: Some(true), ..Settings::default() }.shortcuts_enabled());
        assert!(!Settings { shortcuts_on: Some(false), ..Settings::default() }.shortcuts_enabled());
    }

    #[test]
    fn shortcuts_turned_off_in_2_3_0_stay_off() {
        // 2.3.0 always saved shortcuts_enabled, so true is not a choice.
        let on = Settings::parse(r#"{"shortcuts_enabled":true}"#).unwrap();
        assert_eq!(on.shortcuts_on, None);
        let off = Settings::parse(r#"{"shortcuts_enabled":false}"#).unwrap();
        assert_eq!(off.shortcuts_on, Some(false));
        let saved = serde_json::to_string(&off).unwrap();
        assert!(!saved.contains("shortcuts_enabled"));
        assert_eq!(Settings::parse(&saved).unwrap().shortcuts_on, Some(false));
    }
}
