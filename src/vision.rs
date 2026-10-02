//! Image descriptions from a local AI model (Ollama), with an optional note
//! of where a geotagged photo was taken.
//!
//! The image is only ever sent to the Ollama server on this computer. If the
//! user turns on location lookup, the photo's rounded GPS coordinates (not the
//! photo) are sent to OpenStreetMap's Nominatim service to find a place name.

use anyhow::{Context, bail};
use base64::Engine;
use serde_json::{Value, json};
use std::io::Cursor;
use std::path::Path;
use std::sync::OnceLock;
use std::time::Duration;

pub const OLLAMA_URL: &str = "http://127.0.0.1:11434";
const MAX_IMAGE_BYTES: u64 = 100 * 1024 * 1024;
const MAX_EDGE: u32 = 1600;

const PROMPT: &str = "Describe this image for someone who is blind or has low vision. \
Start with a one-sentence summary. Then describe the important details: the setting, \
people and what they are doing, objects, colours, and any text that appears in the image, \
quoted exactly. Do not guess who anyone is. Write in plain sentences without markdown, \
headings, lists or emoji, because your answer will be read aloud.";

fn local_agent() -> &'static ureq::Agent {
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

fn web_agent() -> &'static ureq::Agent {
    static AGENT: OnceLock<ureq::Agent> = OnceLock::new();
    AGENT.get_or_init(|| {
        ureq::Agent::config_builder()
            .https_only(true)
            .timeout_global(Some(Duration::from_secs(20)))
            // Nominatim's usage policy requires an identifying user agent.
            .user_agent(concat!(
                "speechout/",
                env!("CARGO_PKG_VERSION"),
                " (+https://github.com/mediaswing/speechout)"
            ))
            .build()
            .into()
    })
}

/// Lists the models installed in Ollama.
pub fn list_models() -> anyhow::Result<Vec<String>> {
    let body: Value = local_agent()
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

/// Picks the model most likely to understand images: the first whose name
/// matches a well-known vision model family, otherwise the first model.
pub fn preferred_model(models: &[String]) -> Option<&String> {
    const VISION_HINTS: &[&str] =
        &["vision", "llava", "gemma3", "vl", "minicpm-v", "moondream", "bakllava", "pixtral", "mistral-small3"];
    models
        .iter()
        .find(|m| VISION_HINTS.iter().any(|h| m.to_lowercase().contains(h)))
        .or_else(|| models.first())
}

/// Encodes an image as a JPEG of reasonable quality.
pub fn encode_jpeg(image: &image::DynamicImage) -> anyhow::Result<Vec<u8>> {
    let mut out = Vec::new();
    let encoder = image::codecs::jpeg::JpegEncoder::new_with_quality(&mut out, 85);
    image.to_rgb8().write_with_encoder(encoder).context("could not encode the image")?;
    Ok(out)
}

fn is_heif(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| e.eq_ignore_ascii_case("heic") || e.eq_ignore_ascii_case("heif"))
}

/// Loads the photo, applies its rotation, shrinks it and re-encodes it as a
/// JPEG. Re-encoding also strips metadata such as GPS from what is sent.
fn prepare_image(path: &Path) -> anyhow::Result<Vec<u8>> {
    if std::fs::metadata(path)?.len() > MAX_IMAGE_BYTES {
        bail!("the image is too large (the limit is 100 MB)");
    }
    let jpeg = if is_heif(path) { crate::platform::heic_to_jpeg(path)? } else { std::fs::read(path)? };

    use image::ImageDecoder;
    let mut decoder = image::ImageReader::new(Cursor::new(jpeg))
        .with_guessed_format()?
        .into_decoder()
        .context("the image could not be read")?;
    let orientation = decoder.orientation().unwrap_or(image::metadata::Orientation::NoTransforms);
    let mut img = image::DynamicImage::from_decoder(decoder).context("the image could not be decoded")?;
    img.apply_orientation(orientation);
    if img.width() > MAX_EDGE || img.height() > MAX_EDGE {
        img = img.resize(MAX_EDGE, MAX_EDGE, image::imageops::FilterType::Triangle);
    }
    encode_jpeg(&img)
}

/// Removes markdown that would otherwise be read out as symbols.
fn plain_text(text: &str) -> String {
    text.lines()
        .map(|l| l.trim_start_matches(['#', '-', '*', '>', ' ']).replace("**", "").replace('*', ""))
        .filter(|l| !l.trim().is_empty())
        .collect::<Vec<_>>()
        .join("\n\n")
}

pub fn describe(path: &Path, model: &str, resolve_location: bool) -> anyhow::Result<String> {
    if model.is_empty() {
        bail!("choose an image description model on the Settings tab first");
    }
    let jpeg = prepare_image(path)?;
    let b64 = base64::engine::general_purpose::STANDARD.encode(jpeg);
    log::info!("describing image with model {model}");
    let body: Value = local_agent()
        .post(&format!("{OLLAMA_URL}/api/generate"))
        .send_json(json!({
            "model": model,
            "prompt": PROMPT,
            "images": [b64],
            "stream": false,
        }))
        .context("the local AI model could not describe the image. Check that Ollama is running and that the model supports images.")?
        .body_mut()
        .read_json()
        .context("Ollama sent an unexpected reply")?;
    let mut description = plain_text(body["response"].as_str().unwrap_or_default());
    if description.is_empty() {
        bail!("the model returned an empty description. Try a vision model such as llama3.2-vision, gemma3 or llava.");
    }

    if resolve_location {
        match gps_coordinates(path) {
            Ok(Some((lat, lon))) => {
                description.push_str("\n\n");
                description.push_str(&describe_location(lat, lon));
            }
            Ok(None) => log::info!("image has no GPS location"),
            Err(e) => log::warn!("could not read image location: {e:#}"),
        }
    }
    Ok(description)
}

/// Reads GPS latitude and longitude from a JPEG or HEIF file's EXIF data.
pub fn gps_coordinates(path: &Path) -> anyhow::Result<Option<(f64, f64)>> {
    let file = std::fs::File::open(path)?;
    let exif = match exif::Reader::new().read_from_container(&mut std::io::BufReader::new(file)) {
        Ok(exif) => exif,
        Err(exif::Error::NotFound(_)) => return Ok(None),
        Err(e) => return Err(e.into()),
    };
    let coord = |value_tag, ref_tag, negative: &str| -> Option<f64> {
        let field = exif.get_field(value_tag, exif::In::PRIMARY)?;
        let exif::Value::Rational(parts) = &field.value else { return None };
        if parts.len() < 3 || parts.iter().any(|r| r.denom == 0) {
            return None;
        }
        let deg = parts[0].to_f64() + parts[1].to_f64() / 60.0 + parts[2].to_f64() / 3600.0;
        let sign = exif
            .get_field(ref_tag, exif::In::PRIMARY)
            .map(|f| f.display_value().to_string())
            .filter(|r| r.trim().eq_ignore_ascii_case(negative))
            .map_or(1.0, |_| -1.0);
        Some(deg * sign)
    };
    let lat = coord(exif::Tag::GPSLatitude, exif::Tag::GPSLatitudeRef, "S");
    let lon = coord(exif::Tag::GPSLongitude, exif::Tag::GPSLongitudeRef, "W");
    Ok(match (lat, lon) {
        (Some(lat), Some(lon))
            if (-90.0..=90.0).contains(&lat) && (-180.0..=180.0).contains(&lon) && (lat, lon) != (0.0, 0.0) =>
        {
            Some((lat, lon))
        }
        _ => None,
    })
}

fn describe_location(lat: f64, lon: f64) -> String {
    match reverse_geocode(lat, lon) {
        Ok(place) if !place.is_empty() => format!("This photo was taken in or near {place}."),
        other => {
            if let Err(e) = other {
                log::warn!("reverse geocoding failed: {e:#}");
            }
            format!(
                "This photo has location information: latitude {lat:.4}, longitude {lon:.4}, \
                 but the place name could not be looked up."
            )
        }
    }
}

fn reverse_geocode(lat: f64, lon: f64) -> anyhow::Result<String> {
    // Four decimal places is about 11 metres: precise enough for a place
    // name without sending the exact spot.
    let body: Value = web_agent()
        .get("https://nominatim.openstreetmap.org/reverse")
        .query("format", "jsonv2")
        .query("lat", format!("{lat:.4}"))
        .query("lon", format!("{lon:.4}"))
        .query("zoom", "14")
        .query("accept-language", "en")
        .call()?
        .body_mut()
        .with_config()
        .limit(256 * 1024)
        .read_json()?;
    Ok(place_name(&body["address"]))
}

fn place_name(address: &Value) -> String {
    let field = |keys: &[&str]| keys.iter().find_map(|k| address[*k].as_str()).map(str::to_owned);
    let mut parts: Vec<String> = Vec::new();
    for keys in [
        &["suburb", "village", "hamlet", "neighbourhood"][..],
        &["town", "city", "municipality"][..],
        &["county", "state_district"][..],
        &["state"][..],
        &["country"][..],
    ] {
        if let Some(v) = field(keys)
            && !parts.contains(&v) {
                parts.push(v);
            }
    }
    parts.join(", ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builds_place_names() {
        let addr = json!({"suburb": "Digbeth", "city": "Birmingham", "state": "England", "country": "United Kingdom"});
        assert_eq!(place_name(&addr), "Digbeth, Birmingham, England, United Kingdom");
    }

    #[test]
    fn prefers_vision_models() {
        let models = vec!["llama3.1:8b".to_owned(), "llama3.2-vision:11b".to_owned()];
        assert_eq!(preferred_model(&models).unwrap(), "llama3.2-vision:11b");
        assert_eq!(preferred_model(&models[..1]).unwrap(), "llama3.1:8b");
        assert!(preferred_model(&[]).is_none());
    }

    #[test]
    fn strips_markdown() {
        assert_eq!(plain_text("# Title\n\n- **A** cat\n* sits"), "Title\n\nA cat\n\nsits");
    }
}
