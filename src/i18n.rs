//! The words the app shows and says about itself, in English and in any
//! language the user has had translated.
//!
//! Every piece of interface text has a key, such as `general.read_aloud`, and
//! an English version in `ENGLISH`. A translation is a JSON file in the
//! languages folder that gives the text for some or all of the keys in
//! another language. Anything a translation leaves out, or gets wrong, is
//! shown in English instead, so a damaged or out-of-date file never leaves a
//! control without a label.
//!
//! Translations are made by a local AI model in Ollama (see `translate_batch`),
//! so the interface text never leaves the computer. The files are plain JSON,
//! so anyone can correct them, or share them with others.
//!
//! Text that is spoken as part of a document (such as "This is part 1 of 3")
//! stays in English, because it goes to the voice, not the interface.

use anyhow::{Context, bail};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::fmt::Display;
use std::path::{Path, PathBuf};
use std::sync::{OnceLock, RwLock};

/// The language code for English, which needs no translation file.
pub const ENGLISH_CODE: &str = "en";

/// How many pieces of text are sent to the AI model at once. Small enough
/// for a small model to keep track of, large enough not to be slow.
pub const BATCH_SIZE: usize = 20;

/// Translation files can come from other people, so they are checked as
/// carefully as wordlists. The whole catalogue in English is about 15 KB.
const MAX_FILE_BYTES: u64 = 1024 * 1024;
/// Longest language name accepted from a translation file.
const MAX_NAME_CHARS: usize = 60;

/// Every piece of interface text, in British English. `{name}` marks a
/// value filled in when the text is shown; translations must keep these.
const ENGLISH: &[(&str, &str)] = &[
    // Tabs
    ("tab.general", "General"),
    ("tab.settings", "Settings"),
    ("tab.wordlists", "Wordlists"),
    ("tab.player", "Audio player"),
    ("tab.chosen", "{tab} tab."),
    // Start-up
    ("status.ready", "Ready. Press {key_name}+O to choose a file."),
    ("status.log_off", "Debug logging is off because the log folder could not be opened: {error}."),
    // F7
    ("progress.paused", "Paused at {percent} percent."),
    ("progress.reading", "Reading aloud, {percent} percent."),
    ("progress.previewing", "Previewing the voice."),
    ("progress.saving", "Saving audio, {percent} percent."),
    ("progress.downloading", "Downloading the AI model, {percent} percent."),
    ("progress.translating", "Translating the app, {percent} percent."),
    ("progress.transcribing", "Transcribing, {percent} percent."),
    ("progress.setting_up", "Still setting up Ollama."),
    ("progress.opening", "Still opening the file."),
    ("progress.nothing", "Nothing is playing."),
    // Progress bar
    ("bar.label", "Progress"),
    ("bar.paused", "Paused: {percent}%"),
    ("bar.reading", "Reading aloud: {percent}%"),
    ("bar.previewing", "Previewing the voice"),
    ("bar.saving", "Saving audio: {percent}%"),
    ("bar.downloading", "Downloading the AI model: {percent}%"),
    ("bar.translating", "Translating the app: {percent}%"),
    ("bar.transcribing", "Transcribing: {percent}%"),
    ("bar.audio_playing", "Playing the audio file: {percent}%"),
    ("bar.audio_paused", "Audio file paused: {percent}%"),
    ("bar.setting_up", "Setting up Ollama…"),
    ("bar.opening", "Opening the file…"),
    ("bar.nothing", "Nothing playing"),
    // Clipboard
    ("copy.nothing", "There is no text to copy yet."),
    ("copy.done", "Copied {count} words to the clipboard."),
    // Choosing and loading files
    ("file.dialog_title", "Choose a document or photo"),
    ("file.filter_all", "Documents and photos"),
    ("file.filter_documents", "Documents"),
    ("file.filter_photos", "Photos"),
    ("file.unsupported", "That type of file is not supported. Choose a PDF, TXT, DOCX, CSV, JPEG or HEIC file."),
    ("file.model_downloading", "The AI model is still downloading. Choose the photo again when it has finished."),
    ("file.looking_for_model", "Looking for the local AI model to describe {name}."),
    ("file.describing", "Describing {name}. This can take a minute."),
    ("file.opening", "Opening {name}."),
    ("file.loaded", "Loaded {name}, {count} words. Press F5 to read it aloud or {key_name}+S to save it as audio."),
    ("file.load_failed", "Could not load {name}. {error}"),
    ("file.fallback_name", "the file"),
    // Ollama and AI models
    ("models.none", "Ollama is running but has no models. Download one that describes photos on the Settings tab."),
    ("models.found", "Found {count} local AI model(s)."),
    ("models.looking", "Looking for local AI models."),
    ("model.size", "about {size}"),
    ("model.downloaded", "Downloaded {model}. You can now describe photos."),
    (
        "model.download_stopped",
        "Stopped downloading the AI model. What was downloaded is kept, so the next download carries on from there.",
    ),
    ("model.download_failed", "Could not download the AI model. {error}"),
    ("model.download_title", "Download an AI model"),
    (
        "model.download_question",
        "Ollama does not have an AI model that can describe photos yet.\n\nDownload {model} now? It is {size} and can \
         take a while. It is stored by Ollama on this computer. You can press Escape to stop.",
    ),
    ("model.not_downloaded", "No model was downloaded. You can download one later on the Settings tab."),
    ("model.downloading", "Downloading {model}, {size}. You will hear the progress. Press Escape to stop."),
    ("model.download_progress", "Downloading the AI model, {percent}% done."),
    ("ollama.running", "Ollama is running. Looking for a model to describe the photo."),
    ("ollama.setup_failed", "Could not set up Ollama. {error}"),
    (
        "ollama.intro",
        "Photos are described by Ollama, a free program that runs an AI model on this computer, so your photos are \
         not sent anywhere.",
    ),
    ("ollama.start_title", "Start Ollama"),
    ("ollama.start_question", "Ollama is installed but not running. Start it now?"),
    ("ollama.install_title", "Install Ollama"),
    (
        "ollama.install_question",
        "Ollama is not installed. Install it now with {manager}? It is a large download and can take several minutes.",
    ),
    ("ollama.password", "You will be asked for your password."),
    ("ollama.download_question", "Ollama is not installed. Open the Ollama download page in your web browser?"),
    ("ollama.declined", "Did not describe {name}. To describe photos, install Ollama from ollama.com and start it."),
    ("ollama.page_opened", "Opened the Ollama download page. Install Ollama, then choose the photo again."),
    ("ollama.page_failed", "{error}. Download Ollama from {url}"),
    (
        "ollama.installing",
        "Installing Ollama with {manager}. This can take several minutes. You will hear when it is done.",
    ),
    ("ollama.installed", "Ollama is installed. Starting it."),
    ("ollama.starting", "Starting Ollama."),
    // Updates
    ("update.checking", "Checking for updates."),
    ("update.latest", "You have the latest version, {version}."),
    ("update.title", "Update available"),
    (
        "update.question",
        "Version {version} of the {app} is available. You have version {current}.\n\nOpen the download page in \
         your web browser?",
    ),
    ("update.opened", "Opened the download page for version {version}."),
    ("update.open_failed", "{error}. The download page is {url}"),
    ("update.later", "Version {version} is available. You can download it later from the Settings tab."),
    // Reading aloud and saving
    ("read.nothing", "There is nothing to read yet. Press {key_name}+O to choose a file."),
    ("read.started", "Reading aloud. Press F6 to pause or Escape to stop."),
    ("read.stopped", "Stopped reading."),
    ("read.finished", "Finished reading."),
    ("read.paused", "Paused. Press F6 to resume."),
    ("read.resumed", "Resumed."),
    (
        "read.audio_playing",
        "The audio file on the Audio player tab is playing or paused. Stop it first, or press Escape.",
    ),
    ("read.stopping", "Stopping."),
    ("preview.stopped", "Stopped the preview."),
    ("preview.finished", "Finished the preview."),
    ("voice.choose_first", "Choose a voice first. The voice list may still be loading."),
    ("voice.default", "Default system voice"),
    ("voices.failed", "Could not load voices for {service}: {error}"),
    ("usage.exact.one", "1 character will be sent to {service}."),
    ("usage.exact.other", "{count} characters will be sent to {service}."),
    ("usage.up_to.one", "Up to 1 character will be sent to {service}."),
    ("usage.up_to.other", "Up to {count} characters will be sent to {service}."),
    ("save.dialog_title", "Save spoken text as audio"),
    ("save.default_name", "speech"),
    ("save.preparing", "Preparing {name}."),
    ("save.cancel_hint", "Press Escape to cancel."),
    ("save.progress", "Preparing audio, {percent}% done."),
    ("save.cancelled", "Cancelled saving audio."),
    ("save.saved", "Saved the audio as {name}."),
    ("retry.rate_limited", "{service} is limiting how fast requests can be made."),
    ("retry.busy", "{service} is busy."),
    ("retry.unreachable", "Could not reach {service}."),
    ("retry.wait.one", "Trying again in 1 second. Press Escape to stop."),
    ("retry.wait.other", "Trying again in {count} seconds. Press Escape to stop."),
    // Speech services, voices and formats
    ("service.system", "System voices (built in)"),
    ("service.openai", "OpenAI text to speech"),
    ("service.local_note", "Speech is made on this computer."),
    ("service.cloud_note", "Your text is sent to {service} to make the speech."),
    ("speed.normal", "Normal speed"),
    ("speed.slower", "{speed} times normal speed (slower)"),
    ("speed.faster", "{speed} times normal speed (faster)"),
    ("format.mp3", "MP3 (smaller file)"),
    ("format.wav", "WAV (uncompressed)"),
    // General tab
    ("general.heading", "Read a document or describe a photo"),
    ("general.current_file", "Current file"),
    ("general.loading", "Loading…"),
    ("general.no_file", "No file chosen"),
    ("general.choose_file", "Choose a file… ({key}+O)"),
    ("general.service", "Speech service"),
    ("general.service_chosen", "Speech service: {service}. {note}"),
    ("general.voice", "Voice"),
    ("general.no_voices", "No voices found"),
    ("general.voices_failed", "Voices could not be loaded"),
    ("general.loading_voices", "Loading voices…"),
    ("general.retry_voices", "Try loading voices again"),
    ("general.speed", "Speaking speed"),
    ("general.preview", "Preview voice"),
    ("general.read_aloud", "Read aloud (F5)"),
    ("general.pause", "Pause (F6)"),
    ("general.resume", "Resume (F6)"),
    ("general.stop", "Stop (Esc)"),
    ("general.cancel_saving", "Cancel saving (Esc)"),
    ("general.stop_downloading", "Stop downloading (Esc)"),
    ("general.stop_translating", "Stop translating (Esc)"),
    ("general.stop_transcribing", "Stop transcribing (Esc)"),
    ("general.format", "Audio file format"),
    ("general.save", "Save spoken text as audio… ({key}+S)"),
    ("general.copy", "Copy text to the clipboard"),
    // Settings tab
    ("settings.heading", "Settings"),
    ("settings.model", "Image description model (Ollama)"),
    ("settings.no_models", "No models installed in Ollama"),
    ("settings.ollama_off", "Ollama is not running"),
    ("settings.looking", "Looking for Ollama…"),
    ("settings.download_model", "Download a model that describes photos ({model}, {size})"),
    ("settings.refresh_models", "Refresh the list of models"),
    ("settings.location", "Photo location"),
    ("settings.location_off", "Do not read out where photos were taken"),
    ("settings.location_on", "Read out where geotagged photos were taken (looks up the place with OpenStreetMap)"),
    ("settings.whisper", "Speech recognition model (Whisper)"),
    ("settings.whisper_base", "{model}: quicker, good with clear speech ({size})"),
    ("settings.whisper_small", "{model}: slower, better with accents, noise and other languages ({size})"),
    ("settings.whisper_downloaded", "– downloaded"),
    (
        "settings.whisper_note",
        "Speech in audio files is transcribed on this computer, so it is not sent anywhere. The model is downloaded \
         from Hugging Face the first time it is needed, and kept in {folder}.",
    ),
    ("settings.parts", "Long texts (read in parts of up to {count} characters)"),
    ("settings.parts_run_on", "Run the parts on with no announcement"),
    ("settings.parts_announce", "Say \"This is part 1 of 3\" at the start of each part"),
    ("settings.updates", "Updates"),
    ("settings.updates_on", "Check for updates when the app starts"),
    ("settings.updates_off", "Do not check for updates automatically"),
    ("settings.check_now", "Check for updates now"),
    ("settings.log_folder", "Debug log folder"),
    ("settings.choose_log", "Choose the debug log folder…"),
    ("settings.log_dialog", "Choose where to save debug logs"),
    ("settings.use_typed_log", "Use the folder typed above for debug logs"),
    ("settings.full_path", "Type a full folder path, for example one starting with a drive letter or a slash."),
    ("settings.log_saved", "Debug logs are now saved to {file}."),
    ("settings.log_failed", "Could not use that folder for logs: {error}"),
    // API keys
    ("keys.heading", "Cloud voice API keys"),
    ("keys.stored", "Keys are stored in {place}. Leave a box empty to keep the key already saved."),
    ("keys.label_saved", "{service} API key (a key is saved)"),
    ("keys.label_none", "{service} API key (no key saved)"),
    ("keys.save", "Save API keys"),
    ("keys.remove", "Remove all saved API keys…"),
    ("keys.save_failed", "Could not save the {service} key: {error}"),
    ("keys.type_first", "Type a key into one of the API key boxes first."),
    ("keys.saved", "Saved the API key for {services} in {place}."),
    ("keys.and", "and"),
    ("keys.none", "No API keys are saved."),
    ("keys.remove_title", "Remove saved API keys"),
    ("keys.remove_question", "Remove every saved API key? You will need to enter them again to use cloud voices."),
    ("keys.removed", "Removed all saved API keys."),
    // Audio player tab
    ("player.heading", "Play an audio file and transcribe speech"),
    ("player.choose", "Choose an audio file… ({key}+O)"),
    ("player.dialog_title", "Choose an audio file"),
    ("player.filter", "Audio files (WAV and MP3)"),
    ("player.opening", "Opening {name} and listening for speech. Press Escape to stop."),
    ("player.open_stopped", "Stopped opening {name}."),
    ("player.wait_transcribing", "Wait for transcribing to finish, or stop it, before choosing another audio file."),
    ("player.loaded", "Opened {name}, {length} long."),
    ("player.speech", "It sounds like speech, so it will be transcribed."),
    ("player.cut_short", "Only the first three hours can be transcribed."),
    (
        "player.not_speech",
        "It sounds mostly like music or other sounds rather than speech, so it was not transcribed. Press \
         \"{button}\" to try anyway. Press F5 to play it.",
    ),
    ("player.silent", "It seems to be silent."),
    ("player.summary", "Length: {length}. Sounds like: {sound}."),
    ("player.kind_speech", "speech"),
    ("player.kind_other", "music or other sounds"),
    ("player.kind_silent", "silence"),
    ("player.waveform", "Waveform. {summary}"),
    ("player.play", "Play (F5)"),
    ("player.back", "Back 10 seconds"),
    ("player.forward", "Forward 10 seconds"),
    ("player.transcribe", "Transcribe the speech"),
    ("player.nothing", "Choose an audio file first. Press {key_name}+O on this tab."),
    ("player.reading_aloud", "Something is being read aloud. Stop it before playing the audio file."),
    ("player.playing", "Playing. Press F6 to pause, F7 to hear the time, or Escape to stop."),
    ("player.paused", "Paused at {position}. Press F6 to resume."),
    ("player.position", "{position} of {length}."),
    ("player.position_paused", "Paused at {position} of {length}."),
    ("player.finished", "Finished playing the audio file."),
    ("player.stopped", "Stopped playing the audio file."),
    ("player.play_failed", "Could not play the audio file. {error}"),
    ("time.hour", "1 hour"),
    ("time.hours", "{count} hours"),
    ("time.minute", "1 minute"),
    ("time.minutes", "{count} minutes"),
    ("time.second", "1 second"),
    ("time.seconds", "{count} seconds"),
    ("transcript.heading", "Transcript"),
    ("transcript.label", "Transcript text"),
    ("transcript.none", "No transcript yet."),
    ("transcript.busy", "It can be transcribed when the app has finished what it is doing."),
    (
        "transcript.started",
        "Transcribing with {model} on this computer. This can take a few minutes, and you will hear the progress. \
         You can play the file meanwhile.",
    ),
    ("transcript.progress", "Transcribing, {percent}% done."),
    ("transcript.stop", "Stop transcribing"),
    ("transcript.stopped", "Stopped transcribing."),
    ("transcript.no_speech", "No speech was found in {name}."),
    (
        "transcript.done",
        "Transcribed {name}, {count} words. The transcript is under the Transcribe button. Press {key_name}+S to \
         save it.",
    ),
    ("transcript.failed", "Could not transcribe the audio file. {error}"),
    ("transcript.copy", "Copy the transcript to the clipboard"),
    ("transcript.save", "Save the transcript as a text file… ({key}+S)"),
    ("transcript.save_title", "Save the transcript"),
    ("transcript.filter", "Text file"),
    ("transcript.default_name", "transcript"),
    ("transcript.nothing", "There is no transcript to save yet."),
    ("transcript.saved", "Saved the transcript as {name}."),
    ("transcript.save_failed", "Could not save {name}. {error}"),
    ("whisper.download_title", "Download the speech recognition model"),
    (
        "whisper.download_question",
        "Transcribing speech needs {model}, an AI model that runs on this computer, so your audio is not sent \
         anywhere.\n\nDownload it now from Hugging Face? It is {size} and is only downloaded once. You can press \
         Escape to stop.",
    ),
    ("whisper.not_downloaded", "Not transcribed. To download the model later, press \"{button}\"."),
    ("whisper.downloading", "Downloading {model}, {size}. You will hear the progress. Press Escape to stop."),
    ("whisper.downloaded", "Downloaded {model}."),
    (
        "whisper.download_stopped",
        "Stopped downloading the speech recognition model. Nothing was kept, so the next download starts again.",
    ),
    ("whisper.download_failed", "Could not download the speech recognition model. {error}"),
    // Language
    ("language.label", "Language of the app"),
    ("language.model", "Translation model (Ollama)"),
    ("language.translate", "Translate the app into {language}"),
    (
        "language.note",
        "Translations are made by the AI model on this computer, so no text is sent anywhere. They can contain \
         mistakes. They are saved in {folder}, where you can correct them.",
    ),
    ("language.chosen", "The app is now in {language}."),
    ("language.partly", "The app is now in {language}. {count} pieces of text are not translated yet and stay in English."),
    (
        "language.not_yet",
        "There is no translation into {language} yet. Press \"{button}\" to make one with the local AI model. It \
         takes a few minutes.",
    ),
    ("language.load_failed", "Could not open the translation into {language}: {error}"),
    (
        "language.no_model",
        "Translating needs a local AI model in Ollama. Install Ollama and download a model, then try again.",
    ),
    ("language.translating", "Translating the app into {language} with {model}. This takes a few minutes. Press Escape to stop."),
    ("language.progress", "Translating the app, {percent}% done."),
    ("language.done", "Translated the app into {language}."),
    (
        "language.done_partly",
        "Translated the app into {language}. {count} pieces of text could not be translated and stay in English. \
         Translate again to try them.",
    ),
    (
        "language.stopped",
        "Stopped translating. What was translated is kept, so translating again carries on from there.",
    ),
    ("language.failed", "Could not translate the app. {error}"),
    ("language.again_title", "Translate again"),
    (
        "language.again_question",
        "Everything is already translated into {language}. Translate it all again? This replaces any corrections \
         you have made to the translation.",
    ),
    // Wordlists tab
    ("wordlists.heading", "Wordlists"),
    (
        "wordlists.intro",
        "Wordlists change how words are spoken, for example to fix pronunciation or to keep reading classroom-safe. \
         Ticked wordlists are used when reading aloud and saving audio.",
    ),
    ("wordlists.import", "Import a wordlist… (XML)"),
    ("wordlists.reload", "Reload wordlists"),
    ("wordlists.count", "{count} wordlist(s) installed."),
    ("wordlists.installed", "Installed wordlists"),
    ("wordlists.none", "No wordlists are installed."),
    ("wordlists.item", "{name} – {description} ({count} entries)"),
    ("wordlists.unreadable", "{file} could not be read: {error}"),
    ("wordlists.enabled", "{name} enabled."),
    ("wordlists.disabled", "{name} disabled."),
    ("wordlists.to_remove", "Wordlist to remove"),
    ("wordlists.remove", "Remove the selected wordlist…"),
    ("wordlists.import_title", "Import a wordlist"),
    ("wordlists.filter", "XML wordlist"),
    ("wordlists.imported", "Imported and enabled the wordlist \"{name}\"."),
    ("wordlists.import_failed", "Could not import {name}: {error}"),
    ("wordlists.remove_title", "Remove wordlist"),
    ("wordlists.remove_question", "Remove the wordlist \"{name}\"?"),
    ("wordlists.removed", "Removed the wordlist \"{name}\"."),
];

/// Languages offered on the Settings tab: code, name in the language itself,
/// and name in English. Only languages written in the Latin, Greek and
/// Cyrillic alphabets are listed, because the window cannot yet draw other
/// scripts, or text that runs from right to left.
const LANGUAGES: &[(&str, &str, &str)] = &[
    ("en", "English", "English"),
    ("cy", "Cymraeg", "Welsh"),
    ("ga", "Gaeilge", "Irish"),
    ("gd", "Gàidhlig", "Scottish Gaelic"),
    ("cs", "Čeština", "Czech"),
    ("da", "Dansk", "Danish"),
    ("de", "Deutsch", "German"),
    ("el", "Ελληνικά", "Greek"),
    ("es", "Español", "Spanish"),
    ("fr", "Français", "French"),
    ("hr", "Hrvatski", "Croatian"),
    ("it", "Italiano", "Italian"),
    ("lt", "Lietuvių", "Lithuanian"),
    ("lv", "Latviešu", "Latvian"),
    ("hu", "Magyar", "Hungarian"),
    ("nl", "Nederlands", "Dutch"),
    ("nb", "Norsk bokmål", "Norwegian"),
    ("pl", "Polski", "Polish"),
    ("pt", "Português", "Portuguese"),
    ("ro", "Română", "Romanian"),
    ("sk", "Slovenčina", "Slovak"),
    ("sl", "Slovenščina", "Slovenian"),
    ("sq", "Shqip", "Albanian"),
    ("fi", "Suomi", "Finnish"),
    ("sv", "Svenska", "Swedish"),
    ("tr", "Türkçe", "Turkish"),
    ("uk", "Українська", "Ukrainian"),
    ("bg", "Български", "Bulgarian"),
    ("ru", "Русский", "Russian"),
    ("sr", "Српски", "Serbian"),
];

/// The translation in use, or `None` for English.
static ACTIVE: RwLock<Option<HashMap<String, String>>> = RwLock::new(None);

fn english_map() -> &'static HashMap<&'static str, &'static str> {
    static MAP: OnceLock<HashMap<&'static str, &'static str>> = OnceLock::new();
    MAP.get_or_init(|| ENGLISH.iter().copied().collect())
}

/// The English text for `key`.
pub fn english(key: &str) -> Option<&'static str> {
    english_map().get(key).copied()
}

/// The text for `key` in the language in use.
pub fn t(key: &str) -> String {
    tf(key, &[])
}

/// The text for `key` in the language in use, with each `{name}` replaced by
/// its value from `args`.
pub fn tf(key: &str, args: &[(&str, &dyn Display)]) -> String {
    let active = ACTIVE.read().unwrap_or_else(|e| e.into_inner());
    render(active.as_ref(), key, args)
}

fn render(active: Option<&HashMap<String, String>>, key: &str, args: &[(&str, &dyn Display)]) -> String {
    let template = match active.and_then(|a| a.get(key)) {
        Some(text) => text.as_str(),
        None => english(key).unwrap_or_else(|| {
            log::warn!("no English text for {key}");
            key
        }),
    };
    fill(template, args)
}

/// Replaces each `{name}` in `template` with its value. Braces that are not a
/// known name are left as they are.
fn fill(template: &str, args: &[(&str, &dyn Display)]) -> String {
    let mut out = String::with_capacity(template.len());
    let mut rest = template;
    while let Some(start) = rest.find('{') {
        out.push_str(&rest[..start]);
        let after = &rest[start + 1..];
        let value = after
            .find('}')
            .and_then(|end| args.iter().find(|(name, _)| *name == &after[..end]).map(|(_, v)| (end, v)));
        match value {
            Some((end, v)) => {
                out.push_str(&v.to_string());
                rest = &after[end + 1..];
            }
            None => {
                out.push('{');
                rest = after;
            }
        }
    }
    out.push_str(rest);
    out
}

/// The names in braces in `text`, such as `name` in "Opening {name}."
fn placeholders(text: &str) -> BTreeSet<&str> {
    let mut names = BTreeSet::new();
    let mut rest = text;
    while let Some(start) = rest.find('{') {
        let after = &rest[start + 1..];
        match after.find('}') {
            Some(end) if after[..end].chars().all(|c| c.is_ascii_lowercase() || c == '_') && end > 0 => {
                names.insert(&after[..end]);
                rest = &after[end + 1..];
            }
            _ => rest = after,
        }
    }
    names
}

/// Characters that could hide or disguise text: control characters, and the
/// marks that reorder text or make it invisible. They could be used to make a
/// file name or button read as something it isn't.
fn is_hidden_char(c: char) -> bool {
    (c.is_control() && c != '\n')
        || matches!(c, '\u{200b}'..='\u{200f}' | '\u{202a}'..='\u{202e}' | '\u{2060}'..='\u{2069}' | '\u{feff}')
}

/// Whether a translation of `key` can be used: it is not empty or
/// unreasonably long, has no hidden characters, only has line breaks where
/// the English does, and has the same `{name}` values as the English.
fn fits(key: &str, text: &str) -> bool {
    let Some(en) = english(key) else { return false };
    let limit = en.chars().count() * 4 + 40;
    !text.trim().is_empty()
        && text.chars().count() <= limit
        && !text.chars().any(is_hidden_char)
        && (en.contains('\n') || !text.contains('\n'))
        && placeholders(en) == placeholders(text)
}

/// A language name from a translation file, if it is safe to show.
fn clean_name(name: &str) -> Option<String> {
    let name = name.trim();
    let ok = !name.is_empty() && name.chars().count() <= MAX_NAME_CHARS && !name.chars().any(|c| is_hidden_char(c) || c == '\n');
    ok.then(|| name.to_owned())
}

// ----- languages and translation files -----------------------------------

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Language {
    pub code: String,
    /// The language's name in that language, such as "Français".
    pub name: String,
    /// The language's name in English, such as "French".
    pub english_name: String,
}

impl Language {
    /// How the language is shown in the list, such as "Français (French)".
    pub fn label(&self) -> String {
        if self.name == self.english_name || self.english_name.is_empty() {
            self.name.clone()
        } else {
            format!("{} ({})", self.name, self.english_name)
        }
    }
}

/// A translation file: the language's names, which model made it, and the
/// text for each key.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Translation {
    pub language: String,
    pub english_name: String,
    /// The AI model that made the translation, if any.
    pub made_with: String,
    pub strings: BTreeMap<String, String>,
}

impl Translation {
    pub fn new(language: &Language) -> Self {
        Self {
            language: language.name.clone(),
            english_name: language.english_name.clone(),
            ..Self::default()
        }
    }

    /// Keys with no usable translation, in catalogue order.
    pub fn missing(&self) -> Vec<&'static str> {
        ENGLISH
            .iter()
            .map(|(key, _)| *key)
            .filter(|key| !self.strings.get(*key).is_some_and(|text| fits(key, text)))
            .collect()
    }

    /// The translations that can be used: known keys whose `{name}` values
    /// match the English. The rest are left out, so they show in English.
    fn usable(&self) -> HashMap<String, String> {
        self.strings
            .iter()
            .filter(|(key, text)| fits(key, text))
            .map(|(key, text)| (key.clone(), text.clone()))
            .collect()
    }
}

pub fn file_for(dir: &Path, code: &str) -> PathBuf {
    dir.join(format!("{code}.json"))
}

/// A language code that is safe to use as a file name.
fn valid_code(code: &str) -> bool {
    !code.is_empty() && code.len() <= 16 && code.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}

pub fn load(dir: &Path, code: &str) -> anyhow::Result<Translation> {
    if !valid_code(code) {
        bail!("\"{code}\" is not a language code");
    }
    let path = file_for(dir, code);
    let size = std::fs::metadata(&path).context("the file could not be read")?.len();
    if size > MAX_FILE_BYTES {
        bail!("the file is too large (the limit is 1 MB)");
    }
    let text = std::fs::read_to_string(&path).context("the file could not be read")?;
    serde_json::from_str(&text).context("the file is not a valid translation")
}

pub fn save(dir: &Path, code: &str, translation: &Translation) -> anyhow::Result<()> {
    if !valid_code(code) {
        bail!("\"{code}\" is not a language code");
    }
    let json = serde_json::to_vec_pretty(translation)?;
    // Written to a new file and then swapped in, so a crash can't leave a
    // half-written translation, and a link in its place is replaced rather
    // than followed.
    crate::paths::write_private_file(&file_for(dir, code), &json).context("the translation could not be saved")
}

/// The languages to offer: English, the built-in list, and any other
/// translation files someone has put in the languages folder.
pub fn available(dir: &Path) -> Vec<Language> {
    let mut languages: Vec<Language> = LANGUAGES
        .iter()
        .map(|(code, name, english_name)| Language {
            code: (*code).into(),
            name: (*name).into(),
            english_name: (*english_name).into(),
        })
        .collect();
    let Ok(entries) = std::fs::read_dir(dir) else { return languages };
    let mut extra = Vec::new();
    for path in entries.flatten().map(|e| e.path()) {
        if path.extension().is_none_or(|e| e != "json") {
            continue;
        }
        let Some(code) = path.file_stem().and_then(|s| s.to_str()) else { continue };
        if languages.iter().any(|l| l.code == code) {
            continue;
        }
        match load(dir, code) {
            Ok(t) => match clean_name(&t.language) {
                Some(name) => extra.push(Language {
                    code: code.to_owned(),
                    name,
                    english_name: clean_name(&t.english_name).unwrap_or_default(),
                }),
                None => log::warn!("translation {code} has no usable language name"),
            },
            Err(e) => log::warn!("could not read translation {code}: {e:#}"),
        }
    }
    extra.sort_by_key(|l| l.name.to_lowercase());
    languages.extend(extra);
    languages
}

/// Uses `translation` for all interface text, or English if `None`.
pub fn activate(translation: Option<&Translation>) {
    let usable = translation.map(Translation::usable);
    *ACTIVE.write().unwrap_or_else(|e| e.into_inner()) = usable;
}

/// Switches to the saved language at start-up. Falls back to English, with
/// a note in the log, if its translation can't be read.
pub fn activate_saved(dir: &Path, code: &str) {
    if code == ENGLISH_CODE || code.is_empty() {
        return;
    }
    match load(dir, code) {
        Ok(translation) => activate(Some(&translation)),
        Err(e) => log::warn!("could not load translation {code}, using English: {e:#}"),
    }
}

// ----- translating with a local AI model ----------------------------------

fn prompt(language: &Language, batch: &BTreeMap<&str, &str>) -> String {
    let target = if language.english_name.is_empty() {
        language.name.clone()
    } else {
        format!("{} ({})", language.english_name, language.name)
    };
    let input = serde_json::to_string_pretty(batch).unwrap_or_default();
    format!(
        "You are translating the interface of an accessible app that reads documents aloud, used by people who \
         rely on screen readers. Translate each value in the JSON object below from British English into {target}.\n\n\
         Rules:\n\
         - Reply with only a JSON object that has exactly the same keys, with each value translated.\n\
         - Keep every name in curly braces, such as {{name}} or {{count}}, exactly as it is. Do not translate it.\n\
         - Keep these unchanged: key names such as Ctrl, Cmd, Esc, Escape, Tab, F5, F6 and F7; product names such as \
           Ollama, ElevenLabs, OpenAI, Deepgram, Speechify, OpenStreetMap and GitHub; and file types such as PDF, \
           TXT, DOCX, CSV, JPEG, HEIC, XML, MP3 and WAV.\n\
         - Keep line breaks, quotation marks and the … character where the English has them.\n\
         - Use plain, everyday words, and the form of address usual for software in that language.\n\
         - Translate every value, even short ones.\n\n\
         For example, into French, {{\"general.voice\": \"Voice\", \"file.opening\": \"Opening {{name}}.\"}} \
         becomes {{\"general.voice\": \"Voix\", \"file.opening\": \"Ouverture de {{name}}.\"}}\n\n\
         {input}"
    )
}

/// Translates one batch of keys with the Ollama model `model`. Returns the
/// translations that can be used; any the model got wrong are left out.
pub fn translate_batch(model: &str, language: &Language, keys: &[&'static str]) -> anyhow::Result<BTreeMap<String, String>> {
    let batch: BTreeMap<&str, &str> = keys.iter().filter_map(|k| Some((*k, english(k)?))).collect();
    let reply = crate::vision::generate(model, &prompt(language, &batch), Some(json!("json")))?;
    let reply: Value = serde_json::from_str(reply.trim()).context("the AI model did not reply with a translation")?;
    let Some(reply) = reply.as_object() else { bail!("the AI model did not reply with a translation") };
    let mut out = BTreeMap::new();
    for key in keys {
        match reply.get(*key).and_then(Value::as_str) {
            // Small models sometimes hand the English back. Leaving it out
            // shows the same text, and lets the next run try it again.
            Some(text) if english(key) == Some(text.trim()) => log::info!("{key} came back untranslated"),
            Some(text) if fits(key, text) => {
                out.insert((*key).to_owned(), text.trim().to_owned());
            }
            Some(_) => log::info!("translation of {key} dropped: placeholders changed"),
            None => log::info!("translation of {key} missing from the reply"),
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn map(pairs: &[(&str, &str)]) -> HashMap<String, String> {
        pairs.iter().map(|(k, v)| ((*k).to_owned(), (*v).to_owned())).collect()
    }

    #[test]
    fn keys_are_unique() {
        assert_eq!(english_map().len(), ENGLISH.len(), "a key appears twice in ENGLISH");
    }

    const SOURCES: [&str; 5] = [
        include_str!("app.rs"),
        include_str!("worker.rs"),
        include_str!("main.rs"),
        include_str!("audio.rs"),
        include_str!("speech/mod.rs"),
    ];

    #[test]
    fn every_key_used_in_the_code_has_english_text() {
        let call = regex::Regex::new(r#"\btf?\(\s*"([a-z_.]+)""#).unwrap();
        let mut count = 0;
        for source in SOURCES {
            for cap in call.captures_iter(source) {
                count += 1;
                assert!(english(&cap[1]).is_some(), "no English text for {}", &cap[1]);
            }
        }
        assert!(count > 100, "only found {count} uses of t() and tf()");
    }

    #[test]
    fn every_english_text_is_used() {
        for (key, _) in ENGLISH {
            let quoted = format!("\"{key}\"");
            assert!(SOURCES.iter().any(|s| s.contains(&quoted)), "{key} is not used anywhere");
        }
    }

    #[test]
    fn fills_in_values() {
        let name = "notes.pdf";
        assert_eq!(render(None, "file.opening", &[("name", &name)]), "Opening notes.pdf.");
        assert_eq!(
            render(None, "file.loaded", &[("name", &name), ("count", &12), ("key_name", &"Control")]),
            "Loaded notes.pdf, 12 words. Press F5 to read it aloud or Control+S to save it as audio."
        );
        // Unknown names and stray braces are left alone.
        assert_eq!(fill("{a} {b} {", &[("a", &1)]), "1 {b} {");
    }

    #[test]
    fn uses_the_translation_and_falls_back_to_english() {
        let fr = map(&[("file.opening", "Ouverture de {name}.")]);
        let name = "notes.pdf";
        assert_eq!(render(Some(&fr), "file.opening", &[("name", &name)]), "Ouverture de notes.pdf.");
        assert_eq!(render(Some(&fr), "general.voice", &[]), "Voice");
    }

    #[test]
    fn finds_placeholders() {
        assert_eq!(placeholders("{a} and {b_c}, not {Up} or {}"), BTreeSet::from(["a", "b_c"]));
    }

    #[test]
    fn drops_translations_that_lose_or_change_values() {
        let mut t = Translation::default();
        t.strings.insert("file.opening".into(), "Ouverture de {nom}.".into());
        t.strings.insert("general.voice".into(), "Voix".into());
        t.strings.insert("general.copy".into(), "   ".into());
        t.strings.insert("no.such.key".into(), "Rien".into());
        let usable = t.usable();
        assert_eq!(usable.len(), 1);
        assert_eq!(usable["general.voice"], "Voix");
        let missing = t.missing();
        assert!(missing.contains(&"file.opening"));
        assert!(missing.contains(&"general.copy"));
        assert!(!missing.contains(&"general.voice"));
        assert_eq!(missing.len(), ENGLISH.len() - 1);
    }

    #[test]
    fn saves_loads_and_lists_translations() {
        let dir = tempfile::tempdir().unwrap();
        let mut fr = Translation::new(&Language { code: "fr".into(), name: "Français".into(), english_name: "French".into() });
        fr.strings.insert("general.voice".into(), "Voix".into());
        save(dir.path(), "fr", &fr).unwrap();
        assert_eq!(load(dir.path(), "fr").unwrap().strings["general.voice"], "Voix");

        // A file for a language not in the built-in list is offered too.
        let eo = Translation { language: "Esperanto".into(), ..Translation::default() };
        save(dir.path(), "eo", &eo).unwrap();
        let languages = available(dir.path());
        assert_eq!(languages[0].code, ENGLISH_CODE);
        assert!(languages.iter().any(|l| l.code == "eo" && l.label() == "Esperanto"));
        assert_eq!(languages.iter().filter(|l| l.code == "fr").count(), 1);

        assert!(load(dir.path(), "../settings").is_err());
        assert!(save(dir.path(), "", &eo).is_err());
    }

    #[test]
    fn rejects_hidden_characters_and_overlong_text() {
        assert!(fits("general.voice", "Voix"));
        // Right-to-left override, which can make text read backwards.
        assert!(!fits("general.voice", "Vo\u{202e}ix"));
        assert!(!fits("general.voice", "Vo\u{200b}ix"));
        assert!(!fits("general.voice", "Vo\u{7}ix"));
        assert!(!fits("general.voice", "Vo\nix"));
        assert!(!fits("general.voice", &"x".repeat(100)));
        // A line break is fine where the English has one.
        assert!(fits("update.question", "{version} {app} {current}\n\n?"));
        assert_eq!(clean_name("  Esperanto "), Some("Esperanto".into()));
        assert_eq!(clean_name("Esper\u{202e}anto"), None);
        assert_eq!(clean_name(&"x".repeat(61)), None);
    }

    #[test]
    fn refuses_files_that_are_too_large() {
        let dir = tempfile::tempdir().unwrap();
        let big = format!("{{\"language\": \"Big\", \"made_with\": \"{}\"}}", "x".repeat(MAX_FILE_BYTES as usize));
        std::fs::write(file_for(dir.path(), "xx"), big).unwrap();
        assert!(load(dir.path(), "xx").is_err());
        assert!(available(dir.path()).iter().all(|l| l.code != "xx"));
    }

    #[test]
    fn labels_languages() {
        let fr = Language { code: "fr".into(), name: "Français".into(), english_name: "French".into() };
        assert_eq!(fr.label(), "Français (French)");
        let en = Language { code: "en".into(), name: "English".into(), english_name: "English".into() };
        assert_eq!(en.label(), "English");
    }

    /// Needs Ollama running. Uses the model named in `SPEECHOUT_OLLAMA_MODEL`,
    /// or gemma3:4b.
    #[test]
    #[ignore]
    fn translates_with_a_local_model() {
        let model = std::env::var("SPEECHOUT_OLLAMA_MODEL").unwrap_or_else(|_| "gemma3:4b".into());
        let fr = Language { code: "fr".into(), name: "Français".into(), english_name: "French".into() };
        let keys: Vec<&str> = ENGLISH.iter().map(|(k, _)| *k).take(BATCH_SIZE).collect();
        let done = translate_batch(&model, &fr, &keys).unwrap();
        for (key, text) in &done {
            println!("{key}: {text}");
        }
        assert!(done.len() >= BATCH_SIZE * 3 / 4, "only {} of {BATCH_SIZE} came back usable", done.len());
    }

    #[test]
    fn the_prompt_keeps_braces() {
        let fr = Language { code: "fr".into(), name: "Français".into(), english_name: "French".into() };
        let batch = BTreeMap::from([("file.opening", "Opening {name}.")]);
        let p = prompt(&fr, &batch);
        assert!(p.contains("such as {name} or {count}"));
        assert!(p.contains("French (Français)"));
        assert!(p.contains("\"file.opening\": \"Opening {name}.\""));
    }
}
