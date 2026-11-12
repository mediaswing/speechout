//! Windows: system voices use the WinRT speech synthesiser (the same voices
//! Narrator offers). HEIC conversion uses the libheif built into release builds,
//! or the Windows Imaging Component with the HEIF and HEVC Video Extensions
//! from the Microsoft Store.

use std::path::Path;

use anyhow::{Context, bail};
use windows::Media::SpeechSynthesis::{SpeechSynthesizer, VoiceInformation};
use windows::Storage::Streams::DataReader;
use windows::Win32::Foundation::GENERIC_READ;
use windows::Win32::Graphics::Imaging::{
    CLSID_WICImagingFactory, GUID_WICPixelFormat24bppBGR, IWICImagingFactory, WICConvertBitmapSource,
    WICDecodeMetadataCacheOnDemand,
};
use windows::Win32::System::Com::{CLSCTX_INPROC_SERVER, COINIT_MULTITHREADED, CoCreateInstance, CoInitializeEx};
use windows::core::HSTRING;

use crate::speech::Voice;

/// Refuse to decode images larger than this many pixels (about 100 megapixels).
const MAX_PIXELS: u64 = 100_000_000;

fn all_voices() -> windows::core::Result<Vec<VoiceInformation>> {
    let list = SpeechSynthesizer::AllVoices()?;
    let mut voices = Vec::new();
    for i in 0..list.Size()? {
        voices.push(list.GetAt(i)?);
    }
    Ok(voices)
}

pub fn system_voices() -> anyhow::Result<Vec<Voice>> {
    let voices = all_voices().context("could not list Windows voices")?;
    Ok(voices
        .iter()
        .filter_map(|v| {
            let id = v.Id().ok()?.to_string();
            let name = v.DisplayName().ok()?.to_string();
            let language = v.Language().map(|l| l.to_string()).unwrap_or_default();
            Some(Voice { id, name: format!("{name} ({language})") })
        })
        .collect())
}

pub fn synthesize(text: &str, voice_id: &str) -> anyhow::Result<Vec<u8>> {
    let run = || -> windows::core::Result<Vec<u8>> {
        let synth = SpeechSynthesizer::new()?;
        if !voice_id.is_empty() {
            let wanted = HSTRING::from(voice_id);
            for voice in all_voices()? {
                if voice.Id()? == wanted {
                    synth.SetVoice(&voice)?;
                    break;
                }
            }
        }
        // This call takes plain text, not SSML, so a document can't control the voice.
        let stream = synth.SynthesizeTextToStreamAsync(&HSTRING::from(text))?.join()?;
        let size = u32::try_from(stream.Size()?).unwrap_or(u32::MAX);
        let reader = DataReader::CreateDataReader(&stream.GetInputStreamAt(0)?)?;
        reader.LoadAsync(size)?.join()?;
        let mut bytes = vec![0u8; size as usize];
        reader.ReadBytes(&mut bytes)?;
        Ok(bytes)
    };
    run().context("the Windows speech synthesiser failed")
}

/// MF_E_TOPO_CODEC_NOT_FOUND: "No suitable transform was found to encode or
/// decode the content."
const CODEC_NOT_FOUND: u32 = 0xC00D_5212;

/// The picture inside a HEIC file is compressed with HEVC, which Windows can
/// only decode with HEVC Video Extensions, a separate add-on from the HEIF one.
fn decode_error(e: windows::core::Error) -> anyhow::Error {
    if e.code().0 as u32 == CODEC_NOT_FOUND {
        log::warn!("HEIC decode failed: {e}");
        anyhow::anyhow!(
            "Windows needs \"HEVC Video Extensions\" from the Microsoft Store to open HEIC photos. Install it and try \
             again"
        )
    } else {
        e.into()
    }
}

/// Converts a HEIC photo with the libheif built into the app, if this build has
/// it, so no Store add-ons are needed. Falls back to Windows' own codecs.
pub fn heic_to_jpeg(path: &Path) -> anyhow::Result<Vec<u8>> {
    #[cfg(feature = "bundled-heif")]
    match heic_with_libheif(path) {
        Ok(jpeg) => return Ok(jpeg),
        Err(e) => log::warn!("libheif could not convert the photo, trying Windows instead: {e:#}"),
    }
    heic_with_wic(path)
}

#[cfg(feature = "bundled-heif")]
fn heic_with_libheif(path: &Path) -> anyhow::Result<Vec<u8>> {
    use libheif_rs::{ColorSpace, HeifContext, LibHeif, RgbChroma};
    let bytes = std::fs::read(path).context("the photo could not be read")?;
    let context = HeifContext::read_from_bytes(&bytes)?;
    let handle = context.primary_image_handle()?;
    if u64::from(handle.width()) * u64::from(handle.height()) > MAX_PIXELS {
        bail!("this image is too large to describe");
    }
    // Decoding applies the rotation and cropping stored in the file.
    let image = LibHeif::new().decode(&handle, ColorSpace::Rgb(RgbChroma::Rgb), None)?;
    let plane = image.planes().interleaved.context("libheif returned no pixels")?;
    if plane.storage_bits_per_pixel != 24 {
        bail!("libheif returned {} bits per pixel, not 24", plane.storage_bits_per_pixel);
    }
    // Rows can be padded, so copy each one without its padding.
    let row = plane.width as usize * 3;
    let mut rgb = Vec::with_capacity(row * plane.height as usize);
    for line in plane.data.chunks(plane.stride).take(plane.height as usize) {
        rgb.extend_from_slice(line.get(..row).context("libheif returned a short row")?);
    }
    let image = image::RgbImage::from_raw(plane.width, plane.height, rgb).context("invalid image data")?;
    crate::vision::encode_jpeg(&image::DynamicImage::ImageRgb8(image))
}

fn heic_with_wic(path: &Path) -> anyhow::Result<Vec<u8>> {
    // SAFETY: plain COM calls on interfaces owned by this function. COM may
    // already be initialised on this thread, which CoInitializeEx reports as a
    // harmless error code that is ignored.
    let (width, height, bgr) = unsafe {
        let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
        let factory: IWICImagingFactory = CoCreateInstance(&CLSID_WICImagingFactory, None, CLSCTX_INPROC_SERVER)
            .context("Windows Imaging Component is unavailable")?;
        let decoder = factory
            .CreateDecoderFromFilename(&HSTRING::from(path.as_os_str()), None, GENERIC_READ, WICDecodeMetadataCacheOnDemand)
            .context(
                "Windows could not open this HEIC image. Install \"HEIF Image Extensions\" from the Microsoft Store \
                 and try again",
            )?;
        let frame = decoder.GetFrame(0).map_err(decode_error)?;
        let source = WICConvertBitmapSource(&GUID_WICPixelFormat24bppBGR, &frame).map_err(decode_error)?;
        let (mut width, mut height) = (0u32, 0u32);
        source.GetSize(&mut width, &mut height)?;
        if u64::from(width) * u64::from(height) > MAX_PIXELS || width == 0 || height == 0 {
            bail!("this image is too large to describe");
        }
        let stride = width * 3;
        let mut bgr = vec![0u8; stride as usize * height as usize];
        source.CopyPixels(std::ptr::null(), stride, &mut bgr).map_err(decode_error)?;
        (width, height, bgr)
    };
    let mut rgb = bgr;
    for pixel in rgb.as_chunks_mut::<3>().0 {
        pixel.swap(0, 2);
    }
    let image = image::RgbImage::from_raw(width, height, rgb).context("invalid image data")?;
    crate::vision::encode_jpeg(&image::DynamicImage::ImageRgb8(image))
}

/// Stops console programs flashing up a window.
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

/// Starts Ollama's tray app, which runs the server in the background, or the
/// server alone if the tray app is missing. Only the per-user folder Ollama's
/// installer uses is searched, so nothing unexpected on PATH can be started.
pub fn start_ollama() -> bool {
    use std::os::windows::process::CommandExt;
    let Some(dir) = dirs::data_local_dir().map(|d| d.join("Programs").join("Ollama")) else { return false };
    let tray = dir.join("ollama app.exe");
    let server = dir.join("ollama.exe");
    let mut command = if tray.is_file() {
        std::process::Command::new(tray)
    } else if server.is_file() {
        let mut command = std::process::Command::new(server);
        command.arg("serve").creation_flags(CREATE_NO_WINDOW);
        command
    } else {
        return false;
    };
    command
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .is_ok()
}

/// Folders under %APPDATA% are already private to the user.
pub fn restrict_dir_to_owner(_dir: &Path) {}

pub fn create_private_file(path: &Path) -> std::io::Result<()> {
    std::fs::OpenOptions::new().create(true).append(true).open(path).map(|_| ())
}

/// whisper.cpp has no installer for Windows, so it is only found where the user
/// chooses it, or in the app's own whisper folder.
pub fn whisper_programs() -> Vec<std::path::PathBuf> {
    Vec::new()
}

pub fn whisper_installer() -> Option<crate::transcribe::whisper::Installer> {
    None
}

pub const WHISPER_HELP: &str = "whisper.cpp isn't installed. Download it for Windows from \
     github.com/ggml-org/whisper.cpp/releases, then choose whisper-cli.exe on the Settings tab";
