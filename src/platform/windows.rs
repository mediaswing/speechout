//! Windows: system voices use the WinRT speech synthesiser (the same voices
//! Narrator offers), HEIC conversion uses the Windows Imaging Component with the
//! HEIF Image Extensions from the Microsoft Store, and API keys are kept under
//! HKEY_CURRENT_USER in the registry.

use super::SecretStore;
use crate::speech::Voice;
use anyhow::{Context, bail};
use std::path::Path;
use windows::Media::SpeechSynthesis::{SpeechSynthesizer, VoiceInformation};
use windows::Storage::Streams::DataReader;
use windows::Win32::Foundation::GENERIC_READ;
use windows::Win32::Graphics::Imaging::{
    CLSID_WICImagingFactory, GUID_WICPixelFormat24bppBGR, IWICImagingFactory,
    WICConvertBitmapSource, WICDecodeMetadataCacheOnDemand,
};
use windows::Win32::System::Com::{
    CLSCTX_INPROC_SERVER, COINIT_MULTITHREADED, CoCreateInstance, CoInitializeEx,
};
use windows::Win32::UI::Shell::ShellExecuteW;
use windows::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;
use windows::core::{HSTRING, w};
use winreg::RegKey;
use winreg::enums::{HKEY_CURRENT_USER, KEY_READ, KEY_WRITE};

const REGISTRY_PATH: &str = r"Software\SpeechOut";

/// Refuse to decode images larger than this many pixels (about 100 megapixels).
const MAX_PIXELS: u64 = 100_000_000;

pub const SECRET_STORE_DESCRIPTION: &str =
    r"the Windows registry (HKEY_CURRENT_USER\Software\SpeechOut)";

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

pub fn heic_to_jpeg(path: &Path) -> anyhow::Result<Vec<u8>> {
    // SAFETY: plain COM calls on interfaces owned by this function. COM may
    // already be initialised on this thread, which CoInitializeEx reports as a
    // harmless error code that we ignore.
    let (width, height, bgr) = unsafe {
        let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
        let factory: IWICImagingFactory =
            CoCreateInstance(&CLSID_WICImagingFactory, None, CLSCTX_INPROC_SERVER)
                .context("Windows Imaging Component is unavailable")?;
        let decoder = factory
            .CreateDecoderFromFilename(
                &HSTRING::from(path.as_os_str()),
                None,
                GENERIC_READ,
                WICDecodeMetadataCacheOnDemand,
            )
            .context(
                "Windows could not open this HEIC image. Install \"HEIF Image Extensions\" \
                 from the Microsoft Store and try again.",
            )?;
        let frame = decoder.GetFrame(0)?;
        let source = WICConvertBitmapSource(&GUID_WICPixelFormat24bppBGR, &frame)?;
        let (mut width, mut height) = (0u32, 0u32);
        source.GetSize(&mut width, &mut height)?;
        if u64::from(width) * u64::from(height) > MAX_PIXELS || width == 0 || height == 0 {
            bail!("this image is too large to describe");
        }
        let stride = width * 3;
        let mut bgr = vec![0u8; stride as usize * height as usize];
        source.CopyPixels(std::ptr::null(), stride, &mut bgr)?;
        (width, height, bgr)
    };
    let mut rgb = bgr;
    for px in rgb.as_chunks_mut::<3>().0 {
        px.swap(0, 2);
    }
    let image = image::RgbImage::from_raw(width, height, rgb).context("invalid image data")?;
    crate::vision::encode_jpeg(&image::DynamicImage::ImageRgb8(image))
}

pub fn open_url(url: &str) -> anyhow::Result<()> {
    // SAFETY: all strings are valid, NUL-terminated wide strings that outlive the call.
    let result = unsafe {
        ShellExecuteW(None, w!("open"), &HSTRING::from(url), None, None, SW_SHOWNORMAL)
    };
    // ShellExecute reports success with a value greater than 32.
    if result.0 as usize <= 32 {
        bail!("could not open the web browser");
    }
    Ok(())
}

pub fn secret_get(name: &str) -> SecretStore<Option<String>> {
    let hkcu = RegKey::predef(HKEY_CURRENT_USER);
    match hkcu.open_subkey_with_flags(REGISTRY_PATH, KEY_READ) {
        Ok(key) => SecretStore::Ok(key.get_value::<String, _>(name).ok()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => SecretStore::Ok(None),
        Err(e) => {
            log::warn!("registry read failed: {e}");
            SecretStore::Unsupported
        }
    }
}

pub fn secret_set(name: &str, value: &str) -> SecretStore<()> {
    let hkcu = RegKey::predef(HKEY_CURRENT_USER);
    match hkcu
        .create_subkey_with_flags(REGISTRY_PATH, KEY_READ | KEY_WRITE)
        .and_then(|(key, _)| key.set_value(name, &value))
    {
        Ok(()) => SecretStore::Ok(()),
        Err(e) => {
            log::warn!("registry write failed: {e}");
            SecretStore::Unsupported
        }
    }
}

pub fn secret_delete(name: &str) -> SecretStore<()> {
    let hkcu = RegKey::predef(HKEY_CURRENT_USER);
    match hkcu.open_subkey_with_flags(REGISTRY_PATH, KEY_READ | KEY_WRITE) {
        Ok(key) => match key.delete_value(name) {
            Ok(()) => SecretStore::Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => SecretStore::Ok(()),
            Err(_) => SecretStore::Unsupported,
        },
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => SecretStore::Ok(()),
        Err(_) => SecretStore::Unsupported,
    }
}
