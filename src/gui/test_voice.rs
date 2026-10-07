//! "Test my voice": record a few seconds of the raw mic, then play it back with the current
//! effects (re-rendered on every play, so you can tweak and replay) or as the original.
//!
//! Playback goes to your headphones (the monitor device), never to the virtual mic, and the
//! virtual mic is muted while recording so others don't hear the test.

use super::App;
use eframe::egui::{self, RichText};
use rtrb::{Consumer, RingBuffer};
use std::sync::Arc;
use std::sync::atomic::Ordering::Relaxed;
use voice_changer::audio::Command;
use voice_changer::audio::playback::{self, Playback};
use voice_changer::dsp::{CoreParams, db_to_gain};
use voice_changer::offline;

const SECONDS: f32 = 5.0;

struct Recording {
    rx: Consumer<f32>,
    samples: Vec<f32>,
    rate: u32,
    was_muted: bool,
}

#[derive(Default)]
pub struct TestVoice {
    recording: Option<Recording>,
    clip: Option<(Arc<Vec<f32>>, u32)>,
    playing: Option<(Arc<Playback>, bool)>,
}

impl App {
    fn start_test_recording(&mut self) {
        let rate = self.status.sample_rate.max(8_000);
        let (tx, rx) = RingBuffer::new((rate as f32 * (SECONDS + 1.0)) as usize);
        self.engine.send(Command::SetTap(Some(tx)));
        let was_muted = self.shared().mute.swap(true, Relaxed);
        self.test.recording =
            Some(Recording { rx, samples: Vec::with_capacity((rate as f32 * SECONDS) as usize), rate, was_muted });
        self.stop_test_playback();
    }

    fn finish_test_recording(&mut self) {
        if let Some(rec) = self.test.recording.take() {
            self.engine.send(Command::SetTap(None));
            self.shared().mute.store(rec.was_muted, Relaxed);
            if !rec.samples.is_empty() {
                self.test.clip = Some((Arc::new(rec.samples), rec.rate));
            }
        }
    }

    fn stop_test_playback(&mut self) {
        if let Some((p, _)) = self.test.playing.take() {
            p.stop.store(true, Relaxed);
        }
    }

    fn play_test(&mut self, processed: bool) {
        let Some((clip, rate)) = self.test.clip.clone() else { return };
        self.stop_test_playback();
        let fx = self.cfg.fx.clone();
        let params = CoreParams {
            input_gain: db_to_gain(self.cfg.input_gain_db),
            output_gain: db_to_gain(self.cfg.output_gain_db),
            ..Default::default()
        };
        let render = move || {
            if processed { offline::render(&clip, rate, params, &fx, offline::DEFAULT_BLOCK) } else { clip.to_vec() }
        };
        let p = playback::play(render, rate, self.cfg.monitor.clone());
        self.test.playing = Some((p, processed));
    }

    /// Per-frame bookkeeping: collect recorded audio, notice when playback ends.
    pub(super) fn test_tick(&mut self, ctx: &egui::Context) {
        let mut done = false;
        if let Some(rec) = &mut self.test.recording {
            let n = rec.rx.slots();
            if let Ok(chunk) = rec.rx.read_chunk(n) {
                rec.samples.extend(chunk);
            }
            done = rec.samples.len() as f32 >= rec.rate as f32 * SECONDS;
            ctx.request_repaint_after(std::time::Duration::from_millis(50));
        }
        if done || (self.test.recording.is_some() && !self.is_active()) {
            self.finish_test_recording();
        }
        if let Some((p, _)) = &self.test.playing {
            if p.finished.load(Relaxed) {
                self.test.playing = None;
            } else {
                ctx.request_repaint_after(std::time::Duration::from_millis(50));
            }
        }
    }

    pub(super) fn test_voice_row(&mut self, ui: &mut egui::Ui) {
        if let Some(rec) = &self.test.recording {
            let progress = rec.samples.len() as f32 / (rec.rate as f32 * SECONDS);
            ui.horizontal(|ui| {
                ui.add(
                    egui::ProgressBar::new(progress.min(1.0))
                        .text(format!("Recording… speak now ({:.1} s)", SECONDS * (1.0 - progress).max(0.0)))
                        .desired_width(ui.available_width() - 70.0),
                );
                if ui.button("Stop").clicked() {
                    self.finish_test_recording();
                }
            });
            ui.label(RichText::new("The virtual mic is muted while recording.").small().weak());
            return;
        }
        ui.horizontal_wrapped(|ui| {
            let active = self.is_active();
            let label = if self.test.clip.is_some() { "Record again" } else { "Test my voice" };
            if ui
                .add_enabled(active, egui::Button::new(label))
                .on_hover_text("Record 5 seconds, then hear it with your current effects")
                .on_disabled_hover_text("Press Start first")
                .clicked()
            {
                self.start_test_recording();
            }
            if self.test.clip.is_some() {
                let playing = self.test.playing.as_ref().map(|(_, processed)| *processed);
                if ui
                    .add(egui::Button::new("▶ Changed voice").selected(playing == Some(true)))
                    .on_hover_text("Uses the current settings: tweak and play again")
                    .clicked()
                {
                    self.play_test(true);
                }
                if ui.add(egui::Button::new("▶ Original").selected(playing == Some(false))).clicked() {
                    self.play_test(false);
                }
                if playing.is_some() && ui.button("■ Stop").clicked() {
                    self.stop_test_playback();
                }
            }
        });
        if let Some((p, _)) = &self.test.playing {
            let total = p.total.load(Relaxed).max(1);
            ui.add(egui::ProgressBar::new(p.position.load(Relaxed) as f32 / total as f32).desired_height(4.0));
        }
    }
}
