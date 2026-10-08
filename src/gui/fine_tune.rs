//! Simple mode's "Fine-tune": the few voice controls people reach for after picking a preset,
//! without opening the full effect panels.

use super::App;
use super::effects::Dice;
use super::widgets::slider_cells;
use eframe::egui::{self, RichText};
use voice_changer::dsp::EffectKind;

/// (effect, parameter, label shown here).
const CONTROLS: [(EffectKind, &str, &str); 5] = [
    (EffectKind::Pitch, "semitones", "Pitch"),
    (EffectKind::Pitch, "formant", "Formant"),
    (EffectKind::Character, "tone", "Tone"),
    (EffectKind::Character, "breath", "Breathiness"),
    (EffectKind::Character, "rough", "Roughness"),
];

impl App {
    pub(super) fn fine_tune(&mut self, ui: &mut egui::Ui) {
        let mut fx = self.cfg.fx.clone();
        let mut locks = self.cfg.locked.clone();
        let mut dice = Dice { rng: &mut self.rng, locks: &mut locks };
        ui.add_space(4.0);
        egui::Frame::group(ui.style()).show(ui, |ui| {
            ui.set_width(ui.available_width());
            egui::CollapsingHeader::new(RichText::new("Fine-tune").strong())
                .id_salt("fine-tune")
                .default_open(false)
                .show(ui, |ui| {
                    ui.label(
                        RichText::new("Adjust the current voice. The preset shows * until you pick it again.")
                            .small()
                            .weak(),
                    );
                    egui::Grid::new("fine-tune-grid").num_columns(4).spacing([8.0, 6.0]).show(ui, |ui| {
                        for (kind, key, label) in CONTROLS {
                            let p = kind.spec().params[kind.spec().index(key).expect("fine-tune key")];
                            // A disabled effect contributes nothing, so show its neutral value.
                            let mut v = if fx.enabled(kind) { fx.get(kind, key) } else { p.default };
                            let shown = v;
                            slider_cells(ui, label, &mut v, p.min..=p.max, p.unit, p.step as f64, p.default)
                                .on_hover_text(p.help);
                            if v != shown {
                                if !fx.enabled(kind) {
                                    // Start from neutral rather than whatever an old setting left.
                                    for q in kind.spec().params {
                                        fx.set(kind, q.key, q.default);
                                    }
                                    fx.set_enabled(kind, true);
                                }
                                fx.set(kind, key, v);
                            }
                            dice.lock_cell(ui, kind, key);
                            ui.end_row();
                        }
                    });
                    ui.horizontal(|ui| {
                        if ui
                            .button("🎲 Randomize voice")
                            .on_hover_text("A random pitch, formant and voice character (locked controls stay)")
                            .clicked()
                        {
                            dice.roll(&mut fx, EffectKind::Pitch);
                            dice.roll(&mut fx, EffectKind::Character);
                        }
                        ui.label(RichText::new("More controls in Advanced mode.").small().weak());
                    });
                });
        });
        self.apply_fx_edit(fx, locks);
    }
}
