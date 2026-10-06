//! Voice changer core: real-time audio engine, DSP and configuration.
//!
//! The GUI lives in the binary (`main.rs` / `gui.rs`); everything here is UI-agnostic so it can
//! be unit-tested and reused by the offline file renderer.

pub mod audio;
pub mod config;
pub mod dsp;
pub mod logging;
