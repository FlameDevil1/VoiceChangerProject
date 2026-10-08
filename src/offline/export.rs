//! File export: the pipeline behind the GUI's "Process files" and the `vcrender` CLI.
//!
//! decode -> mono -> speed (pitch kept) -> effects -> aligned output (WAV or MP3).
//!
//! "Aligned" means the result lines up with the original: the effects' processing delay is
//! removed from the start, and the end runs on long enough for reverb tails and delays to finish.

use super::{Audio, WavFormat};
use crate::dsp::util::HannTable;
use crate::dsp::wsola::Wsola;
use crate::dsp::{Chain, CoreParams, EffectKind, EngineCore, FxParams, FxSettings};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering::Relaxed};

/// Output file formats.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum Format {
    /// 32-bit float WAV: lossless, best for further editing.
    #[default]
    Wav32,
    /// 16-bit WAV with dither: lossless-enough, half the size, plays everywhere.
    Wav16,
    /// MP3 (Windows' built-in encoder): small, for sharing.
    Mp3,
}

impl Format {
    pub const ALL: [Format; 3] = [Format::Wav32, Format::Wav16, Format::Mp3];

    pub fn label(self) -> &'static str {
        match self {
            Format::Wav32 => "WAV (32-bit float)",
            Format::Wav16 => "WAV (16-bit)",
            Format::Mp3 => "MP3",
        }
    }

    pub fn extension(self) -> &'static str {
        match self {
            Format::Mp3 => "mp3",
            _ => "wav",
        }
    }
}

#[derive(Clone, Debug)]
pub struct ExportOptions {
    pub params: CoreParams,
    pub fx: FxSettings,
    /// `None` = mix all channels.
    pub channel: Option<usize>,
    /// Playback speed, 1.0 = unchanged; the pitch stays the same.
    pub speed: f32,
    pub format: Format,
}

impl Default for ExportOptions {
    fn default() -> Self {
        Self {
            params: CoreParams::default(),
            fx: FxSettings::default(),
            channel: None,
            speed: 1.0,
            format: Format::Wav32,
        }
    }
}

pub const MIN_SPEED: f32 = 0.5;
pub const MAX_SPEED: f32 = 2.0;
const MP3_KBPS: u32 = 160;

/// Change the speed of `x` without changing its pitch (WSOLA). `speed` 2.0 = twice as fast.
pub fn time_stretch(x: &[f32], sample_rate: u32, speed: f32) -> Vec<f32> {
    let speed = speed.clamp(MIN_SPEED, MAX_SPEED);
    if speed == 1.0 || x.is_empty() {
        return x.to_vec();
    }
    let mut reader = Wsola::new(sample_rate as f32);
    // The whole file is "recorded" already; pad so grains may read past the end into silence.
    let size = (x.len() + 4 * reader.hop + 8).next_power_of_two();
    let mut h = vec![0.0f32; size];
    h[..x.len()].copy_from_slice(x);
    let mask = size - 1;
    let hann = HannTable::new();
    reader.start(0, speed as f64);
    let out_len = (x.len() as f64 / speed as f64).round() as usize;
    let written = (x.len() + 2 * reader.hop) as i64;
    let mut y: Vec<f32> = (0..out_len).map(|_| reader.next(&h, mask, written, &hann)).collect();
    // The first grain fades in over one hop and reads the input 1:1: use the input itself.
    let head = reader.hop.min(y.len()).min(x.len());
    y[..head].copy_from_slice(&x[..head]);
    y
}

/// Resample with a windowed-sinc interpolator (offline quality, not real-time).
pub fn resample(x: &[f32], from: u32, to: u32) -> Vec<f32> {
    if from == to || x.is_empty() {
        return x.to_vec();
    }
    const TAPS: i64 = 32;
    let ratio = to as f64 / from as f64;
    // Low-pass at the lower Nyquist (with a little margin) when going down.
    let cutoff = ratio.min(1.0) * 0.95;
    let out_len = (x.len() as f64 * ratio).round() as usize;
    (0..out_len)
        .map(|j| {
            let pos = j as f64 / ratio;
            let center = pos.floor() as i64;
            let mut acc = 0.0f64;
            for k in center - TAPS + 1..=center + TAPS {
                if k < 0 || k as usize >= x.len() {
                    continue;
                }
                let t = pos - k as f64;
                let a = std::f64::consts::PI * t * cutoff;
                let sinc = if t.abs() < 1e-9 { 1.0 } else { a.sin() / a };
                let w = 0.5 + 0.5 * (std::f64::consts::PI * t / TAPS as f64).cos();
                acc += x[k as usize] as f64 * sinc * cutoff * w;
            }
            acc as f32
        })
        .collect()
}

/// Seconds of extra output after the input ends, so tails can finish.
fn tail_seconds(fx: &FxSettings) -> f32 {
    let mut t = 0.3;
    if fx.enabled(EffectKind::Reverb) {
        t += fx.get(EffectKind::Reverb, "decay") + fx.get(EffectKind::Reverb, "predelay") / 1000.0;
    }
    if fx.enabled(EffectKind::Network) {
        // A lag or a catch-up in progress at the end plays out.
        t += fx.get(EffectKind::Network, "lag_ms") / 1000.0 + 0.5;
    }
    t.min(10.0)
}

/// Run the effects over `input` and return output aligned with it: the processing delay is
/// removed and tails ring out (trailing silence trimmed). `progress` gets 0..1; setting
/// `cancel` stops early with an error.
pub fn render_aligned(
    input: &[f32],
    sample_rate: u32,
    params: CoreParams,
    fx: &FxSettings,
    block: usize,
    progress: &dyn Fn(f32),
    cancel: &AtomicBool,
) -> Result<Vec<f32>, String> {
    let block = block.clamp(1, crate::dsp::MAX_BLOCK);
    let fx_params = FxParams::from_settings(fx);
    let chain = Chain::build(&fx.order, &fx_params, sample_rate as f32, block);
    let mut core = EngineCore::with_chain(sample_rate as f32, block, chain);
    let tail = (tail_seconds(fx) * sample_rate as f32) as usize;
    let mut out = Vec::with_capacity(input.len() + tail + 8192);
    out.extend_from_slice(input);
    out.resize(input.len() + tail, 0.0);
    // Room for the processing delay, known once the chain has run a block.
    let mut latency = 0;
    let total = out.len();
    let mut done = 0;
    let mut chunk = vec![0.0f32; block];
    let mut processed = Vec::with_capacity(total + 8192);
    while done < total + latency {
        let n = block.min(total + latency - done);
        chunk[..n].iter_mut().enumerate().for_each(|(i, v)| *v = out.get(done + i).copied().unwrap_or(0.0));
        core.process(&mut chunk[..n], params);
        processed.extend_from_slice(&chunk[..n]);
        done += n;
        latency = latency.max(core.latency());
        if done % (block * 100) < block {
            if cancel.load(Relaxed) {
                return Err("cancelled".into());
            }
            progress(done as f32 / (total + latency) as f32);
        }
    }
    let mut y = processed.split_off(latency.min(processed.len()));
    y.truncate(total);
    // Drop silence after the tail has died away (but never cut into the original length).
    let quiet = 1e-4;
    let last = y.iter().rposition(|v| v.abs() > quiet).map_or(0, |i| i + 1);
    y.truncate(last.max(input.len()).min(y.len()));
    progress(1.0);
    Ok(y)
}

/// `<stem>_vc.<ext>` in `out_dir` (or next to the input), never an existing file, an input or
/// one of `taken`: adds " (2)", " (3)", ... as needed.
pub fn output_path(input: &Path, out_dir: Option<&Path>, format: Format, taken: &[PathBuf]) -> PathBuf {
    let stem = input.file_stem().and_then(|s| s.to_str()).unwrap_or("output");
    let dir =
        out_dir.map(Path::to_path_buf).unwrap_or_else(|| input.parent().map(Path::to_path_buf).unwrap_or_default());
    let ext = format.extension();
    (1..)
        .map(|n| {
            let name = if n == 1 { format!("{stem}_vc.{ext}") } else { format!("{stem}_vc ({n}).{ext}") };
            dir.join(name)
        })
        .find(|p| !p.exists() && !taken.contains(p) && p != input)
        .expect("unbounded range")
}

/// Process one file. Returns the seconds of audio written.
pub fn process_file(
    input: &Path,
    output: &Path,
    opts: &ExportOptions,
    progress: &dyn Fn(f32),
    cancel: &AtomicBool,
) -> Result<f64, String> {
    let audio: Audio = super::load(input)?;
    let mono = super::to_mono(&audio, opts.channel);
    progress(0.05);
    let stretched = time_stretch(&mono, audio.rate, opts.speed);
    let y = render_aligned(
        &stretched,
        audio.rate,
        opts.params,
        &opts.fx,
        super::DEFAULT_BLOCK,
        &|p| progress(0.05 + 0.85 * p),
        cancel,
    )?;
    if cancel.load(Relaxed) {
        return Err("cancelled".into());
    }
    if let Some(dir) = output.parent()
        && !dir.as_os_str().is_empty()
    {
        std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    }
    match opts.format {
        Format::Wav32 => super::save_wav(output, &y, audio.rate, WavFormat::Float32)?,
        Format::Wav16 => super::save_wav(output, &y, audio.rate, WavFormat::Pcm16)?,
        Format::Mp3 => super::mp3::encode(output, &y, audio.rate, MP3_KBPS)?,
    }
    progress(1.0);
    Ok(y.len() as f64 / audio.rate as f64)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::offline::{analysis, signals};

    #[test]
    fn speed_changes_length_but_not_pitch() {
        let x = signals::vowel(48_000, 2.0, 160.0);
        for speed in [0.5f32, 0.75, 1.5, 2.0] {
            let y = time_stretch(&x, 48_000, speed);
            let expected = (x.len() as f32 / speed) as usize;
            assert!(y.len().abs_diff(expected) <= 1, "speed {speed}: {} vs {expected}", y.len());
            let f0 = analysis::estimate_f0(&y[4800..14_400], 48_000, 50.0, 800.0).unwrap();
            assert!((f0 - 160.0).abs() < 3.0, "speed {speed}: f0 {f0}");
            let level = analysis::rms_db(&y[4800..y.len() - 4800]) - analysis::rms_db(&x[4800..x.len() - 4800]);
            assert!(level.abs() < 1.5, "speed {speed}: level {level:+.1} dB");
        }
        assert_eq!(time_stretch(&x, 48_000, 1.0), x);
    }

    #[test]
    fn resampling_keeps_pitch_and_length() {
        let x = signals::sine(44_100, 1.0, 1000.0, 0.5);
        let y = resample(&x, 44_100, 48_000);
        assert_eq!(y.len(), 48_000);
        let f0 = analysis::estimate_f0(&y[4800..9600], 48_000, 200.0, 2000.0).unwrap();
        assert!((f0 - 1000.0).abs() < 2.0, "{f0}");
        assert!((analysis::rms_db(&y[1000..47_000]) - analysis::rms_db(&x[1000..43_000])).abs() < 0.2);
    }

    #[test]
    fn aligned_render_removes_delay_and_keeps_tails() {
        let mut x = signals::vowel(48_000, 0.5, 150.0);
        x.extend(signals::silence(48_000, 0.5));
        let never = AtomicBool::new(false);
        // Pitch adds processing delay; aligned output starts with the voice anyway.
        let fx = FxSettings::default().with(EffectKind::Pitch, &[("semitones", 0.0), ("formant", 0.0)]);
        let y = render_aligned(&x, 48_000, CoreParams::default(), &fx, 480, &|_| {}, &never).unwrap();
        let onset = |v: &[f32]| v.iter().position(|s| s.abs() > 0.01).unwrap();
        assert!(onset(&y).abs_diff(onset(&x)) < 48, "onset {} vs {}", onset(&y), onset(&x));
        assert_eq!(y.len(), x.len(), "no tail beyond the input's own silence");
        // A long reverb rings out past the end of the input.
        let fx = FxSettings::default().with(EffectKind::Reverb, &[("decay", 3.0)]).with_mix(EffectKind::Reverb, 0.5);
        let short = signals::vowel(48_000, 0.3, 150.0);
        let y = render_aligned(&short, 48_000, CoreParams::default(), &fx, 480, &|_| {}, &never).unwrap();
        assert!(y.len() > short.len() + 48_000, "tail kept: {} samples", y.len());
        // Cancelling stops the render.
        let cancel = AtomicBool::new(true);
        let long = signals::vowel(48_000, 10.0, 150.0);
        assert!(render_aligned(&long, 48_000, CoreParams::default(), &fx, 480, &|_| {}, &cancel).is_err());
    }

    #[test]
    fn output_names_never_clash() {
        let dir = std::env::temp_dir().join(format!("vc_export_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let input = dir.join("voice.wav");
        std::fs::write(&input, b"x").unwrap();
        let a = output_path(&input, None, Format::Wav32, &[]);
        assert_eq!(a, dir.join("voice_vc.wav"));
        let b = output_path(&input, None, Format::Wav32, std::slice::from_ref(&a));
        assert_eq!(b, dir.join("voice_vc (2).wav"));
        std::fs::write(&a, b"y").unwrap();
        assert_eq!(output_path(&input, None, Format::Wav32, &[]), b, "existing files are skipped");
        assert_eq!(output_path(&input, None, Format::Mp3, &[]), dir.join("voice_vc.mp3"));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
