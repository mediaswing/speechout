//! Cloud text-to-speech services. All requests use HTTPS, and API keys are
//! only ever sent to the service they belong to.

use super::retry::{self, Kind, ServiceError};
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

/// Turns an HTTP error into a message suitable for showing the user, and
/// records whether it is worth retrying. The key is never included.
fn check(provider: Provider, mut response: ureq::http::Response<ureq::Body>) -> anyhow::Result<ureq::http::Response<ureq::Body>> {
    let status = response.status().as_u16();
    if (200..300).contains(&status) {
        return Ok(response);
    }
    let retry_after = response
        .headers()
        .get("retry-after")
        .and_then(|v| v.to_str().ok())
        .and_then(retry::parse_retry_after);
    let detail = response
        .body_mut()
        .with_config()
        .limit(16 * 1024)
        .read_to_string()
        .unwrap_or_default();
    let kind = retry::classify(status, &detail, retry_after);
    log::warn!("{} returned HTTP {status} ({kind:?})", provider.label());
    let reason = match (status, kind) {
        _ if retry::is_out_of_credit(&detail) => {
            "the account has run out of credit or reached its usage limit.".to_owned()
        }
        (401 | 403, _) => "the API key was rejected. Check it in Settings.".to_owned(),
        (402, _) => "the account has run out of credit.".to_owned(),
        (_, Kind::RateLimited { .. }) => "too many requests. Wait a moment and try again.".to_owned(),
        (_, Kind::Unavailable { .. }) => {
            format!("the service is busy or unavailable (HTTP error {status}). Try again later.")
        }
        _ => {
            let short: String = detail.chars().filter(|c| !c.is_control()).take(200).collect();
            format!("HTTP error {status}. {short}")
        }
    };
    Err(ServiceError { kind, message: format!("{} reported a problem: {reason}", provider.label()) }.into())
}

/// A request or response that failed at the network level. The technical
/// cause is kept for the debug log; the user sees the plain message.
fn net_err(provider: Provider, error: ureq::Error) -> anyhow::Error {
    let kind = net_kind(&error);
    let name = provider.label();
    let message = match kind {
        Kind::Unreachable => format!("could not reach {name}. Check your internet connection."),
        Kind::Interrupted => format!("the connection to {name} was lost. Check your internet connection."),
        _ => format!("{name} sent a reply that could not be used."),
    };
    anyhow::Error::from(error).context(ServiceError { kind, message })
}

fn net_kind(error: &ureq::Error) -> Kind {
    use std::io::ErrorKind;
    use ureq::Timeout;
    match error {
        // The request was never delivered, so retrying cannot cost anything.
        // ureq reports a refused or unroutable connection as an I/O error.
        ureq::Error::HostNotFound
        | ureq::Error::ConnectionFailed
        | ureq::Error::Timeout(Timeout::Resolve | Timeout::Connect) => Kind::Unreachable,
        ureq::Error::Io(e)
            if matches!(
                e.kind(),
                ErrorKind::ConnectionRefused
                    | ErrorKind::HostUnreachable
                    | ErrorKind::NetworkUnreachable
                    | ErrorKind::AddrNotAvailable
            ) =>
        {
            Kind::Unreachable
        }
        // The connection broke part way through.
        ureq::Error::Timeout(_) | ureq::Error::Io(_) => Kind::Interrupted,
        // Anything else, such as an oversized or malformed reply.
        _ => Kind::Permanent,
    }
}

pub fn list_voices(provider: Provider, key: &str) -> anyhow::Result<Vec<Voice>> {
    match provider {
        Provider::ElevenLabs => {
            let resp = agent()
                .get("https://api.elevenlabs.io/v1/voices")
                .header("xi-api-key", key)
                .call()
                .map_err(|e| net_err(provider, e))?;
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
                .map_err(|e| net_err(provider, e))?;
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

/// `speed` is 1.0 for normal speed, or one of `provider.speed_choices()`.
pub fn synthesize(provider: Provider, key: &str, voice_id: &str, text: &str, speed: f32) -> anyhow::Result<Vec<u8>> {
    let voice = safe_id(voice_id)?;
    // Only send a speed when it differs from normal, so the request is
    // exactly what the service would get without the setting.
    let speed = provider.clamp_speed(speed);
    let custom_speed = speed != 1.0;
    let resp = match provider {
        Provider::ElevenLabs => {
            let mut body = json!({ "text": text, "model_id": "eleven_multilingual_v2" });
            if custom_speed {
                body["voice_settings"] = json!({ "speed": speed });
            }
            agent()
                .post(&format!("https://api.elevenlabs.io/v1/text-to-speech/{voice}"))
                .query("output_format", "mp3_44100_128")
                .header("xi-api-key", key)
                .send_json(body)
        }
        Provider::OpenAi => agent()
            .post("https://api.openai.com/v1/audio/speech")
            .header("Authorization", &format!("Bearer {key}"))
            .send_json(json!({
                "model": "gpt-4o-mini-tts",
                "input": text,
                "voice": voice,
                "response_format": "mp3",
            })),
        Provider::Deepgram => {
            let mut request = agent()
                .post("https://api.deepgram.com/v1/speak")
                .query("model", voice)
                .query("encoding", "mp3");
            if custom_speed {
                request = request.query("speed", format!("{speed}"));
            }
            request.header("Authorization", &format!("Token {key}")).send_json(json!({ "text": text }))
        }
        Provider::Speechify => agent()
            .post("https://api.sws.speechify.com/v1/audio/speech")
            .header("Authorization", &format!("Bearer {key}"))
            .send_json(json!({ "input": text, "voice_id": voice, "audio_format": "mp3" })),
        Provider::System => unreachable!("system voices are not cloud voices"),
    }
    .map_err(|e| net_err(provider, e))?;

    let mut resp = check(provider, resp)?;
    if provider == Provider::Speechify {
        let body: Value = resp
            .body_mut()
            .with_config()
            .limit(MAX_AUDIO_BYTES)
            .read_json()
            .map_err(|e| net_err(provider, e))?;
        let data = body["audio_data"].as_str().context("Speechify returned no audio")?;
        return base64::engine::general_purpose::STANDARD
            .decode(data)
            .context("Speechify returned audio that could not be decoded");
    }
    resp.body_mut().with_config().limit(MAX_AUDIO_BYTES).read_to_vec().map_err(|e| net_err(provider, e))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io;

    #[test]
    fn sorts_network_failures() {
        let io_err = |kind| ureq::Error::Io(io::Error::from(kind));
        assert_eq!(net_kind(&ureq::Error::HostNotFound), Kind::Unreachable);
        assert_eq!(net_kind(&ureq::Error::Timeout(ureq::Timeout::Connect)), Kind::Unreachable);
        assert_eq!(net_kind(&io_err(io::ErrorKind::ConnectionRefused)), Kind::Unreachable);
        assert_eq!(net_kind(&io_err(io::ErrorKind::NetworkUnreachable)), Kind::Unreachable);
        assert_eq!(net_kind(&io_err(io::ErrorKind::ConnectionReset)), Kind::Interrupted);
        assert_eq!(net_kind(&ureq::Error::Timeout(ureq::Timeout::RecvBody)), Kind::Interrupted);
        assert_eq!(net_kind(&ureq::Error::BodyExceedsLimit(1)), Kind::Permanent);
    }

    #[test]
    fn messages_match_the_failure() {
        let msg = |e| net_err(Provider::OpenAi, e).to_string();
        assert!(msg(ureq::Error::HostNotFound).starts_with("could not reach"));
        assert!(msg(ureq::Error::BodyExceedsLimit(1)).contains("could not be used"));
    }
}
