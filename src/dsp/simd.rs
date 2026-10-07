//! Runtime CPU feature dispatch.
//!
//! Release builds target the baseline x86-64 instruction set, so the app runs on any 64-bit PC.
//! The few loops where wider SIMD measurably helps are compiled twice, and the AVX2 version is
//! picked at startup when the CPU supports it. Rust never fuses multiply-adds on its own, so both
//! versions give bit-identical results: presets sound the same on every machine.

use std::sync::OnceLock;

/// Instruction set used for the dispatched loops.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Level {
    Baseline,
    Avx2,
}

impl Level {
    pub fn label(self) -> &'static str {
        match self {
            Level::Baseline => "SSE2",
            Level::Avx2 => "AVX2",
        }
    }
}

/// Detected once, then a cached load. `VC_FORCE_BASELINE=1` disables AVX2 (for testing).
pub fn level() -> Level {
    static LEVEL: OnceLock<Level> = OnceLock::new();
    *LEVEL.get_or_init(|| {
        #[cfg(target_arch = "x86_64")]
        if std::env::var_os("VC_FORCE_BASELINE").is_none() && std::is_x86_feature_detected!("avx2") {
            return Level::Avx2;
        }
        Level::Baseline
    })
}

/// Sum of squared differences of two equal-length slices (the YIN pitch tracker's hot loop).
pub type SqDiff = fn(&[f32], &[f32]) -> f32;

/// Best `sq_diff` implementation for this CPU; resolve once and keep the function pointer.
pub fn sq_diff_fn() -> SqDiff {
    #[cfg(target_arch = "x86_64")]
    if level() == Level::Avx2 {
        return sq_diff_avx2_safe;
    }
    sq_diff_generic
}

/// Written with 8 independent accumulators so it vectorises (a single float accumulator can't be
/// reordered, which blocks SIMD). Inlined into each target-specific wrapper.
#[inline(always)]
fn sq_diff_body(a: &[f32], b: &[f32]) -> f32 {
    let mut acc = [0.0f32; 8];
    let ((ca, ra), (cb, rb)) = (a.as_chunks::<8>(), b.as_chunks::<8>());
    for (x, y) in ca.iter().zip(cb) {
        for i in 0..8 {
            let d = x[i] - y[i];
            acc[i] += d * d;
        }
    }
    let mut s: f32 = acc.iter().sum();
    for (x, y) in ra.iter().zip(rb) {
        s += (x - y) * (x - y);
    }
    s
}

pub fn sq_diff_generic(a: &[f32], b: &[f32]) -> f32 {
    sq_diff_body(a, b)
}

#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "avx2")]
fn sq_diff_avx2(a: &[f32], b: &[f32]) -> f32 {
    sq_diff_body(a, b)
}

#[cfg(target_arch = "x86_64")]
fn sq_diff_avx2_safe(a: &[f32], b: &[f32]) -> f32 {
    // SAFETY: only handed out by `sq_diff_fn` after AVX2 was detected on this CPU.
    unsafe { sq_diff_avx2(a, b) }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dispatched_matches_generic_bit_for_bit() {
        let a: Vec<f32> = (0..1003).map(|i| (i as f32 * 0.37).sin()).collect();
        let b: Vec<f32> = (0..1003).map(|i| (i as f32 * 0.11).cos()).collect();
        assert_eq!(sq_diff_fn()(&a, &b).to_bits(), sq_diff_generic(&a, &b).to_bits());
    }
}
