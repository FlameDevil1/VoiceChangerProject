//! Voice changer core: real-time audio engine, DSP and configuration.
//!
//! The GUI lives in the binary (`main.rs` / `gui.rs`); everything here is UI-agnostic so it can
//! be unit-tested and reused by the offline file renderer.

pub mod audio;
pub mod autostart;
pub mod backup;
pub mod calibrate;
pub mod config;
pub mod dsp;
pub mod elevation;
pub mod hotkeys;
pub mod logging;
pub mod offline;
pub mod presets;
pub mod updates;
