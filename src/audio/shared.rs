//! Lock-free state shared between the audio callbacks and the rest of the app.
//!
//! Controls flow UI -> audio, statistics flow audio -> UI. Everything is an atomic so the audio
//! thread never blocks.

use crate::dsp::chain::FxParams;
pub use crate::dsp::shared_params::AtomicF32;
use std::sync::atomic::{AtomicBool, AtomicI32, AtomicU32, AtomicUsize, Ordering::Relaxed};

/// Samples kept for the spectrum display (power of two).
pub const SCOPE_LEN: usize = 4096;

/// Recent input and output samples for the spectrum display. The capture callback writes them
/// (plain atomic stores, no locks) only while the display is visible; the UI reads a snapshot.
/// A read can overlap a write and mix two blocks, which is invisible in a spectrum.
#[derive(Debug)]
pub struct Scope {
    pub enabled: AtomicBool,
    input: Box<[AtomicF32]>,
    output: Box<[AtomicF32]>,
    pos: AtomicUsize,
}

impl Default for Scope {
    fn default() -> Self {
        let buf = || (0..SCOPE_LEN).map(|_| AtomicF32::new(0.0)).collect();
        Self { enabled: AtomicBool::new(false), input: buf(), output: buf(), pos: AtomicUsize::new(0) }
    }
}

impl Scope {
    /// Store a block of input samples (before effects) at the current position.
    pub fn write_input(&self, block: &[f32]) {
        let p = self.pos.load(Relaxed);
        for (i, s) in block.iter().enumerate() {
            self.input[(p + i) & (SCOPE_LEN - 1)].store(*s);
        }
    }

    /// Store the matching output block (after effects) and advance.
    pub fn write_output(&self, block: &[f32]) {
        let p = self.pos.load(Relaxed);
        for (i, s) in block.iter().enumerate() {
            self.output[(p + i) & (SCOPE_LEN - 1)].store(*s);
        }
        self.pos.store(p.wrapping_add(block.len()), Relaxed);
    }

    /// The latest `n` (<= SCOPE_LEN) input and output samples, oldest first.
    pub fn snapshot(&self, n: usize) -> (Vec<f32>, Vec<f32>) {
        let n = n.min(SCOPE_LEN);
        let end = self.pos.load(Relaxed);
        let read = |b: &[AtomicF32]| (0..n).map(|i| b[(end.wrapping_sub(n) + i) & (SCOPE_LEN - 1)].load()).collect();
        (read(&self.input), read(&self.output))
    }
}

/// Statistics for one output (virtual cable or monitor).
#[derive(Debug, Default)]
pub struct SinkStats {
    pub peak: AtomicF32,
    /// Smoothed ring-buffer fill in milliseconds.
    pub fill_ms: AtomicF32,
    /// Fill level the drift controller is holding (block sizes + margin), in milliseconds.
    pub target_ms: AtomicF32,
    /// Current drift correction in parts per million.
    pub correction_ppm: AtomicF32,
    /// Current safety margin in ms (grows automatically after underruns).
    pub margin_ms: AtomicF32,
    pub underruns: AtomicU32,
    /// Set by the stream's error callback or a caught panic; the controller rebuilds the stream.
    pub failed: AtomicBool,
}

#[derive(Debug)]
pub struct Shared {
    // ---- controls (UI -> audio) ----
    pub bypass: AtomicBool,
    pub mute: AtomicBool,
    pub monitor_enabled: AtomicBool,
    /// "Hear myself" skips the bad connection effect (no lagged self-monitoring).
    pub monitor_pre: AtomicBool,
    pub input_gain: AtomicF32,
    pub output_gain: AtomicF32,
    /// -1 = average all channels, otherwise channel index.
    pub input_channel: AtomicI32,
    /// Base latency margin in seconds; bump `margin_gen` after changing it.
    pub margin: AtomicF32,
    pub margin_gen: AtomicU32,

    // ---- statistics (audio -> UI) ----
    pub in_peak: AtomicF32,
    pub out_peak: AtomicF32,
    /// Max callback processing time as a fraction of the block duration since last read.
    pub load: AtomicF32,
    /// Latest capture block size in frames.
    pub in_block: AtomicU32,
    pub capture_xruns: AtomicU32,
    /// Total processing latency of the core (effect chain + limiter), in samples.
    pub dsp_latency: AtomicU32,
    pub input_failed: AtomicBool,
    pub cable: SinkStats,
    pub monitor: SinkStats,

    // ---- effects (UI -> audio) ----
    pub fx: FxParams,
    pub scope: Scope,
}

impl Default for Shared {
    fn default() -> Self {
        Self {
            bypass: AtomicBool::new(false),
            mute: AtomicBool::new(false),
            monitor_enabled: AtomicBool::new(false),
            monitor_pre: AtomicBool::new(true),
            input_gain: AtomicF32::new(1.0),
            output_gain: AtomicF32::new(1.0),
            input_channel: AtomicI32::new(-1),
            margin: AtomicF32::new(0.006),
            margin_gen: AtomicU32::new(0),
            in_peak: AtomicF32::default(),
            out_peak: AtomicF32::default(),
            load: AtomicF32::default(),
            in_block: AtomicU32::new(480),
            capture_xruns: AtomicU32::new(0),
            dsp_latency: AtomicU32::new(0),
            input_failed: AtomicBool::new(false),
            cable: SinkStats::default(),
            monitor: SinkStats::default(),
            fx: FxParams::default(),
            scope: Scope::default(),
        }
    }
}

impl Shared {
    pub fn set_margin(&self, seconds: f64) {
        self.margin.store(seconds as f32);
        self.margin_gen.fetch_add(1, Relaxed);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scope_snapshot_returns_latest_samples_in_order() {
        let s = Scope::default();
        for block in 0..10 {
            let input: Vec<f32> = (0..1000).map(|i| (block * 1000 + i) as f32).collect();
            let output: Vec<f32> = input.iter().map(|v| -v).collect();
            s.write_input(&input);
            s.write_output(&output);
        }
        let (inp, out) = s.snapshot(2048);
        assert_eq!(inp.len(), 2048);
        assert_eq!(inp.last(), Some(&9999.0));
        assert_eq!(inp[0], 9999.0 - 2047.0);
        assert!(inp.windows(2).all(|w| w[1] == w[0] + 1.0));
        assert_eq!(out.last(), Some(&-9999.0));
    }
}
