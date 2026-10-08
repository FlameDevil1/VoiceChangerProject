//! Effect processors. Each module exports a `SPEC` (its controls) and a `Processor`.
//!
//! Conventions: percentages are stored as 0..100 (what the UI shows), times in ms, levels in dB.
//! Everything that changes over time is computed per sample from absolute sample counts, so the
//! output never depends on how audio is split into blocks.

pub mod badmic;
pub mod character;
pub mod compressor;
pub mod denoise;
pub mod eq;
pub mod gate;
pub mod network;
pub mod pitch_fx;
pub mod radio;
pub mod reverb;
pub mod robot;

/// One-pole smoothing coefficient for a time constant in milliseconds.
#[inline]
pub(crate) fn coef(ms: f32, sample_rate: f32) -> f32 {
    (-1.0 / (ms.max(0.01) * 0.001 * sample_rate)).exp()
}

/// Fast dB -> gain for the per-sample paths.
#[inline]
pub(crate) fn db_gain(db: f32) -> f32 {
    (db * (std::f32::consts::LOG2_10 / 20.0)).exp2()
}
