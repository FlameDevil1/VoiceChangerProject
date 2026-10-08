//! Noise suppression with RNNoise (nnnoiseless, a pure-Rust port).
//!
//! RNNoise works on fixed 10 ms frames at 48 kHz and adds 10 ms of its own (overlap-add).
//!
//! - **Direct mode** (normal): while audio arrives in whole 10 ms frames, which is what WASAPI
//!   delivers at 48 kHz, each frame is processed as soon as it is complete. Total delay: 10 ms.
//! - **Buffered mode** (fallback): the first time a block that isn't a whole number of frames
//!   arrives, the effect switches for good to collecting samples into frames, with a FIFO pre-filled
//!   with one frame of silence. Total delay: 20 ms. The switch costs one 10 ms gap.
//!
//! Both modes compute identical frames; buffered output is exactly one frame later. Disabled, the
//! effect is removed from the chain and adds nothing.
//!
//! At sample rates other than 48 kHz the effect passes audio through and reports status 1, so
//! the UI can explain how to switch the mic to 48 kHz.

use crate::dsp::Processor;
use crate::dsp::params::{EffectParams, EffectSpec};
use nnnoiseless::DenoiseState;
use std::sync::Arc;

pub const SPEC: EffectSpec = EffectSpec {
    label: "Noise suppression",
    help: "AI noise removal (RNNoise): fans, keyboards, traffic. Adds 10 ms of delay while on.",
    params: &[],
    mix_label: "Strength",
    default_mix: 1.0,
    send_mix: false,
    choices: &[],
    random: &[],
    presets: &[],
};

pub const STATUS_UNSUPPORTED_RATE: u32 = 1;
/// Running in buffered (20 ms) mode because the audio arrived in uneven blocks.
pub const STATUS_BUFFERED: u32 = 2;

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
    buffered: bool,
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
            buffered: false,
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
        if !self.buffered && !buf.len().is_multiple_of(FRAME) {
            self.buffered = true;
            self.params.status.store(STATUS_BUFFERED, std::sync::atomic::Ordering::Relaxed);
        }
        if !self.buffered {
            for chunk in buf.as_chunks_mut::<FRAME>().0 {
                for (dst, s) in self.frame_in.iter_mut().zip(chunk.iter()) {
                    *dst = *s * SCALE;
                }
                self.vad = state.process_frame(&mut self.frame_out, &self.frame_in);
                for (s, v) in chunk.iter_mut().zip(self.frame_out.iter()) {
                    *s = *v / SCALE;
                }
            }
            self.params.meter.store(self.vad);
            return;
        }
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
        match (&self.state, self.buffered) {
            (None, _) => 0,
            (Some(_), false) => FRAME,
            (Some(_), true) => 2 * FRAME,
        }
    }

    fn max_latency(&self) -> usize {
        2 * FRAME
    }

    /// Clears the framing buffers. The network's internal state is kept: rebuilding it would
    /// allocate on the audio thread, and it re-adapts within a few frames anyway.
    fn reset(&mut self) {
        self.filled = 0;
        self.fifo = [0.0; 2 * FRAME];
        self.read = 0;
        self.write = FRAME;
        self.vad = 0.0;
        // A fresh start gets another chance at direct mode.
        if self.buffered {
            self.buffered = false;
            self.params.status.store(0, std::sync::atomic::Ordering::Relaxed);
        }
    }
}
