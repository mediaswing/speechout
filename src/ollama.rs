//! Talking to Ollama, the AI model server that runs on this computer: listing
//! and downloading its models, and asking a model for an answer. Photo
//! descriptions (see `vision`) and translations of the interface (see `i18n`)
//! both use it. Every request goes to the Ollama server on this computer,
//! never anywhere else. Installing and starting Ollama differ by platform;
//! see `platform`.

use anyhow::{Context, bail};
use serde_json::{Value, json};
use std::sync::OnceLock;
use std::time::Duration;

pub const OLLAMA_URL: &str = "http://127.0.0.1:11434";

/// The connection to the Ollama server, which also serves requests made
/// elsewhere, such as describing a photo.
pub fn agent() -> &'static ureq::Agent {
    static AGENT: OnceLock<ureq::Agent> = OnceLock::new();
    AGENT.get_or_init(|| {
        ureq::Agent::config_builder()
            .timeout_connect(Some(Duration::from_secs(5)))
            // Large vision models can take several minutes on a laptop.
            .timeout_global(Some(Duration::from_secs(900)))
            .build()
            .into()
    })
}

/// Lists the models installed in Ollama.
pub fn list_models() -> anyhow::Result<Vec<String>> {
    let body: Value = agent()
        .get(&format!("{OLLAMA_URL}/api/tags"))
        .call()
        .context("Ollama is not running on this computer. Install it from ollama.com and start it.")?
        .body_mut()
        .read_json()
        .context("Ollama sent an unexpected reply")?;
    let mut models: Vec<String> = body["models"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|m| m["name"].as_str().map(str::to_owned))
        .collect();
    models.sort();
    Ok(models)
}

/// Downloads a model into Ollama, calling `progress` with the fraction done.
/// Returns `Ok(false)` if `stopped` became true first. Ollama keeps what was
/// downloaded, so a later attempt carries on from there.
pub fn pull_model(model: &str, mut progress: impl FnMut(f32), stopped: impl Fn() -> bool) -> anyhow::Result<bool> {
    // A download of several gigabytes can take far longer than other calls.
    static AGENT: OnceLock<ureq::Agent> = OnceLock::new();
    let agent = AGENT.get_or_init(|| {
        ureq::Agent::config_builder()
            .timeout_connect(Some(Duration::from_secs(5)))
            .timeout_recv_response(Some(Duration::from_secs(120)))
            .build()
            .into()
    });
    log::info!("downloading model {model}");
    let response = agent
        .post(&format!("{OLLAMA_URL}/api/pull"))
        .send_json(json!({ "model": model, "stream": true }))
        .context("Ollama could not start the download")?;
    // One JSON object per line: a status, and for each file being fetched,
    // its digest with the total and completed byte counts.
    let reader = std::io::BufReader::new(response.into_body().into_reader());
    let mut files: std::collections::HashMap<String, (u64, u64)> = std::collections::HashMap::new();
    for line in std::io::BufRead::lines(reader) {
        if stopped() {
            log::info!("model download stopped");
            return Ok(false);
        }
        let line = line.context("the download from Ollama was interrupted")?;
        let Ok(event) = serde_json::from_str::<Value>(&line) else { continue };
        if let Some(error) = event["error"].as_str() {
            bail!("Ollama could not download the model: {error}");
        }
        if event["status"] == "success" {
            log::info!("downloaded model {model}");
            return Ok(true);
        }
        if let (Some(digest), Some(total)) = (event["digest"].as_str(), event["total"].as_u64()) {
            let done = event["completed"].as_u64().unwrap_or(0);
            files.insert(digest.to_owned(), (done, total));
            let (done, total) = files.values().fold((0, 0), |(d, t), (fd, ft)| (d + fd, t + ft));
            if total > 0 {
                progress(done as f32 / total as f32);
            }
        }
    }
    bail!("the download from Ollama ended before it finished")
}

/// Waits up to `limit` for the Ollama server to answer. Returns whether it did.
pub fn wait_until_running(limit: Duration) -> bool {
    let end = std::time::Instant::now() + limit;
    loop {
        if list_models().is_ok() {
            return true;
        }
        if std::time::Instant::now() >= end {
            return false;
        }
        std::thread::sleep(Duration::from_secs(1));
    }
}

/// Sends a text prompt to an Ollama model and returns its answer. `format`
/// asks for a particular kind of reply, such as `"json"`.
pub fn generate(model: &str, prompt: &str, format: Option<Value>) -> anyhow::Result<String> {
    let mut request = json!({
        "model": model,
        "prompt": prompt,
        "stream": false,
        // Keep answers steady, so the same text gets the same translation.
        // Ollama's default context can be as small as 2,048 tokens, which
        // a batch of text and its translation can overflow, cutting the
        // answer short.
        "options": { "temperature": 0.1, "num_ctx": 8192 },
    });
    if let Some(format) = format {
        request["format"] = format;
    }
    // Ollama explains a failure, such as a model it can't load, in the reply.
    let body: Value = agent()
        .post(&format!("{OLLAMA_URL}/api/generate"))
        .config()
        .http_status_as_error(false)
        .build()
        .send_json(request)
        .context("the local AI model did not answer. Check that Ollama is running.")?
        .body_mut()
        .read_json()
        .context("Ollama sent an unexpected reply")?;
    if let Some(error) = body["error"].as_str() {
        bail!("Ollama could not use the model {model}: {error}");
    }
    match body["response"].as_str() {
        Some(text) if !text.trim().is_empty() => Ok(text.to_owned()),
        _ => bail!("the local AI model gave an empty answer"),
    }
}
