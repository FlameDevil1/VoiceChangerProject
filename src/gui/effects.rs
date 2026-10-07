//! Effect panels, generated from each effect's spec table.

use super::widgets::{AMBER, arrow_button, slider_row};
use eframe::egui::{self, RichText};
use std::sync::atomic::Ordering::Relaxed;
use voice_changer::dsp::fx::denoise::STATUS_UNSUPPORTED_RATE;
use voice_changer::dsp::params::EffectParams;
use voice_changer::dsp::{EffectKind, FxSettings};

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
                    "Your microphone runs at a rate other than 48 kHz. Set it to 48 kHz in Windows Sound settings → Recording → Properties → Advanced.",
                );
            }
            if !spec.presets.is_empty() {
                ui.horizontal_wrapped(|ui| {
                    for (name, values) in spec.presets {
                        if ui.small_button(*name).clicked() {
                            for (k, v) in *values {
                                fx.set(kind, k, *v);
                            }
                            fx.set_enabled(kind, true);
                        }
                    }
                });
            }
            let mut moved = false;
            egui::Grid::new(("fx-grid", kind.key())).num_columns(3).spacing([8.0, 6.0]).show(ui, |ui| {
                for p in spec.params {
                    let mut v = fx.get(kind, p.key);
                    let r = slider_row(ui, p.label, &mut v, p.min..=p.max, p.unit, p.step as f64, p.default).on_hover_text(p.help);
                    if r.changed() {
                        fx.set(kind, p.key, v);
                        moved = true;
                    }
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
