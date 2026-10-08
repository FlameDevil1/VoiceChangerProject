//! Reusable widgets: sections, sliders, device dropdowns, meters.

use eframe::egui::{self, Color32, RichText};
use voice_changer::audio::devices;
use voice_changer::config::{DeviceRef, ThemePref};
use voice_changer::dsp::gain_to_db;

pub const GREEN: Color32 = Color32::from_rgb(60, 180, 90);
pub const AMBER: Color32 = Color32::from_rgb(230, 160, 40);
pub const RED: Color32 = Color32::from_rgb(220, 70, 60);

pub fn apply_theme(ctx: &egui::Context, theme: ThemePref) {
    ctx.set_theme(match theme {
        ThemePref::System => egui::ThemePreference::System,
        ThemePref::Dark => egui::ThemePreference::Dark,
        ThemePref::Light => egui::ThemePreference::Light,
    });
}

pub fn section(ui: &mut egui::Ui, title: &str, add: impl FnOnce(&mut egui::Ui)) {
    ui.add_space(4.0);
    egui::Frame::group(ui.style()).show(ui, |ui| {
        ui.set_width(ui.available_width());
        ui.label(RichText::new(title).strong());
        ui.add_space(2.0);
        add(ui);
    });
}

pub fn channel_label(ch: Option<usize>) -> &'static str {
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
pub fn device_combo(
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
    egui::ComboBox::from_id_salt(id).selected_text(current).width(ui.available_width().min(300.0)).truncate().show_ui(
        ui,
        |ui| {
            if ui.selectable_label(selected.is_none(), &none_text).clicked() && selected.is_some() {
                *selected = None;
                changed = true;
            }
            for (d, (name, warn)) in list.iter().zip(labels) {
                let is_sel = selected.as_ref().is_some_and(|s| s.id == d.id);
                let text = if *warn { format!("⚠ {name}") } else { name.clone() };
                let r = ui.selectable_label(is_sel, text);
                let r = if *warn {
                    r.on_hover_text("This is a virtual cable output; using it here causes feedback.")
                } else {
                    r
                };
                if r.clicked() && !is_sel {
                    *selected = Some(d.to_ref());
                    changed = true;
                }
            }
        },
    );
    changed
}

/// Labelled slider with typed input (click the number) and a reset-to-default button.
/// Returns the slider's response with `changed()` also covering the reset.
pub fn slider_row(
    ui: &mut egui::Ui,
    label: &str,
    value: &mut f32,
    range: std::ops::RangeInclusive<f32>,
    suffix: &str,
    step: f64,
    default: f32,
) -> egui::Response {
    let r = slider_cells(ui, label, value, range, suffix, step, default);
    ui.end_row();
    r
}

/// The label, slider and reset button of a grid row, leaving the row open for more cells.
pub fn slider_cells(
    ui: &mut egui::Ui,
    label: &str,
    value: &mut f32,
    range: std::ops::RangeInclusive<f32>,
    suffix: &str,
    step: f64,
    default: f32,
) -> egui::Response {
    ui.label(label);
    let decimals = if step >= 1.0 { 0 } else { 1 };
    let r = ui.add(egui::Slider::new(value, range).suffix(suffix).step_by(step).max_decimals(decimals));
    reset_button(ui, value, default, r)
}

/// Label, dropdown and reset button for a parameter stored as an option index.
pub fn choice_cells(ui: &mut egui::Ui, label: &str, value: &mut f32, options: &[&str], default: f32) -> egui::Response {
    ui.label(label);
    let mut index = (value.round().max(0.0) as usize).min(options.len().saturating_sub(1));
    let mut r = egui::ComboBox::from_id_salt(("choice", label))
        .selected_text(options.get(index).copied().unwrap_or(""))
        .show_ui(ui, |ui| {
            for (i, name) in options.iter().enumerate() {
                ui.selectable_value(&mut index, i, *name);
            }
        })
        .response;
    if index as f32 != value.round() {
        *value = index as f32;
        r.mark_changed();
    }
    reset_button(ui, value, default, r)
}

fn reset_button(ui: &mut egui::Ui, value: &mut f32, default: f32, mut r: egui::Response) -> egui::Response {
    if ui.add_enabled(*value != default, egui::Button::new("⟲")).on_hover_text("Reset").clicked() {
        *value = default;
        r.mark_changed();
    }
    r
}

/// Lock toggle for a control the Randomize buttons may change. Returns true when toggled.
pub fn lock_toggle(ui: &mut egui::Ui, locked: &mut bool) -> bool {
    let tip = if *locked {
        "Locked: Randomize keeps this value. Click to unlock."
    } else {
        "Click to lock: Randomize will keep this value."
    };
    let text = if *locked { RichText::new("🔒") } else { RichText::new("🔓").weak() };
    let r = ui.selectable_label(*locked, text).on_hover_text(tip);
    if r.clicked() {
        *locked = !*locked;
    }
    r.clicked()
}

/// Gain slider in dB with a reset button. Returns true on change.
pub fn gain_row(ui: &mut egui::Ui, label: &str, db: &mut f32) -> bool {
    slider_row(ui, label, db, -24.0..=24.0, " dB", 0.5, 0.0).changed()
}

/// Small up/down arrow button, drawn as a triangle (the default fonts have no "▲▼" glyphs).
pub fn arrow_button(ui: &mut egui::Ui, up: bool, enabled: bool, tooltip: &str) -> bool {
    let size = egui::vec2(18.0, 18.0);
    let sense = if enabled { egui::Sense::click() } else { egui::Sense::hover() };
    let (rect, response) = ui.allocate_exact_size(size, sense);
    let visuals = if enabled { ui.style().interact(&response) } else { &ui.visuals().widgets.noninteractive };
    ui.painter().rect_filled(rect, 3.0, visuals.weak_bg_fill);
    let c = rect.center();
    let (dx, dy) = (4.5, if up { -3.0 } else { 3.0 });
    let color = if enabled { visuals.fg_stroke.color } else { ui.visuals().weak_text_color() };
    ui.painter().add(egui::Shape::convex_polygon(
        vec![egui::pos2(c.x - dx, c.y - dy), egui::pos2(c.x + dx, c.y - dy), egui::pos2(c.x, c.y + dy)],
        color,
        egui::Stroke::NONE,
    ));
    let response = response.on_hover_text(tooltip);
    enabled && response.clicked()
}

/// Small filled circle (the default fonts have no "●" glyph).
pub fn status_dot(ui: &mut egui::Ui, color: Color32) {
    let (rect, _) = ui.allocate_exact_size(egui::vec2(10.0, 10.0), egui::Sense::hover());
    ui.painter().circle_filled(rect.center(), 4.0, color);
}

/// Peak meter with fall-back ballistics and a 1 s peak-hold tick.
#[derive(Clone, Copy)]
pub struct Meter {
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
    pub fn update(&mut self, peak: f32, dt: f32) {
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

    pub fn show(&self, ui: &mut egui::Ui) {
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
                    RED
                } else if self.db > -12.0 {
                    Color32::from_rgb(230, 180, 40)
                } else {
                    GREEN
                };
                let mut r = rect;
                r.set_width(rect.width() * level);
                p.rect_filled(r, 2.0, color);
            }
            let hx = rect.left() + rect.width() * frac(self.hold_db);
            if frac(self.hold_db) > 0.0 {
                p.line_segment(
                    [egui::pos2(hx, rect.top()), egui::pos2(hx, rect.bottom())],
                    (1.5, v.strong_text_color()),
                );
            }
            let text = if self.db <= METER_FLOOR { "-∞ dB".to_string() } else { format!("{:.0} dB", self.db) };
            ui.label(RichText::new(text).monospace());
            if self.clipped > 0.0 {
                ui.label(RichText::new("CLIP").small().color(RED));
            }
        });
    }
}
