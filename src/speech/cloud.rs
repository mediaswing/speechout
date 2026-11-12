//! Cloud text-to-speech services. Requests only ever use HTTPS, redirects are
//! refused so a key can't be forwarded to another host, and a key is only sent
//! to the service it belongs to. Keys never appear in errors or logs.

use std::sync::OnceLock;
use std::time::Duration;

use anyhow::{Context, bail};
use base64::Engine;
use serde_json::{Value, json};

use super::{Provider, Voice, text};

/// The largest reply accepted for one piece of speech.
const MAX_AUDIO_BYTES: u64 = 64 * 1024 * 1024;
const MAX_VOICE_LIST_BYTES: u64 = 8 * 1024 * 1024;
/// How many times to try a request that failed for a reason that may pass.
const ATTEMPTS: u32 = 3;

fn agent() -> &'static ureq::Agent {
    static AGENT: OnceLock<ureq::Agent> = OnceLock::new();
    AGENT.get_or_init(|| {
        ureq::Agent::config_builder()
            .https_only(true)
            .max_redirects(0)
            .http_status_as_error(false)
            .timeout_connect(Some(Duration::from_secs(15)))
            .timeout_global(Some(Duration::from_secs(180)))
            .user_agent(concat!("commonspeech/", env!("CARGO_PKG_VERSION")))
            .build()
            .into()
    })
}

const OPENAI_VOICES: &[&str] = &[
    "alloy", "ash", "ballad", "cedar", "coral", "echo", "fable", "marin", "nova", "onyx", "sage", "shimmer", "verse",
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

/// A failed reply, as a message for the user. What the service said is neither
/// shown nor logged, because it could echo back part of the key or the text.
fn explain(provider: Provider, status: u16) -> String {
    let name = provider.label();
    match status {
        401 | 403 => format!("{name} rejected the API key. Check it on the Settings tab"),
        402 => format!("the {name} account has run out of credit"),
        429 => format!("{name} says there have been too many requests, or the account has reached its limit"),
        500..=599 => format!("{name} is busy or unavailable (HTTP error {status}). Try again later"),
        _ => format!("{name} refused the request (HTTP error {status})"),
    }
}

fn worth_retrying(status: u16) -> bool {
    status == 429 || (500..=599).contains(&status)
}

/// A request that failed, and whether trying it again could help.
struct Failure {
    error: anyhow::Error,
    worth_retrying: bool,
}

/// Runs `attempt` until it works, it fails for good or `ATTEMPTS` run out, calling
/// `pause` with a longer wait each time before trying again.
fn with_retries<T>(mut attempt: impl FnMut() -> Result<T, Failure>, mut pause: impl FnMut(Duration)) -> anyhow::Result<T> {
    let mut number = 1;
    loop {
        match attempt() {
            Ok(value) => return Ok(value),
            Err(failure) if failure.worth_retrying && number < ATTEMPTS => {
                log::info!("{:#}; trying again", failure.error);
                pause(Duration::from_secs(u64::from(number) * 2));
                number += 1;
            }
            Err(failure) => return Err(failure.error),
        }
    }
}

type Reply = ureq::http::Response<ureq::Body>;

fn checked(provider: Provider, reply: Result<Reply, ureq::Error>) -> Result<Reply, Failure> {
    let reply = reply.map_err(|e| {
        log::warn!("{} request failed: {e}", provider.label());
        let error = anyhow::anyhow!("could not reach {}. Check your internet connection", provider.label());
        Failure { error, worth_retrying: false }
    })?;
    let status = reply.status().as_u16();
    if !(200..300).contains(&status) {
        log::warn!("{} returned HTTP {status}", provider.label());
        return Err(Failure { error: anyhow::anyhow!(explain(provider, status)), worth_retrying: worth_retrying(status) });
    }
    Ok(reply)
}

pub fn list_voices(provider: Provider, key: &str) -> anyhow::Result<Vec<Voice>> {
    match provider {
        Provider::ElevenLabs => {
            let reply = agent().get("https://api.elevenlabs.io/v1/voices").header("xi-api-key", key).call();
            Ok(elevenlabs_voices(&voice_json(provider, reply)?))
        }
        Provider::Speechify => {
            let reply = agent()
                .get("https://api.sws.speechify.com/v1/voices")
                .header("Authorization", &format!("Bearer {key}"))
                .call();
            Ok(speechify_voices(&voice_json(provider, reply)?))
        }
        Provider::OpenAi => Ok(OPENAI_VOICES
            .iter()
            .map(|v| {
                let mut name = v.to_string();
                name[..1].make_ascii_uppercase();
                Voice { id: v.to_string(), name }
            })
            .collect()),
        Provider::Deepgram => {
            Ok(DEEPGRAM_VOICES.iter().map(|(id, name)| Voice { id: id.to_string(), name: name.to_string() }).collect())
        }
        Provider::System => bail!("system voices are not cloud voices"),
    }
}

fn voice_json(provider: Provider, reply: Result<Reply, ureq::Error>) -> anyhow::Result<Value> {
    let mut reply = checked(provider, reply).map_err(|failure| failure.error)?;
    reply
        .body_mut()
        .with_config()
        .limit(MAX_VOICE_LIST_BYTES)
        .read_json()
        .with_context(|| format!("{} sent a list of voices that could not be understood", provider.label()))
}

fn elevenlabs_voices(body: &Value) -> Vec<Voice> {
    let voices = body["voices"].as_array().into_iter().flatten();
    voices
        .filter_map(|v| {
            let id = safe_id(text(&v["voice_id"])?).ok()?;
            let name = text(&v["name"])?;
            Some(Voice { id: id.to_owned(), name: with_detail(name, v["labels"]["accent"].as_str()) })
        })
        .collect()
}

fn speechify_voices(body: &Value) -> Vec<Voice> {
    let list = body.as_array().or_else(|| body["voices"].as_array());
    list.into_iter()
        .flatten()
        .filter_map(|v| {
            let id = safe_id(text(&v["id"])?).ok()?;
            let name = text(&v["display_name"]).unwrap_or(id);
            Some(Voice { id: id.to_owned(), name: with_detail(name, v["locale"].as_str()) })
        })
        .collect()
}

/// A voice's name for the dropdown, cleaned because it came from a server.
fn with_detail(name: &str, detail: Option<&str>) -> String {
    let name = match detail.filter(|d| !d.is_empty()) {
        Some(detail) => format!("{name} ({detail})"),
        None => name.to_owned(),
    };
    crate::wordlist::clean_display_text(&name).chars().take(200).collect()
}

/// Voice IDs go into URLs, so only allow the characters the services use.
fn safe_id(id: &str) -> anyhow::Result<&str> {
    if !id.is_empty() && id.len() <= 128 && id.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_') {
        Ok(id)
    } else {
        bail!("choose a voice first")
    }
}

/// A request to a service, kept apart from sending it so it can be tested.
#[derive(Debug)]
struct Request {
    url: String,
    query: Vec<(&'static str, String)>,
    auth: (&'static str, String),
    body: Value,
}

fn speech_request(provider: Provider, key: &str, voice: &str, text: &str) -> anyhow::Result<Request> {
    let voice = safe_id(voice)?;
    let bearer = || ("Authorization", format!("Bearer {key}"));
    Ok(match provider {
        Provider::ElevenLabs => Request {
            url: format!("https://api.elevenlabs.io/v1/text-to-speech/{voice}"),
            query: vec![("output_format", "mp3_44100_128".to_owned())],
            auth: ("xi-api-key", key.to_owned()),
            body: json!({ "text": text, "model_id": "eleven_multilingual_v2" }),
        },
        Provider::OpenAi => Request {
            url: "https://api.openai.com/v1/audio/speech".to_owned(),
            query: Vec::new(),
            auth: bearer(),
            body: json!({ "model": "gpt-4o-mini-tts", "input": text, "voice": voice, "response_format": "mp3" }),
        },
        Provider::Deepgram => Request {
            url: "https://api.deepgram.com/v1/speak".to_owned(),
            query: vec![("model", voice.to_owned()), ("encoding", "mp3".to_owned())],
            auth: ("Authorization", format!("Token {key}")),
            body: json!({ "text": text }),
        },
        Provider::Speechify => Request {
            url: "https://api.sws.speechify.com/v1/audio/speech".to_owned(),
            query: Vec::new(),
            auth: bearer(),
            body: json!({ "input": text, "voice_id": voice, "audio_format": "mp3" }),
        },
        Provider::System => bail!("system voices are not cloud voices"),
    })
}

/// Sends one piece of text and returns MP3 audio.
pub fn synthesize(provider: Provider, key: &str, voice: &str, text: &str) -> anyhow::Result<Vec<u8>> {
    let request = speech_request(provider, key, voice, text)?;
    with_retries(|| send_speech_request(provider, &request), std::thread::sleep)
}

fn send_speech_request(provider: Provider, request: &Request) -> Result<Vec<u8>, Failure> {
    let mut call = agent().post(&request.url).header(request.auth.0, &request.auth.1);
    for (name, value) in &request.query {
        call = call.query(*name, value);
    }
    let mut reply = checked(provider, call.send_json(&request.body))?;
    let body = reply.body_mut().with_config().limit(MAX_AUDIO_BYTES);
    let permanent = |error| Failure { error, worth_retrying: false };
    if provider == Provider::Speechify {
        let json: Value = body
            .read_json()
            .context("Speechify sent a reply that could not be understood")
            .map_err(permanent)?;
        return speechify_audio(&json).map_err(permanent);
    }
    body.read_to_vec().with_context(|| format!("the connection to {} was lost", provider.label())).map_err(permanent)
}

fn speechify_audio(body: &Value) -> anyhow::Result<Vec<u8>> {
    let data = body["audio_data"].as_str().context("Speechify returned no audio")?;
    base64::engine::general_purpose::STANDARD.decode(data).context("Speechify returned audio that could not be decoded")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn requests_send_keys_only_to_their_own_service_over_https() {
        let hosts = [
            (Provider::ElevenLabs, "https://api.elevenlabs.io/", "xi-api-key", "KEY"),
            (Provider::OpenAi, "https://api.openai.com/", "Authorization", "Bearer KEY"),
            (Provider::Deepgram, "https://api.deepgram.com/", "Authorization", "Token KEY"),
            (Provider::Speechify, "https://api.sws.speechify.com/", "Authorization", "Bearer KEY"),
        ];
        for (provider, host, header, value) in hosts {
            let request = speech_request(provider, "KEY", "voice-1", "Words").unwrap();
            assert!(request.url.starts_with(host), "{provider:?}");
            assert_eq!(request.auth, (header, value.to_owned()), "{provider:?}");
            assert!(!request.url.contains("KEY") && !request.body.to_string().contains("KEY"), "{provider:?}");
            assert!(request.body.to_string().contains("Words"), "{provider:?}");
        }
    }

    #[test]
    fn voice_ids_cannot_change_the_url() {
        for bad in ["", "../../v1/user", "a/b", "a?x=1", "a b", "a#b", "é"] {
            assert!(speech_request(Provider::ElevenLabs, "k", bad, "t").is_err(), "{bad}");
        }
        assert!(speech_request(Provider::System, "k", "v", "t").is_err());
    }

    #[test]
    fn errors_do_not_contain_what_the_service_said() {
        assert!(explain(Provider::OpenAi, 401).contains("API key"));
        assert!(explain(Provider::OpenAi, 503).contains("503"));
        assert!(worth_retrying(429) && worth_retrying(503) && !worth_retrying(401));
    }

    fn failure(worth_retrying: bool) -> Failure {
        Failure { error: anyhow::anyhow!("it failed"), worth_retrying }
    }

    /// Runs `with_retries` over these results, giving the outcome, how many
    /// attempts it made and the waits between them.
    fn retried(results: Vec<Result<u8, Failure>>) -> (anyhow::Result<u8>, usize, Vec<u64>) {
        let (mut results, mut attempts, mut waits) = (results.into_iter(), 0, Vec::new());
        let outcome = with_retries(
            || {
                attempts += 1;
                results.next().expect("no more attempts than results")
            },
            |wait| waits.push(wait.as_secs()),
        );
        (outcome, attempts, waits)
    }

    #[test]
    fn retries_what_may_pass_with_longer_waits_then_gives_up() {
        let (outcome, attempts, waits) = retried(vec![Err(failure(true)), Err(failure(true)), Ok(7)]);
        assert_eq!(outcome.unwrap(), 7);
        assert_eq!((attempts, waits), (3, vec![2, 4]));

        let (outcome, attempts, waits) = retried(vec![Err(failure(true)), Err(failure(true)), Err(failure(true))]);
        assert!(outcome.is_err());
        assert_eq!((attempts, waits), (ATTEMPTS as usize, vec![2, 4]));
    }

    #[test]
    fn does_not_retry_what_will_not_pass() {
        let (outcome, attempts, waits) = retried(vec![Err(failure(false))]);
        assert!(outcome.is_err());
        assert_eq!((attempts, waits), (1, Vec::new()));
    }

    #[test]
    fn reads_voice_lists_and_skips_unsafe_ids() {
        let body = json!({"voices": [
            {"voice_id": "abc", "name": "Rachel", "labels": {"accent": "American"}},
            {"voice_id": "../evil", "name": "Bad"},
            {"voice_id": "def", "name": "Sam\u{202E}\n"},
        ]});
        let voices = elevenlabs_voices(&body);
        assert_eq!(voices.len(), 2);
        assert_eq!(voices[0], Voice { id: "abc".into(), name: "Rachel (American)".into() });
        assert_eq!(voices[1].name, "Sam");

        let voices = speechify_voices(&json!([{"id": "x1", "display_name": "George", "locale": "en-GB"}]));
        assert_eq!(voices, [Voice { id: "x1".into(), name: "George (en-GB)".into() }]);
        assert_eq!(speechify_voices(&json!({"voices": [{"id": "x2"}]}))[0].name, "x2");
    }

    #[test]
    fn built_in_lists_need_no_network() {
        assert!(!list_voices(Provider::OpenAi, "").unwrap().is_empty());
        assert!(!list_voices(Provider::Deepgram, "").unwrap().is_empty());
        assert_eq!(list_voices(Provider::OpenAi, "").unwrap()[0].name, "Alloy");
    }

    #[test]
    fn decodes_speechify_audio() {
        assert_eq!(speechify_audio(&json!({"audio_data": "aGk="})).unwrap(), b"hi");
        assert!(speechify_audio(&json!({"audio_data": "!!"})).is_err());
        assert!(speechify_audio(&json!({})).is_err());
    }
}
