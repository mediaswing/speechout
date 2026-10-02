//! User settings, stored as JSON in the app's config folder. API keys are not
//! kept here; see `secrets.rs`.

use crate::audio::AudioFormat;
use crate::speech::Provider;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    pub provider: Provider,
    /// Chosen voice ID for each provider, keyed by `Provider` debug name.
    pub voices: BTreeMap<String, String>,
    pub audio_format: AudioFormat,
    pub vision_model: String,
    pub resolve_location: bool,
    pub log_dir: Option<PathBuf>,
    /// File names of installed wordlists the user has turned off.
    pub disabled_wordlists: BTreeSet<String>,
    pub examples_installed: bool,
    /// Ask GitHub for a newer release when the app starts.
    pub check_updates: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            provider: Provider::System,
            voices: BTreeMap::new(),
            audio_format: AudioFormat::Mp3,
            vision_model: String::new(),
            resolve_location: false,
            log_dir: None,
            disabled_wordlists: BTreeSet::new(),
            examples_installed: false,
            check_updates: true,
        }
    }
}

impl Settings {
    pub fn load() -> Self {
        match std::fs::read_to_string(crate::paths::settings_file()) {
            Ok(text) => serde_json::from_str(&text).unwrap_or_else(|e| {
                log::warn!("settings file is damaged, using defaults: {e}");
                Self::default()
            }),
            Err(_) => Self::default(),
        }
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
}
