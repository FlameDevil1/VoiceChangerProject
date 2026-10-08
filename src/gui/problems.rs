//! "Bad mic & connection": problem scenarios on top of any voice, the bad connection master
//! slider and the glitch burst.

use super::App;
use super::effects::remember;
use super::widgets::slider_row;
use eframe::egui::{self, RichText};
use std::sync::atomic::Ordering::Relaxed;
use voice_changer::dsp::EffectKind;
use voice_changer::presets::{PROBLEMS, SCENARIOS, clear_problems};

impl App {
    pub(super) fn problems_section(&mut self, ui: &mut egui::Ui, advanced: bool) {
        let mut fx = self.cfg.fx.clone();
        let scenario = SCENARIOS.iter().find(|s| s.matches(&fx)).map(|s| s.name);
        let any_on = PROBLEMS.iter().any(|&k| fx.enabled(k));
        let title = match (scenario, any_on) {
            (Some(name), _) => format!("Bad mic & connection: {name}"),
            (None, true) => "Bad mic & connection: custom".to_string(),
            (None, false) => "Bad mic & connection".to_string(),
        };
        let mut one_click = false;
        let mut glitch = false;
        let mut skip_lag = self.cfg.monitor_skip_lag;
        ui.add_space(4.0);
        egui::Frame::group(ui.style()).show(ui, |ui| {
            ui.set_width(ui.available_width());
            egui::CollapsingHeader::new(RichText::new(title).strong()).id_salt("problems").default_open(false).show(
                ui,
                |ui| {
                    ui.label(RichText::new("Sound like a bad mic or a bad call, on top of any voice.").small().weak());
                    ui.horizontal_wrapped(|ui| {
                        if ui.add(egui::Button::new("Off").selected(!any_on)).on_hover_text("No problems").clicked() {
                            fx = clear_problems(&fx);
                            one_click = true;
                        }
                        for s in SCENARIOS {
                            let r = ui.add(egui::Button::new(s.name).selected(scenario == Some(s.name)));
                            if r.on_hover_text(s.description).clicked() {
                                fx = s.apply(&fx);
                                one_click = true;
                            }
                        }
                    });
                    egui::Grid::new("problems-grid").num_columns(3).spacing([8.0, 6.0]).show(ui, |ui| {
                        let net = EffectKind::Network;
                        let mut amount = if fx.enabled(net) { fx.get(net, "amount") } else { 0.0 };
                        let r = slider_row(ui, "Bad connection", &mut amount, 0.0..=100.0, " %", 1.0, 0.0);
                        if r.on_hover_text("From a clear call (0) to nearly unusable (100). 0 turns it off.").changed()
                        {
                            fx.set(net, "amount", amount);
                            fx.set_enabled(net, amount > 0.0);
                        }
                    });
                    ui.horizontal(|ui| {
                        glitch = ui
                            .button("⚡ Glitch now")
                            .on_hover_text("A short stutter and lag on the virtual mic. Can also go on a hotkey.")
                            .clicked();
                        if advanced {
                            ui.checkbox(&mut skip_lag, "Hear myself without the lag").on_hover_text(
                                "Hearing your own voice late makes it very hard to keep talking, so \"Hear myself\" \
                                 skips the bad connection by default. Others still hear it.",
                            );
                        }
                    });
                },
            );
        });
        if one_click {
            remember(&mut self.fx_history, &self.cfg.fx);
        }
        self.apply_fx_edit(fx, self.cfg.locked.clone());
        if skip_lag != self.cfg.monitor_skip_lag {
            self.cfg.monitor_skip_lag = skip_lag;
            self.shared().monitor_pre.store(skip_lag, Relaxed);
            self.mark_dirty();
        }
        if glitch {
            self.shared().fx.get(EffectKind::Network).trigger.fetch_add(1, Relaxed);
            self.glitch_burst();
        }
    }

    /// A glitch burst was requested (the trigger is already bumped): make sure the bad connection
    /// is on to play it. Turned on just for this, it runs with no random problems.
    pub(super) fn glitch_burst(&mut self) {
        let net = EffectKind::Network;
        if !self.cfg.fx.enabled(net) {
            let mut fx = self.cfg.fx.clone();
            fx.set(net, "amount", 0.0);
            fx.set_enabled(net, true);
            self.set_fx(fx);
        }
    }
}
