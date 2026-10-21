//! Describing a video: ffmpeg takes out its keyframes (the whole pictures a
//! video is built from; the frames between them only record what changed),
//! and each is then described by the local AI model, as a photo is.
//!
//! ffmpeg is not part of the app. It is found where the usual package
//! managers put it, and runs on this computer, so the video never leaves it.

use anyhow::{Context, bail};
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdout, Command, Stdio};
use std::sync::mpsc::RecvTimeoutError;
use std::time::Duration;

/// Keyframes closer together than this are skipped. Some videos have one
/// every half second, and pictures that close are nearly the same.
const MIN_GAP_SECS: f64 = 5.0;
/// The most pictures described from one video. Each can take a minute on a
/// laptop, so a long video has its pictures spread further apart instead.
pub const MAX_FRAMES: usize = 40;
/// Pictures are saved no larger than this, which is what the image
/// description sends to the model anyway.
const MAX_EDGE: u32 = 1600;

/// The file types offered when choosing a video.
pub const EXTENSIONS: [&str; 10] = ["mp4", "m4v", "mov", "mkv", "webm", "avi", "wmv", "mpg", "mpeg", "3gp"];

/// One picture taken from a video.
pub struct Frame {
    pub path: PathBuf,
    /// How far into the video it comes.
    pub time: Duration,
}

/// The pictures taken from a video, with how long it is if ffmpeg can tell.
pub struct Keyframes {
    pub frames: Vec<Frame>,
    pub length: Option<Duration>,
}

/// Starts ffmpeg without a console window flashing up on Windows.
fn ffmpeg_command(ffmpeg: &Path) -> Command {
    let mut command = Command::new(ffmpeg);
    command.args(["-hide_banner", "-nostdin"]).stdin(Stdio::null());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        command.creation_flags(CREATE_NO_WINDOW);
    }
    command
}

/// How long the video is, if ffmpeg can tell. Fails if the file has no
/// pictures in it, such as an audio file.
pub fn probe(ffmpeg: &Path, video: &Path) -> anyhow::Result<Option<Duration>> {
    // With no output file named, ffmpeg describes the input and stops.
    let output = ffmpeg_command(ffmpeg).arg("-i").arg(video).output().context("could not run ffmpeg")?;
    let info = String::from_utf8_lossy(&output.stderr);
    if !info.contains("Stream #") {
        log::warn!("ffmpeg could not read the video: {}", info.lines().last().unwrap_or_default());
        bail!("ffmpeg could not read this file. It may be damaged, or not a video");
    }
    if !info.lines().any(|l| l.contains("Stream #") && l.contains("Video:")) {
        bail!("this file has no pictures in it");
    }
    Ok(parse_duration(&info))
}

/// Reads "Duration: 00:01:23.45" from ffmpeg's description of a file.
fn parse_duration(info: &str) -> Option<Duration> {
    let rest = &info[info.find("Duration: ")? + "Duration: ".len()..];
    let mut parts = rest.split(',').next()?.trim().split(':');
    let (h, m, s) = (parts.next()?, parts.next()?, parts.next()?);
    let secs = h.parse::<f64>().ok()? * 3600.0 + m.parse::<f64>().ok()? * 60.0 + s.parse::<f64>().ok()?;
    (secs.is_finite() && secs > 0.0).then(|| Duration::from_secs_f64(secs))
}

/// The least time between pictures, so a long video gives at most
/// `MAX_FRAMES` of them, spread across the whole of it.
fn gap_for(duration: Option<Duration>) -> f64 {
    let spread = duration.map_or(0.0, |d| d.as_secs_f64() / MAX_FRAMES as f64);
    MIN_GAP_SECS.max(spread)
}

/// Saves the video's keyframes into `dir` as JPEG files, calling `progress`
/// with the fraction of the video gone through. Returns `None` if `stopped`
/// said to stop.
pub fn keyframes(
    ffmpeg: &Path,
    video: &Path,
    dir: &Path,
    mut progress: impl FnMut(f32),
    stopped: impl Fn() -> bool,
) -> anyhow::Result<Option<Keyframes>> {
    let duration = probe(ffmpeg, video)?;
    let gap = gap_for(duration);
    log::info!("taking keyframes at least {gap:.1} seconds apart from a video of {duration:?}");
    // Only keyframes are decoded (-skip_frame nokey), which is quick. The
    // select filter keeps the first, then each one `gap` seconds after the
    // last kept; showinfo reports when each kept one comes.
    let filter = format!(
        "select='isnan(prev_selected_t)+gte(t-prev_selected_t\\,{gap:.3})',\
         scale='min({MAX_EDGE},iw)':'min({MAX_EDGE},ih)':force_original_aspect_ratio=decrease,showinfo"
    );
    let mut child = ffmpeg_command(ffmpeg)
        .args(["-nostats", "-loglevel", "info", "-skip_frame", "nokey", "-i"])
        .arg(video)
        .args(["-an", "-sn", "-dn", "-vf", &filter, "-fps_mode", "vfr", "-q:v", "3"])
        .args(["-frames:v", &MAX_FRAMES.to_string(), "-progress", "pipe:1"])
        // Named from inside `dir`, since a % in its path would be taken
        // for part of the numbering.
        .arg("%05d.jpg")
        .current_dir(dir)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .context("could not run ffmpeg")?;

    // Each pipe is read on a thread of its own, so neither fills up and
    // stops ffmpeg, and Escape works even if ffmpeg goes quiet.
    let stdout = child.stdout.take().context("could not run ffmpeg")?;
    let stderr = child.stderr.take().context("could not run ffmpeg")?;
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        for line in BufReader::new(stdout).lines().map_while(Result::ok) {
            if tx.send(line).is_err() {
                break;
            }
        }
    });
    let log = std::thread::spawn(move || {
        let (mut times, mut last) = (Vec::new(), String::new());
        for line in BufReader::new(stderr).lines().map_while(Result::ok) {
            if let Some(time) = shown_time(&line) {
                times.push(time);
            } else if !line.trim().is_empty() {
                last = line;
            }
        }
        (times, last)
    });

    loop {
        if stopped() {
            let _ = child.kill();
            let _ = child.wait();
            log::info!("taking keyframes stopped");
            return Ok(None);
        }
        match rx.recv_timeout(Duration::from_millis(100)) {
            Ok(line) => {
                // Microseconds of the video gone through, or "N/A".
                let done = line.strip_prefix("out_time_us=").and_then(|v| v.trim().parse::<f64>().ok());
                if let (Some(done), Some(total)) = (done, duration) {
                    progress((done / 1e6 / total.as_secs_f64()).clamp(0.0, 1.0) as f32);
                }
            }
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => break,
        }
    }
    let status = child.wait().context("ffmpeg stopped unexpectedly")?;
    let (times, last) = log.join().unwrap_or_default();
    if !status.success() {
        log::warn!("ffmpeg failed ({status}): {last}");
        bail!("ffmpeg could not take the pictures out of this video");
    }

    let mut files: Vec<PathBuf> = std::fs::read_dir(dir)
        .context("could not read the pictures taken from the video")?
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().is_some_and(|e| e == "jpg"))
        .collect();
    files.sort();
    let frames = files
        .into_iter()
        .enumerate()
        .map(|(i, path)| {
            // showinfo reports each picture saved, in order. ffmpeg times
            // them from the start of the video, even when the file's own
            // times start later.
            let secs = times.get(i).copied().unwrap_or(i as f64 * gap);
            Frame { path, time: Duration::from_secs_f64(secs.max(0.0)) }
        })
        .collect::<Vec<_>>();
    log::info!("took {} keyframe(s) from the video", frames.len());
    Ok(Some(Keyframes { frames, length: duration }))
}

// ----- playing the video -------------------------------------------------

/// The size of each picture while the video plays. Pictures of another
/// shape are fitted inside it, with black bars.
pub const PLAY_WIDTH: usize = 640;
pub const PLAY_HEIGHT: usize = 360;
/// Pictures a second while the video plays: enough to follow what happens,
/// and little enough work for a laptop.
pub const PLAY_FPS: f64 = 15.0;
/// The video's sound is played as stereo at this rate.
pub const PLAY_RATE: u32 = 48_000;

/// An ffmpeg process that is stopped when this is dropped.
pub struct Running(Child);

impl Running {
    /// What ffmpeg writes, taken once.
    pub fn output(&mut self) -> Option<ChildStdout> {
        self.0.stdout.take()
    }
}

impl Drop for Running {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

/// Starts ffmpeg writing the video's pictures from `start`, `PLAY_FPS` a
/// second, each `PLAY_WIDTH` by `PLAY_HEIGHT` pixels of RGBA.
pub fn play_pictures(ffmpeg: &Path, video: &Path, start: Duration) -> anyhow::Result<Running> {
    let (w, h) = (PLAY_WIDTH, PLAY_HEIGHT);
    let filter = format!(
        "fps={PLAY_FPS},scale={w}:{h}:force_original_aspect_ratio=decrease,pad={w}:{h}:(ow-iw)/2:(oh-ih)/2,format=rgba"
    );
    play_command(ffmpeg, video, start)
        .args(["-an", "-sn", "-dn", "-vf", &filter, "-f", "rawvideo", "-pix_fmt", "rgba", "pipe:1"])
        .spawn()
        .map(Running)
        .context("could not run ffmpeg")
}

/// Starts ffmpeg writing the video's sound from `start`, as stereo 32-bit
/// float samples at `PLAY_RATE`. It writes nothing if the video is silent.
pub fn play_sound(ffmpeg: &Path, video: &Path, start: Duration) -> anyhow::Result<Running> {
    play_command(ffmpeg, video, start)
        .args(["-vn", "-sn", "-dn", "-ac", "2", "-ar", &PLAY_RATE.to_string(), "-f", "f32le", "pipe:1"])
        .spawn()
        .map(Running)
        .context("could not run ffmpeg")
}

/// ffmpeg reading `video` from `start`, writing to a pipe.
fn play_command(ffmpeg: &Path, video: &Path, start: Duration) -> Command {
    let mut command = ffmpeg_command(ffmpeg);
    command
        .args(["-nostats", "-loglevel", "error", "-ss", &format!("{:.3}", start.as_secs_f64()), "-i"])
        .arg(video)
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    command
}

/// When a picture comes, from showinfo's line about it: "... pts_time:12.5 ...".
fn shown_time(line: &str) -> Option<f64> {
    if !line.contains("Parsed_showinfo") {
        return None;
    }
    let rest = &line[line.find("pts_time:")? + "pts_time:".len()..];
    rest.split_whitespace().next()?.parse().ok().filter(|t: &f64| t.is_finite())
}

/// A time in the video as it is said, such as "1 minute 5 seconds". Spoken
/// as part of the description, so it is in English, like the description.
pub fn spoken_time(time: Duration) -> String {
    let total = time.as_secs_f64().round() as u64;
    if total == 0 {
        return "the start".to_owned();
    }
    let units = [(total / 3600, "hour"), (total / 60 % 60, "minute"), (total % 60, "second")];
    units
        .into_iter()
        .filter(|(count, _)| *count > 0)
        .map(|(count, unit)| if count == 1 { format!("1 {unit}") } else { format!("{count} {unit}s") })
        .collect::<Vec<_>>()
        .join(" ")
}

/// The descriptions of a video's pictures, each starting with when it comes.
pub fn join_descriptions(described: &[(Duration, String)]) -> String {
    described
        .iter()
        .map(|(time, text)| format!("At {}: {}", spoken_time(*time), text.trim()))
        .collect::<Vec<_>>()
        .join("\n\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_the_duration() {
        let info = "Input #0, mov,mp4\n  Duration: 01:02:03.50, start: 0.000000, bitrate: 48 kb/s\n";
        assert_eq!(parse_duration(info), Some(Duration::from_secs_f64(3723.5)));
        assert_eq!(parse_duration("  Duration: N/A, bitrate: N/A"), None);
        assert_eq!(parse_duration("nothing here"), None);
    }

    #[test]
    fn spreads_pictures_across_long_videos() {
        assert_eq!(gap_for(None), MIN_GAP_SECS);
        assert_eq!(gap_for(Some(Duration::from_secs(60))), MIN_GAP_SECS);
        // An hour: 40 pictures, 90 seconds apart.
        assert_eq!(gap_for(Some(Duration::from_secs(3600))), 90.0);
    }

    #[test]
    fn reads_when_pictures_come() {
        let line = "[Parsed_showinfo_2 @ 0x6000] n:   1 pts:  76800 pts_time:6       duration:   512 fmt:yuvj420p";
        assert_eq!(shown_time(line), Some(6.0));
        assert_eq!(shown_time("[Parsed_showinfo_2 @ 0x6000] config in time_base: 1/12800"), None);
        assert_eq!(shown_time("frame=1 pts_time:3"), None);
    }

    #[test]
    fn says_times_in_words() {
        assert_eq!(spoken_time(Duration::from_millis(300)), "the start");
        assert_eq!(spoken_time(Duration::from_secs(6)), "6 seconds");
        assert_eq!(spoken_time(Duration::from_secs(61)), "1 minute 1 second");
        assert_eq!(spoken_time(Duration::from_secs(3720)), "1 hour 2 minutes");
        let joined = join_descriptions(&[(Duration::ZERO, "A title card. ".into()), (Duration::from_secs(6), "A kitchen.".into())]);
        assert_eq!(joined, "At the start: A title card.\n\nAt 6 seconds: A kitchen.");
    }

    /// Needs ffmpeg installed.
    #[test]
    #[ignore]
    fn takes_keyframes_with_ffmpeg() {
        let ffmpeg = crate::platform::ffmpeg().expect("ffmpeg is installed");
        let dir = tempfile::tempdir().unwrap();
        let video = dir.path().join("test.mp4");
        // 20 seconds with a keyframe every 2: kept at 0, 6, 12 and 18.
        let made = ffmpeg_command(&ffmpeg)
            .args(["-v", "error", "-f", "lavfi", "-i", "testsrc=duration=20:size=320x240:rate=25", "-g", "50"])
            .arg(&video)
            .status()
            .unwrap();
        assert!(made.success());
        let frames_dir = dir.path().join("frames");
        std::fs::create_dir(&frames_dir).unwrap();
        let mut last = 0.0;
        let frames = keyframes(&ffmpeg, &video, &frames_dir, |f| last = f, || false).unwrap().unwrap().frames;
        let times: Vec<u64> = frames.iter().map(|f| f.time.as_secs()).collect();
        assert_eq!(times, [0, 6, 12, 18]);
        assert!(last > 0.8, "progress reached {last}");
        assert!(image::open(&frames[0].path).is_ok());
        // The same, in a file whose times start at 11.4 seconds.
        let offset = dir.path().join("offset.ts");
        let made = ffmpeg_command(&ffmpeg)
            .args(["-v", "error", "-i"])
            .arg(&video)
            .args(["-c", "copy", "-output_ts_offset", "10"])
            .arg(&offset)
            .status()
            .unwrap();
        assert!(made.success());
        let offset_dir = dir.path().join("offset");
        std::fs::create_dir(&offset_dir).unwrap();
        let frames = keyframes(&ffmpeg, &offset, &offset_dir, |_| (), || false).unwrap().unwrap().frames;
        let times: Vec<u64> = frames.iter().map(|f| f.time.as_secs_f64().round() as u64).collect();
        assert_eq!(times, [0, 6, 12, 18]);
        // Not a video.
        let text = dir.path().join("notes.mp4");
        std::fs::write(&text, "hello").unwrap();
        assert!(keyframes(&ffmpeg, &text, &frames_dir, |_| (), || false).is_err());
    }
}
