//! Effect panels, generated from each effect's spec table.

use super::widgets::{AMBER, arrow_button, choice_cells, lock_toggle, slider_cells, slider_row};
use eframe::egui::{self, RichText};
use std::collections::BTreeSet;
use std::sync::atomic::Ordering::Relaxed;
use voice_changer::dsp::fx::denoise::STATUS_UNSUPPORTED_RATE;
use voice_changer::dsp::params::EffectParams;
use voice_changer::dsp::util::Rng;
use voice_changer::dsp::{EffectKind, FxSettings};

/// What the Randomize buttons need: a generator and the controls the user locked.
pub struct Dice<'a> {
    pub rng: &'a mut Rng,
    pub locks: &'a mut BTreeSet<String>,
}

impl Dice<'_> {
    pub fn roll(&mut self, fx: &mut FxSettings, kind: EffectKind) {
        let locks = &*self.locks;
        fx.randomize(kind, self.rng, |key| locks.contains(&lock_key(kind, key)));
    }

    /// Lock toggle cell for `kind`/`key` if Randomize may change it (an empty cell otherwise).
    pub fn lock_cell(&mut self, ui: &mut egui::Ui, kind: EffectKind, key: &str) {
        if !kind.spec().random.iter().any(|(k, ..)| *k == key) {
            ui.label("");
            return;
        }
        let id = lock_key(kind, key);
        let mut locked = self.locks.contains(&id);
        if lock_toggle(ui, &mut locked) {
            if locked {
                self.locks.insert(id);
            } else {
                self.locks.remove(&id);
            }
        }
    }
}

pub fn lock_key(kind: EffectKind, key: &str) -> String {
    format!("{}.{key}", kind.key())
}

/// Reorder request from a panel header.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Move {
    Up,
    Down,
}

/// One effect: header with on/off, live readout and (optionally) reorder buttons, then presets,
/// sliders and mix. Returns a reorder request if one of the arrows was clicked.
pub fn effect_panel(
    ui: &mut egui::Ui,
    kind: EffectKind,
    fx: &mut FxSettings,
    live: &EffectParams,
    active: bool,
    reorder: Option<(bool, bool)>,
    dice: &mut Dice,
) -> Option<Move> {
    let spec = kind.spec();
    let id = ui.make_persistent_id(("effect", kind.key()));
    let mut enabled = fx.enabled(kind);
    let unsupported = live.status.load(Relaxed) == STATUS_UNSUPPORTED_RATE;
    let mut action = None;
    egui::collapsing_header::CollapsingState::load_with_default_open(ui.ctx(), id, false)
        .show_header(ui, |ui| {
            if ui.checkbox(&mut enabled, RichText::new(spec.label).strong()).on_hover_text(spec.help).changed() {
                fx.set_enabled(kind, enabled);
            }
            if enabled && active {
                let m = live.meter.load();
                let readout = match kind {
                    EffectKind::Gate => Some(if m > 0.5 { "open".to_string() } else { "closed".to_string() }),
                    EffectKind::Compressor => Some(format!("GR {:.1} dB", m.abs())),
                    EffectKind::Denoise if !unsupported => Some(format!("voice {:.0}%", m * 100.0)),
                    _ => None,
                };
                if let Some(r) = readout {
                    ui.label(RichText::new(r).small().monospace().weak());
                }
            }
            if enabled && unsupported {
                ui.label(RichText::new("⚠ needs 48 kHz").small().color(AMBER));
            }
            if let Some((can_up, can_down)) = reorder {
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if arrow_button(ui, false, can_down, "Move later in the chain") {
                        action = Some(Move::Down);
                    }
                    if arrow_button(ui, true, can_up, "Move earlier in the chain") {
                        action = Some(Move::Up);
                    }
                });
            }
        })
        .body(|ui| {
            ui.label(RichText::new(spec.help).small().weak());
            if unsupported {
                ui.colored_label(
                    AMBER,
                    "Your microphone runs at a rate other than 48 kHz. Set it to 48 kHz in Windows Sound settings > Recording > Properties > Advanced.",
                );
            }
            if !spec.presets.is_empty() || !spec.random.is_empty() {
                ui.horizontal_wrapped(|ui| {
                    for (name, values) in spec.presets {
                        if ui.small_button(*name).clicked() {
                            for (k, v) in *values {
                                fx.set(kind, k, *v);
                            }
                            fx.set_enabled(kind, true);
                        }
                    }
                    if !spec.random.is_empty()
                        && ui
                            .small_button("🎲 Randomize")
                            .on_hover_text("Random values for the controls with a lock icon (locked ones stay)")
                            .clicked()
                    {
                        dice.roll(fx, kind);
                    }
                });
            }
            let mut moved = false;
            egui::Grid::new(("fx-grid", kind.key())).num_columns(4).spacing([8.0, 6.0]).show(ui, |ui| {
                for p in spec.params {
                    let mut v = fx.get(kind, p.key);
                    let r = match spec.choices_for(p.key) {
                        Some(options) => choice_cells(ui, p.label, &mut v, options, p.default),
                        None => slider_cells(ui, p.label, &mut v, p.min..=p.max, p.unit, p.step as f64, p.default),
                    };
                    r.on_hover_text(p.help);
                    if v != fx.get(kind, p.key) {
                        fx.set(kind, p.key, v);
                        moved = true;
                    }
                    dice.lock_cell(ui, kind, p.key);
                    ui.end_row();
                }
                let mut pct = fx.mix(kind) * 100.0;
                if slider_row(ui, spec.mix_label, &mut pct, 0.0..=100.0, " %", 1.0, spec.default_mix * 100.0).changed() {
                    fx.set_mix(kind, pct / 100.0);
                    moved = true;
                }
            });
            // Moving a control while the effect is off is a clear sign you want it on.
            if moved && !fx.enabled(kind) {
                fx.set_enabled(kind, true);
            }
        });
    action
}
