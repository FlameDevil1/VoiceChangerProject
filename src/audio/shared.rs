//! Lock-free state shared between the audio callbacks and the rest of the app.
//!
//! Controls flow UI -> audio, statistics flow audio -> UI. Everything is an atomic so the audio
//! thread never blocks.

use crate::dsp::chain::FxParams;
pub use crate::dsp::shared_params::AtomicF32;
use std::sync::atomic::{AtomicBool, AtomicI32, AtomicU32, Ordering::Relaxed};

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
    /// Total processing latency of the core (effect chain + limiter), in samples.
    pub dsp_latency: AtomicU32,
    pub input_failed: AtomicBool,
    pub cable: SinkStats,
    pub monitor: SinkStats,

    // ---- effects (UI -> audio) ----
    pub fx: FxParams,
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
            dsp_latency: AtomicU32::new(0),
            input_failed: AtomicBool::new(false),
            cable: SinkStats::default(),
            monitor: SinkStats::default(),
            fx: FxParams::default(),
        }
    }
}

impl Shared {
    pub fn set_margin(&self, seconds: f64) {
        self.margin.store(seconds as f32);
        self.margin_gen.fetch_add(1, Relaxed);
    }
}

