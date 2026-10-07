//! Noise suppression with RNNoise (nnnoiseless, a pure-Rust port).
//!
//! RNNoise works on fixed 10 ms frames at 48 kHz. Samples are collected into a frame, and a FIFO
//! pre-filled with one frame of silence keeps output flowing while the next frame fills. Total
//! delay is 20 ms: 10 ms of framing plus RNNoise's own 10 ms overlap. This only applies while
//! the effect is on; disabled, it is removed from the chain.
//!
//! At sample rates other than 48 kHz the effect passes audio through and reports status 1, so
//! the UI can explain how to switch the mic to 48 kHz.

use crate::dsp::Processor;
use crate::dsp::params::{EffectParams, EffectSpec};
use nnnoiseless::DenoiseState;
use std::sync::Arc;

pub const SPEC: EffectSpec = EffectSpec {
    label: "Noise suppression",
    help: "AI noise removal (RNNoise): fans, keyboards, traffic. Adds 20 ms of delay while on.",
    params: &[],
    mix_label: "Strength",
    default_mix: 1.0,
    presets: &[],
};

pub const STATUS_UNSUPPORTED_RATE: u32 = 1;

const FRAME: usize = DenoiseState::FRAME_SIZE;
/// RNNoise expects 16-bit sample values in f32.
const SCALE: f32 = 32768.0;

/// Run one throwaway frame on the calling thread.
///
/// RNNoise's FFT library (easyfft) keeps a *thread-local* FFT planner that it builds, with ~50
/// heap allocations, the first time an FFT runs on a thread. That can't be done ahead of time
/// from another thread, so the engine calls this once at the start of every capture thread,
/// in its first callback, before any audio is sent; after that the audio path never allocates.
pub fn warm_up_thread() {
    let mut state = DenoiseState::new();
    let mut out = [0.0f32; FRAME];
    state.process_frame(&mut out, &[0.0f32; FRAME]);
}

pub struct Denoise {
    params: Arc<EffectParams>,
    state: Option<Box<DenoiseState<'static>>>,
    frame_in: [f32; FRAME],
    frame_out: [f32; FRAME],
    filled: usize,
    /// Output FIFO (ring of 2 frames); holds exactly one frame between frame boundaries.
    fifo: [f32; 2 * FRAME],
    read: usize,
    write: usize,
    vad: f32,
}

impl Denoise {
    pub fn new(params: Arc<EffectParams>) -> Self {
        Self {
            params,
            state: None,
            frame_in: [0.0; FRAME],
            frame_out: [0.0; FRAME],
            filled: 0,
            fifo: [0.0; 2 * FRAME],
            read: 0,
            write: FRAME,
            vad: 0.0,
        }
    }
}

impl Processor for Denoise {
    fn prepare(&mut self, sample_rate: f32, _max_block: usize) {
        let supported = (sample_rate - 48_000.0).abs() < 1.0;
        self.state = supported.then(DenoiseState::new);
        self.params
            .status
            .store(if supported { 0 } else { STATUS_UNSUPPORTED_RATE }, std::sync::atomic::Ordering::Relaxed);
        self.reset();
    }

    fn process(&mut self, buf: &mut [f32]) {
        let Some(state) = self.state.as_mut() else { return };
        for s in buf.iter_mut() {
            self.frame_in[self.filled] = *s * SCALE;
            self.filled += 1;
            if self.filled == FRAME {
                self.filled = 0;
                self.vad = state.process_frame(&mut self.frame_out, &self.frame_in);
                for &v in &self.frame_out {
                    self.fifo[self.write] = v / SCALE;
                    self.write = (self.write + 1) % self.fifo.len();
                }
            }
            *s = self.fifo[self.read];
            self.read = (self.read + 1) % self.fifo.len();
        }
        self.params.meter.store(self.vad);
    }

    fn latency(&self) -> usize {
        if self.state.is_some() { 2 * FRAME } else { 0 }
    }

    /// Clears the framing buffers. The network's internal state is kept: rebuilding it would
    /// allocate on the audio thread, and it re-adapts within a few frames anyway.
    fn reset(&mut self) {
        self.filled = 0;
        self.fifo = [0.0; 2 * FRAME];
        self.read = 0;
        self.write = FRAME;
        self.vad = 0.0;
    }
}
