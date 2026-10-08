//! MP3 export through Windows' built-in encoder (Media Foundation). No extra library and no
//! licensing questions for the app; Windows "N" editions need the Media Feature Pack for it.
//!
//! The encoder takes 16-bit PCM at 32, 44.1 or 48 kHz; anything else is resampled to 48 kHz.

use std::path::Path;

/// Sample rates the MP3 encoder accepts (MPEG-1 Layer III).
const RATES: [u32; 3] = [32_000, 44_100, 48_000];

/// Encode mono `samples` at `rate` to an MP3 file at `kbps` (per channel layout, e.g. 160).
pub fn encode(path: &Path, samples: &[f32], rate: u32, kbps: u32) -> Result<(), String> {
    let (samples, rate) = if RATES.contains(&rate) {
        (std::borrow::Cow::Borrowed(samples), rate)
    } else {
        (std::borrow::Cow::Owned(super::export::resample(samples, rate, 48_000)), 48_000)
    };
    let pcm: Vec<i16> = samples.iter().map(|s| (s.clamp(-1.0, 1.0) * 32767.0).round() as i16).collect();
    let result = imp::encode(path, &pcm, rate, kbps);
    if result.is_err() {
        let _ = std::fs::remove_file(path);
    }
    result
}

#[cfg(windows)]
mod imp {
    use std::path::Path;
    use windows::Win32::Media::MediaFoundation::*;
    use windows::Win32::System::Com::{COINIT_MULTITHREADED, CoInitializeEx, CoUninitialize};
    use windows::core::HSTRING;

    const UNAVAILABLE: &str = "MP3 export needs Windows Media Foundation. On Windows \"N\" editions, \
                               install the Media Feature Pack (Settings > Apps > Optional features), \
                               or save as WAV instead.";

    /// Media Foundation (and COM) for the current thread, shut down on drop.
    struct Session {
        com: bool,
    }

    impl Session {
        fn start() -> Result<Self, String> {
            // Already initialised in another mode on this thread is fine: just don't undo it.
            let com = unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) }.is_ok();
            if let Err(e) = unsafe { MFStartup(MF_VERSION, MFSTARTUP_LITE) } {
                if com {
                    unsafe { CoUninitialize() };
                }
                log::warn!("MFStartup failed: {e}");
                return Err(UNAVAILABLE.into());
            }
            Ok(Self { com })
        }
    }

    impl Drop for Session {
        fn drop(&mut self) {
            unsafe {
                let _ = MFShutdown();
                if self.com {
                    CoUninitialize();
                }
            }
        }
    }

    fn audio_type(subtype: &windows::core::GUID, rate: u32, channels: u32) -> windows::core::Result<IMFMediaType> {
        unsafe {
            let t = MFCreateMediaType()?;
            t.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Audio)?;
            t.SetGUID(&MF_MT_SUBTYPE, subtype)?;
            t.SetUINT32(&MF_MT_AUDIO_SAMPLES_PER_SECOND, rate)?;
            t.SetUINT32(&MF_MT_AUDIO_NUM_CHANNELS, channels)?;
            Ok(t)
        }
    }

    /// A sink writer for `path` with an MP3 stream fed by 16-bit PCM.
    fn open(path: &Path, rate: u32, channels: u32, kbps: u32) -> windows::core::Result<(IMFSinkWriter, u32)> {
        unsafe {
            let writer = MFCreateSinkWriterFromURL(&HSTRING::from(path.as_os_str()), None, None)?;
            let out = audio_type(&MFAudioFormat_MP3, rate, channels)?;
            out.SetUINT32(&MF_MT_AUDIO_AVG_BYTES_PER_SECOND, kbps * 1000 / 8)?;
            let index = writer.AddStream(&out)?;
            let input = audio_type(&MFAudioFormat_PCM, rate, channels)?;
            let block_align = 2 * channels;
            input.SetUINT32(&MF_MT_AUDIO_BITS_PER_SAMPLE, 16)?;
            input.SetUINT32(&MF_MT_AUDIO_BLOCK_ALIGNMENT, block_align)?;
            input.SetUINT32(&MF_MT_AUDIO_AVG_BYTES_PER_SECOND, rate * block_align)?;
            writer.SetInputMediaType(index, &input, None)?;
            Ok((writer, index))
        }
    }

    pub fn encode(path: &Path, pcm: &[i16], rate: u32, kbps: u32) -> Result<(), String> {
        let _session = Session::start()?;
        let err = |what: &str, e: windows::core::Error| format!("MP3 export: {what}: {e}");
        // Mono first (half the size for a voice); some encoder versions only take stereo, so
        // fall back to duplicating the channel. Each attempt needs a fresh writer.
        let mut setup = None;
        for channels in [1u32, 2] {
            let _ = std::fs::remove_file(path);
            match open(path, rate, channels, kbps) {
                Ok(w) => {
                    setup = Some((w, channels));
                    break;
                }
                Err(e) => log::info!("MP3 encoder: {channels} ch at {kbps} kbps not accepted: {e}"),
            }
        }
        let Some(((writer, index), channels)) = setup else { return Err(UNAVAILABLE.into()) };
        let block_align = 2 * channels;
        unsafe { writer.BeginWriting() }.map_err(|e| err("start", e))?;

        const CHUNK: usize = 4096;
        let mut written = 0u64;
        for frames in pcm.chunks(CHUNK) {
            let bytes = frames.len() * block_align as usize;
            unsafe {
                let buffer = MFCreateMemoryBuffer(bytes as u32).map_err(|e| err("buffer", e))?;
                let mut data = std::ptr::null_mut();
                buffer.Lock(&mut data, None, None).map_err(|e| err("buffer", e))?;
                let out = std::slice::from_raw_parts_mut(data as *mut i16, frames.len() * channels as usize);
                for (i, s) in frames.iter().enumerate() {
                    for c in 0..channels as usize {
                        out[i * channels as usize + c] = *s;
                    }
                }
                buffer.Unlock().map_err(|e| err("buffer", e))?;
                buffer.SetCurrentLength(bytes as u32).map_err(|e| err("buffer", e))?;
                let sample = MFCreateSample().map_err(|e| err("sample", e))?;
                sample.AddBuffer(&buffer).map_err(|e| err("sample", e))?;
                // Times in 100 ns units.
                sample.SetSampleTime((written * 10_000_000 / rate as u64) as i64).map_err(|e| err("sample", e))?;
                sample
                    .SetSampleDuration((frames.len() as u64 * 10_000_000 / rate as u64) as i64)
                    .map_err(|e| err("sample", e))?;
                writer.WriteSample(index, &sample).map_err(|e| err("encoding", e))?;
            }
            written += frames.len() as u64;
        }
        unsafe { writer.Finalize() }.map_err(|e| err("finishing the file", e))?;
        Ok(())
    }
}

#[cfg(not(windows))]
mod imp {
    pub fn encode(_: &std::path::Path, _: &[i16], _: u32, _: u32) -> Result<(), String> {
        Err("MP3 export is only available on Windows".into())
    }
}

#[cfg(test)]
mod tests {
    use crate::offline::{analysis, load, signals, to_mono};

    #[test]
    fn mp3_roundtrip_keeps_pitch_and_length() {
        let dir = std::env::temp_dir().join(format!("vc_mp3_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        // 22.05 kHz input also exercises the resampling path.
        for rate in [48_000u32, 22_050] {
            let path = dir.join(format!("tone_{rate}.mp3"));
            let x = signals::sine(rate, 2.0, 440.0, 0.5);
            if let Err(e) = super::encode(&path, &x, rate, 160) {
                // CI servers may lack Media Foundation; the app reports the same message.
                assert!(e.contains("Media Foundation"), "{e}");
                eprintln!("skipping MP3 test: {e}");
                return;
            }
            let audio = load(&path).unwrap();
            let y = to_mono(&audio, None);
            assert!((audio.duration_secs() - 2.0).abs() < 0.1, "{} s", audio.duration_secs());
            let mid = y.len() / 2;
            let f0 = analysis::estimate_f0(&y[mid..mid + 4800], audio.rate, 100.0, 1000.0).unwrap();
            assert!((f0 - 440.0).abs() < 3.0, "{f0}");
            assert!((analysis::rms_db(&y[mid - 9600..mid + 9600]) - analysis::rms_db(&x)).abs() < 1.0);
        }
        let _ = std::fs::remove_dir_all(&dir);
    }
}
