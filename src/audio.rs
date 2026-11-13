//! Turning speech into sound: decoding what the voices produce, playing it,
//! and encoding it as MP3.

use std::io::{Cursor, Write};
use std::num::NonZero;
use std::path::Path;
use std::sync::Arc;

use anyhow::Context;

/// Decoded mono audio.
#[derive(Debug, Clone, PartialEq)]
pub struct Pcm {
    pub samples: Vec<f32>,
    pub rate: u32,
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
        interleaved.chunks(channels).map(|frame| frame.iter().sum::<f32>() / frame.len() as f32).collect()
    };
    Ok(Pcm { samples, rate })
}

/// Linear interpolation, which is plenty for speech.
fn resample(pcm: &Pcm, rate: u32) -> Vec<f32> {
    let count = (pcm.samples.len() as u64 * u64::from(rate) / u64::from(pcm.rate.max(1))) as usize;
    (0..count)
        .map(|i| {
            let at = i as f64 * f64::from(pcm.rate) / f64::from(rate);
            let index = at as usize;
            let next = pcm.samples.get(index + 1).or(pcm.samples.get(index)).copied().unwrap_or(0.0);
            let this = pcm.samples.get(index).copied().unwrap_or(0.0);
            this + (next - this) * at.fract() as f32
        })
        .collect()
}

fn lame_err(e: &dyn std::fmt::Display) -> anyhow::Error {
    anyhow::anyhow!("MP3 encoder error: {e}")
}

/// Encodes speech as a 128 kbit/s mono MP3 a piece at a time, so only the
/// compressed sound is kept, not the whole recording. All the pieces come from
/// one voice, so they should share a sample rate, but services don't promise
/// that, so any that differs is resampled to the first one's rate.
pub struct Mp3Encoder {
    encoder: mp3lame_encoder::Encoder,
    rate: u32,
    out: Vec<u8>,
}

impl Mp3Encoder {
    pub fn new(rate: u32) -> anyhow::Result<Self> {
        use mp3lame_encoder::{Bitrate, Builder, Quality};
        let mut builder = Builder::new().context("could not start the MP3 encoder")?;
        builder.set_num_channels(1).map_err(|e| lame_err(&e))?;
        builder.set_sample_rate(rate).map_err(|e| lame_err(&e))?;
        builder.set_brate(Bitrate::Kbps128).map_err(|e| lame_err(&e))?;
        builder.set_quality(Quality::Good).map_err(|e| lame_err(&e))?;
        let encoder = builder.build().map_err(|e| lame_err(&e))?;
        Ok(Self { encoder, rate, out: Vec::new() })
    }

    pub fn push(&mut self, pcm: &Pcm) -> anyhow::Result<()> {
        let resampled;
        let samples = if pcm.rate == self.rate {
            &pcm.samples
        } else {
            resampled = resample(pcm, self.rate);
            &resampled
        };
        let samples: Vec<i16> = samples.iter().map(|s| (s.clamp(-1.0, 1.0) * f32::from(i16::MAX)) as i16).collect();
        for block in samples.chunks(1152 * 64) {
            self.out.reserve(mp3lame_encoder::max_required_buffer_size(block.len()));
            self.encoder.encode_to_vec(mp3lame_encoder::MonoPcm(block), &mut self.out).map_err(|e| lame_err(&e))?;
        }
        Ok(())
    }

    pub fn finish(mut self) -> anyhow::Result<Vec<u8>> {
        self.out.reserve(7200);
        self.encoder.flush_to_vec::<mp3lame_encoder::FlushNoGap>(&mut self.out).map_err(|e| lame_err(&e))?;
        // LAME starts the audio with an empty frame, filled in here now the
        // length is known, so that players show the right duration.
        let mut info = Vec::with_capacity(self.encoder.lame_tag_size());
        if self.encoder.lame_tag_encode_to_vec(&mut info).is_some() && info.len() <= self.out.len() {
            self.out[..info.len()].copy_from_slice(&info);
        }
        Ok(self.out)
    }
}

/// Encodes speech as a 128 kbit/s mono MP3.
#[cfg(test)]
pub fn encode_mp3(pcm: &Pcm) -> anyhow::Result<Vec<u8>> {
    let mut encoder = Mp3Encoder::new(pcm.rate)?;
    encoder.push(pcm)?;
    encoder.finish()
}

/// Writes MP3 bytes to a temporary file beside `path`, ready to be moved into
/// place with [`persist`]. Dropped instead, the temporary file is deleted.
pub fn stage(bytes: &[u8], path: &Path) -> anyhow::Result<tempfile::NamedTempFile> {
    let dir = path.parent().filter(|d| !d.as_os_str().is_empty()).unwrap_or(Path::new("."));
    let mut file = tempfile::Builder::new()
        .prefix(".commonspeech-")
        .suffix(".tmp")
        .tempfile_in(dir)
        .with_context(|| format!("could not write to {}", dir.display()))?;
    file.write_all(bytes).context("could not write the MP3 file")?;
    file.flush().context("could not write the MP3 file")?;
    Ok(file)
}

/// Moves a file made by [`stage`] into place, replacing any file at `path`.
pub fn persist(file: tempfile::NamedTempFile, path: &Path) -> anyhow::Result<()> {
    file.persist(path).map_err(|e| e.error).context("could not save the MP3 file")?;
    Ok(())
}

/// Writes an MP3 file all at once: into a temporary file beside it, then moved
/// into place, so a failure can't leave half a file or spoil an existing one.
/// The app saves speech a piece at a time instead; tests make files with this.
#[cfg(test)]
pub fn save_mp3(pcm: &Pcm, path: &Path) -> anyhow::Result<()> {
    persist(stage(&encode_mp3(pcm)?, path)?, path)
}

/// Plays speech through the default output device.
pub struct Playback {
    _device: rodio::MixerDeviceSink,
    player: Arc<rodio::Player>,
}

impl Playback {
    pub fn open() -> anyhow::Result<Self> {
        let mut device =
            rodio::DeviceSinkBuilder::open_default_sink().context("no audio output device is available")?;
        device.log_on_drop(false);
        let player = Arc::new(rodio::Player::connect_new(device.mixer()));
        Ok(Self { _device: device, player })
    }

    pub fn append(&self, pcm: Pcm) {
        let Some(rate) = NonZero::new(pcm.rate) else { return };
        self.player.append(rodio::buffer::SamplesBuffer::new(NonZero::<u16>::MIN, rate, pcm.samples));
    }

    /// Plays a whole sound, such as a file being decoded as it goes.
    pub fn append_source(&self, source: impl rodio::Source + Send + 'static) {
        self.player.append(source);
    }

    /// Pieces queued or playing.
    pub fn queued(&self) -> usize {
        self.player.len()
    }

    /// A handle another thread can use to pause or stop playback.
    pub fn player(&self) -> Arc<rodio::Player> {
        self.player.clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tone(rate: u32, seconds: f32) -> Pcm {
        let samples = (0..(rate as f32 * seconds) as usize)
            .map(|i| (i as f32 / rate as f32 * 440.0 * std::f32::consts::TAU).sin() * 0.5)
            .collect();
        Pcm { samples, rate }
    }

    #[test]
    fn mp3_round_trip_keeps_length() {
        let pcm = tone(22050, 1.0);
        let decoded = decode(encode_mp3(&pcm).unwrap()).unwrap();
        assert_eq!(decoded.rate, 22050);
        let difference = decoded.samples.len().abs_diff(pcm.samples.len());
        assert!(difference < 22050 / 4, "{} samples became {}", pcm.samples.len(), decoded.samples.len());
        assert!(decoded.samples.iter().any(|s| s.abs() > 0.2), "the sound survived");
    }

    #[test]
    fn saving_replaces_the_whole_file_and_leaves_nothing_else() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("out.mp3");
        std::fs::write(&path, b"old").unwrap();
        save_mp3(&tone(22050, 0.2), &path).unwrap();
        assert!(decode(std::fs::read(&path).unwrap()).is_ok());
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 1);
    }

    #[test]
    fn saving_somewhere_impossible_is_an_error() {
        let dir = tempfile::tempdir().unwrap();
        assert!(save_mp3(&tone(22050, 0.1), &dir.path().join("missing").join("out.mp3")).is_err());
    }

    #[test]
    fn encoding_in_pieces_resamples_to_the_first_rate() {
        let mut encoder = Mp3Encoder::new(22050).unwrap();
        encoder.push(&tone(22050, 1.0)).unwrap();
        encoder.push(&tone(44100, 1.0)).unwrap();
        let decoded = decode(encoder.finish().unwrap()).unwrap();
        assert_eq!(decoded.rate, 22050);
        let difference = decoded.samples.len().abs_diff(44100);
        assert!(difference < 22050 / 4, "two seconds became {} samples", decoded.samples.len());
    }

    #[test]
    fn a_staged_file_is_only_saved_when_persisted() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("out.mp3");
        drop(stage(b"abc", &path).unwrap());
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 0, "dropping leaves nothing");
        persist(stage(b"abc", &path).unwrap(), &path).unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), b"abc");
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 1);
    }

    #[test]
    fn garbage_is_an_error() {
        assert!(decode(b"not audio".to_vec()).is_err());
    }
}
