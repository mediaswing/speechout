//! Everything the app remembers, in one SQLite database in the user's
//! application data folder (`%APPDATA%\CommonSpeech` on Windows): settings,
//! API keys and installed wordlists.
//!
//! API keys are stored as plain text, because the database is meant to be
//! portable between computers and tools. They are protected by the folder's
//! permissions only; see SECURITY.md.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use anyhow::{Context, bail};
use rusqlite::{Connection, OptionalExtension, params};

use crate::speech::Provider;
use crate::wordlist::{self, Wordlist};

const SCHEMA_VERSION: i32 = 1;

/// How image descriptions are asked for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AiKind {
    Ollama,
    /// LM Studio, llama.cpp, Jan and others that copy OpenAI's API.
    OpenAiCompatible,
}

impl AiKind {
    pub const ALL: [AiKind; 2] = [AiKind::Ollama, AiKind::OpenAiCompatible];

    pub fn label(self) -> &'static str {
        match self {
            AiKind::Ollama => "Ollama",
            AiKind::OpenAiCompatible => "OpenAI-compatible server (LM Studio, llama.cpp, Jan)",
        }
    }

    pub fn default_address(self) -> &'static str {
        match self {
            AiKind::Ollama => "http://127.0.0.1:11434",
            AiKind::OpenAiCompatible => "http://127.0.0.1:1234",
        }
    }

    fn key(self) -> &'static str {
        match self {
            AiKind::Ollama => "ollama",
            AiKind::OpenAiCompatible => "openai-compatible",
        }
    }
}

/// The window's colours and font weight.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Appearance {
    Standard,
    HighContrast,
}

impl Appearance {
    pub const ALL: [Appearance; 2] = [Appearance::Standard, Appearance::HighContrast];

    fn key(self) -> &'static str {
        match self {
            Appearance::Standard => "standard",
            Appearance::HighContrast => "high-contrast",
        }
    }
}

/// Everything on the Settings tab apart from API keys, and the choices on the
/// General tab.
#[derive(Debug, Clone, PartialEq)]
pub struct Settings {
    pub provider: Provider,
    /// The voice chosen for each speech service, by the service's key.
    pub voices: BTreeMap<String, String>,
    pub ai_kind: AiKind,
    pub ai_address: String,
    pub ai_model: String,
    /// Look up where a geotagged photo was taken. Off unless asked for,
    /// because it sends the photo's rounded coordinates to OpenStreetMap.
    pub resolve_gps: bool,
    /// Where the debug log goes. `None` is the default folder.
    pub log_dir: Option<PathBuf>,
    pub appearance: Appearance,
    /// The whisper.cpp program chosen on the Settings tab. `None` finds it.
    pub whisper_program: Option<PathBuf>,
    /// Ask GitHub for the latest version number when the app starts.
    pub check_updates: bool,
    /// Save text over [`crate::speech::PART_CHARS`] characters as one MP3
    /// file, rather than a file for each part.
    pub stitch_parts: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            provider: Provider::System,
            voices: BTreeMap::new(),
            ai_kind: AiKind::Ollama,
            ai_address: AiKind::Ollama.default_address().to_owned(),
            ai_model: String::new(),
            resolve_gps: false,
            log_dir: None,
            appearance: Appearance::Standard,
            whisper_program: None,
            check_updates: true,
            stitch_parts: false,
        }
    }
}

/// An installed wordlist.
#[derive(Debug, Clone)]
pub struct Installed {
    pub id: i64,
    pub enabled: bool,
    pub list: Wordlist,
}

pub struct Store {
    conn: Connection,
    path: Option<PathBuf>,
}

/// The app's folder for its database and, by default, its logs: `%APPDATA%`
/// on Windows, `~/Library/Application Support` on macOS and `~/.config` on
/// Linux, each with a `CommonSpeech` folder.
pub fn data_dir() -> PathBuf {
    dirs::config_dir().unwrap_or_else(std::env::temp_dir).join("CommonSpeech")
}

pub fn default_db_path() -> PathBuf {
    data_dir().join("settings.sqlite3")
}

impl Store {
    /// Opens the database at `path`, creating it and its folder if needed.
    pub fn open(path: &Path) -> anyhow::Result<Self> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir).with_context(|| format!("could not create {}", dir.display()))?;
            crate::platform::restrict_dir_to_owner(dir);
        }
        crate::platform::create_private_file(path).with_context(|| format!("could not create {}", path.display()))?;
        let conn = Connection::open(path).with_context(|| format!("could not open {}", path.display()))?;
        let mut store = Self { conn, path: Some(path.to_owned()) };
        store.prepare().with_context(|| format!("could not read {}", path.display()))?;
        Ok(store)
    }

    /// A database that lasts until the app closes, used when the real one
    /// can't be opened, so the app still works, and by tests.
    pub fn in_memory() -> anyhow::Result<Self> {
        let mut store = Self { conn: Connection::open_in_memory()?, path: None };
        store.prepare()?;
        Ok(store)
    }

    /// Where the database is, or `None` if it won't outlast this session.
    pub fn path(&self) -> Option<&Path> {
        self.path.as_deref()
    }

    fn prepare(&mut self) -> anyhow::Result<()> {
        // secure_delete overwrites removed API keys rather than leaving them
        // in free pages; trusted_schema stops a tampered database's schema
        // from running SQL functions.
        self.conn.execute_batch("PRAGMA secure_delete = ON; PRAGMA trusted_schema = OFF;")?;
        let version: i32 = self.conn.query_row("PRAGMA user_version", [], |r| r.get(0))?;
        if version > SCHEMA_VERSION {
            bail!("it was made by a newer version of this app");
        }
        if version < 1 {
            self.conn.execute_batch(
                "BEGIN;
                 CREATE TABLE IF NOT EXISTS settings (key TEXT PRIMARY KEY, value TEXT NOT NULL) WITHOUT ROWID;
                 CREATE TABLE IF NOT EXISTS wordlists (
                     id INTEGER PRIMARY KEY,
                     name TEXT NOT NULL UNIQUE,
                     xml TEXT NOT NULL,
                     enabled INTEGER NOT NULL
                 );
                 PRAGMA user_version = 1;
                 COMMIT;",
            )?;
        }
        Ok(())
    }

    fn get(&self, key: &str) -> anyhow::Result<Option<String>> {
        Ok(self
            .conn
            .query_row("SELECT value FROM settings WHERE key = ?1", params![key], |r| r.get(0))
            .optional()?)
    }

    fn set(&self, key: &str, value: &str) -> anyhow::Result<()> {
        self.conn.execute(
            "INSERT INTO settings (key, value) VALUES (?1, ?2)
             ON CONFLICT(key) DO UPDATE SET value = excluded.value",
            params![key, value],
        )?;
        Ok(())
    }

    /// Reads the settings. A value this version doesn't understand, perhaps
    /// from a hand-edited database, falls back to its default.
    pub fn settings(&self) -> anyhow::Result<Settings> {
        let mut s = Settings::default();
        if let Some(v) = self.get("provider")? {
            s.provider = Provider::ALL.into_iter().find(|p| p.key() == v).unwrap_or(s.provider);
        }
        for provider in Provider::ALL {
            if let Some(voice) = self.get(&format!("voice.{}", provider.key()))? {
                s.voices.insert(provider.key().to_owned(), voice);
            }
        }
        if let Some(v) = self.get("ai_kind")? {
            s.ai_kind = AiKind::ALL.into_iter().find(|k| k.key() == v).unwrap_or(s.ai_kind);
        }
        s.ai_address = self.get("ai_address")?.unwrap_or_else(|| s.ai_kind.default_address().to_owned());
        s.ai_model = self.get("ai_model")?.unwrap_or_default();
        s.resolve_gps = self.get("resolve_gps")?.as_deref() == Some("1");
        s.log_dir = self.get("log_dir")?.filter(|v| !v.is_empty()).map(PathBuf::from);
        if let Some(v) = self.get("appearance")? {
            s.appearance = Appearance::ALL.into_iter().find(|a| a.key() == v).unwrap_or(s.appearance);
        }
        s.whisper_program = self.get("whisper_program")?.filter(|v| !v.is_empty()).map(PathBuf::from);
        s.check_updates = self.get("check_updates")?.as_deref() != Some("0");
        s.stitch_parts = self.get("stitch_parts")?.as_deref() == Some("1");
        Ok(s)
    }

    pub fn save_settings(&mut self, s: &Settings) -> anyhow::Result<()> {
        let tx = self.conn.unchecked_transaction()?;
        self.set("provider", s.provider.key())?;
        for provider in Provider::ALL {
            let key = format!("voice.{}", provider.key());
            match s.voices.get(provider.key()) {
                Some(voice) => self.set(&key, voice)?,
                None => {
                    self.conn.execute("DELETE FROM settings WHERE key = ?1", params![key])?;
                }
            }
        }
        self.set("ai_kind", s.ai_kind.key())?;
        self.set("ai_address", &s.ai_address)?;
        self.set("ai_model", &s.ai_model)?;
        self.set("resolve_gps", if s.resolve_gps { "1" } else { "0" })?;
        self.set("log_dir", &s.log_dir.as_deref().map(|d| d.display().to_string()).unwrap_or_default())?;
        self.set("appearance", s.appearance.key())?;
        self.set("whisper_program", &s.whisper_program.as_deref().map(|d| d.display().to_string()).unwrap_or_default())?;
        self.set("check_updates", if s.check_updates { "1" } else { "0" })?;
        self.set("stitch_parts", if s.stitch_parts { "1" } else { "0" })?;
        tx.commit()?;
        Ok(())
    }

    fn key_name(provider: Provider) -> String {
        format!("api_key.{}", provider.key())
    }

    pub fn api_key(&self, provider: Provider) -> anyhow::Result<Option<String>> {
        Ok(self.get(&Self::key_name(provider))?.filter(|k| !k.is_empty()))
    }

    pub fn set_api_key(&self, provider: Provider, key: &str) -> anyhow::Result<()> {
        self.set(&Self::key_name(provider), key)
    }

    pub fn remove_api_key(&self, provider: Provider) -> anyhow::Result<()> {
        self.conn.execute("DELETE FROM settings WHERE key = ?1", params![Self::key_name(provider)])?;
        Ok(())
    }

    /// Adds the example wordlists the first time the app runs. They are not
    /// added again, so one the user removed stays removed.
    pub fn install_examples(&self) -> anyhow::Result<()> {
        if self.get("examples_installed")?.is_some() {
            return Ok(());
        }
        let tx = self.conn.unchecked_transaction()?;
        for (xml, enabled) in wordlist::EXAMPLES {
            let list = wordlist::parse(xml)?;
            self.conn.execute(
                "INSERT OR IGNORE INTO wordlists (name, xml, enabled) VALUES (?1, ?2, ?3)",
                params![list.name, xml, enabled],
            )?;
        }
        self.set("examples_installed", "1")?;
        tx.commit()?;
        Ok(())
    }

    /// The installed wordlists, in order of name. One that no longer parses,
    /// perhaps because the database was edited by hand, is logged and left out.
    pub fn wordlists(&self) -> anyhow::Result<Vec<Installed>> {
        let mut statement =
            self.conn.prepare("SELECT id, enabled, xml, name FROM wordlists ORDER BY name COLLATE NOCASE")?;
        let rows = statement.query_map([], |r| {
            Ok((r.get::<_, i64>(0)?, r.get::<_, bool>(1)?, r.get::<_, String>(2)?))
        })?;
        let mut out = Vec::new();
        for row in rows {
            let (id, enabled, xml) = row?;
            match wordlist::parse(&xml) {
                Ok(list) => out.push(Installed { id, enabled, list }),
                Err(e) => log::warn!("Skipping stored wordlist {id}: {e:#}"),
            }
        }
        Ok(out)
    }

    /// Checks the XML is a wordlist and stores it, turned on. Returns its name.
    pub fn import_wordlist(&self, xml: &str) -> anyhow::Result<String> {
        let list = wordlist::parse(xml)?;
        let changed = self.conn.execute(
            "INSERT OR IGNORE INTO wordlists (name, xml, enabled) VALUES (?1, ?2, 1)",
            params![list.name, xml],
        )?;
        if changed == 0 {
            bail!("a wordlist called \"{}\" is already installed", list.name);
        }
        Ok(list.name)
    }

    pub fn set_wordlist_enabled(&self, id: i64, enabled: bool) -> anyhow::Result<()> {
        self.conn.execute("UPDATE wordlists SET enabled = ?2 WHERE id = ?1", params![id, enabled])?;
        Ok(())
    }

    pub fn remove_wordlist(&self, id: i64) -> anyhow::Result<()> {
        self.conn.execute("DELETE FROM wordlists WHERE id = ?1", params![id])?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const LIST: &str = r#"<wordlist name="Mine"><entry match="a" replace="b"/></wordlist>"#;

    #[test]
    fn settings_and_keys_survive_reopening() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("sub").join("settings.sqlite3");
        let mut wanted = Settings {
            provider: Provider::Deepgram,
            ai_kind: AiKind::OpenAiCompatible,
            ai_address: "http://localhost:8080".into(),
            ai_model: "moondream".into(),
            resolve_gps: true,
            log_dir: Some(PathBuf::from("/tmp/logs")),
            appearance: Appearance::HighContrast,
            whisper_program: Some(PathBuf::from("/opt/whisper/whisper-cli")),
            check_updates: false,
            stitch_parts: true,
            ..Settings::default()
        };
        wanted.voices.insert(Provider::Deepgram.key().into(), "aura-2-thalia-en".into());
        {
            let mut store = Store::open(&path).unwrap();
            store.save_settings(&wanted).unwrap();
            store.set_api_key(Provider::ElevenLabs, "secret'; DROP TABLE settings;--").unwrap();
        }
        let store = Store::open(&path).unwrap();
        assert_eq!(store.settings().unwrap(), wanted);
        assert_eq!(store.api_key(Provider::ElevenLabs).unwrap().unwrap(), "secret'; DROP TABLE settings;--");
        store.remove_api_key(Provider::ElevenLabs).unwrap();
        assert_eq!(store.api_key(Provider::ElevenLabs).unwrap(), None);
    }

    #[test]
    fn removed_key_does_not_stay_in_the_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.sqlite3");
        let store = Store::open(&path).unwrap();
        store.set_api_key(Provider::OpenAi, "sk-very-recognisable-key").unwrap();
        store.remove_api_key(Provider::OpenAi).unwrap();
        drop(store);
        let bytes = std::fs::read(&path).unwrap();
        assert!(!bytes.windows(10).any(|w| w == b"recognisab"));
    }

    #[cfg(unix)]
    #[test]
    fn database_is_private() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("x").join("settings.sqlite3");
        Store::open(&path).unwrap();
        assert_eq!(std::fs::metadata(&path).unwrap().permissions().mode() & 0o777, 0o600);
        assert_eq!(std::fs::metadata(path.parent().unwrap()).unwrap().permissions().mode() & 0o777, 0o700);
    }

    #[test]
    fn unknown_values_fall_back_to_defaults() {
        let store = Store::in_memory().unwrap();
        store.set("provider", "nonsense").unwrap();
        store.set("appearance", "nonsense").unwrap();
        assert_eq!(store.settings().unwrap(), Settings::default());
    }

    #[test]
    fn refuses_a_database_from_a_newer_version() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.sqlite3");
        Connection::open(&path).unwrap().execute_batch("PRAGMA user_version = 99").unwrap();
        assert!(Store::open(&path).is_err());
    }

    #[test]
    fn examples_install_once_and_stay_removed() {
        let store = Store::in_memory().unwrap();
        store.install_examples().unwrap();
        let lists = store.wordlists().unwrap();
        assert_eq!(lists.len(), wordlist::EXAMPLES.len());
        // Changing what is read aloud without asking would surprise people.
        assert!(!lists.iter().find(|l| l.list.name == "Classroom safe").unwrap().enabled);
        store.remove_wordlist(lists[0].id).unwrap();
        store.install_examples().unwrap();
        assert_eq!(store.wordlists().unwrap().len(), wordlist::EXAMPLES.len() - 1);
    }

    #[test]
    fn wordlists_import_toggle_and_remove() {
        let store = Store::in_memory().unwrap();
        assert_eq!(store.import_wordlist(LIST).unwrap(), "Mine");
        assert!(store.import_wordlist(LIST).unwrap_err().to_string().contains("already installed"));
        assert!(store.import_wordlist("<nonsense/>").is_err());
        let id = store.wordlists().unwrap()[0].id;
        assert!(store.wordlists().unwrap()[0].enabled);
        store.set_wordlist_enabled(id, false).unwrap();
        assert!(!store.wordlists().unwrap()[0].enabled);
        store.remove_wordlist(id).unwrap();
        assert!(store.wordlists().unwrap().is_empty());
    }
}
