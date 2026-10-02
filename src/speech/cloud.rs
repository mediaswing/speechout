//! Cloud text-to-speech services. All requests use HTTPS, and API keys are
//! only ever sent to the service they belong to.

use super::{Provider, Voice};
use anyhow::{Context, bail};
use base64::Engine;
use serde_json::{Value, json};
use std::sync::OnceLock;
use std::time::Duration;

/// Largest audio response accepted for a single chunk.
const MAX_AUDIO_BYTES: u64 = 64 * 1024 * 1024;

fn agent() -> &'static ureq::Agent {
    static AGENT: OnceLock<ureq::Agent> = OnceLock::new();
    AGENT.get_or_init(|| {
        ureq::Agent::config_builder()
            .https_only(true)
            // None of these APIs redirect. Refusing redirects guarantees that a
            // key in a custom header (such as xi-api-key) cannot be forwarded
            // to another host.
            .max_redirects(0)
            .http_status_as_error(false)
            .timeout_connect(Some(Duration::from_secs(15)))
            .timeout_global(Some(Duration::from_secs(180)))
            .user_agent(concat!("speechout/", env!("CARGO_PKG_VERSION")))
            .build()
            .into()
    })
}

const OPENAI_VOICES: &[&str] = &[
    "alloy", "ash", "ballad", "cedar", "coral", "echo", "fable", "marin", "nova", "onyx", "sage",
    "shimmer", "verse",
];

const DEEPGRAM_VOICES: &[(&str, &str)] = &[
    ("aura-2-thalia-en", "Thalia (American English, feminine)"),
    ("aura-2-andromeda-en", "Andromeda (American English, feminine)"),
    ("aura-2-helena-en", "Helena (American English, feminine)"),
    ("aura-2-apollo-en", "Apollo (American English, masculine)"),
    ("aura-2-arcas-en", "Arcas (American English, masculine)"),
    ("aura-2-aries-en", "Aries (American English, masculine)"),
    ("aura-2-asteria-en", "Asteria (American English, feminine)"),
    ("aura-2-athena-en", "Athena (American English, feminine)"),
    ("aura-2-atlas-en", "Atlas (American English, masculine)"),
    ("aura-2-draco-en", "Draco (British English, masculine)"),
    ("aura-2-hyperion-en", "Hyperion (Australian English, masculine)"),
    ("aura-2-luna-en", "Luna (American English, feminine)"),
    ("aura-2-orion-en", "Orion (American English, masculine)"),
    ("aura-2-pandora-en", "Pandora (British English, feminine)"),
    ("aura-2-theia-en", "Theia (Australian English, feminine)"),
    ("aura-2-zeus-en", "Zeus (American English, masculine)"),
];

/// Turns an HTTP error into a message suitable for showing the user. The key
/// is never included.
fn check(provider: Provider, mut response: ureq::http::Response<ureq::Body>) -> anyhow::Result<ureq::http::Response<ureq::Body>> {
    let status = response.status().as_u16();
    if (200..300).contains(&status) {
        return Ok(response);
    }
    let detail = response
        .body_mut()
        .with_config()
        .limit(16 * 1024)
        .read_to_string()
        .unwrap_or_default();
    log::warn!("{} returned HTTP {status}", provider.label());
    let reason = match status {
        401 | 403 => "the API key was rejected. Check it in Settings.".to_owned(),
        402 => "the account has run out of credit.".to_owned(),
        429 => "too many requests or quota exceeded. Wait a moment and try again.".to_owned(),
        _ => {
            let short: String = detail.chars().filter(|c| !c.is_control()).take(200).collect();
            format!("HTTP error {status}. {short}")
        }
    };
    bail!("{} reported a problem: {reason}", provider.label())
}

fn net_err(provider: Provider) -> String {
    format!("could not reach {}. Check your internet connection.", provider.label())
}

pub fn list_voices(provider: Provider, key: &str) -> anyhow::Result<Vec<Voice>> {
    match provider {
        Provider::ElevenLabs => {
            let resp = agent()
                .get("https://api.elevenlabs.io/v1/voices")
                .header("xi-api-key", key)
                .call()
                .with_context(|| net_err(provider))?;
            let body: Value = check(provider, resp)?.body_mut().read_json()?;
            Ok(body["voices"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|v| {
                    let id = v["voice_id"].as_str()?;
                    let name = v["name"].as_str()?;
                    let accent = v["labels"]["accent"].as_str().unwrap_or("");
                    let name = if accent.is_empty() { name.to_owned() } else { format!("{name} ({accent})") };
                    Some(Voice { id: id.to_owned(), name })
                })
                .collect())
        }
        Provider::Speechify => {
            let resp = agent()
                .get("https://api.sws.speechify.com/v1/voices")
                .header("Authorization", &format!("Bearer {key}"))
                .call()
                .with_context(|| net_err(provider))?;
            let body: Value = check(provider, resp)?.body_mut().read_json()?;
            let list = body.as_array().or_else(|| body["voices"].as_array());
            Ok(list
                .into_iter()
                .flatten()
                .filter_map(|v| {
                    let id = v["id"].as_str()?;
                    let name = v["display_name"].as_str().unwrap_or(id);
                    let locale = v["locale"].as_str().unwrap_or("");
                    let name = if locale.is_empty() { name.to_owned() } else { format!("{name} ({locale})") };
                    Some(Voice { id: id.to_owned(), name })
                })
                .collect())
        }
        Provider::OpenAi => Ok(OPENAI_VOICES
            .iter()
            .map(|v| {
                let mut name = v.to_string();
                name[..1].make_ascii_uppercase();
                Voice { id: v.to_string(), name }
            })
            .collect()),
        Provider::Deepgram => Ok(DEEPGRAM_VOICES
            .iter()
            .map(|(id, name)| Voice { id: id.to_string(), name: name.to_string() })
            .collect()),
        Provider::System => unreachable!("system voices are not cloud voices"),
    }
}

/// Voice IDs are put into URLs, so only allow the characters the services use.
fn safe_id(id: &str) -> anyhow::Result<&str> {
    if !id.is_empty() && id.len() <= 128 && id.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_') {
        Ok(id)
    } else {
        bail!("choose a voice first")
    }
}

pub fn synthesize(provider: Provider, key: &str, voice_id: &str, text: &str) -> anyhow::Result<Vec<u8>> {
    let voice = safe_id(voice_id)?;
    let resp = match provider {
        Provider::ElevenLabs => agent()
            .post(&format!("https://api.elevenlabs.io/v1/text-to-speech/{voice}"))
            .query("output_format", "mp3_44100_128")
            .header("xi-api-key", key)
            .send_json(json!({ "text": text, "model_id": "eleven_multilingual_v2" })),
        Provider::OpenAi => agent()
            .post("https://api.openai.com/v1/audio/speech")
            .header("Authorization", &format!("Bearer {key}"))
            .send_json(json!({
                "model": "gpt-4o-mini-tts",
                "input": text,
                "voice": voice,
                "response_format": "mp3",
            })),
        Provider::Deepgram => agent()
            .post("https://api.deepgram.com/v1/speak")
            .query("model", voice)
            .query("encoding", "mp3")
            .header("Authorization", &format!("Token {key}"))
            .send_json(json!({ "text": text })),
        Provider::Speechify => agent()
            .post("https://api.sws.speechify.com/v1/audio/speech")
            .header("Authorization", &format!("Bearer {key}"))
            .send_json(json!({ "input": text, "voice_id": voice, "audio_format": "mp3" })),
        Provider::System => unreachable!("system voices are not cloud voices"),
    }
    .with_context(|| net_err(provider))?;

    let mut resp = check(provider, resp)?;
    if provider == Provider::Speechify {
        let body: Value = resp.body_mut().with_config().limit(MAX_AUDIO_BYTES).read_json()?;
        let data = body["audio_data"].as_str().context("Speechify returned no audio")?;
        return base64::engine::general_purpose::STANDARD
            .decode(data)
            .context("Speechify returned audio that could not be decoded");
    }
    Ok(resp.body_mut().with_config().limit(MAX_AUDIO_BYTES).read_to_vec()?)
}
