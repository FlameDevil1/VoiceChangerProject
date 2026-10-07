//! Main window. Kept deliberately light:
//! - redraws only on input, status changes, or at 30 fps while audio is running and visible;
//! - all device work happens on the engine's controller thread, never here;
//! - meters read lock-free atomics.
//!
//! Two layouts: **Simple** (devices, levels, preset buttons, the common toggles) and
//! **Advanced** (every control, effect order, preset management).

mod calibrate_ui;
mod effects;
mod help;
pub mod icon;
mod presets;
mod spectrum;
mod system;
mod test_voice;
mod tray;
mod widgets;

use eframe::egui::{self, RichText};
use std::sync::Arc;
use std::sync::atomic::Ordering::Relaxed;
use std::time::{Duration, Instant};
use voice_changer::audio::engine::OutputInfo;
use voice_changer::audio::{Command, EngineHandle, EngineSettings, EngineState, Shared, Status, devices};
use voice_changer::config::{Config, LatencyMode, ThemePref, UiMode};
use voice_changer::dsp::{EffectKind, FxSettings, db_to_gain, simd};
use voice_changer::presets::PresetStore;
use widgets::{AMBER, GREEN, Meter, RED, apply_theme, channel_label, device_combo, gain_row, section, status_dot};

/// Meter redraw rates (focused, background). Measured ~0.45 % of one core per fps on a laptop
/// iGPU, so the background rate matters most: the app usually sits behind a game or Discord.
const FPS_NORMAL: (u64, u64) = (30, 10);
const FPS_LOW_POWER: (u64, u64) = (12, 5);
const DEVICE_REFRESH: Duration = Duration::from_secs(5);
const VB_CABLE_URL: &str = "https://vb-audio.com/Cable/";

pub struct App {
    engine: EngineHandle,
    cfg: Config,
    dirty_since: Option<Instant>,
    status: Status,
    meters: [Meter; 3],
    load: f32,
    last_refresh: Instant,
    was_focused: bool,
    auto_start_pending: bool,
    last_meter_update: Instant,
    presets: PresetStore,
    preset_ui: presets::PresetUi,
    /// Tray, hotkeys and toasts.
    system: system::System,
    /// Result of the last Help & diagnostics action: (text, is_error).
    help_message: Option<(String, bool)>,
    ctx: egui::Context,
    spectrum: spectrum::Spectrum,
    test: test_voice::TestVoice,
    calibration: calibrate_ui::CalibrationUi,
    /// Windows privacy settings block the microphone (checked on start and focus).
    mic_blocked: bool,
    /// How long the running mic has delivered exact digital silence.
    silent_for: f32,
    /// Newer release found by the startup check (filled in by a background thread).
    update: Arc<std::sync::Mutex<Option<voice_changer::updates::Update>>>,
    update_dismissed: bool,
    /// Keeps this process the single running instance.
    _instance: crate::single_instance::Guard,
}

/// Show, restore and focus the main window (another launch, tray click or hotkey asked for it).
pub fn bring_to_front(ctx: &egui::Context) {
    ctx.send_viewport_cmd(egui::ViewportCommand::Visible(true));
    ctx.send_viewport_cmd(egui::ViewportCommand::Minimized(false));
    ctx.send_viewport_cmd(egui::ViewportCommand::Focus);
    ctx.request_repaint();
}

impl App {
    pub fn new(cc: &eframe::CreationContext<'_>, cfg: Config, instance: crate::single_instance::Guard) -> Self {
        let shared = Arc::new(Shared::default());
        shared.input_gain.store(db_to_gain(cfg.input_gain_db));
        shared.output_gain.store(db_to_gain(cfg.output_gain_db));
        shared.monitor_enabled.store(cfg.monitor_enabled, Relaxed);
        shared.input_channel.store(cfg.input_channel.map_or(-1, |c| c as i32), Relaxed);
        shared.set_margin(cfg.latency.margin_seconds());
        shared.fx.store(&cfg.fx);

        let ctx = cc.egui_ctx.clone();
        let system = system::System::start(&cc.egui_ctx, shared.clone(), cfg.hotkeys.clone());
        let engine = EngineHandle::spawn(shared, Box::new(move || ctx.request_repaint()));
        if cfg.start_minimized && system.tray.is_none() {
            // Hidden with no tray icon would leave no way back in.
            bring_to_front(&cc.egui_ctx);
        }
        apply_theme(&cc.egui_ctx, cfg.theme);
        cc.egui_ctx.set_zoom_factor(cfg.ui_scale.clamp(0.5, 3.0));
        log::info!("DSP SIMD level: {}", simd::level().label());

        let auto_start_pending = cfg.was_running;
        let check_updates = cfg.check_updates;
        let app = Self {
            engine,
            cfg,
            dirty_since: None,
            status: Status::default(),
            meters: Default::default(),
            load: 0.0,
            last_refresh: Instant::now(),
            was_focused: true,
            auto_start_pending,
            last_meter_update: Instant::now(),
            presets: PresetStore::load(&PresetStore::default_dir()),
            preset_ui: Default::default(),
            system,
            help_message: None,
            ctx: cc.egui_ctx.clone(),
            spectrum: Default::default(),
            test: Default::default(),
            calibration: Default::default(),
            mic_blocked: voice_changer::audio::privacy::microphone_blocked(),
            silent_for: 0.0,
            update: Arc::default(),
            update_dismissed: false,
            _instance: instance,
        };
        if check_updates {
            app.start_update_check(cc.egui_ctx.clone());
        }
        app
    }

    /// Background update check; the result shows as a banner under the Start button.
    fn start_update_check(&self, ctx: egui::Context) {
        let slot = self.update.clone();
        let _ =
            std::thread::Builder::new().name("update-check".into()).spawn(move || match voice_changer::updates::check(
                env!("CARGO_PKG_VERSION"),
            ) {
                Ok(Some(u)) => {
                    log::info!("update available: {}", u.version);
                    if let Ok(mut s) = slot.lock() {
                        *s = Some(u);
                    }
                    ctx.request_repaint();
                }
                Ok(None) => log::info!("update check: up to date"),
                Err(e) => log::info!("update check skipped: {e}"),
            });
    }

    fn shared(&self) -> &Shared {
        &self.engine.shared
    }

    fn settings(&self) -> EngineSettings {
        EngineSettings {
            input: self.cfg.input.clone(),
            cable: self.cfg.cable.clone(),
            monitor: self.cfg.monitor.clone(),
            monitor_enabled: self.cfg.monitor_enabled,
            chain_order: self.cfg.fx.order.clone(),
        }
    }

    fn is_active(&self) -> bool {
        matches!(self.status.state, EngineState::Running | EngineState::Reconnecting)
    }

    fn mark_dirty(&mut self) {
        self.dirty_since.get_or_insert_with(Instant::now);
    }

    /// Replace all effect settings (preset load, reorder, edits): publish to the audio thread,
    /// rebuild the chain if the order changed (crossfaded, no restart), and save later.
    fn set_fx(&mut self, fx: FxSettings) {
        if fx.order != self.cfg.fx.order {
            self.engine.send(Command::SetChainOrder(fx.order.clone()));
        }
        self.shared().fx.store(&fx);
        self.cfg.fx = fx;
        self.mark_dirty();
    }

    fn start(&mut self) {
        self.engine.send(Command::Start(self.settings()));
    }

    /// Restart only if running (e.g. the microphone changed).
    fn restart_if_active(&mut self) {
        if self.is_active() {
            self.start();
        }
    }

    fn send_monitor(&self) {
        self.shared().monitor_enabled.store(self.cfg.monitor_enabled, Relaxed);
        self.engine.send(Command::SetMonitor { device: self.cfg.monitor.clone(), enabled: self.cfg.monitor_enabled });
    }

    /// Debounced config save (also runs while the window is hidden).
    fn save_if_due(&mut self, ctx: &egui::Context) {
        if let Some(t) = self.dirty_since {
            if t.elapsed() > Duration::from_secs(1) {
                self.cfg.save();
                self.dirty_since = None;
            } else {
                ctx.request_repaint_after(Duration::from_millis(1100));
            }
        }
    }

    /// Background bookkeeping done once per frame.
    fn tick(&mut self, ctx: &egui::Context) {
        self.status = self.engine.status();

        // Keyboard zoom (Ctrl+= / Ctrl+-) is built into egui; remember whatever size is in use.
        let zoom = ctx.zoom_factor();
        if (zoom - self.cfg.ui_scale).abs() > 1e-3 {
            self.cfg.ui_scale = zoom;
            self.mark_dirty();
        }

        // First run: pick the virtual cable automatically if one is installed.
        if !self.cfg.cable_choice_made {
            let first_cable = self.status.devices.cables().next().cloned();
            if let Some(cable) = first_cable {
                self.cfg.cable = Some(cable.to_ref());
                self.cfg.cable_choice_made = true;
                self.engine.send(Command::SetCable(self.cfg.cable.clone()));
                self.mark_dirty();
            }
        }
        if self.auto_start_pending && !self.status.devices.inputs.is_empty() {
            self.auto_start_pending = false;
            self.start();
        }

        // Refresh the device list when the window regains focus, and periodically while focused,
        // so newly plugged devices show up without polling in the background.
        let focused = ctx.input(|i| i.focused);
        if focused && (!self.was_focused || self.last_refresh.elapsed() > DEVICE_REFRESH) {
            self.last_refresh = Instant::now();
            self.engine.send(Command::RefreshDevices);
            self.mic_blocked = voice_changer::audio::privacy::microphone_blocked();
        }
        self.was_focused = focused;

        // eframe skips this function entirely while minimized or fully covered, so meters cost
        // nothing then. In the background (e.g. next to a game) a lower rate is plenty.
        if self.is_active() {
            let dt = self.last_meter_update.elapsed().as_secs_f32().min(0.25);
            self.last_meter_update = Instant::now();
            let sh = &self.engine.shared;
            let in_peak = sh.in_peak.take();
            // Real microphones always have some noise; exact zeros mean muted or blocked.
            self.silent_for = if in_peak == 0.0 { self.silent_for + dt } else { 0.0 };
            self.meters[0].update(in_peak, dt);
            self.meters[1].update(sh.out_peak.take(), dt);
            self.meters[2].update(sh.monitor.peak.take(), dt);
            let load = sh.load.take();
            self.load = load.max(self.load * 0.9);
            sh.scope.enabled.store(self.cfg.show_spectrum, Relaxed);
            if self.cfg.show_spectrum {
                self.spectrum.update(&sh.scope, self.status.sample_rate, dt);
            }
            let (fps_focused, fps_background) = if self.cfg.low_power_ui { FPS_LOW_POWER } else { FPS_NORMAL };
            let fps = if focused { fps_focused } else { fps_background };
            ctx.request_repaint_after(Duration::from_millis(1000 / fps));
        } else {
            self.meters = Default::default();
            self.load = 0.0;
            self.spectrum.reset();
            self.engine.shared.scope.enabled.store(false, Relaxed);
        }
        self.test_tick(ctx);
        self.calibration_tick(ctx);
    }

    // ---- sections ------------------------------------------------------------------------

    fn header(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            ui.heading("Voice Changer");
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let (icon, next) = match self.cfg.theme {
                    ThemePref::System => ("🖥", ThemePref::Dark),
                    ThemePref::Dark => ("🌙", ThemePref::Light),
                    ThemePref::Light => ("☀", ThemePref::System),
                };
                if ui.button(icon).on_hover_text("Theme: system / dark / light").clicked() {
                    self.cfg.theme = next;
                    apply_theme(ui.ctx(), next);
                    self.mark_dirty();
                }
                let mut mode = self.cfg.ui_mode;
                ui.selectable_value(&mut mode, UiMode::Advanced, "Advanced").on_hover_text("Every control");
                ui.selectable_value(&mut mode, UiMode::Simple, "Simple").on_hover_text("Presets and essentials");
                if mode != self.cfg.ui_mode {
                    self.cfg.ui_mode = mode;
                    self.mark_dirty();
                }
            });
        });

        let active = self.is_active();
        ui.horizontal(|ui| {
            let (color, text) = match &self.status.state {
                EngineState::Running => (GREEN, "Running"),
                EngineState::Reconnecting => (AMBER, "Reconnecting…"),
                EngineState::Error(_) => (RED, "Error"),
                EngineState::Stopped => (ui.visuals().weak_text_color(), "Stopped"),
            };
            status_dot(ui, color);
            ui.label(RichText::new(text).color(color));
        });
        let label = if active { "■  Stop" } else { "▶  Start" };
        let button =
            egui::Button::new(RichText::new(label).size(18.0)).min_size(egui::vec2(ui.available_width(), 36.0));
        if ui.add(button).clicked() {
            if active {
                self.engine.send(Command::Stop);
            } else {
                self.start();
            }
            self.cfg.was_running = !active;
            self.mark_dirty();
        }
        if let EngineState::Error(e) = &self.status.state {
            ui.colored_label(RED, e);
        }
        if let Some(w) = &self.status.warning {
            ui.colored_label(AMBER, format!("⚠ {w}"));
        }
        let update = self.update.lock().ok().and_then(|u| u.clone());
        if let (Some(u), false) = (update, self.update_dismissed) {
            ui.horizontal_wrapped(|ui| {
                ui.label(RichText::new(format!("Version {} is available.", u.version)).color(GREEN));
                ui.hyperlink_to("Download", &u.url);
                if ui.small_button("Later").clicked() {
                    self.update_dismissed = true;
                }
            });
        }
    }

    fn devices_section(&mut self, ui: &mut egui::Ui, advanced: bool) {
        let list = self.status.devices.clone();
        section(ui, "Devices", |ui| {
            egui::Grid::new("devices").num_columns(2).spacing([8.0, 6.0]).show(ui, |ui| {
                ui.label("Microphone");
                let default_name = list.default_input.clone().unwrap_or_default();
                let inputs: Vec<(String, bool)> =
                    list.inputs.iter().map(|d| (d.name.clone(), devices::is_cable_capture(&d.name))).collect();
                if device_combo(ui, "mic", &mut self.cfg.input, &list.inputs, &inputs, Some(&default_name)) {
                    self.mark_dirty();
                    self.restart_if_active();
                }
                ui.end_row();

                if advanced {
                    ui.label("Mic channel");
                    let mut ch = self.cfg.input_channel;
                    egui::ComboBox::from_id_salt("chan")
                        .selected_text(channel_label(ch))
                        .show_ui(ui, |ui| {
                            for opt in [None, Some(0), Some(1)] {
                                ui.selectable_value(&mut ch, opt, channel_label(opt));
                            }
                        })
                        .response
                        .on_hover_text("Use \"Left only\" if your interface puts the mic on one channel.");
                    if ch != self.cfg.input_channel {
                        self.cfg.input_channel = ch;
                        self.shared().input_channel.store(ch.map_or(-1, |c| c as i32), Relaxed);
                        self.mark_dirty();
                    }
                    ui.end_row();
                }

                ui.label("Virtual mic");
                let outputs: Vec<(String, bool)> = list.outputs.iter().map(|d| (d.name.clone(), false)).collect();
                if device_combo(ui, "cable", &mut self.cfg.cable, &list.outputs, &outputs, None) {
                    self.cfg.cable_choice_made = true;
                    self.engine.send(Command::SetCable(self.cfg.cable.clone()));
                    self.mark_dirty();
                }
                ui.end_row();

                let mut enabled = self.cfg.monitor_enabled;
                ui.checkbox(&mut enabled, "Hear myself").on_hover_text("Play the processed voice to your headphones.");
                let default_out = list.default_output.clone().unwrap_or_default();
                let dev_changed =
                    device_combo(ui, "monitor", &mut self.cfg.monitor, &list.outputs, &outputs, Some(&default_out));
                if enabled != self.cfg.monitor_enabled || dev_changed {
                    self.cfg.monitor_enabled = enabled;
                    self.send_monitor();
                    self.mark_dirty();
                }
                ui.end_row();
            });

            let listening = match &self.cfg.monitor {
                None => list.default_output_listening,
                Some(m) => list
                    .outputs
                    .iter()
                    .find(|d| d.id == m.id || d.name == m.name)
                    .map_or(devices::Listening::Unknown, |d| d.listening),
            };
            if self.cfg.monitor_enabled {
                match listening {
                    devices::Listening::OutLoud => {
                        ui.colored_label(
                            AMBER,
                            "⚠ \"Hear myself\" is playing through speakers. Your mic will pick it up and may howl; use headphones.",
                        );
                    }
                    devices::Listening::Unknown => {
                        ui.label(
                            RichText::new("Use headphones for \"Hear myself\": if this device plays out loud, your mic will pick it up.")
                                .small()
                                .weak(),
                        );
                    }
                    devices::Listening::Ears => {}
                }
            }

            self.calibration_row(ui);
            self.mic_warning(ui);
            self.cable_indicator(ui);
        });
    }

    /// Microphone blocked by Windows privacy settings, or delivering pure silence.
    fn mic_warning(&mut self, ui: &mut egui::Ui) {
        let silent = self.is_active() && self.silent_for > 3.0;
        if !self.mic_blocked && !silent {
            return;
        }
        let text = if self.mic_blocked {
            "⚠ Windows is blocking microphone access for desktop apps, so this app only hears silence."
        } else {
            "⚠ Your microphone is sending complete silence. It may be muted (check its switch or Windows Sound settings), or blocked in Windows privacy settings."
        };
        ui.colored_label(AMBER, text);
        if ui.button("Open microphone privacy settings").clicked() {
            voice_changer::audio::privacy::open_privacy_settings();
        }
    }

    /// Shows which device is being fed, or a setup helper when no virtual cable is installed.
    fn cable_indicator(&mut self, ui: &mut egui::Ui) {
        let cable_installed = self.status.devices.cables().next().is_some();
        ui.add_space(4.0);
        match (&self.cfg.cable, &self.status.cable) {
            (Some(_), Some(OutputInfo { name, lost: false, .. })) if self.is_active() => {
                ui.horizontal_wrapped(|ui| {
                    status_dot(ui, GREEN);
                    ui.label(format!("Feeding {name}"));
                });
                if devices::is_cable_playback(name) {
                    let capture = name.replacen("Input", "Output", 1);
                    ui.label(
                        RichText::new(format!(
                            "In Discord, games or OBS, choose \"{capture}\" as the microphone, and turn off their own noise suppression for it."
                        ))
                        .small()
                        .weak(),
                    );
                }
            }
            (Some(d), Some(OutputInfo { lost: true, .. })) => {
                ui.colored_label(AMBER, format!("⚠ {} is unavailable; retrying…", d.name));
            }
            (Some(d), _) => {
                ui.label(RichText::new(format!("Will feed {} when started.", d.name)).weak());
            }
            (None, _) => {
                ui.label(RichText::new("Virtual mic is off; other apps won't hear the changed voice.").weak());
            }
        }

        if !cable_installed {
            egui::CollapsingHeader::new(RichText::new("⚠ No virtual cable found: set up").strong())
                .default_open(true)
                .show(ui, |ui| {
                    ui.label("Other apps need a virtual microphone to hear the changed voice. This app uses the free VB-CABLE driver:");
                    ui.label("1. Download VB-CABLE and extract the zip.");
                    ui.label("2. Right-click VBCABLE_Setup_x64.exe, choose \"Run as administrator\", then click \"Install Driver\".");
                    ui.label("3. Restart Windows if asked, then press Rescan.");
                    ui.label("4. In Discord/OBS/games, select \"CABLE Output\" as your microphone.");
                    ui.horizontal(|ui| {
                        ui.hyperlink_to("Get VB-CABLE (vb-audio.com)", VB_CABLE_URL);
                        if ui.button("Rescan").clicked() {
                            self.engine.send(Command::RefreshDevices);
                        }
                    });
                });
        }
    }

    fn levels_section(&mut self, ui: &mut egui::Ui) {
        section(ui, "Levels", |ui| {
            egui::Grid::new("meters").num_columns(2).spacing([8.0, 6.0]).show(ui, |ui| {
                ui.label("Mic");
                self.meters[0].show(ui);
                ui.end_row();
                ui.label("Virtual mic");
                self.meters[1].show(ui);
                ui.end_row();
                if self.cfg.monitor_enabled {
                    ui.label("Headphones");
                    self.meters[2].show(ui);
                    ui.end_row();
                }
            });
            if self.cfg.show_spectrum && self.is_active() {
                self.spectrum.show(ui);
                ui.label(RichText::new("Bars: your voice after effects · lines: before").small().weak());
            }
            if ui.checkbox(&mut self.cfg.show_spectrum, "Spectrum").on_hover_text("Live frequency display").changed() {
                self.mark_dirty();
            }
        });
    }

    /// Bypass / mute buttons, plus (Simple mode) quick toggles for the mic cleanup effects.
    fn controls_section(&mut self, ui: &mut egui::Ui, advanced: bool) {
        section(ui, "Controls", |ui| {
            ui.horizontal_wrapped(|ui| {
                let sh = &self.engine.shared;
                let bypass = sh.bypass.load(Relaxed);
                if ui
                    .add(egui::Button::new("Bypass effects").selected(bypass))
                    .on_hover_text("Send your original voice")
                    .clicked()
                {
                    sh.bypass.store(!bypass, Relaxed);
                }
                let mute = sh.mute.load(Relaxed);
                if ui.add(egui::Button::new("Mute virtual mic").selected(mute)).clicked() {
                    sh.mute.store(!mute, Relaxed);
                }
            });
            self.test_voice_row(ui);
            if !advanced {
                let mut fx = self.cfg.fx.clone();
                ui.horizontal_wrapped(|ui| {
                    for kind in [EffectKind::Denoise, EffectKind::Gate] {
                        let mut on = fx.enabled(kind);
                        if ui.checkbox(&mut on, kind.label()).on_hover_text(kind.spec().help).changed() {
                            fx.set_enabled(kind, on);
                        }
                    }
                });
                if fx != self.cfg.fx {
                    self.set_fx(fx);
                }
            }
            ui.add_space(4.0);
            egui::Grid::new("gains").num_columns(3).spacing([8.0, 6.0]).show(ui, |ui| {
                if advanced && gain_row(ui, "Input gain", &mut self.cfg.input_gain_db) {
                    self.engine.shared.input_gain.store(db_to_gain(self.cfg.input_gain_db));
                    self.mark_dirty();
                }
                if gain_row(ui, if advanced { "Output gain" } else { "Volume" }, &mut self.cfg.output_gain_db) {
                    self.engine.shared.output_gain.store(db_to_gain(self.cfg.output_gain_db));
                    self.mark_dirty();
                }
            });
        });
    }

    fn effects_section(&mut self, ui: &mut egui::Ui) {
        let mut fx = self.cfg.fx.clone();
        let shared = self.engine.shared.clone();
        let active = self.is_active();
        section(ui, "Effects", |ui| {
            ui.label(
                RichText::new(
                    "Effects that are off add no delay and use no CPU. The arrows change the processing order.",
                )
                .small()
                .weak(),
            );
            let order = fx.order.clone();
            let n = order.len();
            for (i, kind) in order.into_iter().enumerate() {
                let reorder = Some((i > 0, i + 1 < n));
                if let Some(m) = effects::effect_panel(ui, kind, &mut fx, shared.fx.get(kind), active, reorder) {
                    let j = if m == effects::Move::Up { i - 1 } else { i + 1 };
                    fx.order.swap(i, j);
                }
            }
            if ui.small_button("Reset order").on_hover_text("Back to the recommended processing order").clicked() {
                fx.order = EffectKind::ALL.to_vec();
            }
        });
        if fx != self.cfg.fx {
            self.set_fx(fx);
        }
    }

    fn performance_section(&mut self, ui: &mut egui::Ui) {
        section(ui, "Latency & performance", |ui| {
            ui.horizontal(|ui| {
                ui.label("Latency");
                let mut mode = self.cfg.latency;
                for m in [LatencyMode::Low, LatencyMode::Balanced, LatencyMode::Safe] {
                    ui.selectable_value(&mut mode, m, m.label());
                }
                if mode != self.cfg.latency {
                    self.cfg.latency = mode;
                    self.engine.shared.set_margin(mode.margin_seconds());
                    self.mark_dirty();
                }
            })
            .response
            .on_hover_text("Low = least delay, Safe = most headroom. Grows automatically if audio glitches.");

            ui.horizontal(|ui| {
                ui.label("Interface size");
                let current = self.cfg.ui_scale;
                egui::ComboBox::from_id_salt("ui-scale").selected_text(format!("{:.0} %", current * 100.0)).show_ui(
                    ui,
                    |ui| {
                        for s in [0.8f32, 0.9, 1.0, 1.1, 1.25, 1.5, 1.75, 2.0] {
                            if ui.selectable_label((s - current).abs() < 0.01, format!("{:.0} %", s * 100.0)).clicked()
                            {
                                ui.ctx().set_zoom_factor(s);
                            }
                        }
                    },
                );
                ui.label(RichText::new("or Ctrl+= / Ctrl+- / Ctrl+0").small().weak());
            });

            if ui
                .checkbox(&mut self.cfg.low_power_ui, "Low-power UI")
                .on_hover_text("Refresh meters less often to save CPU. Audio quality is unaffected.")
                .changed()
            {
                self.mark_dirty();
            }

            if !self.is_active() {
                return;
            }
            let sh = &self.engine.shared;
            let rate = self.status.sample_rate.max(1) as f32;
            let in_ms = sh.in_block.load(Relaxed) as f32 / rate * 1000.0;
            let dsp_ms = sh.dsp_latency.load(Relaxed) as f32 / rate * 1000.0;
            let glitches =
                sh.cable.underruns.load(Relaxed) + sh.monitor.underruns.load(Relaxed) + sh.capture_xruns.load(Relaxed);
            let path_ms = |info: &Option<OutputInfo>, fill: f32| {
                info.as_ref()
                    .filter(|i| !i.lost && i.sample_rate > 0)
                    .map(|i| in_ms + dsp_ms + fill + i.buffer_frames as f32 / i.sample_rate as f32 * 1000.0)
            };
            let cable_ms = path_ms(&self.status.cable, sh.cable.fill_ms.load());
            let mon_ms = path_ms(&self.status.monitor, sh.monitor.fill_ms.load());

            ui.horizontal_wrapped(|ui| {
                if let Some(ms) = cable_ms {
                    ui.label(format!("Virtual mic ≈ {ms:.0} ms"));
                    ui.separator();
                }
                if let Some(ms) = mon_ms.filter(|_| self.cfg.monitor_enabled) {
                    ui.label(format!("Headphones ≈ {ms:.0} ms"));
                    ui.separator();
                }
                ui.label(format!("DSP load {:.1}%", self.load * 100.0))
                    .on_hover_text("Processing time as a share of each audio block's time budget.");
                ui.separator();
                let g = ui.label(format!("Glitches {glitches}"));
                if glitches > 0 {
                    g.on_hover_text("Audio dropouts so far. If this keeps rising, choose a higher latency setting.");
                }
            });

            egui::CollapsingHeader::new("Details").show(ui, |ui| {
                ui.label(format!(
                    "Engine: {} @ {} Hz, {:.1} ms blocks · effects add {:.1} ms · DSP uses {}",
                    self.status.input_name,
                    self.status.sample_rate,
                    in_ms,
                    dsp_ms,
                    simd::level().label()
                ));
                for (label, info, st) in
                    [("Virtual mic", &self.status.cable, &sh.cable), ("Headphones", &self.status.monitor, &sh.monitor)]
                {
                    if let Some(i) = info.as_ref().filter(|i| !i.lost) {
                        ui.label(format!(
                            "{label}: {} @ {} Hz · buffer {:.1} ms (target {:.1}) · margin {:.0} ms · drift {:+.0} ppm",
                            i.name,
                            i.sample_rate,
                            st.fill_ms.load(),
                            st.target_ms.load(),
                            st.margin_ms.load(),
                            st.correction_ppm.load(),
                        ));
                    }
                }
            });
        });
    }
}

impl eframe::App for App {
    /// Runs before every frame and on wake-ups while hidden (hotkeys, tray).
    fn logic(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.background(ctx);
    }

    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        self.intercept_close(&ctx);
        self.tick(&ctx);
        let advanced = self.cfg.ui_mode == UiMode::Advanced;
        egui::CentralPanel::default().show(ui, |ui| {
            egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
                self.header(ui);
                ui.add_space(6.0);
                self.devices_section(ui, advanced);
                self.levels_section(ui);
                if advanced {
                    self.controls_section(ui, true);
                    self.preset_manager(ui);
                    self.effects_section(ui);
                    self.system_section(ui);
                    self.performance_section(ui);
                    self.help_section(ui);
                } else {
                    self.preset_grid(ui);
                    self.controls_section(ui, false);
                }
                ui.add_space(4.0);
                ui.label(RichText::new(format!("v{}", env!("CARGO_PKG_VERSION"))).small().weak());
            });
        });
    }

    fn on_exit(&mut self, _gl: Option<&eframe::glow::Context>) {
        self.cfg.save();
    }
}
