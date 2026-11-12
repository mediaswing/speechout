//! Describing images with an AI model running on this computer, through Ollama
//! or a server that copies OpenAI's API, and saying where a geotagged photo was
//! taken.
//!
//! The image only ever goes to the local server: addresses that aren't this
//! computer are refused, proxies are ignored and redirects aren't followed. It
//! is re-encoded first, which also strips its metadata, GPS included. If the
//! user turns on location lookup, the photo's coordinates, rounded to about 100
//! metres, go to OpenStreetMap's Nominatim service. The photo itself never does.

use std::io::Cursor;
use std::net::IpAddr;
use std::path::Path;
use std::sync::OnceLock;
use std::time::Duration;

use anyhow::{Context, bail};
use base64::Engine;
use serde_json::{Value, json};
use url::{Host, Url};

use crate::document::FileKind;
use crate::store::AiKind;
use crate::wordlist::clean_display_text;

const MAX_IMAGE_BYTES: u64 = 100 * 1024 * 1024;
/// The longest side sent to the model. Bigger takes longer and adds little.
const MAX_EDGE: u32 = 1600;
const MAX_REPLY_BYTES: u64 = 4 * 1024 * 1024;
/// How long Ollama gets to start answering. The app version can be slow to open.
const MAX_START_WAIT: Duration = Duration::from_secs(30);

const PROMPT: &str = "Describe this image for someone who is blind or has low vision. \
Start with a one-sentence summary. Then describe the important details: the setting, \
people and what they are doing, objects, colours, and any text that appears in the image, \
quoted exactly. Do not guess who anyone is. Write in plain sentences without markdown, \
headings, lists or emoji, because your answer will be read aloud.";

/// For models that return nothing when given the prompt above, as moondream does.
const SHORT_PROMPT: &str = "Describe this image in plain sentences.";

/// A model that answered but couldn't help. Another prompt or model may do
/// better, which isn't so of a server that isn't running.
#[derive(Debug)]
struct ModelProblem(String);

impl std::fmt::Display for ModelProblem {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for ModelProblem {}

/// Which local server to use, and how.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LocalAi {
    pub kind: AiKind,
    pub address: String,
    pub model: String,
}

/// Checks that an address is a web address on this computer, because images
/// must not leave it, and returns it without any trailing slash.
pub fn check_local_address(address: &str) -> anyhow::Result<String> {
    const WRONG: &str = "the local AI address must be on this computer, such as http://127.0.0.1:11434";
    let url = Url::parse(address.trim()).context(WRONG)?;
    if !matches!(url.scheme(), "http" | "https") || !url.username().is_empty() || url.password().is_some() {
        bail!(WRONG);
    }
    let local = match url.host() {
        Some(Host::Domain(name)) => name.eq_ignore_ascii_case("localhost"),
        Some(Host::Ipv4(ip)) => IpAddr::V4(ip).is_loopback(),
        Some(Host::Ipv6(ip)) => IpAddr::V6(ip).is_loopback(),
        None => false,
    };
    if !local || url.path() != "/" || url.query().is_some() || url.fragment().is_some() {
        bail!(WRONG);
    }
    Ok(url.as_str().trim_end_matches('/').to_owned())
}

fn local_agent() -> &'static ureq::Agent {
    static AGENT: OnceLock<ureq::Agent> = OnceLock::new();
    AGENT.get_or_init(|| {
        ureq::Agent::config_builder()
            // ureq would otherwise pick up a proxy from the environment, which
            // would send the image to it.
            .proxy(None)
            .max_redirects(0)
            .http_status_as_error(false)
            .timeout_connect(Some(Duration::from_secs(5)))
            // Large vision models can take several minutes on a laptop.
            .timeout_global(Some(Duration::from_secs(900)))
            .build()
            .into()
    })
}

fn unreachable_message(kind: AiKind, address: &str) -> String {
    format!(
        "no local AI system answered at {address}. Check that {} is installed and running, and that this address on \
         the Settings tab is right",
        match kind {
            AiKind::Ollama => "Ollama",
            AiKind::OpenAiCompatible => "the server",
        }
    )
}

/// Lists the local server's models, sorted by name.
pub fn list_models(kind: AiKind, address: &str) -> anyhow::Result<Vec<String>> {
    let address = check_local_address(address)?;
    let path = match kind {
        AiKind::Ollama => "/api/tags",
        AiKind::OpenAiCompatible => "/v1/models",
    };
    let body: Value = local_agent()
        .get(&format!("{address}{path}"))
        .call()
        .with_context(|| unreachable_message(kind, &address))?
        .body_mut()
        .with_config()
        .limit(MAX_REPLY_BYTES)
        .read_json()
        .context("the local AI system sent a list of models that could not be understood")?;
    let (list, field) = match kind {
        AiKind::Ollama => (&body["models"], "name"),
        AiKind::OpenAiCompatible => (&body["data"], "id"),
    };
    let mut models: Vec<String> = list
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|m| m[field].as_str())
        .map(clean_display_text)
        .filter(|m| !m.is_empty())
        .collect();
    models.sort();
    Ok(models)
}

fn is_vision_model(name: &str) -> bool {
    const VISION_HINTS: &[&str] =
        &["vision", "llava", "gemma3", "vl", "minicpm-v", "moondream", "bakllava", "pixtral", "mistral-small3"];
    let name = name.to_lowercase();
    VISION_HINTS.iter().any(|h| name.contains(h))
}

/// The model most likely to understand images: the first with a name from a
/// well-known vision family, or else the first model.
pub fn preferred_model(models: &[String]) -> Option<&String> {
    models.iter().find(|m| is_vision_model(m)).or_else(|| models.first())
}

/// The models to try in turn: `first`, then the other vision models.
fn models_to_try(first: String, models: &[String]) -> Vec<String> {
    let others: Vec<String> = models.iter().filter(|m| **m != first && is_vision_model(m)).cloned().collect();
    std::iter::once(first).chain(others).collect()
}

/// Encodes an image as a JPEG of reasonable quality.
pub fn encode_jpeg(image: &image::DynamicImage) -> anyhow::Result<Vec<u8>> {
    let mut out = Vec::new();
    let encoder = image::codecs::jpeg::JpegEncoder::new_with_quality(&mut out, 85);
    image.to_rgb8().write_with_encoder(encoder).context("could not encode the image")?;
    Ok(out)
}

/// Loads the image, turns it the right way up, shrinks it and re-encodes it as
/// a JPEG with no metadata. The decoder's default limits keep what a small file
/// can claim to expand to within 512 MB.
fn prepare_image(path: &Path, kind: FileKind) -> anyhow::Result<Vec<u8>> {
    use image::ImageDecoder;
    if std::fs::metadata(path).context("the image could not be opened")?.len() > MAX_IMAGE_BYTES {
        bail!("the image is larger than 100 MB");
    }
    let bytes = match kind {
        FileKind::Heif => crate::platform::heic_to_jpeg(path)?,
        _ => std::fs::read(path).context("the image could not be read")?,
    };
    let mut decoder = image::ImageReader::new(Cursor::new(bytes))
        .with_guessed_format()?
        .into_decoder()
        .context("this file is not a JPEG image that can be read")?;
    let orientation = decoder.orientation().unwrap_or(image::metadata::Orientation::NoTransforms);
    let mut img = image::DynamicImage::from_decoder(decoder).context("the image could not be decoded")?;
    img.apply_orientation(orientation);
    if img.width() > MAX_EDGE || img.height() > MAX_EDGE {
        img = img.resize(MAX_EDGE, MAX_EDGE, image::imageops::FilterType::Triangle);
    }
    encode_jpeg(&img)
}

/// Removes markdown that would otherwise be read out as symbols, and control
/// characters, since what a model says is influenced by what is in the image.
fn plain_text(text: &str) -> String {
    clean_lines(text)
        .lines()
        .map(|l| l.trim_start_matches(['#', '-', '*', '>', ' ']).replace("**", "").replace('*', ""))
        .filter(|l| !l.trim().is_empty())
        .collect::<Vec<_>>()
        .join("\n\n")
}

fn clean_lines(text: &str) -> String {
    text.split('\n').map(clean_display_text).collect::<Vec<_>>().join("\n")
}

/// Describes an image, and if `resolve_gps` is on, says where it was taken.
/// If Ollama is installed but not running, starts it first, calling `on_starting`.
pub fn describe(
    path: &Path,
    kind: FileKind,
    ai: &LocalAi,
    resolve_gps: bool,
    on_starting: &dyn Fn(),
) -> anyhow::Result<String> {
    let address = check_local_address(&ai.address)?;
    let models = pick_models(ai, &address, on_starting)?;
    let image = base64::engine::general_purpose::STANDARD.encode(prepare_image(path, kind)?);
    let mut description = ask_until_answered(ai.kind, &address, &models, &image)?;
    if resolve_gps {
        match gps_coordinates(path) {
            Ok(Some((lat, lon))) => {
                description.push_str("\n\n");
                description.push_str(&location_sentence(lat, lon, reverse_geocode(lat, lon)));
            }
            Ok(None) => log::info!("The image has no GPS location"),
            Err(e) => log::warn!("Could not read the image's location: {e:#}"),
        }
    }
    Ok(description)
}

/// The models to try: the chosen one, or if there isn't one, the server's best
/// guess, then the others that understand images. If the server isn't running,
/// tries to start it.
fn pick_models(ai: &LocalAi, address: &str, on_starting: &dyn Fn()) -> anyhow::Result<Vec<String>> {
    let models = models_starting_ollama(ai.kind, address, crate::platform::start_ollama, on_starting, MAX_START_WAIT)?;
    if !ai.model.is_empty() {
        return Ok(models_to_try(ai.model.clone(), &models));
    }
    let first = preferred_model(&models).cloned().context(
        "the local AI system has no models. Install one that understands images, such as with \
         \"ollama pull moondream\", then choose it on the Settings tab",
    )?;
    Ok(models_to_try(first, &models))
}

/// Tries each model with each prompt until one answers. If none does, the
/// first model's problem is the one reported, as that's the one the user chose.
fn ask_until_answered(kind: AiKind, address: &str, models: &[String], image: &str) -> anyhow::Result<String> {
    let mut first_problem = None;
    for model in models {
        for prompt in [PROMPT, SHORT_PROMPT] {
            log::info!("Describing an image with the model {model}");
            match ask(kind, address, model, prompt, image) {
                Ok(description) => return Ok(description),
                Err(e) if e.is::<ModelProblem>() => {
                    log::warn!("{e:#}");
                    first_problem.get_or_insert(e);
                }
                Err(e) => return Err(e),
            }
        }
    }
    Err(first_problem.unwrap_or_else(|| anyhow::anyhow!("the local AI system has no models")))
}

/// Lists the models, starting Ollama first if it isn't answering. Only Ollama's
/// own port is worth it: `ollama serve` wouldn't answer at any other address, so
/// waiting for it there would be pointless.
fn models_starting_ollama(
    kind: AiKind,
    address: &str,
    start: impl FnOnce() -> bool,
    on_starting: &dyn Fn(),
    wait: Duration,
) -> anyhow::Result<Vec<String>> {
    let first = match list_models(kind, address) {
        Ok(models) => return Ok(models),
        Err(e) => e,
    };
    if kind != AiKind::Ollama || !address.ends_with(":11434") {
        return Err(first);
    }
    on_starting();
    start_and_wait(kind, address, start, wait).ok_or(first)
}

fn start_and_wait(kind: AiKind, address: &str, start: impl FnOnce() -> bool, wait: Duration) -> Option<Vec<String>> {
    if !start() {
        log::info!("Ollama could not be started");
        return None;
    }
    log::info!("Started Ollama; waiting for it to answer");
    let end = std::time::Instant::now() + wait;
    while std::time::Instant::now() < end {
        if let Ok(models) = list_models(kind, address) {
            return Some(models);
        }
        std::thread::sleep(Duration::from_millis(250));
    }
    None
}

fn ask(kind: AiKind, address: &str, model: &str, prompt: &str, image: &str) -> anyhow::Result<String> {
    let (path, request) = match kind {
        AiKind::Ollama => (
            "/api/generate",
            json!({ "model": model, "prompt": prompt, "images": [image], "stream": false }),
        ),
        AiKind::OpenAiCompatible => (
            "/v1/chat/completions",
            json!({
                "model": model,
                "stream": false,
                "messages": [{ "role": "user", "content": [
                    { "type": "text", "text": prompt },
                    { "type": "image_url", "image_url": { "url": format!("data:image/jpeg;base64,{image}") } },
                ] }],
            }),
        ),
    };
    let body: Value = local_agent()
        .post(&format!("{address}{path}"))
        .send_json(request)
        .with_context(|| unreachable_message(kind, address))?
        .body_mut()
        .with_config()
        .limit(MAX_REPLY_BYTES)
        .read_json()
        .context("the local AI system sent a reply that could not be understood")?;
    // Both kinds explain a failure, such as a model they can't load, in the reply.
    let error = body["error"].as_str().or(body["error"]["message"].as_str());
    if let Some(error) = error {
        let error: String = clean_display_text(error).chars().take(300).collect();
        return Err(ModelProblem(format!("the model {model} could not describe the image: {error}")).into());
    }
    let answer = match kind {
        AiKind::Ollama => body["response"].as_str(),
        AiKind::OpenAiCompatible => body["choices"][0]["message"]["content"].as_str(),
    };
    let description = plain_text(answer.unwrap_or_default());
    if description.is_empty() {
        return Err(ModelProblem(format!(
            "the model {model} gave no description. Choose one that understands images on the Settings tab"
        ))
        .into());
    }
    Ok(description)
}

/// Reads GPS latitude and longitude from a JPEG or HEIF file's EXIF data.
pub fn gps_coordinates(path: &Path) -> anyhow::Result<Option<(f64, f64)>> {
    let mut file = std::io::BufReader::new(std::fs::File::open(path)?);
    match exif::Reader::new().read_from_container(&mut file) {
        Ok(exif) => Ok(coordinates_from(&exif)),
        Err(exif::Error::NotFound(_)) => Ok(None),
        Err(e) => Err(e.into()),
    }
}

fn coordinates_from(exif: &exif::Exif) -> Option<(f64, f64)> {
    let coordinate = |value_tag, ref_tag, negative: &str| -> Option<f64> {
        let field = exif.get_field(value_tag, exif::In::PRIMARY)?;
        let exif::Value::Rational(parts) = &field.value else { return None };
        if parts.len() < 3 || parts.iter().any(|r| r.denom == 0) {
            return None;
        }
        let degrees = parts[0].to_f64() + parts[1].to_f64() / 60.0 + parts[2].to_f64() / 3600.0;
        let is_negative = exif
            .get_field(ref_tag, exif::In::PRIMARY)
            .is_some_and(|f| f.display_value().to_string().trim().eq_ignore_ascii_case(negative));
        Some(if is_negative { -degrees } else { degrees })
    };
    let lat = coordinate(exif::Tag::GPSLatitude, exif::Tag::GPSLatitudeRef, "S")?;
    let lon = coordinate(exif::Tag::GPSLongitude, exif::Tag::GPSLongitudeRef, "W")?;
    // Phones without a fix sometimes write 0, 0.
    let plausible = (-90.0..=90.0).contains(&lat) && (-180.0..=180.0).contains(&lon) && (lat, lon) != (0.0, 0.0);
    plausible.then_some((lat, lon))
}

/// The closing sentence of a description. If the place couldn't be found, the
/// coordinates are still worth reading out.
fn location_sentence(lat: f64, lon: f64, place: anyhow::Result<String>) -> String {
    match place {
        Ok(place) if !place.is_empty() => format!("This photo was taken in or near {place}."),
        other => {
            if let Err(e) = other {
                log::warn!("Could not look up the location: {e:#}");
            }
            format!(
                "This photo has location information: latitude {lat:.4}, longitude {lon:.4}, but the place name \
                 could not be looked up."
            )
        }
    }
}

fn reverse_geocode(lat: f64, lon: f64) -> anyhow::Result<String> {
    static AGENT: OnceLock<ureq::Agent> = OnceLock::new();
    let agent = AGENT.get_or_init(|| {
        ureq::Agent::config_builder()
            .https_only(true)
            .max_redirects(0)
            .timeout_global(Some(Duration::from_secs(20)))
            // Nominatim's usage policy asks for a user agent naming the app.
            .user_agent(concat!(
                "commonspeech/",
                env!("CARGO_PKG_VERSION"),
                " (+https://github.com/mediaswing/speechout)"
            ))
            .build()
            .into()
    });
    let (lat, lon) = rounded(lat, lon);
    let body: Value = agent
        .get("https://nominatim.openstreetmap.org/reverse")
        .query("format", "jsonv2")
        .query("lat", lat)
        .query("lon", lon)
        .query("zoom", "14")
        .query("accept-language", "en")
        .call()?
        .body_mut()
        .with_config()
        .limit(256 * 1024)
        .read_json()?;
    Ok(place_name(&body["address"]))
}

/// Three decimal places is about 100 metres: enough to name a neighbourhood
/// without saying which house.
fn rounded(lat: f64, lon: f64) -> (String, String) {
    (format!("{lat:.3}"), format!("{lon:.3}"))
}

fn place_name(address: &Value) -> String {
    let field = |keys: &[&str]| keys.iter().find_map(|k| address[*k].as_str()).map(clean_display_text);
    let mut parts: Vec<String> = Vec::new();
    for keys in [
        &["suburb", "village", "hamlet", "neighbourhood"][..],
        &["town", "city", "municipality"][..],
        &["county", "state_district"][..],
        &["state"][..],
        &["country"][..],
    ] {
        if let Some(value) = field(keys)
            && !parts.contains(&value)
        {
            parts.push(value);
        }
    }
    parts.join(", ").chars().take(300).collect()
}

/// A tiny web server on this computer that answers every request with `reply`
/// and hands back each request it got, for testing the local AI code.
#[cfg(test)]
pub(crate) fn mock_server(reply: &'static str) -> (String, std::sync::mpsc::Receiver<String>) {
    mock_server_on(std::net::TcpListener::bind("127.0.0.1:0").unwrap(), reply)
}

#[cfg(test)]
fn mock_server_on(listener: std::net::TcpListener, reply: &'static str) -> (String, std::sync::mpsc::Receiver<String>) {
    use std::io::{Read, Write};
    let address = format!("http://{}", listener.local_addr().unwrap());
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { return };
            let mut request = Vec::new();
            let mut buffer = [0u8; 65536];
            loop {
                let n = stream.read(&mut buffer).unwrap_or(0);
                request.extend_from_slice(&buffer[..n]);
                let text = String::from_utf8_lossy(&request);
                let Some(header_end) = text.find("\r\n\r\n") else {
                    if n == 0 {
                        break;
                    }
                    continue;
                };
                let length = text[..header_end]
                    .lines()
                    .find_map(|l| l.to_ascii_lowercase().strip_prefix("content-length: ").map(str::to_owned))
                    .and_then(|l| l.trim().parse::<usize>().ok())
                    .unwrap_or(0);
                let chunked = text[..header_end].to_ascii_lowercase().contains("transfer-encoding: chunked");
                let complete = if chunked { text.ends_with("0\r\n\r\n") } else { request.len() >= header_end + 4 + length };
                if n == 0 || complete {
                    break;
                }
            }
            let _ = tx.send(String::from_utf8_lossy(&request).into_owned());
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{reply}",
                reply.len()
            );
            let _ = stream.write_all(response.as_bytes());
        }
    });
    (address, rx)
}

/// A JPEG with EXIF data giving `gps` as its location.
#[cfg(test)]
pub(crate) fn geotagged_jpeg(gps: Option<(&str, f64, &str, f64)>, size: (u32, u32)) -> Vec<u8> {
    let mut jpeg = Vec::new();
    image::RgbImage::from_pixel(size.0, size.1, image::Rgb([200, 40, 40]))
        .write_to(&mut Cursor::new(&mut jpeg), image::ImageFormat::Jpeg)
        .unwrap();
    let Some((lat_ref, lat, lon_ref, lon)) = gps else { return jpeg };
    let dms = |deg: f64| {
        let seconds = (deg * 3600.0).round() as u32;
        exif::Value::Rational(vec![(seconds / 3600, 1).into(), (seconds / 60 % 60, 1).into(), (seconds % 60, 1).into()])
    };
    let field = |tag, value| exif::Field { tag, ifd_num: exif::In::PRIMARY, value };
    let fields = [
        field(exif::Tag::GPSLatitudeRef, exif::Value::Ascii(vec![lat_ref.into()])),
        field(exif::Tag::GPSLatitude, dms(lat)),
        field(exif::Tag::GPSLongitudeRef, exif::Value::Ascii(vec![lon_ref.into()])),
        field(exif::Tag::GPSLongitude, dms(lon)),
    ];
    let mut writer = exif::experimental::Writer::new();
    fields.iter().for_each(|f| writer.push_field(f));
    let mut tiff = Cursor::new(Vec::new());
    writer.write(&mut tiff, false).unwrap();
    // An APP1 segment straight after the start of the JPEG.
    let mut segment = b"Exif\0\0".to_vec();
    segment.extend_from_slice(tiff.get_ref());
    let mut out = jpeg[..2].to_vec();
    out.extend_from_slice(&[0xFF, 0xE1]);
    out.extend_from_slice(&((segment.len() + 2) as u16).to_be_bytes());
    out.extend_from_slice(&segment);
    out.extend_from_slice(&jpeg[2..]);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_addresses_on_this_computer_are_allowed() {
        for good in ["http://127.0.0.1:11434", "http://127.0.0.1:11434/", "http://localhost:1234", "http://[::1]:8080", "https://LOCALHOST"] {
            assert!(check_local_address(good).is_ok(), "{good}");
        }
        for bad in [
            "http://192.168.1.5:11434",
            "http://example.com",
            "http://127.0.0.1.example.com",
            "http://localhost.example.com",
            "http://user:pw@127.0.0.1",
            "http://127.0.0.1@example.com",
            "ftp://127.0.0.1",
            "file:///etc/passwd",
            "http://127.0.0.1/path",
            "http://127.0.0.1/?x=1",
            "127.0.0.1:11434",
            "",
        ] {
            assert!(check_local_address(bad).is_err(), "{bad}");
        }
        assert_eq!(check_local_address(" http://127.0.0.1:11434/ ").unwrap(), "http://127.0.0.1:11434");
    }

    #[test]
    fn describe_refuses_a_remote_address_without_reading_the_image() {
        let ai = LocalAi { kind: AiKind::Ollama, address: "http://example.com".into(), model: "m".into() };
        let error = describe(Path::new("/does/not/exist.jpg"), FileKind::Jpeg, &ai, false, &|| ()).unwrap_err();
        assert!(error.to_string().contains("on this computer"));
    }

    #[test]
    fn prefers_vision_models() {
        let models = vec!["llama3.1:8b".to_owned(), "llama3.2-vision:11b".to_owned()];
        assert_eq!(preferred_model(&models).unwrap(), "llama3.2-vision:11b");
        assert_eq!(preferred_model(&models[..1]).unwrap(), "llama3.1:8b");
        assert!(preferred_model(&[]).is_none());
    }

    #[test]
    fn strips_markdown_and_control_characters() {
        assert_eq!(plain_text("# Title\n\n- **A** cat\u{202E}\u{7}\n* sits"), "Title\n\nA cat\n\nsits");
    }

    #[test]
    fn reads_gps_from_a_jpeg() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("a.jpg");
        std::fs::write(&path, geotagged_jpeg(Some(("S", 33.8569, "E", 151.2153)), (4, 4))).unwrap();
        let (lat, lon) = gps_coordinates(&path).unwrap().unwrap();
        assert!((lat + 33.8569).abs() < 0.001 && (lon - 151.2153).abs() < 0.001, "{lat}, {lon}");
        std::fs::write(&path, geotagged_jpeg(None, (4, 4))).unwrap();
        assert_eq!(gps_coordinates(&path).unwrap(), None);
        std::fs::write(&path, geotagged_jpeg(Some(("N", 0.0, "E", 0.0)), (4, 4))).unwrap();
        assert_eq!(gps_coordinates(&path).unwrap(), None, "0, 0 means no fix");
    }

    #[test]
    fn the_model_never_sees_the_location() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("a.jpg");
        std::fs::write(&path, geotagged_jpeg(Some(("N", 52.4, "W", 1.9)), (4, 4))).unwrap();
        let prepared = prepare_image(&path, FileKind::Jpeg).unwrap();
        std::fs::write(&path, prepared).unwrap();
        assert_eq!(gps_coordinates(&path).unwrap(), None);
    }

    #[test]
    fn large_images_are_shrunk_and_fakes_are_refused() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("big.jpg");
        std::fs::write(&path, geotagged_jpeg(None, (3200, 800))).unwrap();
        let sent = image::load_from_memory(&prepare_image(&path, FileKind::Jpeg).unwrap()).unwrap();
        assert_eq!((sent.width(), sent.height()), (MAX_EDGE, 400));
        std::fs::write(&path, b"this is not a picture").unwrap();
        assert!(prepare_image(&path, FileKind::Jpeg).is_err());
    }

    #[test]
    fn coordinates_are_rounded_before_leaving_the_computer() {
        assert_eq!(rounded(52.123456, -1.987654), ("52.123".to_owned(), "-1.988".to_owned()));
    }

    #[test]
    fn location_sentences() {
        let place = location_sentence(52.4, -1.9, Ok("Digbeth, Birmingham".into()));
        assert_eq!(place, "This photo was taken in or near Digbeth, Birmingham.");
        let failed = location_sentence(52.4, -1.9, Err(anyhow::anyhow!("offline")));
        assert!(failed.contains("latitude 52.4000, longitude -1.9000"));
        assert!(location_sentence(1.0, 1.0, Ok(String::new())).contains("latitude"));
    }

    #[test]
    fn builds_place_names() {
        let address = json!({"suburb": "Digbeth", "city": "Birmingham", "state": "England", "country": "United Kingdom\u{202E}"});
        assert_eq!(place_name(&address), "Digbeth, Birmingham, England, United Kingdom");
    }

    #[test]
    fn describes_with_ollama() {
        let (address, requests) =
            mock_server(r##"{"models":[{"name":"moondream"}],"response":"# A red square.\n\n**Nothing else.**"}"##);
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("a.jpg");
        std::fs::write(&path, geotagged_jpeg(Some(("N", 52.4, "W", 1.9)), (8, 8))).unwrap();
        let ai = LocalAi { kind: AiKind::Ollama, address, model: String::new() };
        assert_eq!(describe(&path, FileKind::Jpeg, &ai, false, &|| ()).unwrap(), "A red square.\n\nNothing else.");
        assert!(requests.recv().unwrap().starts_with("GET /api/tags"));
        let generate = requests.recv().unwrap();
        assert!(generate.starts_with("POST /api/generate"));
        assert!(generate.contains("\"model\": \"moondream\"") && generate.contains("\"images\""));
    }

    #[test]
    fn describes_with_an_openai_compatible_server() {
        let (address, requests) = mock_server(
            r#"{"data":[{"id":"qwen-vl"}],"choices":[{"message":{"content":"A red square."}}]}"#,
        );
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("a.jpg");
        std::fs::write(&path, geotagged_jpeg(None, (8, 8))).unwrap();
        let ai = LocalAi { kind: AiKind::OpenAiCompatible, address, model: "chosen".into() };
        assert_eq!(describe(&path, FileKind::Jpeg, &ai, false, &|| ()).unwrap(), "A red square.");
        assert!(requests.recv().unwrap().starts_with("GET /v1/models"));
        let chat = requests.recv().unwrap();
        assert!(chat.starts_with("POST /v1/chat/completions"));
        assert!(chat.contains("\"model\": \"chosen\"") && chat.contains("data:image/jpeg;base64,"));
    }

    #[test]
    fn reports_a_model_that_fails_or_says_nothing() {
        let (address, _r) = mock_server(r#"{"models":[{"name":"m"}],"error":"model requires more memory"}"#);
        let error = ask(AiKind::Ollama, &address, "m", PROMPT, "x").unwrap_err().to_string();
        assert!(error.contains("more memory"), "{error}");
        let (address, _r) = mock_server(r#"{"models":[{"name":"m"}],"response":"  "}"#);
        assert!(ask(AiKind::Ollama, &address, "m", PROMPT, "x").unwrap_err().to_string().contains("no description"));
    }

    #[test]
    fn falls_back_to_other_vision_models() {
        let models: Vec<String> = ["llama3:8b", "moondream", "qwen-vl"].map(String::from).into();
        assert_eq!(models_to_try("moondream".into(), &models), ["moondream", "qwen-vl"]);
        assert_eq!(models_to_try("llama3:8b".into(), &models), ["llama3:8b", "moondream", "qwen-vl"]);
    }

    #[test]
    fn tries_every_model_and_prompt_then_reports_the_first_problem() {
        let (address, requests) = mock_server(r#"{"response":""}"#);
        let models = ["first".to_owned(), "second".to_owned()];
        let error = ask_until_answered(AiKind::Ollama, &address, &models, "x").unwrap_err().to_string();
        assert!(error.contains("first"), "{error}");
        let asked: Vec<String> = requests.try_iter().collect();
        assert_eq!(asked.len(), 4);
        assert!(asked[1].contains(SHORT_PROMPT) && asked[3].contains("second"));
    }

    #[test]
    fn a_server_that_is_not_running_is_not_retried() {
        let error = ask_until_answered(AiKind::Ollama, "http://127.0.0.1:9", &["a".to_owned()], "x").unwrap_err();
        assert!(error.to_string().contains("no local AI system answered"), "{error}");
    }

    fn wait_after_start(address: &str, start: impl FnOnce() -> bool, wait: Duration) -> anyhow::Result<Vec<String>> {
        start_and_wait(AiKind::Ollama, address, start, wait).ok_or_else(|| {
            list_models(AiKind::Ollama, address).err().unwrap_or_else(|| anyhow::anyhow!("no local AI system answered"))
        })
    }

    /// Nothing is listening on the port this returns, until something binds it.
    fn free_port() -> u16 {
        std::net::TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port()
    }

    fn try_start(address: &str, start: impl FnOnce() -> bool) -> (anyhow::Result<Vec<String>>, bool) {
        let told = std::cell::Cell::new(false);
        let told_user = || told.set(true);
        let models = models_starting_ollama(AiKind::Ollama, address, start, &told_user, Duration::from_secs(5));
        (models, told.get())
    }

    #[test]
    fn a_running_server_is_not_started_again() {
        let (address, _r) = mock_server(r#"{"models":[{"name":"m"}]}"#);
        // Stands in for Ollama's port, as the address decides whether to start it.
        let (models, told) = try_start(&address, || panic!("it is already running"));
        assert_eq!(models.unwrap(), ["m"]);
        assert!(!told);
    }

    #[test]
    fn starting_is_only_tried_on_ollamas_own_port_and_for_ollama() {
        let address = format!("http://127.0.0.1:{}", free_port());
        let (models, told) = try_start(&address, || panic!("ollama serve wouldn't answer here"));
        assert!(models.is_err() && !told);
        let not_ollama = models_starting_ollama(
            AiKind::OpenAiCompatible,
            "http://127.0.0.1:9/:11434",
            || panic!("this isn't Ollama"),
            &|| panic!("nothing is starting"),
            Duration::ZERO,
        );
        assert!(not_ollama.is_err());
    }

    #[test]
    fn waits_for_ollama_to_answer_after_starting_it() {
        // Ollama's real port may be in use on this computer, so this checks the
        // waiting through a server that appears a moment after being started.
        let port = free_port();
        let address = format!("http://127.0.0.1:{port}");
        let start = move || {
            std::thread::spawn(move || {
                std::thread::sleep(Duration::from_millis(400));
                let listener = std::net::TcpListener::bind(("127.0.0.1", port)).unwrap();
                let (_address, requests) = mock_server_on(listener, r#"{"models":[{"name":"m"}]}"#);
                for _ in requests {}
            });
            true
        };
        let models = wait_after_start(&address, start, Duration::from_secs(5));
        assert_eq!(models.unwrap(), ["m"]);
    }

    #[test]
    fn says_when_ollama_could_not_be_started_or_never_answers() {
        let address = format!("http://127.0.0.1:{}", free_port());
        assert!(wait_after_start(&address, || false, Duration::from_secs(1)).is_err());
        let error = wait_after_start(&address, || true, Duration::from_millis(300)).unwrap_err().to_string();
        assert!(error.contains("no local AI system answered"), "{error}");
    }

    #[test]
    fn lists_models_sorted() {
        let (address, _r) = mock_server(r#"{"models":[{"name":"b"},{"name":"a"},{"other":1}]}"#);
        assert_eq!(list_models(AiKind::Ollama, &address).unwrap(), ["a", "b"]);
    }
}

/// Needs a real Ollama with a vision model, and the internet for the location:
/// `E2E_IMAGE=photo.jpg cargo test real_description -- --ignored --nocapture`
#[cfg(test)]
#[test]
#[ignore]
fn real_description() {
    let path = std::path::PathBuf::from(std::env::var("E2E_IMAGE").expect("E2E_IMAGE"));
    let kind = FileKind::of(&path).unwrap();
    let ai = LocalAi {
        kind: AiKind::Ollama,
        address: AiKind::Ollama.default_address().to_owned(),
        model: std::env::var("E2E_MODEL").unwrap_or_default(),
    };
    let starting = || println!("Starting Ollama");
    println!("{}", describe(&path, kind, &ai, true, &starting).unwrap());
}
