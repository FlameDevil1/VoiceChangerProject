//! Lock-free parameter primitives shared between the UI and the audio thread.

use std::sync::atomic::{AtomicBool, AtomicU32, Ordering::Relaxed};

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

/// Controls every effect slot has: on/off and wet/dry mix.
#[derive(Debug)]
pub struct SlotParams {
    pub enabled: AtomicBool,
    /// 0.0 = dry only, 1.0 = effect only.
    pub mix: AtomicF32,
}

impl Default for SlotParams {
    fn default() -> Self {
        Self { enabled: AtomicBool::new(false), mix: AtomicF32::new(1.0) }
    }
}

impl SlotParams {
    pub fn enabled(&self) -> bool {
        self.enabled.load(Relaxed)
    }
    pub fn set_enabled(&self, on: bool) {
        self.enabled.store(on, Relaxed)
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
