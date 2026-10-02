//! Decoding speech audio, playing it, and saving it as WAV or MP3.

use anyhow::{Context, bail};
use serde::{Deserialize, Serialize};
use std::io::Cursor;
use std::num::NonZero;
use std::path::Path;

/// Mono audio as floating-point samples.
#[derive(Clone, Debug, Default)]
pub struct Pcm {
    pub samples: Vec<f32>,
    pub rate: u32,
}

impl Pcm {
    pub fn duration_secs(&self) -> f32 {
        if self.rate == 0 { 0.0 } else { self.samples.len() as f32 / self.rate as f32 }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum AudioFormat {
    #[default]
    Mp3,
    Wav,
}

impl AudioFormat {
    pub fn label(self) -> &'static str {
        match self {
            AudioFormat::Mp3 => "MP3 (smaller file)",
            AudioFormat::Wav => "WAV (uncompressed)",
        }
    }

    pub fn extension(self) -> &'static str {
        match self {
            AudioFormat::Mp3 => "mp3",
            AudioFormat::Wav => "wav",
        }
    }
}

/// Decodes WAV or MP3 bytes into mono samples.
pub fn decode(bytes: Vec<u8>) -> anyhow::Result<Pcm> {
    use rodio::Source;
    let decoder = rodio::Decoder::new(Cursor::new(bytes)).context("the speech audio could not be decoded")?;
    let channels = usize::from(decoder.channels().get());
    let rate = decoder.sample_rate().get();
    let interleaved: Vec<f32> = decoder.collect();
    let samples = if channels <= 1 {
        interleaved
    } else {
        interleaved
            .chunks(channels)
            .map(|frame| frame.iter().sum::<f32>() / frame.len() as f32)
            .collect()
    };
    Ok(Pcm { samples, rate })
}

/// Linear-interpolation resampling; good enough for speech.
fn resample(pcm: &Pcm, rate: u32) -> Vec<f32> {
    if pcm.rate == rate || pcm.samples.is_empty() {
        return pcm.samples.clone();
    }
    let ratio = f64::from(pcm.rate) / f64::from(rate);
    let out_len = (pcm.samples.len() as f64 / ratio) as usize;
    (0..out_len)
        .map(|i| {
            let pos = i as f64 * ratio;
            let idx = pos as usize;
            let frac = (pos - idx as f64) as f32;
            let a = pcm.samples[idx.min(pcm.samples.len() - 1)];
            let b = pcm.samples[(idx + 1).min(pcm.samples.len() - 1)];
            a + (b - a) * frac
        })
        .collect()
}

/// Joins pieces of speech into one recording at the first piece's sample rate,
/// with a short pause between pieces.
pub fn concat(pieces: &[Pcm]) -> Pcm {
    let rate = pieces.first().map(|p| p.rate).unwrap_or(22_050);
    let gap = vec![0.0; rate as usize / 5];
    let mut samples = Vec::new();
    for (i, piece) in pieces.iter().enumerate() {
        if i > 0 {
            samples.extend_from_slice(&gap);
        }
        samples.extend(resample(piece, rate));
    }
    Pcm { samples, rate }
}

fn to_i16(samples: &[f32]) -> Vec<i16> {
    samples.iter().map(|s| (s.clamp(-1.0, 1.0) * f32::from(i16::MAX)) as i16).collect()
}

pub fn save(pcm: &Pcm, path: &Path, format: AudioFormat) -> anyhow::Result<()> {
    if pcm.samples.is_empty() {
        bail!("there is no audio to save");
    }
    let bytes = match format {
        AudioFormat::Wav => encode_wav(pcm)?,
        AudioFormat::Mp3 => encode_mp3(pcm)?,
    };
    std::fs::write(path, bytes).with_context(|| format!("could not write {}", path.display()))
}

fn encode_wav(pcm: &Pcm) -> anyhow::Result<Vec<u8>> {
    let spec = hound::WavSpec {
        channels: 1,
        sample_rate: pcm.rate,
        bits_per_sample: 16,
        sample_format: hound::SampleFormat::Int,
    };
    let mut out = Cursor::new(Vec::new());
    let mut writer = hound::WavWriter::new(&mut out, spec)?;
    for s in to_i16(&pcm.samples) {
        writer.write_sample(s)?;
    }
    writer.finalize()?;
    Ok(out.into_inner())
}

fn encode_mp3(pcm: &Pcm) -> anyhow::Result<Vec<u8>> {
    use mp3lame_encoder::{Bitrate, Builder, FlushNoGap, MonoPcm, Quality};
    let lame_err = |e: &dyn std::fmt::Display| anyhow::anyhow!("MP3 encoder error: {e}");
    let mut builder = Builder::new().context("could not start the MP3 encoder")?;
    builder.set_num_channels(1).map_err(|e| lame_err(&e))?;
    builder.set_sample_rate(pcm.rate).map_err(|e| lame_err(&e))?;
    builder.set_brate(Bitrate::Kbps128).map_err(|e| lame_err(&e))?;
    builder.set_quality(Quality::Good).map_err(|e| lame_err(&e))?;
    let mut encoder = builder.build().map_err(|e| lame_err(&e))?;

    let samples = to_i16(&pcm.samples);
    let mut out = Vec::new();
    for block in samples.chunks(1152 * 64) {
        out.reserve(mp3lame_encoder::max_required_buffer_size(block.len()));
        encoder.encode_to_vec(MonoPcm(block), &mut out).map_err(|e| lame_err(&e))?;
    }
    out.reserve(7200);
    encoder.flush_to_vec::<FlushNoGap>(&mut out).map_err(|e| lame_err(&e))?;
    Ok(out)
}

/// Plays decoded speech through the default output device.
pub struct Playback {
    _device: rodio::MixerDeviceSink,
    player: std::sync::Arc<rodio::Player>,
}

impl Playback {
    pub fn open() -> anyhow::Result<Self> {
        let mut device = rodio::DeviceSinkBuilder::open_default_sink()
            .context("no audio output device is available")?;
        device.log_on_drop(false);
        let player = std::sync::Arc::new(rodio::Player::connect_new(device.mixer()));
        Ok(Self { _device: device, player })
    }

    /// A handle that other threads can use to pause or stop playback.
    pub fn handle(&self) -> std::sync::Arc<rodio::Player> {
        self.player.clone()
    }

    pub fn append(&self, pcm: Pcm) {
        let Some(rate) = NonZero::new(pcm.rate) else { return };
        self.player
            .append(rodio::buffer::SamplesBuffer::new(NonZero::<u16>::MIN, rate, pcm.samples));
    }

    /// How far into the piece now playing.
    pub fn position(&self) -> std::time::Duration {
        self.player.get_pos()
    }

    /// Number of pieces queued or playing.
    pub fn queued(&self) -> usize {
        self.player.len()
    }

}

#[cfg(test)]
mod tests {
    use super::*;

    fn tone() -> Pcm {
        Pcm { samples: (0..22_050).map(|i| (i as f32 * 0.05).sin() * 0.5).collect(), rate: 22_050 }
    }

    #[test]
    fn wav_round_trip() {
        let bytes = encode_wav(&tone()).unwrap();
        let back = decode(bytes).unwrap();
        assert_eq!(back.rate, 22_050);
        assert_eq!(back.samples.len(), 22_050);
    }

    #[test]
    fn mp3_encodes_and_decodes() {
        let bytes = encode_mp3(&tone()).unwrap();
        let back = decode(bytes).unwrap();
        assert!(back.samples.len() > 20_000);
    }

    #[test]
    fn concat_resamples() {
        let a = tone();
        let b = Pcm { samples: vec![0.0; 44_100], rate: 44_100 };
        let joined = concat(&[a, b]);
        assert_eq!(joined.rate, 22_050);
        assert!((joined.samples.len() as i64 - (22_050 + 4_410 + 22_050)).abs() < 3);
    }
}
