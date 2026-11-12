//! whisper.cpp, which turns speech into text on this computer. The app runs its
//! `whisper-cli` program, as it does other programs: directly, by absolute path,
//! never through a shell. It needs a model, which is downloaded once, only when
//! the user agrees, and checked against the size and SHA-256 it should have.

use std::io::{BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Duration;

use anyhow::{Context, bail};
use serde_json::Value;
use sha2::{Digest, Sha256};

use crate::worker::Control;

/// whisper.cpp's base English model: quick, and good with clear speech.
pub const MODEL_FILE: &str = "ggml-base.en.bin";
const MODEL_URL: &str = "https://huggingface.co/ggerganov/whisper.cpp/resolve/main/ggml-base.en.bin";
const MODEL_SHA256: &str = "a03779c86df3323075f5e796cb2ce5029f00ec8869eee3fdfb897afe36c6d002";
pub const MODEL_BYTES: u64 = 147_964_211;
/// The program's own names for itself, newest first.
pub const PROGRAM_NAMES: [&str; 2] = ["whisper-cli", "whisper-cpp"];
/// The most a transcript from whisper.cpp can be.
const MAX_JSON_BYTES: u64 = 64 * 1024 * 1024;

/// Where the model is kept: a `whisper` folder beside the settings.
pub fn model_path() -> PathBuf {
    crate::store::data_dir().join("whisper").join(MODEL_FILE)
}

/// The whisper.cpp program: the one chosen on the Settings tab, or else the
/// first found where it is usually installed.
pub fn find_program(chosen: Option<&Path>) -> Option<PathBuf> {
    if let Some(chosen) = chosen {
        return chosen.is_file().then(|| chosen.to_owned());
    }
    let own = crate::store::data_dir().join("whisper");
    let in_own_folder = PROGRAM_NAMES.iter().map(|n| own.join(format!("{n}{}", std::env::consts::EXE_SUFFIX)));
    crate::platform::whisper_programs().into_iter().chain(in_own_folder).find(|p| p.is_file())
}

/// A way to install whisper.cpp on this system, offered only with the user's
/// agreement.
pub struct Installer {
    pub program: PathBuf,
    pub args: &'static [&'static str],
    /// The command, as the user would type it, for the question that asks.
    pub command: &'static str,
}

/// Runs the installer. Returns `Ok(false)` if stopped.
pub fn install(installer: &Installer, control: &Control) -> anyhow::Result<bool> {
    log::info!("Installing whisper.cpp with {}", installer.command);
    let mut child = Command::new(&installer.program)
        .args(installer.args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .with_context(|| format!("{} could not be started", installer.command))?;
    loop {
        if control.stopped() {
            let _ = child.kill();
            let _ = child.wait();
            return Ok(false);
        }
        if let Some(status) = child.try_wait()? {
            if !status.success() {
                bail!("{} failed. Try running it yourself in Terminal to see why", installer.command);
            }
            return Ok(true);
        }
        std::thread::sleep(Duration::from_millis(200));
    }
}

/// Downloads the model, calling `progress` with the share done. Returns
/// `Ok(false)` if stopped. Nothing is left behind unless the whole file arrives
/// and is exactly what it should be.
pub fn download_model(dest: &Path, control: &Control, progress: &dyn Fn(f32)) -> anyhow::Result<bool> {
    if control.stopped() {
        return Ok(false);
    }
    let dir = dest.parent().context("the model has nowhere to go")?;
    std::fs::create_dir_all(dir).with_context(|| format!("could not create {}", dir.display()))?;
    let agent: ureq::Agent = ureq::Agent::config_builder()
        .https_only(true)
        // Hugging Face sends the file from its download network. The checksum
        // below is what makes it safe to follow.
        .max_redirects(5)
        .timeout_connect(Some(Duration::from_secs(30)))
        .timeout_recv_body(Some(Duration::from_secs(60)))
        .user_agent(concat!("commonspeech/", env!("CARGO_PKG_VERSION")))
        .build()
        .into();
    let mut response = agent.get(MODEL_URL).call().context("the speech recognition model could not be downloaded")?;
    let mut reader = response.body_mut().with_config().limit(MODEL_BYTES + 1).reader();
    let mut file = tempfile::Builder::new()
        .prefix(".commonspeech-")
        .suffix(".part")
        .tempfile_in(dir)
        .with_context(|| format!("could not write to {}", dir.display()))?;
    let mut hash = Sha256::new();
    let mut received: u64 = 0;
    let mut buffer = vec![0u8; 256 * 1024];
    loop {
        if control.stopped() {
            return Ok(false);
        }
        let n = reader.read(&mut buffer).context("the download of the speech recognition model broke off")?;
        if n == 0 {
            break;
        }
        hash.update(&buffer[..n]);
        file.write_all(&buffer[..n]).context("could not save the speech recognition model")?;
        received += n as u64;
        progress(received as f32 / MODEL_BYTES as f32);
    }
    let digest: String = hash.finalize().iter().map(|b| format!("{b:02x}")).collect();
    if received != MODEL_BYTES || digest != MODEL_SHA256 {
        bail!("the speech recognition model that arrived isn't the one expected, so it was thrown away");
    }
    file.flush()?;
    file.persist(dest).map_err(|e| e.error).context("could not save the speech recognition model")?;
    Ok(true)
}

/// A stretch of speech whisper.cpp found, in milliseconds from the start.
#[derive(Debug, Clone, PartialEq)]
pub struct Segment {
    pub from_ms: u64,
    pub to_ms: u64,
    pub text: String,
}

/// Runs whisper.cpp on a 16 kHz WAV file. Returns `None` if stopped.
pub fn run(
    program: &Path,
    model: &Path,
    wav: &Path,
    control: &Control,
    progress: &dyn Fn(f32),
) -> anyhow::Result<Option<Vec<Segment>>> {
    let out_dir = tempfile::tempdir().context("could not create a temporary folder")?;
    let out_base = out_dir.path().join("transcript");
    let threads = std::thread::available_parallelism().map_or(4, |n| n.get().clamp(1, 8));
    let mut command = Command::new(program);
    #[cfg(windows)]
    {
        // Stops a console window flashing up.
        use std::os::windows::process::CommandExt;
        command.creation_flags(0x0800_0000);
    }
    let mut child = command
        .arg("--model")
        .arg(model)
        .arg("--file")
        .arg(wav)
        .args(["--language", "en", "--threads", &threads.to_string(), "--output-json", "--print-progress", "--no-prints"])
        .arg("--output-file")
        .arg(&out_base)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .with_context(|| format!("whisper.cpp could not be started from {}", program.display()))?;

    // Progress, and the last few lines in case it fails, come on standard error.
    let stderr = child.stderr.take().context("could not read from whisper.cpp")?;
    let (tx, rx) = std::sync::mpsc::channel::<String>();
    let reader = std::thread::spawn(move || {
        for line in BufReader::new(stderr).lines().map_while(Result::ok) {
            if tx.send(line).is_err() {
                break;
            }
        }
    });
    let mut last_lines: Vec<String> = Vec::new();
    let status = loop {
        for line in rx.try_iter() {
            if let Some(p) = progress_of(&line) {
                progress(p);
            } else if !line.trim().is_empty() {
                last_lines.push(line);
                if last_lines.len() > 5 {
                    last_lines.remove(0);
                }
            }
        }
        if control.stopped() {
            let _ = child.kill();
            let _ = child.wait();
            return Ok(None);
        }
        if let Some(status) = child.try_wait()? {
            break status;
        }
        std::thread::sleep(Duration::from_millis(100));
    };
    let _ = reader.join();
    last_lines.extend(rx.try_iter().filter(|l| progress_of(l).is_none()));
    if !status.success() {
        log::warn!("whisper.cpp failed ({status}): {}", last_lines.join(" | "));
        bail!("whisper.cpp gave up partway through. The debug log has what it said");
    }
    let json = std::fs::File::open(out_base.with_extension("json"))
        .context("whisper.cpp finished but left no transcript")?;
    let mut text = String::new();
    json.take(MAX_JSON_BYTES).read_to_string(&mut text).context("whisper.cpp's transcript could not be read")?;
    Ok(Some(parse(&text)?))
}

/// Reads lines such as `whisper_print_progress_callback: progress =  42%`.
fn progress_of(line: &str) -> Option<f32> {
    let (_, rest) = line.split_once("progress =")?;
    let percent: f32 = rest.trim().trim_end_matches('%').trim().parse().ok()?;
    Some((percent / 100.0).clamp(0.0, 1.0))
}

/// Reads whisper.cpp's JSON transcript, dropping notes such as `[BLANK_AUDIO]`
/// or `(music)` that aren't words anyone said.
pub fn parse(json: &str) -> anyhow::Result<Vec<Segment>> {
    let value: Value = serde_json::from_str(json).context("whisper.cpp's transcript could not be understood")?;
    let segments = value["transcription"].as_array().context("whisper.cpp's transcript has no text in it")?;
    Ok(segments
        .iter()
        .filter_map(|s| {
            let text = crate::wordlist::clean_display_text(s["text"].as_str()?).trim().to_owned();
            let from_ms = s["offsets"]["from"].as_u64()?;
            let to_ms = s["offsets"]["to"].as_u64()?.max(from_ms);
            (!is_only_a_note(&text)).then_some(Segment { from_ms, to_ms, text })
        })
        .collect())
}

fn is_only_a_note(text: &str) -> bool {
    let bracketed = |open, close| text.starts_with(open) && text.ends_with(close) && text.matches(close).count() == 1;
    text.is_empty() || bracketed('[', ']') || bracketed('(', ')') || text.chars().all(|c| !c.is_alphanumeric())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_whisper_json_and_drops_notes() {
        let json = r#"{"transcription": [
            {"offsets": {"from": 0, "to": 2840}, "text": " Hello there."},
            {"offsets": {"from": 2840, "to": 4000}, "text": " [BLANK_AUDIO]"},
            {"offsets": {"from": 4000, "to": 5000}, "text": " (music)"},
            {"offsets": {"from": 5000, "to": 6000}, "text": " RLOSecond line."},
            {"offsets": {"from": 6000, "to": 7000}, "text": " ..."}
        ]}"#
        .replace("RLO", "\u{202e}");
        let segments = parse(&json).unwrap();
        assert_eq!(
            segments,
            [
                Segment { from_ms: 0, to_ms: 2840, text: "Hello there.".to_owned() },
                Segment { from_ms: 5000, to_ms: 6000, text: "Second line.".to_owned() },
            ]
        );
        assert!(parse("not json").is_err());
        assert!(parse("{}").is_err());
    }

    #[test]
    fn reads_progress_lines() {
        assert_eq!(progress_of("whisper_print_progress_callback: progress =  42%"), Some(0.42));
        assert_eq!(progress_of("whisper_print_progress_callback: progress = 100%"), Some(1.0));
        assert_eq!(progress_of("read_audio_data: reading audio data"), None);
    }

    #[test]
    fn a_chosen_program_that_is_missing_is_not_used() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(find_program(Some(&dir.path().join("whisper-cli"))), None);
        let real = dir.path().join("whisper-cli");
        std::fs::write(&real, b"").unwrap();
        assert_eq!(find_program(Some(&real)), Some(real));
    }

    #[test]
    fn a_stopped_download_leaves_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let dest = dir.path().join(MODEL_FILE);
        let control = Control::default();
        control.request_stop();
        // Stopped before it starts, so nothing goes over the network.
        assert!(!download_model(&dest, &control, &|_| {}).unwrap());
        assert!(!dest.exists());
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 0);
    }

    /// Downloads the real model, about 148 MB, into a folder of your choosing:
    /// `E2E_MODEL_DIR=/tmp/m cargo test real_model_download -- --ignored`.
    #[test]
    #[ignore]
    fn real_model_download() {
        let dir = PathBuf::from(std::env::var("E2E_MODEL_DIR").expect("set E2E_MODEL_DIR to a folder"));
        let dest = dir.join(MODEL_FILE);
        assert!(download_model(&dest, &Control::default(), &|_| {}).unwrap());
        assert_eq!(std::fs::metadata(&dest).unwrap().len(), MODEL_BYTES);
    }
}
