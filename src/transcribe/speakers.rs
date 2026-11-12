//! Working out who is speaking, so the transcript can say "Speaker 1",
//! "Speaker 2" and so on. whisper.cpp can't do this for one-channel audio.
//!
//! Each stretch of speech whisper.cpp finds gets a voice print: the average
//! shape of its spectrum (mel-frequency cepstral coefficients, as in speaker
//! recognition) and its typical pitch. Stretches whose prints are close are then
//! joined, closest first, until the closest left are too far apart to be one
//! person. This tells voices apart well when they differ clearly, as a man and a
//! woman, or two people on different microphones, usually do. Similar voices
//! can be taken for one, and a change of speaker in the middle of a stretch is
//! missed.

use std::f32::consts::PI;
use std::ops::Range;

use super::RATE;

/// 25 ms frames, every 10 ms.
const FRAME: usize = 400;
const HOP: usize = 160;
const FFT: usize = 512;
const MEL_BANDS: usize = 26;
const COEFFICIENTS: usize = 12;
/// Stretches shorter than this are too short to judge, and go to the closest voice.
const MIN_JUDGED: usize = 2 * RATE as usize;
/// A voice heard for less than this, and less than [`MIN_SHARE`] of the
/// recording, is more likely a quirk of one stretch than another person.
const MIN_VOICE: usize = 8 * RATE as usize;
const MIN_SHARE: f32 = 0.03;
/// The most stretches clustered: a few seconds' work.
const MAX_JUDGED: usize = 600;
/// How far apart, on average per measure, two prints can be and still be one voice.
const SAME_VOICE: f32 = 2.0;
/// How much pitch counts for, against each spectral measure. Pitch tells men,
/// women and children apart better than anything else here.
const PITCH_WEIGHT: f32 = 3.0;

/// One number per stretch: which speaker it is, counting from 0 in the order
/// they are first heard.
pub fn label(samples: &[f32], stretches: &[Range<usize>]) -> Vec<usize> {
    let analyser = Analyser::new();
    let prints: Vec<Option<Vec<f32>>> = stretches
        .iter()
        .map(|r| {
            let r = r.start.min(samples.len())..r.end.min(samples.len());
            analyser.print(&samples[r])
        })
        .collect();
    // Clustering takes time with the cube of how many there are, so in a long
    // recording only the longest stretches are judged, and the rest go to the
    // closest voice, as short ones do.
    let mut judged: Vec<usize> =
        (0..prints.len()).filter(|&i| prints[i].is_some() && stretches[i].len() >= MIN_JUDGED).collect();
    judged.sort_by_key(|&i| std::cmp::Reverse(stretches[i].len()));
    judged.truncate(MAX_JUDGED);
    judged.sort_unstable();
    let vectors: Vec<&Vec<f32>> = judged.iter().map(|&i| prints[i].as_ref().unwrap()).collect();
    let lengths: Vec<usize> = judged.iter().map(|&i| stretches[i].len()).collect();
    let groups = without_quirks(cluster(&vectors), &vectors, &lengths);

    // Each judged stretch's group, then the rest go to the closest group, or
    // the one before them if nothing could be judged.
    let mut group_of: Vec<Option<usize>> = vec![None; stretches.len()];
    for (members, group) in groups.iter().zip(0..) {
        for &m in members {
            group_of[judged[m]] = Some(group);
        }
    }
    let centres: Vec<Vec<f32>> = groups.iter().map(|g| centre(g.iter().map(|&m| vectors[m]))).collect();
    for i in 0..stretches.len() {
        if group_of[i].is_some() {
            continue;
        }
        let closest = prints[i].as_ref().and_then(|p| {
            (0..centres.len()).min_by(|&a, &b| distance(p, &centres[a]).total_cmp(&distance(p, &centres[b])))
        });
        group_of[i] = closest.or_else(|| i.checked_sub(1).and_then(|j| group_of[j]));
    }

    // Number them in the order they are first heard.
    let mut order: Vec<usize> = Vec::new();
    group_of
        .into_iter()
        .map(|g| {
            let g = g.unwrap_or(usize::MAX);
            match order.iter().position(|&o| o == g) {
                Some(n) => n,
                None => {
                    order.push(g);
                    order.len() - 1
                }
            }
        })
        .collect()
}

/// Average-linkage clustering: joins the closest pair of groups until the
/// closest pair is too far apart. Returns each group's members.
fn cluster(vectors: &[&Vec<f32>]) -> Vec<Vec<usize>> {
    let n = vectors.len();
    let mut groups: Vec<Option<Vec<usize>>> = (0..n).map(|i| Some(vec![i])).collect();
    // The average distance between each pair of groups, kept up to date as
    // they are joined (the Lance-Williams formula), so nothing is worked out twice.
    let mut d = vec![0.0f32; n * n];
    for i in 0..n {
        for j in i + 1..n {
            let v = distance(vectors[i], vectors[j]);
            d[i * n + j] = v;
            d[j * n + i] = v;
        }
    }
    loop {
        let alive: Vec<usize> = (0..n).filter(|&i| groups[i].is_some()).collect();
        let mut best = (f32::INFINITY, 0, 0);
        for (x, &a) in alive.iter().enumerate() {
            for &b in &alive[x + 1..] {
                if d[a * n + b] < best.0 {
                    best = (d[a * n + b], a, b);
                }
            }
        }
        let (closest, a, b) = best;
        if closest > SAME_VOICE {
            break;
        }
        let joined = groups[b].take().unwrap_or_default();
        let (size_a, size_b) = (groups[a].as_ref().map_or(0, Vec::len) as f32, joined.len() as f32);
        for &k in &alive {
            if k != a && k != b {
                let v = (size_a * d[a * n + k] + size_b * d[b * n + k]) / (size_a + size_b);
                d[a * n + k] = v;
                d[k * n + a] = v;
            }
        }
        if let Some(group) = groups[a].as_mut() {
            group.extend(joined);
        }
    }
    let mut groups: Vec<Vec<usize>> = groups.into_iter().flatten().collect();
    groups.sort_by_key(|g| g.iter().copied().min());
    groups
}

/// Gives the members of groups heard too little to be a person to the closest
/// group that is, if any is.
fn without_quirks(groups: Vec<Vec<usize>>, vectors: &[&Vec<f32>], lengths: &[usize]) -> Vec<Vec<usize>> {
    let total: usize = lengths.iter().sum();
    let heard = |g: &Vec<usize>| g.iter().map(|&m| lengths[m]).sum::<usize>();
    let (mut kept, quirks): (Vec<_>, Vec<_>) =
        groups.into_iter().partition(|g| heard(g) >= MIN_VOICE || heard(g) as f32 >= MIN_SHARE * total as f32);
    if kept.is_empty() {
        return quirks;
    }
    let centres: Vec<Vec<f32>> = kept.iter().map(|g| centre(g.iter().map(|&m| vectors[m]))).collect();
    for member in quirks.into_iter().flatten() {
        let closest = (0..centres.len())
            .min_by(|&a, &b| distance(vectors[member], &centres[a]).total_cmp(&distance(vectors[member], &centres[b])))
            .unwrap_or(0);
        kept[closest].push(member);
    }
    kept.sort_by_key(|g| g.iter().copied().min());
    kept
}

fn centre<'a>(prints: impl Iterator<Item = &'a Vec<f32>>) -> Vec<f32> {
    let mut sum = [0.0; COEFFICIENTS + 1];
    let mut count = 0.0f32;
    for p in prints {
        sum.iter_mut().zip(p).for_each(|(s, v)| *s += v);
        count += 1.0;
    }
    sum.iter().map(|s| s / count.max(1.0)).collect()
}

/// The root mean square difference per measure.
fn distance(a: &[f32], b: &[f32]) -> f32 {
    (a.iter().zip(b).map(|(x, y)| (x - y).powi(2)).sum::<f32>() / a.len() as f32).sqrt()
}

struct Analyser {
    window: Vec<f32>,
    /// For each mel band, its weight for each FFT bin.
    bands: Vec<Vec<(usize, f32)>>,
}

impl Analyser {
    fn new() -> Self {
        let window = (0..FRAME).map(|i| 0.54 - 0.46 * (2.0 * PI * i as f32 / (FRAME - 1) as f32).cos()).collect();
        let mel = |hz: f32| 2595.0 * (1.0 + hz / 700.0).log10();
        let hz = |mel: f32| 700.0 * (10f32.powf(mel / 2595.0) - 1.0);
        let (low, high) = (mel(100.0), mel(7600.0));
        let edges: Vec<f32> =
            (0..MEL_BANDS + 2).map(|i| hz(low + (high - low) * i as f32 / (MEL_BANDS + 1) as f32)).collect();
        let bin_hz = RATE as f32 / FFT as f32;
        let bands = (0..MEL_BANDS)
            .map(|b| {
                let (left, middle, right) = (edges[b], edges[b + 1], edges[b + 2]);
                (0..=FFT / 2)
                    .filter_map(|k| {
                        let f = k as f32 * bin_hz;
                        let w = if f > left && f <= middle {
                            (f - left) / (middle - left)
                        } else if f > middle && f < right {
                            (right - f) / (right - middle)
                        } else {
                            0.0
                        };
                        (w > 0.0).then_some((k, w))
                    })
                    .collect()
            })
            .collect();
        Self { window, bands }
    }

    /// A stretch's voice print, from its louder frames, where the voice is.
    /// `None` if too little of it is voiced to say.
    fn print(&self, samples: &[f32]) -> Option<Vec<f32>> {
        let mut frames: Vec<(f32, &[f32])> = (0..samples.len().saturating_sub(FRAME) / HOP)
            .map(|i| &samples[i * HOP..i * HOP + FRAME])
            .map(|f| (f.iter().map(|s| s * s).sum::<f32>(), f))
            .collect();
        frames.sort_by(|a, b| b.0.total_cmp(&a.0));
        frames.truncate(frames.len() * 6 / 10);
        if frames.len() < 20 || frames[0].0 < 1e-6 {
            return None;
        }
        let mut sum = [0.0f32; COEFFICIENTS];
        let mut pitches = Vec::new();
        for (_, frame) in &frames {
            sum.iter_mut().zip(self.cepstrum(frame)).for_each(|(s, c)| *s += c);
            if let Some(p) = pitch(frame) {
                pitches.push(p);
            }
        }
        let mut print: Vec<f32> = sum.iter().map(|s| s / frames.len() as f32).collect();
        // Pitch in semitones from 100 Hz, so a fifth up is as far as a fifth down.
        // Whispered or noisy speech has none, and gets the middle of the range.
        pitches.sort_by(f32::total_cmp);
        let semitones = match pitches.get(pitches.len() / 2) {
            Some(p) if pitches.len() * 5 >= frames.len() => 12.0 * (p / 100.0).log2(),
            _ => 6.0,
        };
        print.push(semitones / 2.0 * PITCH_WEIGHT);
        Some(print)
    }

    /// Mel-frequency cepstral coefficients 1 to 12 of one frame.
    fn cepstrum(&self, frame: &[f32]) -> Vec<f32> {
        let mut re = vec![0.0f32; FFT];
        let mut im = vec![0.0f32; FFT];
        for (i, (s, w)) in frame.iter().zip(&self.window).enumerate() {
            re[i] = s * w;
        }
        fft(&mut re, &mut im);
        let power: Vec<f32> = (0..=FFT / 2).map(|k| re[k] * re[k] + im[k] * im[k]).collect();
        let logs: Vec<f32> =
            self.bands.iter().map(|band| band.iter().map(|&(k, w)| power[k] * w).sum::<f32>().max(1e-10).ln()).collect();
        (1..=COEFFICIENTS)
            .map(|c| {
                logs.iter()
                    .enumerate()
                    .map(|(m, l)| l * (PI * c as f32 * (m as f32 + 0.5) / MEL_BANDS as f32).cos())
                    .sum::<f32>()
                    * (2.0 / MEL_BANDS as f32).sqrt()
            })
            .collect()
    }
}

/// The pitch of a voiced frame, by autocorrelation between 60 and 400 Hz.
fn pitch(frame: &[f32]) -> Option<f32> {
    let energy: f32 = frame.iter().map(|s| s * s).sum();
    if energy <= 0.0 {
        return None;
    }
    let (shortest, longest) = (RATE as usize / 400, RATE as usize / 60);
    let (lag, strength) = (shortest..=longest.min(frame.len() - 1))
        .map(|lag| {
            let r: f32 = frame[..frame.len() - lag].iter().zip(&frame[lag..]).map(|(a, b)| a * b).sum();
            (lag, r / energy)
        })
        .max_by(|a, b| a.1.total_cmp(&b.1))?;
    (strength > 0.35).then(|| RATE as f32 / lag as f32)
}

/// An in-place radix-2 fast Fourier transform. The length must be a power of two.
fn fft(re: &mut [f32], im: &mut [f32]) {
    let n = re.len();
    let mut j = 0;
    for i in 1..n {
        let mut bit = n >> 1;
        while j & bit != 0 {
            j ^= bit;
            bit >>= 1;
        }
        j |= bit;
        if i < j {
            re.swap(i, j);
            im.swap(i, j);
        }
    }
    let mut len = 2;
    while len <= n {
        let angle = -2.0 * PI / len as f32;
        for start in (0..n).step_by(len) {
            for k in 0..len / 2 {
                let (sin, cos) = (angle * k as f32).sin_cos();
                let (a, b) = (start + k, start + k + len / 2);
                let (tr, ti) = (re[b] * cos - im[b] * sin, re[b] * sin + im[b] * cos);
                re[b] = re[a] - tr;
                im[b] = im[a] - ti;
                re[a] += tr;
                im[a] += ti;
            }
        }
        len <<= 1;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A buzzy vowel-like sound at a given pitch, with harmonics shaped by
    /// `brightness`, standing in for a voice.
    fn voice(pitch: f32, brightness: f32, seconds: f32) -> Vec<f32> {
        let n = (seconds * RATE as f32) as usize;
        (0..n)
            .map(|i| {
                let t = i as f32 / RATE as f32;
                (1..30)
                    .map(|h| {
                        let f = pitch * h as f32;
                        if f > 7000.0 { 0.0 } else { (t * f * 2.0 * PI).sin() * (-(h as f32) / brightness).exp() * 0.2 }
                    })
                    .sum::<f32>()
            })
            .collect()
    }

    #[test]
    fn fft_finds_a_tone() {
        let mut re: Vec<f32> = (0..FFT).map(|i| (2.0 * PI * 32.0 * i as f32 / FFT as f32).cos()).collect();
        let mut im = vec![0.0; FFT];
        fft(&mut re, &mut im);
        let peak = (0..FFT / 2).max_by(|&a, &b| re[a].hypot(im[a]).total_cmp(&re[b].hypot(im[b]))).unwrap();
        assert_eq!(peak, 32);
    }

    #[test]
    fn pitch_is_found() {
        let p = pitch(&voice(120.0, 4.0, 0.03)[..FRAME]).unwrap();
        assert!((p - 120.0).abs() < 6.0, "{p}");
        assert_eq!(pitch(&[0.0; FRAME]), None);
    }

    #[test]
    fn two_different_voices_are_two_speakers() {
        let low = voice(110.0, 3.0, 2.0);
        let high = voice(230.0, 8.0, 2.0);
        let mut samples = Vec::new();
        let mut stretches = Vec::new();
        // Long enough each to count as a person.
        let (low, high) = (low.repeat(3), high.repeat(3));
        for part in [&low, &high, &low, &high, &high] {
            stretches.push(samples.len()..samples.len() + part.len());
            samples.extend_from_slice(part);
        }
        // A stretch too short to judge goes to the closest voice.
        let short = voice(110.0, 3.0, 0.5);
        stretches.push(samples.len()..samples.len() + short.len());
        samples.extend(short);
        assert_eq!(label(&samples, &stretches), [0, 1, 0, 1, 1, 0]);
    }

    #[test]
    fn a_voice_heard_once_briefly_is_not_another_person() {
        // 2.5 seconds in two minutes is under the share a voice needs.
        let (low, high) = (voice(110.0, 3.0, 30.0), voice(230.0, 8.0, 30.0));
        let odd = voice(330.0, 12.0, 2.5);
        let mut samples = Vec::new();
        let mut stretches = Vec::new();
        for part in [&low, &high, &odd, &low, &high] {
            stretches.push(samples.len()..samples.len() + part.len());
            samples.extend_from_slice(part);
        }
        let labels = label(&samples, &stretches);
        assert_eq!(labels.iter().max(), Some(&1), "{labels:?}");
        assert_eq!(labels[..2], [0, 1]);
        assert_eq!(labels[3..], [0, 1]);
    }

    #[test]
    fn one_voice_is_one_speaker() {
        let samples: Vec<f32> = [voice(120.0, 4.0, 2.0), voice(124.0, 4.2, 3.0), voice(118.0, 3.9, 1.5)].concat();
        let stretches = [0..32000, 32000..80000, 80000..samples.len()];
        assert_eq!(label(&samples, &stretches), [0, 0, 0]);
    }

    #[test]
    fn nothing_to_judge_is_still_labelled() {
        assert_eq!(label(&[0.0; 100], &[0..50, 50..100, 100..300]), [0, 0, 0]);
        assert!(label(&[], &[]).is_empty());
    }
}
