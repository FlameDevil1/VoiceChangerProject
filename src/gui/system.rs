//! Hotkeys, tray and toasts: everything that works while the window is hidden.
//!
//! Hotkey actions that only flip audio state (effects on/off, hold, panic, mute) are applied on
//! the hook thread directly through the shared atomics, so they take effect instantly even if the
//! UI is busy. Everything else arrives here as an event and is handled in `App::logic`, which
//! eframe runs on every wake-up, visible window or not.

use super::App;
use super::tray::{Tray, TrayCommand};
use super::widgets::{AMBER, section};
use crate::toast::Toaster;
use eframe::egui::{self, RichText};
use std::sync::Arc;
use std::sync::atomic::Ordering::Relaxed;
use std::sync::mpsc::{self, Receiver};
use voice_changer::audio::Shared;
use voice_changer::dsp::EffectKind;
use voice_changer::hotkeys::{Action, Event, HotkeyService};

/// Audio-state part of a hotkey, applied immediately on the thread that saw the key.
fn apply_audio_action(shared: &Shared, e: Event) {
    match e {
        Event::Pressed(Action::ToggleEffects) => {
            shared.bypass.fetch_xor(true, Relaxed);
        }
        Event::Pressed(Action::HoldEffects) => shared.bypass.store(false, Relaxed),
        Event::Released(Action::HoldEffects) => shared.bypass.store(true, Relaxed),
        Event::Pressed(Action::NormalVoice) => shared.bypass.store(true, Relaxed),
        Event::Pressed(Action::ToggleMute) => {
            shared.mute.fetch_xor(true, Relaxed);
        }
        Event::Pressed(Action::GlitchBurst) => {
            shared.fx.get(EffectKind::Network).trigger.fetch_add(1, Relaxed);
        }
        _ => {}
    }
}

pub struct System {
    pub tray: Option<Tray>,
    pub toaster: Option<Toaster>,
    pub hotkeys: Option<HotkeyService>,
    events: Receiver<Event>,
    /// Action whose key combination is being recorded in the settings panel.
    pub capturing: Option<Action>,
    pub quitting: bool,
    /// "Start with Windows" (read from the registry on start and when the window gets focus).
    pub autostart: bool,
    /// Apps running as administrator that came to the front (hotkeys can't reach them).
    admin_apps: Receiver<String>,
    pub admin_seen: Vec<String>,
}

impl System {
    pub fn start(ctx: &egui::Context, shared: Arc<Shared>, config: voice_changer::hotkeys::HotkeyConfig) -> Self {
        let (admin_tx, admin_apps) = mpsc::channel();
        let wake = ctx.clone();
        voice_changer::elevation::watch(move |name| {
            let _ = admin_tx.send(name);
            wake.request_repaint();
        });
        let (tx, events) = mpsc::channel();
        let wake = ctx.clone();
        let hotkeys = HotkeyService::start(config, move |e| {
            // Instant audio-state changes, right here on the hook thread.
            apply_audio_action(&shared, e);
            let _ = tx.send(e);
            wake.request_repaint();
        });
        Self {
            tray: Tray::new(ctx),
            toaster: Toaster::start(),
            hotkeys,
            events,
            capturing: None,
            quitting: false,
            autostart: voice_changer::autostart::is_enabled(),
            admin_apps,
            admin_seen: Vec::new(),
        }
    }
}

impl App {
    pub(super) fn toast(&self, text: &str) {
        if let Some(t) = &self.system.toaster {
            t.show(text);
        }
    }

    fn effects_toast(&self) {
        let sh = self.shared();
        let text = if sh.mute.load(Relaxed) {
            "Virtual mic muted".to_string()
        } else if sh.bypass.load(Relaxed) {
            "Normal voice (effects off)".to_string()
        } else {
            format!("Voice: {}", self.cfg.preset.as_deref().unwrap_or("custom"))
        };
        self.toast(&text);
    }

    fn switch_preset(&mut self, name: &str) {
        self.load_preset(name);
        // Picking a voice from a hotkey or the tray means you want to hear it.
        self.shared().bypass.store(false, Relaxed);
        self.toast(&format!("Voice: {name}"));
    }

    /// Hotkey and tray events, tray state and config saving. Runs even while hidden.
    pub(super) fn background(&mut self, ctx: &egui::Context) {
        let events: Vec<Event> = self.system.events.try_iter().collect();
        for e in events {
            log::info!("hotkey: {e:?}");
            match e {
                Event::Pressed(Action::ToggleEffects | Action::NormalVoice | Action::ToggleMute) => {
                    self.effects_toast()
                }
                Event::Pressed(action @ (Action::NextPreset | Action::PrevPreset)) => {
                    let forward = action == Action::NextPreset;
                    if let Some(name) = self.presets.neighbour(self.cfg.preset.as_deref(), forward).map(str::to_string)
                    {
                        self.switch_preset(&name);
                    }
                }
                Event::Pressed(Action::ShowWindow) => super::bring_to_front(ctx),
                Event::Pressed(Action::GlitchBurst) => self.glitch_burst(),
                Event::Captured(key) => self.finish_capture(key),
                Event::CaptureCancelled => {
                    self.system.capturing = None;
                    if let Some(h) = &self.system.hotkeys {
                        h.capture(false);
                    }
                }
                _ => {}
            }
        }

        // Once per app and session: hotkeys can't reach an app running as administrator.
        let admin: Vec<String> = self.system.admin_apps.try_iter().collect();
        for name in admin {
            if self.system.admin_seen.contains(&name) {
                continue;
            }
            log::info!("{name} runs as administrator; hotkeys can't reach it");
            if self.cfg.hotkeys.enabled && !self.cfg.hotkeys.bindings.is_empty() {
                self.toast(&format!("Hotkeys don't work in {name} (it runs as administrator)"));
            }
            self.system.admin_seen.push(name);
        }

        let commands = self.system.tray.as_ref().map(Tray::poll).unwrap_or_default();
        for cmd in commands {
            log::info!("tray: {}", cmd.describe());
            match cmd {
                TrayCommand::Show => super::bring_to_front(ctx),
                TrayCommand::ToggleEffects => {
                    self.shared().bypass.fetch_xor(true, Relaxed);
                    self.effects_toast();
                }
                TrayCommand::ToggleMute => {
                    self.shared().mute.fetch_xor(true, Relaxed);
                    self.effects_toast();
                }
                TrayCommand::Preset(name) => self.switch_preset(&name),
                TrayCommand::Scenario(name) => {
                    self.set_scenario(name.as_deref());
                    self.toast(&format!("Bad mic & connection: {}", name.as_deref().unwrap_or("off")));
                }
                TrayCommand::Quit => {
                    self.system.quitting = true;
                    ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                }
            }
        }

        let names: Vec<String> = self.presets.entries().iter().map(|e| e.preset.name.clone()).collect();
        let (effects_on, muted) = (!self.shared().bypass.load(Relaxed), self.shared().mute.load(Relaxed));
        let current = self.cfg.preset.clone();
        let scenario = self.scenario_state();
        if let Some(tray) = &mut self.system.tray {
            tray.sync(&names, current.as_deref(), effects_on, muted, scenario);
        }
        self.end_glitch_only(ctx);

        self.save_if_due(ctx);
    }

    /// Closing the window hides it to the tray (if enabled) instead of quitting.
    pub(super) fn intercept_close(&mut self, ctx: &egui::Context) {
        let close = ctx.input(|i| i.viewport().close_requested());
        if close && self.cfg.close_to_tray && !self.system.quitting && self.system.tray.is_some() {
            ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
            ctx.send_viewport_cmd(egui::ViewportCommand::Visible(false));
            if !self.cfg.tray_hint_shown {
                self.toast("Still running in the tray. Right-click its icon to quit.");
                self.cfg.tray_hint_shown = true;
                self.mark_dirty();
            }
        }
    }

    fn finish_capture(&mut self, key: Option<voice_changer::hotkeys::Hotkey>) {
        let Some(action) = self.system.capturing.take() else { return };
        if let Some(h) = &self.system.hotkeys {
            h.capture(false);
        }
        match key {
            Some(k) => {
                if let Some(other) = self.cfg.hotkeys.conflict(&k, action) {
                    self.cfg.hotkeys.bindings.remove(&other);
                    self.toast(&format!("{} moved from \"{}\"", k.label(), other.label()));
                }
                self.cfg.hotkeys.bindings.insert(action, k);
                if action == Action::HoldEffects {
                    // Push-to-talk semantics start now: normal voice unless the key is held.
                    self.shared().bypass.store(true, Relaxed);
                    self.toast(&format!("Hold {} for the changed voice", k.label()));
                }
            }
            None => {
                self.cfg.hotkeys.bindings.remove(&action);
            }
        }
        self.apply_hotkeys();
    }

    fn apply_hotkeys(&mut self) {
        if let Some(h) = &self.system.hotkeys {
            h.set_config(self.cfg.hotkeys.clone());
        }
        self.mark_dirty();
    }

    /// Advanced mode: key bindings, tray behaviour and starting with Windows.
    pub(super) fn system_section(&mut self, ui: &mut egui::Ui) {
        section(ui, "Hotkeys, tray & startup", |ui| {
            if self.system.hotkeys.is_none() {
                ui.colored_label(AMBER, "Global hotkeys are unavailable (the keyboard hook could not be installed).");
            }
            if ui
                .checkbox(&mut self.cfg.hotkeys.enabled, "Global hotkeys")
                .on_hover_text("Work while a game or other app is focused.")
                .changed()
            {
                self.apply_hotkeys();
            }
            egui::Grid::new("hotkeys").num_columns(3).spacing([8.0, 4.0]).show(ui, |ui| {
                for action in Action::ALL {
                    ui.label(action.label()).on_hover_text(action.help());
                    let capturing = self.system.capturing == Some(action);
                    let text = if capturing {
                        "Press keys…".to_string()
                    } else {
                        self.cfg.hotkeys.bindings.get(&action).map_or_else(|| "Not set".to_string(), |k| k.label())
                    };
                    let button = egui::Button::new(text).selected(capturing).min_size(egui::vec2(150.0, 0.0));
                    if ui
                        .add(button)
                        .on_hover_text("Click, then press a key combination or a mouse side/middle button. Esc cancels, Backspace clears.")
                        .clicked()
                    {
                        self.system.capturing = Some(action);
                        if let Some(h) = &self.system.hotkeys {
                            h.capture(true);
                        }
                    }
                    let has = self.cfg.hotkeys.bindings.contains_key(&action);
                    if ui.add_enabled(has, egui::Button::new("✖").small()).on_hover_text("Remove").clicked() {
                        self.cfg.hotkeys.bindings.remove(&action);
                        self.apply_hotkeys();
                    }
                    ui.end_row();
                }
            });
            ui.horizontal(|ui| {
                if ui.small_button("Restore default keys").clicked() {
                    let enabled = self.cfg.hotkeys.enabled;
                    self.cfg.hotkeys = voice_changer::hotkeys::HotkeyConfig { enabled, ..Default::default() };
                    self.apply_hotkeys();
                }
            });
            if self.system.admin_seen.is_empty() {
                ui.label(
                    RichText::new("Hotkeys don't work while an app running as administrator is focused.")
                        .small()
                        .weak(),
                );
            } else {
                ui.colored_label(
                    AMBER,
                    format!(
                        "Hotkeys can't reach {}: running as administrator. To use them there, run Voice Changer \
                         as administrator too (dropping files onto its window won't work then).",
                        self.system.admin_seen.join(", ")
                    ),
                );
            }
            ui.separator();
            if ui.checkbox(&mut self.cfg.close_to_tray, "Keep running in the tray when the window is closed").changed()
            {
                self.mark_dirty();
            }
            if ui.checkbox(&mut self.cfg.start_minimized, "Start hidden in the tray").changed() {
                self.mark_dirty();
            }
            let mut autostart = self.system.autostart;
            if ui
                .checkbox(&mut autostart, "Start with Windows")
                .on_hover_text("Starts hidden in the tray when you sign in, and turns the voice on if it was on when you last closed the app.")
                .changed()
            {
                match voice_changer::autostart::set(autostart) {
                    Ok(()) => self.system.autostart = autostart,
                    Err(e) => {
                        log::warn!("start with Windows: {e}");
                        self.toast("Couldn't change \"Start with Windows\"");
                    }
                }
            }
            if self.system.tray.is_none() {
                ui.colored_label(AMBER, "The tray icon could not be created.");
            }
        });
    }
}
