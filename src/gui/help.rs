//! Help & diagnostics: open the settings folder, copy a diagnostics report, back up and restore.

use super::App;
use super::widgets::{GREEN, RED, section};
use eframe::egui;
use std::sync::atomic::Ordering::Relaxed;
use voice_changer::backup::Backup;
use voice_changer::config::{self, Config};
use voice_changer::dsp::{EffectKind, simd};

const LOG_LINES: usize = 40;

impl App {
    pub(super) fn help_section(&mut self, ui: &mut egui::Ui) {
        section(ui, "Help & diagnostics", |ui| {
            ui.horizontal_wrapped(|ui| {
                if ui.button("Open settings folder").on_hover_text("Config, presets and logs").clicked() {
                    let _ = std::process::Command::new("explorer").arg(config::app_dir()).spawn();
                }
                if ui
                    .button("Copy diagnostics")
                    .on_hover_text("Copies a report (devices, settings, recent log) to paste into a bug report")
                    .clicked()
                {
                    ui.ctx().copy_text(self.diagnostics());
                    self.help_message = Some(("Diagnostics copied to the clipboard.".into(), false));
                }
            });
            ui.horizontal_wrapped(|ui| {
                if ui.button("Back up settings…").on_hover_text("Settings and your presets in one file").clicked() {
                    self.backup_settings();
                }
                if ui.button("Restore from backup…").clicked() {
                    self.restore_settings();
                }
            });
            if let Some((text, is_error)) = &self.help_message {
                ui.colored_label(if *is_error { RED } else { GREEN }, text);
            }
        });
    }

    fn backup_settings(&mut self) {
        let Some(path) = rfd::FileDialog::new()
            .set_title("Back up settings")
            .add_filter("Voice Changer backup", &["json"])
            .set_file_name("VoiceChanger-backup.json")
            .save_file()
        else {
            return;
        };
        self.help_message = Some(match Backup::capture(&self.cfg, &self.presets).save(&path) {
            Ok(()) => (format!("Backed up to {}.", path.display()), false),
            Err(e) => (e, true),
        });
    }

    fn restore_settings(&mut self) {
        let Some(path) = rfd::FileDialog::new()
            .set_title("Restore from backup")
            .add_filter("Voice Changer backup", &["json"])
            .pick_file()
        else {
            return;
        };
        let result = Backup::load(&path).and_then(|b| {
            let n = b.restore_presets(&mut self.presets)?;
            self.apply_config(b.config);
            Ok(n)
        });
        self.help_message = Some(match result {
            Ok(n) => (format!("Settings restored, {n} preset(s) added."), false),
            Err(e) => (e, true),
        });
    }

    /// Replace the whole configuration and push every part of it to the running engine.
    pub(super) fn apply_config(&mut self, new: Config) {
        let old = std::mem::replace(&mut self.cfg, new);
        let sh = self.engine.shared.clone();
        sh.input_gain.store(voice_changer::dsp::db_to_gain(self.cfg.input_gain_db));
        sh.output_gain.store(voice_changer::dsp::db_to_gain(self.cfg.output_gain_db));
        sh.input_channel.store(self.cfg.input_channel.map_or(-1, |c| c as i32), Relaxed);
        sh.set_margin(self.cfg.latency.margin_seconds());
        sh.fx.store(&self.cfg.fx);
        if self.cfg.fx.order != old.fx.order {
            self.engine.send(voice_changer::audio::Command::SetChainOrder(self.cfg.fx.order.clone()));
        }
        if self.cfg.cable != old.cable {
            self.engine.send(voice_changer::audio::Command::SetCable(self.cfg.cable.clone()));
        }
        self.send_monitor();
        if self.cfg.input != old.input {
            self.restart_if_active();
        }
        if let Some(h) = &self.system.hotkeys {
            h.set_config(self.cfg.hotkeys.clone());
        }
        super::widgets::apply_theme(&self.ctx, self.cfg.theme);
        self.mark_dirty();
    }

    /// Plain-text report for bug reports. Contains device names and settings, nothing personal.
    fn diagnostics(&self) -> String {
        use std::fmt::Write;
        let mut r = String::new();
        let st = &self.status;
        let sh = &self.engine.shared;
        let _ = writeln!(r, "Voice Changer {} ({})", env!("CARGO_PKG_VERSION"), simd::level().label());
        let _ = writeln!(
            r,
            "Engine: {:?}; mic \"{}\" @ {} Hz; block {}",
            st.state,
            st.input_name,
            st.sample_rate,
            sh.in_block.load(Relaxed)
        );
        for (label, info, stats) in [("Virtual mic", &st.cable, &sh.cable), ("Monitor", &st.monitor, &sh.monitor)] {
            match info {
                Some(i) => {
                    let _ = writeln!(
                        r,
                        "{label}: \"{}\" @ {} Hz, buffer {} frames, lost {}, fill {:.1}/{:.1} ms, margin {:.0} ms, underruns {}",
                        i.name,
                        i.sample_rate,
                        i.buffer_frames,
                        i.lost,
                        stats.fill_ms.load(),
                        stats.target_ms.load(),
                        stats.margin_ms.load(),
                        stats.underruns.load(Relaxed)
                    );
                }
                None => {
                    let _ = writeln!(r, "{label}: off");
                }
            }
        }
        let _ = writeln!(
            r,
            "Latency mode {:?}; effects add {} samples; capture xruns {}; bypass {}; mute {}",
            self.cfg.latency,
            sh.dsp_latency.load(Relaxed),
            sh.capture_xruns.load(Relaxed),
            sh.bypass.load(Relaxed),
            sh.mute.load(Relaxed)
        );
        if let Some(w) = &st.warning {
            let _ = writeln!(r, "Warning: {w}");
        }
        let _ = writeln!(r, "Preset: {:?}", self.cfg.preset);
        let order: Vec<&str> = self.cfg.fx.order.iter().map(|k| k.key()).collect();
        let _ = writeln!(r, "Effect order: {}", order.join(" > "));
        for kind in EffectKind::ALL.into_iter().filter(|k| self.cfg.fx.enabled(*k)) {
            let params: Vec<String> =
                kind.spec().params.iter().map(|p| format!("{}={}", p.key, self.cfg.fx.get(kind, p.key))).collect();
            let _ = writeln!(r, "  {} (mix {:.2}): {}", kind.key(), self.cfg.fx.mix(kind), params.join(", "));
        }
        let keys: Vec<String> = self.cfg.hotkeys.bindings.iter().map(|(a, k)| format!("{a:?}={}", k.label())).collect();
        let _ = writeln!(r, "Hotkeys ({}): {}", if self.cfg.hotkeys.enabled { "on" } else { "off" }, keys.join(", "));
        let _ = writeln!(
            r,
            "Devices: {} inputs, {} outputs, cable installed: {}",
            st.devices.inputs.len(),
            st.devices.outputs.len(),
            st.devices.cables().next().is_some()
        );
        let _ = writeln!(r, "--- last {LOG_LINES} log lines ---");
        if let Ok(log) = std::fs::read_to_string(config::app_dir().join("voicechanger.log")) {
            let lines: Vec<&str> = log.lines().collect();
            for line in &lines[lines.len().saturating_sub(LOG_LINES)..] {
                let _ = writeln!(r, "{line}");
            }
        }
        r
    }
}
