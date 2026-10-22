//! Image descriptions from a local AI model (through Ollama; see `ollama`),
//! with optional notes of where a geotagged photo was taken and the camera
//! that took it.
//!
//! The image is only ever sent to the Ollama server on this computer. If the
//! user turns on location lookup, the photo's rounded GPS coordinates (not the
//! photo) are sent to OpenStreetMap's Nominatim service to find a place name.
//! The camera is read from the photo itself, so nothing is sent for it.

use anyhow::{Context, bail};
use base64::Engine;
use serde_json::{Value, json};
use std::io::{Cursor, Read, Seek};
use std::path::Path;
use std::sync::OnceLock;
use std::time::Duration;

const MAX_IMAGE_BYTES: u64 = 100 * 1024 * 1024;
const MAX_EDGE: u32 = 1600;
/// The most samples (pixels times channels, such as red, green, blue and
/// alpha) a JPEG XL image or any of its frames may have: 67 megapixels with
/// transparency, or 89 without. The decoder works in 32-bit floats, so this
/// keeps it to about a gigabyte of memory. A file of a few kilobytes can claim
/// to be far larger, and the image crate's own limits don't reach inside
/// jxl-rs.
const MAX_JXL_SAMPLES: usize = 1 << 28;

const PROMPT: &str = "Describe this image for someone who is blind or has low vision. \
Start with a one-sentence summary. Then describe the important details: the setting, \
people and what they are doing, objects, colours, and any text that appears in the image, \
quoted exactly. Do not guess who anyone is. Write in plain sentences without markdown, \
headings, lists or emoji, because your answer will be read aloud.";

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

/// The model offered for download when Ollama has none that understands
/// images: small enough for most computers, and good at reading text in photos.
pub const SUGGESTED_MODEL: &str = "gemma3:4b";
/// Shown as "about 3.3 GB".
pub const SUGGESTED_MODEL_SIZE: &str = "3.3 GB";

/// Whether any of `models` looks like one that understands images.
pub fn has_vision_model(models: &[String]) -> bool {
    models.iter().any(|m| is_vision_model(m))
}

/// Picks the model most likely to understand images: the first whose name
/// matches a well-known vision model family, otherwise the first model.
pub fn preferred_model(models: &[String]) -> Option<&String> {
    models.iter().find(|m| is_vision_model(m)).or_else(|| models.first())
}

/// Whether the name matches a well-known vision model family.
fn is_vision_model(name: &str) -> bool {
    const VISION_HINTS: &[&str] =
        &["vision", "llava", "gemma3", "vl", "minicpm-v", "moondream", "bakllava", "pixtral", "mistral-small3"];
    let name = name.to_lowercase();
    VISION_HINTS.iter().any(|h| name.contains(h))
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

/// A photo ready to send to the model.
struct Prepared {
    jpeg: Vec<u8>,
    /// Whether the file held a PNG image, whatever its name.
    was_png: bool,
}

/// Loads the photo, applies its rotation, shrinks it and re-encodes it as a
/// JPEG. Re-encoding also strips metadata such as GPS from what is sent.
fn prepare_image(path: &Path) -> anyhow::Result<Prepared> {
    if std::fs::metadata(path)?.len() > MAX_IMAGE_BYTES {
        bail!("the image is too large (the limit is 100 MB)");
    }
    let bytes = if is_heif(path) { crate::platform::heic_to_jpeg(path)? } else { std::fs::read(path)? };
    let was_png = image::guess_format(&bytes).is_ok_and(|f| f == image::ImageFormat::Png);
    Ok(Prepared { jpeg: encode_jpeg(&decode(bytes)?)?, was_png })
}

/// Decodes a JPEG, PNG or JPEG XL image, whatever its file name says, the
/// right way up, no larger than `MAX_EDGE` and with no transparency.
fn decode(bytes: Vec<u8>) -> anyhow::Result<image::DynamicImage> {
    let mut img = if bytes.starts_with(&JXL_CODESTREAM) || bytes.starts_with(&JXL_CONTAINER) {
        decode_jpeg_xl(&bytes, MAX_JXL_SAMPLES)?
    } else {
        use image::ImageDecoder;
        // The image crate's default limits keep what it decodes to 512 MB.
        let mut decoder = image::ImageReader::new(Cursor::new(bytes))
            .with_guessed_format()?
            .into_decoder()
            .context("the image could not be read")?;
        let orientation = decoder.orientation().unwrap_or(image::metadata::Orientation::NoTransforms);
        let mut img = image::DynamicImage::from_decoder(decoder).context("the image could not be decoded")?;
        img.apply_orientation(orientation);
        img
    };
    // Before shrinking, which would otherwise blend the colour hidden in
    // transparent pixels into the edges of the ones beside them.
    img = on_white(img);
    if img.width() > MAX_EDGE || img.height() > MAX_EDGE {
        img = img.resize(MAX_EDGE, MAX_EDGE, image::imageops::FilterType::Triangle);
    }
    Ok(img)
}

/// Decodes the first frame of a JPEG XL image, as 8 bits a channel, if
/// neither the image nor any frame has more than `sample_limit` samples. The
/// decoder applies the image's own rotation.
fn decode_jpeg_xl(mut input: &[u8], sample_limit: usize) -> anyhow::Result<image::DynamicImage> {
    use jxl::api::{JxlColorType, JxlDataFormat, JxlDecoder, JxlDecoderOptions, JxlOutputBuffer, JxlPixelFormat, ProcessingResult};
    use jxl::headers::extra_channels::ExtraChannel;
    // The whole file is in `input`, so needing more means it was cut short.
    fn complete<T, F>(result: jxl::error::Result<ProcessingResult<T, F>>) -> anyhow::Result<T> {
        match result.context("the JPEG XL image could not be decoded")? {
            ProcessingResult::Complete { result } => Ok(result),
            ProcessingResult::NeedsMoreInput { .. } => bail!("the JPEG XL image is incomplete"),
        }
    }

    let mut options = JxlDecoderOptions::default();
    options.sample_limit = Some(sample_limit);
    let mut decoder = complete(JxlDecoder::new(options).process(&mut input, None))?;
    let info = decoder.basic_info();
    let (width, height) = info.size;
    let alpha = info.extra_channels.iter().any(|c| c.ec_type == ExtraChannel::Alpha);
    let extra_channels = info.extra_channels.len();
    let gray = decoder.current_pixel_format().color_type.is_grayscale();
    let (color_type, channels) = match (gray, alpha) {
        (true, false) => (JxlColorType::Grayscale, 1),
        (true, true) => (JxlColorType::GrayscaleAlpha, 2),
        (false, false) => (JxlColorType::Rgb, 3),
        (false, true) => (JxlColorType::Rgba, 4),
    };
    decoder
        .set_pixel_format(JxlPixelFormat {
            color_type,
            color_data_format: Some(JxlDataFormat::U8 { bit_depth: 8 }),
            extra_channel_format: vec![None; extra_channels],
        })
        .context("the JPEG XL image could not be decoded")?;

    let frame = complete(decoder.process(&mut input, None))?;
    let row = width.checked_mul(channels).context("the JPEG XL image is too large")?;
    let mut pixels = vec![0u8; row.checked_mul(height).context("the JPEG XL image is too large")?];
    let mut output = JxlOutputBuffer::new(&mut pixels, height, row);
    complete(frame.process(&mut input, std::slice::from_mut(&mut output), None))?;

    let (width, height) = (u32::try_from(width)?, u32::try_from(height)?);
    let img = match channels {
        1 => image::GrayImage::from_raw(width, height, pixels).map(image::DynamicImage::ImageLuma8),
        2 => image::GrayAlphaImage::from_raw(width, height, pixels).map(image::DynamicImage::ImageLumaA8),
        3 => image::RgbImage::from_raw(width, height, pixels).map(image::DynamicImage::ImageRgb8),
        _ => image::RgbaImage::from_raw(width, height, pixels).map(image::DynamicImage::ImageRgba8),
    };
    img.context("the JPEG XL image could not be decoded")
}

/// Puts an image with transparent parts on a white background, as most
/// viewers show it. JPEG has no transparency, and just dropping it would
/// leave whatever colour the hidden pixels have, often black, which can hide
/// dark text and lines in logos, diagrams and screenshots.
fn on_white(img: image::DynamicImage) -> image::DynamicImage {
    if !img.color().has_alpha() {
        return img;
    }
    let mut rgba = img.into_rgba8();
    for pixel in rgba.pixels_mut() {
        let [r, g, b, a] = pixel.0;
        let blend = |c: u8| ((u16::from(c) * u16::from(a) + 255 * (255 - u16::from(a)) + 127) / 255) as u8;
        pixel.0 = [blend(r), blend(g), blend(b), 255];
    }
    image::DynamicImage::ImageRgba8(rgba)
}

/// Removes markdown that would otherwise be read out as symbols.
fn plain_text(text: &str) -> String {
    text.lines()
        .map(|l| l.trim_start_matches(['#', '-', '*', '>', ' ']).replace("**", "").replace('*', ""))
        .filter(|l| !l.trim().is_empty())
        .collect::<Vec<_>>()
        .join("\n\n")
}

/// What the model said an image shows.
pub struct Description {
    pub text: String,
    /// Whether the file held a PNG image, whatever its name.
    pub was_png: bool,
}

pub fn describe(path: &Path, model: &str, resolve_location: bool, read_camera: bool) -> anyhow::Result<Description> {
    if model.is_empty() {
        bail!("choose an image description model on the Settings tab first");
    }
    let Prepared { jpeg, was_png } = prepare_image(path)?;
    let b64 = base64::engine::general_purpose::STANDARD.encode(jpeg);
    log::info!("describing image with model {model}");
    // Ollama explains a failure, such as a model it can't load, in the reply.
    let body: Value = crate::ollama::agent()
        .post(&format!("{}/api/generate", crate::ollama::OLLAMA_URL))
        .config()
        .http_status_as_error(false)
        .build()
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
    if let Some(error) = body["error"].as_str() {
        bail!("Ollama could not use the model {model}: {error}");
    }
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
    if read_camera {
        match camera_name(path) {
            Ok(Some(camera)) => {
                description.push_str("\n\n");
                description.push_str(&format!("Camera used: {camera}."));
            }
            Ok(None) => log::info!("image does not say which camera took it"),
            Err(e) => log::warn!("could not read image camera: {e:#}"),
        }
    }
    Ok(Description { text: description, was_png })
}

/// The signature box that starts a JPEG XL file in the container format,
/// which is the only form that can hold EXIF data.
const JXL_CONTAINER: [u8; 12] = [0, 0, 0, 0x0c, b'J', b'X', b'L', b' ', 0x0d, 0x0a, 0x87, 0x0a];
/// The start of a bare JPEG XL codestream, which has no metadata.
const JXL_CODESTREAM: [u8; 2] = [0xff, 0x0a];

/// Finds the EXIF data in a JPEG XL container's `Exif` box. Returns `None`
/// if there is none, or if it is Brotli-compressed (in a `brob` box), which
/// isn't supported.
fn jxl_exif(reader: &mut (impl Read + Seek)) -> std::io::Result<Option<Vec<u8>>> {
    let invalid = || std::io::Error::new(std::io::ErrorKind::InvalidData, "a JPEG XL box has an impossible size");
    let mut header = [0u8; 8];
    loop {
        match reader.read_exact(&mut header) {
            Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => return Ok(None),
            result => result?,
        }
        let kind = &header[4..];
        // A size of 1 means the real size follows; 0 means the box runs to
        // the end of the file. Sizes include the header.
        let body = match u32::from_be_bytes(header[..4].try_into().unwrap()) {
            0 => None,
            1 => {
                let mut large = [0u8; 8];
                reader.read_exact(&mut large)?;
                Some(u64::from_be_bytes(large).checked_sub(16).ok_or_else(invalid)?)
            }
            size => Some(u64::from(size).checked_sub(8).ok_or_else(invalid)?),
        };
        if kind == b"Exif" {
            let mut data = Vec::new();
            match body {
                Some(len) => reader.take(len).read_to_end(&mut data)?,
                None => reader.read_to_end(&mut data)?,
            };
            // The box starts with where the EXIF data begins after these 4 bytes.
            let Some(start) = data.get(..4) else { return Ok(None) };
            let start = 4usize.saturating_add(u32::from_be_bytes(start.try_into().unwrap()) as usize);
            return Ok(data.get(start..).map(<[u8]>::to_vec));
        }
        match body {
            Some(len) => reader.seek(std::io::SeekFrom::Current(i64::try_from(len).map_err(|_| invalid())?))?,
            None => return Ok(None),
        };
    }
}

/// Reads the EXIF data of a JPEG, PNG, HEIF or JPEG XL file. Returns `None`
/// if it has none.
fn read_exif(path: &Path) -> anyhow::Result<Option<exif::Exif>> {
    let mut file = std::io::BufReader::new(std::fs::File::open(path)?);
    // Enough to tell a JPEG XL file, however the reads come back.
    let mut start = Vec::with_capacity(JXL_CONTAINER.len());
    (&mut file).take(JXL_CONTAINER.len() as u64).read_to_end(&mut start)?;
    file.seek(std::io::SeekFrom::Start(0))?;
    let read = if start.starts_with(&JXL_CODESTREAM) {
        return Ok(None);
    } else if start.starts_with(&JXL_CONTAINER) {
        match jxl_exif(&mut file)? {
            Some(raw) => exif::Reader::new().read_raw(raw),
            None => return Ok(None),
        }
    } else {
        exif::Reader::new().read_from_container(&mut file)
    };
    match read {
        Ok(exif) => Ok(Some(exif)),
        Err(exif::Error::NotFound(_)) => Ok(None),
        Err(e) => Err(e.into()),
    }
}

/// Reads GPS latitude and longitude from the EXIF data of a JPEG, PNG,
/// HEIF or JPEG XL file.
pub fn gps_coordinates(path: &Path) -> anyhow::Result<Option<(f64, f64)>> {
    let Some(exif) = read_exif(path)? else { return Ok(None) };
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

/// Reads the make and model of the camera that took a photo from its EXIF
/// data, such as "Apple iPhone 15 Pro".
pub fn camera_name(path: &Path) -> anyhow::Result<Option<String>> {
    let Some(exif) = read_exif(path)? else { return Ok(None) };
    let text = |tag| -> String {
        match exif.get_field(tag, exif::In::PRIMARY).map(|f| &f.value) {
            Some(exif::Value::Ascii(parts)) => parts
                .iter()
                .map(|p| String::from_utf8_lossy(p).trim_matches(|c: char| c == '\0' || c.is_whitespace()).to_owned())
                .find(|p| !p.is_empty())
                .unwrap_or_default(),
            _ => String::new(),
        }
    };
    Ok(camera_from(&text(exif::Tag::Make), &text(exif::Tag::Model)))
}

/// Joins a camera's make and model, leaving out the make when the model
/// already starts with it, as in "NIKON CORPORATION" and "NIKON D750".
fn camera_from(make: &str, model: &str) -> Option<String> {
    let brand = make.split_whitespace().next().unwrap_or_default();
    let starts_with_brand = model.get(..brand.len()).is_some_and(|start| start.eq_ignore_ascii_case(brand));
    match (make.is_empty(), model.is_empty()) {
        (true, true) => None,
        (false, true) => Some(make.to_owned()),
        (true, false) => Some(model.to_owned()),
        (false, false) if starts_with_brand => Some(model.to_owned()),
        (false, false) => Some(format!("{make} {model}")),
    }
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

    /// A lossless 4×2 JPEG XL codestream: red, green, blue and transparent
    /// pixels, then white, black, grey and half-transparent black ones.
    const JXL: &[u8] = include_bytes!("../assets/tests/data/rgba-4x2.jxl");
    /// What `JXL` should look like once it is on a white background.
    const JXL_ON_WHITE: [[u8; 3]; 8] = [
        [255, 0, 0], [0, 255, 0], [0, 0, 255], [255, 255, 255],
        [255, 255, 255], [0, 0, 0], [128, 128, 128], [127, 127, 127],
    ];

    fn pixels(img: &image::DynamicImage) -> Vec<[u8; 3]> {
        img.to_rgb8().pixels().map(|p| p.0).collect()
    }

    /// A JPEG XL container holding `JXL`, with EXIF data giving `gps` as its
    /// location, and the boxes a real file has around them.
    fn jxl_container(gps: Option<(&str, f64, &str, f64)>) -> Vec<u8> {
        let mut file = JXL_CONTAINER.to_vec();
        let mut push_box = |kind: &[u8], body: &[u8]| {
            file.extend_from_slice(&(8 + body.len() as u32).to_be_bytes());
            file.extend_from_slice(kind);
            file.extend_from_slice(body);
        };
        push_box(b"ftyp", b"jxl \0\0\0\0jxl ");
        if let Some((lat_ref, lat, lon_ref, lon)) = gps {
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
            // EXIF data written by cameras often starts after a few bytes of
            // padding, which the offset at the start of the box skips.
            let mut body = vec![0, 0, 0, 2, 0xaa, 0xbb];
            body.extend_from_slice(tiff.get_ref());
            push_box(b"Exif", &body);
        }
        push_box(b"jxlc", JXL);
        file
    }

    fn gps_of(bytes: &[u8]) -> Option<(f64, f64)> {
        let mut file = tempfile::NamedTempFile::new().unwrap();
        std::io::Write::write_all(&mut file, bytes).unwrap();
        gps_coordinates(file.path()).unwrap()
    }

    #[test]
    fn decodes_jpeg_xl_on_white() {
        let img = decode(JXL.to_vec()).unwrap();
        assert_eq!((img.width(), img.height()), (4, 2));
        assert_eq!(pixels(&img), JXL_ON_WHITE);
        // The container form, which can carry metadata, decodes the same.
        assert_eq!(pixels(&decode(jxl_container(Some(("N", 52.4, "W", 1.9)))).unwrap()), JXL_ON_WHITE);
    }

    #[test]
    fn decodes_png_on_white() {
        let source = image::RgbaImage::from_fn(3, 1, |x, _| image::Rgba([[200, 0, 0, 255], [0, 0, 0, 0], [0, 0, 255, 51]][x as usize]));
        let mut png = Vec::new();
        source.write_to(&mut Cursor::new(&mut png), image::ImageFormat::Png).unwrap();
        assert_eq!(pixels(&decode(png).unwrap()), [[200, 0, 0], [255, 255, 255], [204, 204, 255]]);
    }

    #[test]
    fn sends_jpeg_xl_and_png_as_jpeg() {
        let mut png = Vec::new();
        image::RgbImage::new(2000, 1000).write_to(&mut Cursor::new(&mut png), image::ImageFormat::Png).unwrap();
        for (bytes, size) in [(JXL.to_vec(), (4, 2)), (png, (MAX_EDGE, MAX_EDGE / 2))] {
            let jpeg = encode_jpeg(&decode(bytes).unwrap()).unwrap();
            let sent = image::load_from_memory_with_format(&jpeg, image::ImageFormat::Jpeg).unwrap();
            assert_eq!((sent.width(), sent.height()), size);
        }
    }

    #[test]
    fn shrinks_transparent_images_without_dark_edges() {
        // White on the left, then transparent black: once on white and
        // shrunk, every pixel should still be white.
        let source = image::RgbaImage::from_fn(MAX_EDGE * 2, 2, |x, _| {
            image::Rgba(if x < MAX_EDGE { [255, 255, 255, 255] } else { [0, 0, 0, 0] })
        });
        let mut png = Vec::new();
        source.write_to(&mut Cursor::new(&mut png), image::ImageFormat::Png).unwrap();
        let img = decode(png).unwrap();
        assert_eq!(img.width(), MAX_EDGE);
        assert!(pixels(&img).iter().all(|p| *p == [255, 255, 255]));
    }

    #[test]
    fn limits_jpeg_xl_size() {
        // 4×2 pixels of red, green, blue and alpha, which jxl-rs counts as
        // 16×2 (it rounds rows up to 16 pixels) and must be under the limit.
        assert!(decode_jpeg_xl(JXL, 129).is_ok());
        assert!(decode_jpeg_xl(JXL, 128).is_err());
    }

    #[test]
    fn notices_png_images_whatever_their_name() {
        let dir = tempfile::tempdir().unwrap();
        let mut png = Vec::new();
        image::RgbImage::new(4, 4).write_to(&mut Cursor::new(&mut png), image::ImageFormat::Png).unwrap();
        let mut jpeg = Vec::new();
        image::RgbImage::new(4, 4).write_to(&mut Cursor::new(&mut jpeg), image::ImageFormat::Jpeg).unwrap();
        for (name, bytes, was_png) in [("Screenshot.jpg", &png, true), ("Screenshot.png", &png, true), ("Photo.jpg", &jpeg, false)] {
            let path = dir.path().join(name);
            std::fs::write(&path, bytes).unwrap();
            assert_eq!(prepare_image(&path).unwrap().was_png, was_png, "{name}");
        }
        let path = dir.path().join("Photo.jxl");
        std::fs::write(&path, JXL).unwrap();
        assert!(!prepare_image(&path).unwrap().was_png);
    }

    #[test]
    fn rejects_damaged_jpeg_xl() {
        assert!(decode(JXL[..JXL.len() / 2].to_vec()).is_err());
    }

    #[test]
    fn reads_jpeg_xl_location() {
        let (lat, lon) = gps_of(&jxl_container(Some(("S", 33.8569, "E", 151.2153)))).unwrap();
        assert!((lat + 33.8569).abs() < 0.001 && (lon - 151.2153).abs() < 0.001, "{lat}, {lon}");
        assert_eq!(gps_of(&jxl_container(None)), None);
        // A bare codestream has nowhere to keep a location.
        assert_eq!(gps_of(JXL), None);
    }

    #[test]
    fn names_cameras() {
        assert_eq!(camera_from("Apple", "iPhone 15 Pro").as_deref(), Some("Apple iPhone 15 Pro"));
        assert_eq!(camera_from("NIKON CORPORATION", "NIKON D750").as_deref(), Some("NIKON D750"));
        assert_eq!(camera_from("Canon", "Canon EOS R5").as_deref(), Some("Canon EOS R5"));
        assert_eq!(camera_from("", "DSC-RX100").as_deref(), Some("DSC-RX100"));
        assert_eq!(camera_from("SONY", "").as_deref(), Some("SONY"));
        assert_eq!(camera_from("", ""), None);
    }

    #[test]
    fn rejects_impossible_jpeg_xl_box() {
        let mut file = JXL_CONTAINER.to_vec();
        file.extend_from_slice(&[0, 0, 0, 4, b'f', b't', b'y', b'p']);
        assert!(jxl_exif(&mut Cursor::new(file)).is_err());
    }
}
