//! Mic level setup: from a few seconds of room noise and a few seconds of speech, suggest an
//! input gain and a noise-gate threshold.

/// Peak level speech should reach after the suggested gain: loud and clear, with headroom.
const TARGET_SPEECH_PEAK_DB: f32 = -12.0;
const MAX_GAIN_DB: f32 = 24.0;
const MIN_GAIN_DB: f32 = -12.0;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Calibration {
    /// Background noise level (peak of 10 ms windows, 90th percentile), before gain.
    pub noise_db: f32,
    /// Speech level (peak of 10 ms windows, 95th percentile), before gain.
    pub speech_db: f32,
    pub input_gain_db: f32,
    /// Gate threshold to use with the suggested gain.
    pub gate_threshold_db: f32,
    pub verdict: Verdict,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Verdict {
    Good,
    /// Voice barely louder than the room: suggest moving closer or noise suppression.
    Noisy,
    /// Nothing that sounds like speech was recorded.
    TooQuiet,
    /// Speech hit full scale: lower the mic's own level in Windows.
    Clipping,
}

impl Verdict {
    pub fn advice(self) -> &'static str {
        match self {
            Verdict::Good => "Looks good.",
            Verdict::Noisy => {
                "Your voice is only a little louder than the background. Move closer to the mic or turn on noise suppression."
            }
            Verdict::TooQuiet => {
                "Couldn't hear speech. Check the right microphone is selected and speak during the second step."
            }
            Verdict::Clipping => {
                "Your mic is distorting. Lower its level in Windows Sound settings, then run this again."
            }
        }
    }
}

/// Peak dBFS of each 10 ms window.
fn window_peaks(x: &[f32], rate: u32) -> Vec<f32> {
    let w = (rate as usize / 100).max(1);
    x.chunks(w).map(|c| 20.0 * c.iter().fold(0.0f32, |m, s| m.max(s.abs())).max(1e-6).log10()).collect()
}

fn percentile(mut v: Vec<f32>, p: f32) -> f32 {
    if v.is_empty() {
        return -120.0;
    }
    v.sort_by(|a, b| a.total_cmp(b));
    v[((v.len() - 1) as f32 * p).round() as usize]
}

pub fn analyze(quiet: &[f32], speech: &[f32], rate: u32) -> Calibration {
    let noise_db = percentile(window_peaks(quiet, rate), 0.9);
    let speech_peaks = window_peaks(speech, rate);
    let clipped = speech.iter().filter(|s| s.abs() >= 0.99).count() > rate as usize / 100;
    let speech_db = percentile(speech_peaks, 0.95);

    let input_gain_db = (TARGET_SPEECH_PEAK_DB - speech_db).clamp(MIN_GAIN_DB, MAX_GAIN_DB);
    let (noise_after, speech_after) = (noise_db + input_gain_db, speech_db + input_gain_db);
    // Above the noise with margin, but well below speech so word endings aren't chopped.
    let gate_threshold_db = (noise_after + 6.0).max(speech_after - 30.0).min(speech_after - 12.0).clamp(-80.0, -6.0);

    let verdict = if clipped {
        Verdict::Clipping
    } else if speech_db < noise_db + 6.0 || speech_db < -60.0 {
        Verdict::TooQuiet
    } else if speech_db < noise_db + 18.0 {
        Verdict::Noisy
    } else {
        Verdict::Good
    };
    Calibration { noise_db, speech_db, input_gain_db, gate_threshold_db, verdict }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::offline::signals;

    #[test]
    fn quiet_room_and_normal_speech() {
        let quiet = signals::noise(48_000, 3.0, 0.003, 1); // about -50 dBFS peaks
        let speech: Vec<f32> = signals::vowel(48_000, 4.0, 150.0).iter().map(|s| s * 0.25).collect(); // ~-18 dBFS peaks
        let c = analyze(&quiet, &speech, 48_000);
        assert_eq!(c.verdict, Verdict::Good);
        assert!((c.speech_db - -18.0).abs() < 1.5, "{c:?}");
        assert!((c.input_gain_db - 6.0).abs() < 1.5, "{c:?}");
        let (noise_after, speech_after) = (c.noise_db + c.input_gain_db, c.speech_db + c.input_gain_db);
        assert!(c.gate_threshold_db > noise_after + 5.0 && c.gate_threshold_db < speech_after - 11.0, "{c:?}");
    }

    #[test]
    fn detects_noisy_silent_and_clipping_input() {
        let loud_noise = signals::noise(48_000, 3.0, 0.02, 2); // ~14 dB below the voice
        let speech: Vec<f32> = signals::vowel(48_000, 4.0, 150.0).iter().map(|s| s * 0.2).collect();
        assert_eq!(analyze(&loud_noise, &speech, 48_000).verdict, Verdict::Noisy);

        let quiet = signals::noise(48_000, 3.0, 0.003, 3);
        let more_quiet = signals::noise(48_000, 4.0, 0.003, 4);
        let c = analyze(&quiet, &more_quiet, 48_000);
        assert_eq!(c.verdict, Verdict::TooQuiet);
        assert!(c.input_gain_db <= MAX_GAIN_DB);

        let hot: Vec<f32> = signals::vowel(48_000, 4.0, 150.0).iter().map(|s| (s * 4.0).clamp(-1.0, 1.0)).collect();
        assert_eq!(analyze(&quiet, &hot, 48_000).verdict, Verdict::Clipping);
    }
}
