//! Lock-free state shared between the audio callbacks and the rest of the app.
//!
//! Controls flow UI -> audio, statistics flow audio -> UI. Everything is an atomic so the audio
//! thread never blocks.

use std::sync::atomic::{AtomicBool, AtomicI32, AtomicU32, Ordering::Relaxed};

/// An `f32` stored in an `AtomicU32`.
#[derive(Debug, Default)]
pub struct AtomicF32(AtomicU32);

impl AtomicF32 {
    pub fn new(v: f32) -> Self {
        Self(AtomicU32::new(v.to_bits()))
    }
    pub fn load(&self) -> f32 {
        f32::from_bits(self.0.load(Relaxed))
    }
    pub fn store(&self, v: f32) {
        self.0.store(v.to_bits(), Relaxed)
    }
    /// Max-hold for non-negative values (their bit patterns sort like the numbers).
    pub fn fetch_max(&self, v: f32) {
        self.0.fetch_max(v.max(0.0).to_bits(), Relaxed);
    }
    /// Read and reset to zero (used for peak-hold meters read once per UI frame).
    pub fn take(&self) -> f32 {
        f32::from_bits(self.0.swap(0, Relaxed))
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
    pub input_failed: AtomicBool,
    pub cable: SinkStats,
    pub monitor: SinkStats,
}

impl Default for Shared {
    fn default() -> Self {
        Self {
            bypass: AtomicBool::new(false),
            mute: AtomicBool::new(false),
            monitor_enabled: AtomicBool::new(false),
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
            input_failed: AtomicBool::new(false),
            cable: SinkStats::default(),
            monitor: SinkStats::default(),
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
    fn atomic_f32_max_and_take() {
        let a = AtomicF32::default();
        a.fetch_max(0.25);
        a.fetch_max(0.5);
        a.fetch_max(0.1);
        assert_eq!(a.take(), 0.5);
        assert_eq!(a.load(), 0.0);
    }
}
