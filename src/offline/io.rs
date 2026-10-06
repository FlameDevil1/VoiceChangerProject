//! Audio file I/O: decode WAV/MP3/FLAC/OGG with Symphonia, write WAV with hound.

use std::fs::File;
use std::path::Path;
use symphonia::core::codecs::audio::AudioDecoderOptions;
use symphonia::core::errors::Error as SymError;
use symphonia::core::formats::probe::Hint;
use symphonia::core::formats::{FormatOptions, TrackType};
use symphonia::core::io::MediaSourceStream;
use symphonia::core::meta::MetadataOptions;

/// Decoded audio, interleaved `f32` in [-1, 1].
#[derive(Clone, Debug, PartialEq)]
pub struct Audio {
    pub rate: u32,
    pub channels: usize,
    pub samples: Vec<f32>,
}

impl Audio {
    pub fn frames(&self) -> usize {
        self.samples.len() / self.channels.max(1)
    }

    pub fn duration_secs(&self) -> f64 {
        self.frames() as f64 / self.rate.max(1) as f64
    }
}

/// Decode a whole file into memory.
pub fn load(path: &Path) -> Result<Audio, String> {
    let ctx = |e: &dyn std::fmt::Display| format!("{}: {e}", path.display());
    let file = File::open(path).map_err(|e| ctx(&e))?;
    let mss = MediaSourceStream::new(Box::new(file), Default::default());
    let mut hint = Hint::new();
    if let Some(ext) = path.extension().and_then(|e| e.to_str()) {
        hint.with_extension(ext);
    }
    let mut format = symphonia::default::get_probe()
        .probe(&hint, mss, FormatOptions::default(), MetadataOptions::default())
        .map_err(|e| ctx(&e))?;

    let (track_id, mut decoder) = {
        let track = format.default_track(TrackType::Audio).ok_or_else(|| ctx(&"no audio track"))?;
        let params = track
            .codec_params
            .as_ref()
            .and_then(|p| p.audio())
            .ok_or_else(|| ctx(&"unsupported codec"))?;
        let decoder = symphonia::default::get_codecs()
            .make_audio_decoder(params, &AudioDecoderOptions::default())
            .map_err(|e| ctx(&e))?;
        (track.id, decoder)
    };

    let mut audio = Audio { rate: 0, channels: 0, samples: Vec::new() };
    let mut buf: Vec<f32> = Vec::new();
    loop {
        let packet = match format.next_packet() {
            Ok(Some(p)) => p,
            Ok(None) => break,
            Err(SymError::IoError(e)) if e.kind() == std::io::ErrorKind::UnexpectedEof => break,
            Err(e) => return Err(ctx(&e)),
        };
        if packet.track_id != track_id {
            continue;
        }
        match decoder.decode(&packet) {
            Ok(decoded) => {
                let spec = decoded.spec();
                audio.rate = spec.rate();
                audio.channels = spec.channels().count();
                buf.resize(decoded.samples_interleaved(), 0.0);
                decoded.copy_to_slice_interleaved(&mut buf);
                audio.samples.extend_from_slice(&buf);
            }
            // A corrupt packet in a long file shouldn't abort the whole render.
            Err(SymError::DecodeError(e)) => log::warn!("{}: skipped bad packet: {e}", path.display()),
            Err(e) => return Err(ctx(&e)),
        }
    }
    if audio.channels == 0 || audio.samples.is_empty() {
        return Err(ctx(&"file contains no audio"));
    }
    Ok(audio)
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum WavFormat {
    /// 32-bit float: lossless, used for golden files and further editing.
    #[default]
    Float32,
    /// 16-bit PCM with TPDF dither: smallest, plays everywhere.
    Pcm16,
}

/// Write mono samples to a WAV file.
pub fn save_wav(path: &Path, samples: &[f32], rate: u32, format: WavFormat) -> Result<(), String> {
    let ctx = |e: &dyn std::fmt::Display| format!("{}: {e}", path.display());
    let spec = hound::WavSpec {
        channels: 1,
        sample_rate: rate,
        bits_per_sample: if format == WavFormat::Pcm16 { 16 } else { 32 },
        sample_format: if format == WavFormat::Pcm16 { hound::SampleFormat::Int } else { hound::SampleFormat::Float },
    };
    let mut w = hound::WavWriter::create(path, spec).map_err(|e| ctx(&e))?;
    match format {
        WavFormat::Float32 => {
            for &s in samples {
                w.write_sample(s).map_err(|e| ctx(&e))?;
            }
        }
        WavFormat::Pcm16 => {
            let mut rng = crate::offline::signals::Rng::new(0x5EED);
            for &s in samples {
                // TPDF dither (±1 LSB) avoids quantisation distortion on quiet passages.
                let dither = rng.next_f32() - rng.next_f32();
                let v = (s * 32767.0 + dither).round().clamp(-32768.0, 32767.0) as i16;
                w.write_sample(v).map_err(|e| ctx(&e))?;
            }
        }
    }
    w.finalize().map_err(|e| ctx(&e))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(name: &str) -> std::path::PathBuf {
        std::env::temp_dir().join(format!("vc-io-test-{}-{name}", std::process::id()))
    }

    #[test]
    fn float_wav_roundtrip_is_exact() {
        let x = crate::offline::signals::sine(48_000, 0.1, 440.0, 0.5);
        let p = tmp("f32.wav");
        save_wav(&p, &x, 48_000, WavFormat::Float32).unwrap();
        let a = load(&p).unwrap();
        let _ = std::fs::remove_file(&p);
        assert_eq!((a.rate, a.channels), (48_000, 1));
        assert_eq!(a.samples, x);
    }

    #[test]
    fn pcm16_roundtrip_within_dither() {
        let x = crate::offline::signals::sine(44_100, 0.1, 1000.0, 0.5);
        let p = tmp("i16.wav");
        save_wav(&p, &x, 44_100, WavFormat::Pcm16).unwrap();
        let a = load(&p).unwrap();
        let _ = std::fs::remove_file(&p);
        assert_eq!(a.rate, 44_100);
        assert!(crate::offline::analysis::max_abs_diff(&a.samples, &x) < 2.5 / 32768.0);
    }

    #[test]
    fn missing_file_is_an_error() {
        assert!(load(Path::new("definitely/not/here.wav")).is_err());
    }
}
