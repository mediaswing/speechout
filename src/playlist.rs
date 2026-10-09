//! Zip files of audio, played as playlists on the Audio Player tab.
//!
//! The WAV and MP3 files in the zip are unpacked to a temporary folder and
//! played one after another. If the zip has a `listing.txt` file, it sets the
//! order: one file name to a line. Otherwise the files play in name order,
//! with numbers in names counted properly, so "2.mp3" comes before "10.mp3".

use anyhow::{Context, bail};
use std::cmp::Ordering;
use std::collections::HashMap;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

/// The file in a zip that sets the order of its tracks.
const LISTING: &str = "listing.txt";

/// The most a listing file is read, which is far more than any real one.
const MAX_LISTING_BYTES: u64 = 1024 * 1024;

/// The most audio unpacked from one zip file, so a damaged or hostile zip
/// can't fill the disk.
const MAX_UNPACKED_BYTES: u64 = 8 * 1024 * 1024 * 1024;

/// The most audio files a playlist may have, so a zip of countless tiny
/// files can't fill the temporary folder with them.
const MAX_TRACKS: usize = 10_000;

/// An opened zip file of audio.
pub struct Playlist {
    /// The tracks in playing order, unpacked into `_dir`.
    pub tracks: Vec<PathBuf>,
    /// Lines in the listing that don't name a WAV or MP3 file in the zip.
    pub missing: Vec<String>,
    /// Deleted, with the tracks in it, when the playlist is dropped.
    _dir: tempfile::TempDir,
}

pub fn is_zip(path: &Path) -> bool {
    path.extension().is_some_and(|e| e.eq_ignore_ascii_case("zip"))
}

fn is_audio(name: &str) -> bool {
    let lower = name.to_lowercase();
    lower.ends_with(".wav") || lower.ends_with(".mp3")
}

/// Unpacks the audio in the zip file at `path`, in playing order. Returns
/// `None` if `stopped` said to stop, which is checked for every megabyte.
pub fn open(path: &Path, stopped: impl Fn() -> bool) -> anyhow::Result<Option<Playlist>> {
    let file = std::fs::File::open(path).with_context(|| format!("could not open {}", path.display()))?;
    let mut archive = zip::ZipArchive::new(file).context("this is not a valid zip file")?;

    // Paths inside the zip, with "/" between folders, and where to find them.
    let mut audio = HashMap::new();
    let mut listing: Option<(String, usize)> = None;
    for i in 0..archive.len() {
        let entry = archive.by_index_raw(i)?;
        if entry.is_dir() {
            continue;
        }
        let Some(name) = entry.enclosed_name().as_deref().and_then(zip_path) else { continue };
        let file_name = name.rsplit('/').next().unwrap_or(&name);
        if file_name.eq_ignore_ascii_case(LISTING) {
            // The listing nearest the top wins.
            if listing.as_ref().is_none_or(|(l, _)| l.matches('/').count() > name.matches('/').count()) {
                listing = Some((name, i));
            }
        } else if is_audio(&name) {
            audio.insert(name, i);
            if audio.len() > MAX_TRACKS {
                bail!("there are more than {MAX_TRACKS} audio files in it");
            }
        }
    }
    if audio.is_empty() {
        bail!("there are no WAV or MP3 files in it");
    }

    let listing = match listing {
        Some((name, i)) => {
            let mut bytes = Vec::new();
            archive.by_index(i)?.take(MAX_LISTING_BYTES).read_to_end(&mut bytes).context("could not read listing.txt")?;
            let folder = name.rsplit_once('/').map_or("", |(folder, _)| folder).to_owned();
            Some((folder, String::from_utf8_lossy(&bytes).into_owned()))
        }
        None => None,
    };
    let names: Vec<String> = audio.keys().cloned().collect();
    let (order, missing) = order(names, listing.as_ref().map(|(f, text)| (f.as_str(), text.as_str())));

    let dir = tempfile::Builder::new().prefix("speechout-playlist-").tempdir()?;
    let mut unpacked: HashMap<&str, PathBuf> = HashMap::new();
    let mut budget = MAX_UNPACKED_BYTES;
    let mut tracks = Vec::with_capacity(order.len());
    for name in &order {
        if let Some(track) = unpacked.get(name.as_str()) {
            tracks.push(track.clone());
            continue;
        }
        // Each track has a folder of its own, so files with the same name in
        // different folders of the zip keep their names.
        let folder = dir.path().join(unpacked.len().to_string());
        std::fs::create_dir(&folder)?;
        let track = folder.join(safe_file_name(name.rsplit('/').next().unwrap_or(name)));
        let entry = archive.by_index(audio[name]).with_context(|| format!("could not read {name}"))?;
        if !unpack(entry, &track, &mut budget, &stopped).with_context(|| format!("could not unpack {name}"))? {
            return Ok(None);
        }
        unpacked.insert(name, track.clone());
        tracks.push(track);
    }
    Ok(Some(Playlist { tracks, missing, _dir: dir }))
}

/// The path of a zip entry with "/" between folders, or `None` for the extra
/// files macOS adds to zips, which aren't really part of them.
fn zip_path(path: &Path) -> Option<String> {
    let parts: Vec<String> = path.components().map(|c| c.as_os_str().to_string_lossy().into_owned()).collect();
    let hidden = parts.first().is_some_and(|p| p == "__MACOSX") || parts.last().is_some_and(|p| p.starts_with("._"));
    (!hidden && !parts.is_empty()).then(|| parts.join("/"))
}

/// A file name from a zip that can be used on any system: characters
/// Windows doesn't allow in names become "_", and so does the start of a name
/// Windows keeps for devices, such as "CON" or "COM1".
fn safe_file_name(name: &str) -> String {
    let mut safe: String =
        name.chars().map(|c| if c.is_control() || "<>:\"/\\|?*".contains(c) { '_' } else { c }).collect();
    let stem = safe.split('.').next().unwrap_or("").trim_end().to_ascii_uppercase();
    let numbered = |prefix: &str| {
        stem.strip_prefix(prefix).is_some_and(|n| n.len() == 1 && n.chars().all(|c| c.is_ascii_digit() && c != '0'))
    };
    if ["CON", "PRN", "AUX", "NUL"].contains(&stem.as_str()) || numbered("COM") || numbered("LPT") {
        safe.insert(0, '_');
    }
    safe
}

/// Copies a zip entry to `to`, taking its size off `budget`. Returns false if
/// `stopped` said to stop.
fn unpack(mut entry: impl Read, to: &Path, budget: &mut u64, stopped: impl Fn() -> bool) -> anyhow::Result<bool> {
    let mut out = std::io::BufWriter::new(std::fs::File::create(to)?);
    let mut buf = vec![0; 1024 * 1024];
    loop {
        if stopped() {
            return Ok(false);
        }
        let n = entry.read(&mut buf)?;
        if n == 0 {
            break;
        }
        *budget = budget.checked_sub(n as u64).context("the zip file holds too much audio to unpack")?;
        out.write_all(&buf[..n])?;
    }
    out.flush()?;
    Ok(true)
}

/// Puts the audio files `names` in playing order. `listing` is the folder the
/// listing file is in and its text. Returns the order, and the lines of the
/// listing that don't name any of the files.
///
/// A line can name a file by its path from the listing's folder or by its
/// file name alone, in any mix of capitals, with "/" or "\" between folders.
/// A file can be listed more than once. Files the listing leaves out play
/// after the listed ones, in name order.
fn order(mut names: Vec<String>, listing: Option<(&str, &str)>) -> (Vec<String>, Vec<String>) {
    names.sort_by(|a, b| natural_cmp(a, b).then_with(|| a.cmp(b)));
    let Some((folder, text)) = listing else { return (names, Vec::new()) };
    let mut order = Vec::new();
    let mut missing = Vec::new();
    for line in text.trim_start_matches('\u{feff}').lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let wanted = line.replace('\\', "/");
        let wanted = wanted.trim_start_matches("./").to_lowercase();
        let full = if folder.is_empty() { wanted.clone() } else { format!("{}/{wanted}", folder.to_lowercase()) };
        let suffix = format!("/{wanted}");
        let found = names.iter().find(|n| n.to_lowercase() == full).or_else(|| {
            names.iter().find(|n| {
                let n = n.to_lowercase();
                n == wanted || n.ends_with(&suffix)
            })
        });
        match found {
            Some(name) => order.push(name.clone()),
            None => missing.push(line.to_owned()),
        }
    }
    let unlisted: Vec<String> = names.into_iter().filter(|n| !order.contains(n)).collect();
    order.extend(unlisted);
    (order, missing)
}

/// Compares names ignoring capitals, with runs of digits compared as numbers.
fn natural_cmp(a: &str, b: &str) -> Ordering {
    let (mut a, mut b) = (a.chars().peekable(), b.chars().peekable());
    loop {
        match (a.peek().copied(), b.peek().copied()) {
            (None, None) => return Ordering::Equal,
            (None, Some(_)) => return Ordering::Less,
            (Some(_), None) => return Ordering::Greater,
            (Some(x), Some(y)) if x.is_ascii_digit() && y.is_ascii_digit() => {
                let digits = |chars: &mut std::iter::Peekable<std::str::Chars>| {
                    let mut run = String::new();
                    while let Some(c) = chars.next_if(char::is_ascii_digit) {
                        run.push(c);
                    }
                    run.trim_start_matches('0').to_owned()
                };
                let (x, y) = (digits(&mut a), digits(&mut b));
                let by_number = x.len().cmp(&y.len()).then_with(|| x.cmp(&y));
                if by_number != Ordering::Equal {
                    return by_number;
                }
            }
            (Some(x), Some(y)) => {
                a.next();
                b.next();
                let by_letter = x.to_lowercase().cmp(y.to_lowercase());
                if by_letter != Ordering::Equal {
                    return by_letter;
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn names(list: &[&str]) -> Vec<String> {
        list.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn numbers_in_names_sort_as_numbers() {
        let (order, missing) = order(names(&["Track 10.mp3", "track 2.mp3", "Track 1.wav", "intro.mp3"]), None);
        assert_eq!(order, names(&["intro.mp3", "Track 1.wav", "track 2.mp3", "Track 10.mp3"]));
        assert!(missing.is_empty());
        assert_eq!(natural_cmp("a01.mp3", "a1.mp3"), Ordering::Equal);
        assert_eq!(natural_cmp("a9.mp3", "a10.mp3"), Ordering::Less);
    }

    #[test]
    fn the_listing_sets_the_order() {
        let files = names(&["talks/a.mp3", "talks/b.mp3", "talks/c.wav", "talks/extra/d.mp3"]);
        let listing = "\u{feff}C.WAV\r\n\r\n./a.mp3\nextra\\d.mp3\nnot-here.mp3\na.mp3\n";
        let (order, missing) = order(files, Some(("talks", listing)));
        assert_eq!(order, names(&["talks/c.wav", "talks/a.mp3", "talks/extra/d.mp3", "talks/a.mp3", "talks/b.mp3"]));
        assert_eq!(missing, names(&["not-here.mp3"]));
    }

    #[test]
    fn opens_a_zip_in_listing_order() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("talks.zip");
        let mut zip = zip::ZipWriter::new(std::fs::File::create(&path).unwrap());
        let options = zip::write::SimpleFileOptions::default();
        for (name, body) in [
            ("one.mp3", "1"),
            ("sub/two.mp3", "2"),
            ("sub/one.mp3", "3"),
            ("notes.pdf", "x"),
            ("__MACOSX/._one.mp3", "x"),
            ("LISTING.TXT", "sub/one.mp3\none.mp3\n"),
        ] {
            zip.start_file(name, options).unwrap();
            zip.write_all(body.as_bytes()).unwrap();
        }
        zip.finish().unwrap();

        let playlist = open(&path, || false).unwrap().unwrap();
        let read = |p: &PathBuf| std::fs::read_to_string(p).unwrap();
        let bodies: Vec<String> = playlist.tracks.iter().map(read).collect();
        assert_eq!(bodies, names(&["3", "1", "2"]));
        assert!(playlist.tracks.iter().all(|t| t.starts_with(playlist._dir.path())));
        assert_eq!(playlist.tracks[0].file_name().unwrap(), "one.mp3");
        assert!(open(&path, || true).unwrap().is_none());

        let unpacked = playlist._dir.path().to_owned();
        drop(playlist);
        assert!(!unpacked.exists());
    }

    #[test]
    fn file_names_are_safe_everywhere() {
        assert_eq!(safe_file_name("Chapter 1.mp3"), "Chapter 1.mp3");
        assert_eq!(safe_file_name("Q&A: part 2?.mp3"), "Q&A_ part 2_.mp3");
        assert_eq!(safe_file_name("con.mp3"), "_con.mp3");
        assert_eq!(safe_file_name("COM1.wav"), "_COM1.wav");
        assert_eq!(safe_file_name("COM10.wav"), "COM10.wav");
        assert_eq!(safe_file_name("Console.mp3"), "Console.mp3");
    }

    #[test]
    fn a_zip_with_too_many_tracks_is_an_error() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("many.zip");
        let mut zip = zip::ZipWriter::new(std::fs::File::create(&path).unwrap());
        for i in 0..=MAX_TRACKS {
            zip.start_file(format!("{i}.mp3"), zip::write::SimpleFileOptions::default()).unwrap();
        }
        zip.finish().unwrap();
        let error = open(&path, || false).err().unwrap();
        assert!(error.to_string().contains("more than"), "{error}");
    }

    #[test]
    fn a_zip_without_audio_is_an_error() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("empty.zip");
        let mut zip = zip::ZipWriter::new(std::fs::File::create(&path).unwrap());
        zip.start_file("readme.txt", zip::write::SimpleFileOptions::default()).unwrap();
        zip.finish().unwrap();
        assert!(open(&path, || false).is_err());
    }
}
