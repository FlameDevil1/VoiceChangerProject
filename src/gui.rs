//! Main window. Kept deliberately light:
//! - redraws only on input, status changes, or at 30 fps while audio is running and visible;
//! - all device work happens on the engine's controller thread, never here;
//! - meters read lock-free atomics.

use eframe::egui::{self, Color32, RichText};
use std::sync::atomic::Ordering::Relaxed;
use std::sync::Arc;
use std::time::{Duration, Instant};
use voice_changer::audio::engine::OutputInfo;
use voice_changer::audio::{devices, Command, EngineHandle, EngineSettings, EngineState, Shared, Status};
use voice_changer::config::{Config, DeviceRef, LatencyMode, ThemePref};
use voice_changer::dsp::{db_to_gain, gain_to_db};

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
}

impl App {
    pub fn new(cc: &eframe::CreationContext<'_>, cfg: Config) -> Self {
        let shared = Arc::new(Shared::default());
        shared.input_gain.store(db_to_gain(cfg.input_gain_db));
        shared.output_gain.store(db_to_gain(cfg.output_gain_db));
        shared.monitor_enabled.store(cfg.monitor_enabled, Relaxed);
        shared.input_channel.store(cfg.input_channel.map_or(-1, |c| c as i32), Relaxed);
        shared.set_margin(cfg.latency.margin_seconds());

        let ctx = cc.egui_ctx.clone();
        let engine = EngineHandle::spawn(shared, Box::new(move || ctx.request_repaint()));
        apply_theme(&cc.egui_ctx, cfg.theme);

        let auto_start_pending = cfg.was_running;
        Self {
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
        }
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
        }
    }

    fn is_active(&self) -> bool {
        matches!(self.status.state, EngineState::Running | EngineState::Reconnecting)
    }

    fn mark_dirty(&mut self) {
        self.dirty_since.get_or_insert_with(Instant::now);
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

    /// Background bookkeeping done once per frame.
    fn tick(&mut self, ctx: &egui::Context) {
        self.status = self.engine.status();

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
        }
        self.was_focused = focused;

        // Debounced config save.
        if let Some(t) = self.dirty_since {
            if t.elapsed() > Duration::from_secs(1) {
                self.cfg.save();
                self.dirty_since = None;
            } else {
                ctx.request_repaint_after(Duration::from_millis(1100));
            }
        }

        // eframe skips this function entirely while minimized or fully covered, so meters cost
        // nothing then. In the background (e.g. next to a game) a lower rate is plenty.
        if self.is_active() {
            let dt = self.last_meter_update.elapsed().as_secs_f32().min(0.25);
            self.last_meter_update = Instant::now();
            let sh = &self.engine.shared;
            self.meters[0].update(sh.in_peak.take(), dt);
            self.meters[1].update(sh.out_peak.take(), dt);
            self.meters[2].update(sh.monitor.peak.take(), dt);
            let load = sh.load.take();
            self.load = load.max(self.load * 0.9);
            let (fps_focused, fps_background) = if self.cfg.low_power_ui { FPS_LOW_POWER } else { FPS_NORMAL };
            let fps = if focused { fps_focused } else { fps_background };
            ctx.request_repaint_after(Duration::from_millis(1000 / fps));
        } else {
            self.meters = Default::default();
            self.load = 0.0;
        }
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
                let (color, text) = match &self.status.state {
                    EngineState::Running => (Color32::from_rgb(60, 180, 90), "Running"),
                    EngineState::Reconnecting => (Color32::from_rgb(230, 160, 40), "Reconnecting…"),
                    EngineState::Error(_) => (Color32::from_rgb(220, 70, 60), "Error"),
                    EngineState::Stopped => (ui.visuals().weak_text_color(), "Stopped"),
                };
                ui.label(RichText::new(text).color(color));
                status_dot(ui, color);
            });
        });

        let active = self.is_active();
        let label = if active { "■  Stop" } else { "▶  Start" };
        let button = egui::Button::new(RichText::new(label).size(18.0)).min_size(egui::vec2(ui.available_width(), 36.0));
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
            ui.colored_label(Color32::from_rgb(220, 70, 60), e);
        }
        if let Some(w) = &self.status.warning {
            ui.colored_label(Color32::from_rgb(230, 160, 40), format!("⚠ {w}"));
        }
    }

    fn devices_section(&mut self, ui: &mut egui::Ui) {
        let list = self.status.devices.clone();
        section(ui, "Devices", |ui| {
            egui::Grid::new("devices").num_columns(2).spacing([8.0, 6.0]).show(ui, |ui| {
                // Microphone
                ui.label("Microphone");
                let default_name = list.default_input.clone().unwrap_or_default();
                let inputs: Vec<(String, bool)> =
                    list.inputs.iter().map(|d| (d.name.clone(), devices::is_cable_capture(&d.name))).collect();
                let changed = device_combo(ui, "mic", &mut self.cfg.input, &list.inputs, &inputs, Some(&default_name));
                if changed {
                    self.mark_dirty();
                    self.restart_if_active();
                }
                ui.end_row();

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

                // Virtual mic
                ui.label("Virtual mic");
                let outputs: Vec<(String, bool)> =
                    list.outputs.iter().map(|d| (d.name.clone(), false)).collect();
                if device_combo(ui, "cable", &mut self.cfg.cable, &list.outputs, &outputs, None) {
                    self.cfg.cable_choice_made = true;
                    self.engine.send(Command::SetCable(self.cfg.cable.clone()));
                    self.mark_dirty();
                }
                ui.end_row();

                // Monitoring
                let mut enabled = self.cfg.monitor_enabled;
                ui.checkbox(&mut enabled, "Hear myself")
                    .on_hover_text("Play the processed voice to your headphones.");
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

            self.cable_indicator(ui);
        });
    }

    /// Shows which device is being fed, or a setup helper when no virtual cable is installed.
    fn cable_indicator(&mut self, ui: &mut egui::Ui) {
        let cable_installed = self.status.devices.cables().next().is_some();
        ui.add_space(4.0);
        match (&self.cfg.cable, &self.status.cable) {
            (Some(_), Some(OutputInfo { name, lost: false, .. })) if self.is_active() => {
                ui.horizontal_wrapped(|ui| {
                    status_dot(ui, Color32::from_rgb(60, 180, 90));
                    ui.label(format!("Feeding {name}"));
                });
                if devices::is_cable_playback(name) {
                    let capture = name.replacen("Input", "Output", 1);
                    ui.label(
                        RichText::new(format!("In Discord, games or OBS, choose \"{capture}\" as the microphone."))
                            .small()
                            .weak(),
                    );
                }
            }
            (Some(d), Some(OutputInfo { lost: true, .. })) => {
                ui.colored_label(Color32::from_rgb(230, 160, 40), format!("⚠ {} is unavailable; retrying…", d.name));
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
        });
    }

    fn controls_section(&mut self, ui: &mut egui::Ui) {
        section(ui, "Controls", |ui| {
            ui.horizontal(|ui| {
                let sh = &self.engine.shared;
                let bypass = sh.bypass.load(Relaxed);
                if ui.add(egui::Button::new("Bypass effects").selected(bypass)).on_hover_text("Send your original voice").clicked() {
                    sh.bypass.store(!bypass, Relaxed);
                }
                let mute = sh.mute.load(Relaxed);
                if ui.add(egui::Button::new("Mute virtual mic").selected(mute)).clicked() {
                    sh.mute.store(!mute, Relaxed);
                }
            });
            ui.add_space(4.0);
            egui::Grid::new("gains").num_columns(3).spacing([8.0, 6.0]).show(ui, |ui| {
                if gain_row(ui, "Input gain", &mut self.cfg.input_gain_db) {
                    self.engine.shared.input_gain.store(db_to_gain(self.cfg.input_gain_db));
                    self.mark_dirty();
                }
                if gain_row(ui, "Output gain", &mut self.cfg.output_gain_db) {
                    self.engine.shared.output_gain.store(db_to_gain(self.cfg.output_gain_db));
                    self.mark_dirty();
                }
            });
        });
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
            let glitches = sh.cable.underruns.load(Relaxed)
                + sh.monitor.underruns.load(Relaxed)
                + sh.capture_xruns.load(Relaxed);
            let path_ms = |info: &Option<OutputInfo>, fill: f32| {
                info.as_ref().filter(|i| !i.lost && i.sample_rate > 0).map(|i| {
                    in_ms + fill + i.buffer_frames as f32 / i.sample_rate as f32 * 1000.0
                })
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
                ui.label(format!("Engine: {} @ {} Hz, {:.1} ms blocks", self.status.input_name, self.status.sample_rate, in_ms));
                for (label, info, st) in [
                    ("Virtual mic", &self.status.cable, &sh.cable),
                    ("Headphones", &self.status.monitor, &sh.monitor),
                ] {
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
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        self.tick(&ctx);
        egui::CentralPanel::default().show(ui, |ui| {
            egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
                self.header(ui);
                ui.add_space(6.0);
                self.devices_section(ui);
                self.levels_section(ui);
                self.controls_section(ui);
                self.performance_section(ui);
                ui.add_space(4.0);
                ui.label(RichText::new(format!("v{} · Step 1: pass-through", env!("CARGO_PKG_VERSION"))).small().weak());
            });
        });
    }

    fn on_exit(&mut self, _gl: Option<&eframe::glow::Context>) {
        self.cfg.save();
    }
}

// ---- widgets ---------------------------------------------------------------------------------

fn apply_theme(ctx: &egui::Context, theme: ThemePref) {
    ctx.set_theme(match theme {
        ThemePref::System => egui::ThemePreference::System,
        ThemePref::Dark => egui::ThemePreference::Dark,
        ThemePref::Light => egui::ThemePreference::Light,
    });
}

fn section(ui: &mut egui::Ui, title: &str, add: impl FnOnce(&mut egui::Ui)) {
    ui.add_space(4.0);
    egui::Frame::group(ui.style()).show(ui, |ui| {
        ui.set_width(ui.available_width());
        ui.label(RichText::new(title).strong());
        ui.add_space(2.0);
        add(ui);
    });
}

fn channel_label(ch: Option<usize>) -> &'static str {
    match ch {
        None => "Mix all channels",
        Some(0) => "Left / channel 1 only",
        Some(1) => "Right / channel 2 only",
        Some(_) => "Other",
    }
}

/// Device dropdown. `default_label = Some(..)` adds a "Windows default" entry (stored as `None`);
/// otherwise the `None` entry means "Off". Remembered devices that are unplugged stay visible.
/// `labels[i] = (name, warn)` marks risky choices. Returns true if the selection changed.
fn device_combo(
    ui: &mut egui::Ui,
    id: &str,
    selected: &mut Option<DeviceRef>,
    list: &[devices::DeviceInfo],
    labels: &[(String, bool)],
    default_label: Option<&str>,
) -> bool {
    let none_text = match default_label {
        Some(n) if !n.is_empty() => format!("Windows default ({n})"),
        Some(_) => "Windows default".to_string(),
        None => "Off".to_string(),
    };
    let current = match selected {
        None => none_text.clone(),
        Some(d) if list.iter().any(|x| x.id == d.id || x.name == d.name) => d.name.clone(),
        Some(d) => format!("{} (disconnected)", d.name),
    };
    let mut changed = false;
    egui::ComboBox::from_id_salt(id)
        .selected_text(current)
        .width(ui.available_width().min(300.0))
        .truncate()
        .show_ui(ui, |ui| {
            if ui.selectable_label(selected.is_none(), &none_text).clicked() && selected.is_some() {
                *selected = None;
                changed = true;
            }
            for (d, (name, warn)) in list.iter().zip(labels) {
                let is_sel = selected.as_ref().is_some_and(|s| s.id == d.id);
                let text = if *warn { format!("⚠ {name}") } else { name.clone() };
                let r = ui.selectable_label(is_sel, text);
                let r = if *warn { r.on_hover_text("This is a virtual cable output; using it here causes feedback.") } else { r };
                if r.clicked() && !is_sel {
                    *selected = Some(d.to_ref());
                    changed = true;
                }
            }
        });
    changed
}

/// Slider with typed input (click the number) and a reset button. Returns true on change.
fn gain_row(ui: &mut egui::Ui, label: &str, db: &mut f32) -> bool {
    ui.label(label);
    let mut changed = ui
        .add(egui::Slider::new(db, -24.0..=24.0).suffix(" dB").step_by(0.5).fixed_decimals(1))
        .changed();
    if ui.add_enabled(*db != 0.0, egui::Button::new("⟲")).on_hover_text("Reset to 0 dB").clicked() {
        *db = 0.0;
        changed = true;
    }
    ui.end_row();
    changed
}

/// Peak meter with fall-back ballistics and a 1 s peak-hold tick.
#[derive(Clone, Copy)]
struct Meter {
    db: f32,
    hold_db: f32,
    hold_left: f32,
    clipped: f32,
}

impl Default for Meter {
    fn default() -> Self {
        Self { db: -90.0, hold_db: -90.0, hold_left: 0.0, clipped: 0.0 }
    }
}

const METER_FLOOR: f32 = -60.0;

impl Meter {
    fn update(&mut self, peak: f32, dt: f32) {
        let db = gain_to_db(peak);
        self.db = db.max(self.db - 30.0 * dt);
        if db >= self.hold_db || self.hold_left <= 0.0 {
            self.hold_db = db.max(self.db);
            self.hold_left = 1.0;
        } else {
            self.hold_left -= dt;
        }
        self.clipped = if peak >= 0.999 { 2.0 } else { (self.clipped - dt).max(0.0) };
    }

    fn show(&self, ui: &mut egui::Ui) {
        let width = ui.available_width().min(300.0) - 64.0;
        ui.horizontal(|ui| {
            let (rect, _) = ui.allocate_exact_size(egui::vec2(width.max(60.0), 12.0), egui::Sense::hover());
            let p = ui.painter();
            let v = ui.visuals();
            p.rect_filled(rect, 2.0, v.extreme_bg_color);
            let frac = |db: f32| ((db - METER_FLOOR) / -METER_FLOOR).clamp(0.0, 1.0);
            let level = frac(self.db);
            if level > 0.0 {
                let color = if self.db > -3.0 {
                    Color32::from_rgb(220, 70, 60)
                } else if self.db > -12.0 {
                    Color32::from_rgb(230, 180, 40)
                } else {
                    Color32::from_rgb(60, 180, 90)
                };
                let mut r = rect;
                r.set_width(rect.width() * level);
                p.rect_filled(r, 2.0, color);
            }
            let hx = rect.left() + rect.width() * frac(self.hold_db);
            if frac(self.hold_db) > 0.0 {
                p.line_segment([egui::pos2(hx, rect.top()), egui::pos2(hx, rect.bottom())], (1.5, v.strong_text_color()));
            }
            let text = if self.db <= METER_FLOOR { "-∞ dB".to_string() } else { format!("{:.0} dB", self.db) };
            ui.label(RichText::new(text).monospace());
            if self.clipped > 0.0 {
                ui.label(RichText::new("CLIP").small().color(Color32::from_rgb(220, 70, 60)));
            }
        });
    }
}

/// Small filled circle (the default fonts have no "●" glyph).
fn status_dot(ui: &mut egui::Ui, color: Color32) {
    let (rect, _) = ui.allocate_exact_size(egui::vec2(10.0, 10.0), egui::Sense::hover());
    ui.painter().circle_filled(rect.center(), 4.0, color);
}
