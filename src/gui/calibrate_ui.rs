//! "Set up mic level" wizard: 3 s of silence, 4 s of speech, then a suggested input gain and
//! noise-gate threshold to apply with one click.

use super::App;
use super::widgets::{AMBER, GREEN};
use eframe::egui::{self, RichText};
use rtrb::{Consumer, RingBuffer};
use std::sync::atomic::Ordering::Relaxed;
use voice_changer::audio::Command;
use voice_changer::calibrate::{self, Calibration, Verdict};
use voice_changer::dsp::{EffectKind, db_to_gain};

const QUIET_S: f32 = 3.0;
const SPEECH_S: f32 = 4.0;

enum Phase {
    Quiet,
    Speech { quiet: Vec<f32> },
}

struct Listening {
    rx: Consumer<f32>,
    samples: Vec<f32>,
    rate: u32,
    phase: Phase,
    was_muted: bool,
}

#[derive(Default)]
pub struct CalibrationUi {
    listening: Option<Listening>,
    result: Option<Calibration>,
}

impl CalibrationUi {
    pub fn busy(&self) -> bool {
        self.listening.is_some()
    }
}

impl App {
    fn start_calibration(&mut self) {
        let rate = self.status.sample_rate.max(8_000);
        let (tx, rx) = RingBuffer::new((rate as f32 * (SPEECH_S + 1.0)) as usize);
        self.engine.send(Command::SetTap(Some(tx)));
        let was_muted = self.shared().mute.swap(true, Relaxed);
        self.calibration.listening = Some(Listening { rx, samples: Vec::new(), rate, phase: Phase::Quiet, was_muted });
        self.calibration.result = None;
    }

    fn stop_calibration(&mut self) -> Option<Listening> {
        let l = self.calibration.listening.take()?;
        self.engine.send(Command::SetTap(None));
        self.shared().mute.store(l.was_muted, Relaxed);
        Some(l)
    }

    pub(super) fn calibration_tick(&mut self, ctx: &egui::Context) {
        let Some(l) = &mut self.calibration.listening else { return };
        let n = l.rx.slots();
        if let Ok(chunk) = l.rx.read_chunk(n) {
            l.samples.extend(chunk);
        }
        ctx.request_repaint_after(std::time::Duration::from_millis(50));
        let rate = l.rate as f32;
        match &l.phase {
            Phase::Quiet if l.samples.len() as f32 >= rate * QUIET_S => {
                let quiet = std::mem::take(&mut l.samples);
                l.phase = Phase::Speech { quiet };
            }
            Phase::Speech { .. } if l.samples.len() as f32 >= rate * SPEECH_S => {
                if let Some(l) = self.stop_calibration()
                    && let Phase::Speech { quiet } = l.phase
                {
                    self.calibration.result = Some(calibrate::analyze(&quiet, &l.samples, l.rate));
                }
            }
            _ => {}
        }
        if !self.is_active() {
            self.stop_calibration();
        }
    }

    fn apply_calibration(&mut self, c: Calibration) {
        self.cfg.input_gain_db = c.input_gain_db.clamp(-24.0, 24.0);
        self.shared().input_gain.store(db_to_gain(self.cfg.input_gain_db));
        let mut fx = self.cfg.fx.clone();
        fx = fx.with(EffectKind::Gate, &[("threshold", c.gate_threshold_db)]);
        self.set_fx(fx);
    }

    pub(super) fn calibration_row(&mut self, ui: &mut egui::Ui) {
        if let Some(l) = &self.calibration.listening {
            let (text, total) = match l.phase {
                Phase::Quiet => ("Step 1 of 2: stay quiet…", QUIET_S),
                Phase::Speech { .. } => ("Step 2 of 2: talk normally (count to ten)…", SPEECH_S),
            };
            let progress = l.samples.len() as f32 / (l.rate as f32 * total);
            ui.horizontal(|ui| {
                ui.add(egui::ProgressBar::new(progress.min(1.0)).text(text).desired_width(ui.available_width() - 70.0));
                if ui.button("Cancel").clicked() {
                    self.stop_calibration();
                }
            });
            return;
        }
        if let Some(c) = self.calibration.result {
            let color = if c.verdict == Verdict::Good { GREEN } else { AMBER };
            ui.colored_label(color, c.verdict.advice());
            ui.label(
                RichText::new(format!(
                    "Background {:.0} dB, voice {:.0} dB. Suggested: input gain {:+.1} dB, noise gate at {:.0} dB.",
                    c.noise_db, c.speech_db, c.input_gain_db, c.gate_threshold_db
                ))
                .small(),
            );
            ui.horizontal(|ui| {
                if ui.add_enabled(c.verdict != Verdict::TooQuiet, egui::Button::new("Apply")).clicked() {
                    self.apply_calibration(c);
                    self.calibration.result = None;
                }
                if ui.button("Dismiss").clicked() {
                    self.calibration.result = None;
                }
            });
            return;
        }
        let can = self.is_active() && self.test_idle();
        if ui
            .add_enabled(can, egui::Button::new("Set up mic level"))
            .on_hover_text("Measures your room and voice, then suggests input gain and a noise gate threshold")
            .on_disabled_hover_text("Press Start first")
            .clicked()
        {
            self.start_calibration();
        }
    }
}
