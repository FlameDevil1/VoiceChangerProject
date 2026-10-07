//! Preset UI: a button grid for Simple mode and a manager (save, rename, delete, import,
//! export) for Advanced mode.

use super::App;
use super::widgets::{GREEN, RED, section};
use eframe::egui::{self, RichText};
use voice_changer::presets::{Preset, Source};

/// Inline editing state of the preset manager.
#[derive(Default)]
pub enum Edit {
    #[default]
    None,
    SaveAs {
        name: String,
        include_cleanup: bool,
    },
    Rename {
        name: String,
    },
    ConfirmDelete,
}

#[derive(Default)]
pub struct PresetUi {
    pub edit: Edit,
    /// Last result: (text, is_error).
    pub message: Option<(String, bool)>,
}

impl App {
    pub(super) fn load_preset(&mut self, name: &str) {
        let Some(entry) = self.presets.find(name) else { return };
        let fx = entry.preset.apply(&self.cfg.fx);
        self.cfg.preset = Some(name.to_string());
        self.set_fx(fx);
        self.preset_ui.message = None;
    }

    fn current_preset(&self) -> Option<&Preset> {
        self.cfg.preset.as_deref().and_then(|n| self.presets.find(n)).map(|e| &e.preset)
    }

    fn preset_modified(&self) -> bool {
        self.current_preset().is_none_or(|p| !p.matches(&self.cfg.fx))
    }

    /// Simple mode: one button per preset, current one highlighted.
    pub(super) fn preset_grid(&mut self, ui: &mut egui::Ui) {
        let modified = self.preset_modified();
        let current = self.cfg.preset.clone();
        let entries: Vec<(String, String, bool)> = self
            .presets
            .entries()
            .iter()
            .map(|e| (e.preset.name.clone(), e.preset.description.clone(), e.source == Source::BuiltIn))
            .collect();
        let mut clicked = None;
        section(ui, "Voice", |ui| {
            let cols = ((ui.available_width() + 6.0) / 136.0).floor().max(2.0) as usize;
            let w = (ui.available_width() - 6.0 * (cols as f32 - 1.0)) / cols as f32;
            let mut user_header = false;
            let mut i = 0;
            let mut rows: Vec<Vec<&(String, String, bool)>> = Vec::new();
            for e in &entries {
                if !e.2 && !user_header {
                    user_header = true;
                    i = 0;
                    rows.push(Vec::new()); // marker row: "Your presets"
                }
                if i % cols == 0 {
                    rows.push(Vec::new());
                }
                rows.last_mut().unwrap().push(e);
                i += 1;
            }
            for row in rows {
                if row.is_empty() {
                    ui.add_space(4.0);
                    ui.label(RichText::new("Your presets").small().weak());
                    continue;
                }
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = 6.0;
                    for (name, description, _) in row {
                        let selected = current.as_deref() == Some(name.as_str());
                        let label = if selected && modified { format!("{name} *") } else { name.clone() };
                        let r = ui
                            .add_sized([w, 30.0], egui::Button::new(label).selected(selected))
                            .on_hover_text(if description.is_empty() { name.as_str() } else { description.as_str() });
                        if r.clicked() {
                            clicked = Some(name.clone());
                        }
                    }
                });
            }
            if modified && current.is_some() {
                ui.label(
                    RichText::new("* changed since loaded. Save it under a new name in Advanced mode.").small().weak(),
                );
            }
        });
        if let Some(name) = clicked {
            self.load_preset(&name);
        }
    }

    /// Advanced mode: pick, save, rename, delete, import and export.
    pub(super) fn preset_manager(&mut self, ui: &mut egui::Ui) {
        let modified = self.preset_modified();
        let current = self.cfg.preset.clone();
        let is_user =
            current.as_deref().and_then(|n| self.presets.find(n)).is_some_and(|e| e.source != Source::BuiltIn);
        let mut load = None;

        section(ui, "Presets", |ui| {
            ui.horizontal(|ui| {
                let shown = match &current {
                    Some(n) if modified => format!("{n} *"),
                    Some(n) => n.clone(),
                    None => "(custom)".into(),
                };
                egui::ComboBox::from_id_salt("preset-pick").selected_text(shown).width(220.0).show_ui(ui, |ui| {
                    let mut user_header = false;
                    ui.label(RichText::new("Built-in").small().weak());
                    for e in self.presets.entries() {
                        if e.source != Source::BuiltIn && !user_header {
                            user_header = true;
                            ui.separator();
                            ui.label(RichText::new("Your presets").small().weak());
                        }
                        let sel = current.as_deref() == Some(e.preset.name.as_str());
                        if ui.selectable_label(sel, &e.preset.name).on_hover_text(&e.preset.description).clicked() {
                            load = Some(e.preset.name.clone());
                        }
                    }
                });
                if ui
                    .add_enabled(is_user && modified, egui::Button::new("Save"))
                    .on_hover_text("Overwrite this preset")
                    .clicked()
                    && let Some(name) = &current
                {
                    let include = self.presets.find(name).is_some_and(|e| e.preset.include_cleanup);
                    let result = self.presets.save(Preset::capture(name, &self.cfg.fx, include), true);
                    self.preset_ui.message = Some(match result {
                        Ok(()) => (format!("Saved \"{name}\"."), false),
                        Err(e) => (e, true),
                    });
                }
            });
            ui.horizontal_wrapped(|ui| {
                if ui.button("Save as…").clicked() {
                    let name = current.clone().map(|n| format!("{n} (mine)")).unwrap_or_else(|| "My voice".into());
                    self.preset_ui.edit = Edit::SaveAs { name, include_cleanup: false };
                }
                if ui.add_enabled(is_user, egui::Button::new("Rename…")).clicked() {
                    self.preset_ui.edit = Edit::Rename { name: current.clone().unwrap_or_default() };
                }
                if ui.add_enabled(is_user, egui::Button::new("Delete…")).clicked() {
                    self.preset_ui.edit = Edit::ConfirmDelete;
                }
                ui.separator();
                if ui.button("Import…").on_hover_text("Add presets from .json files").clicked() {
                    self.import_presets();
                }
                if ui
                    .add_enabled(current.is_some(), egui::Button::new("Export…"))
                    .on_hover_text("Save this preset as a .json file to share")
                    .clicked()
                    && let Some(name) = &current
                {
                    self.export_preset(name);
                }
            });

            self.preset_edit_row(ui, current.as_deref());

            if let Some((text, is_error)) = &self.preset_ui.message {
                ui.colored_label(if *is_error { RED } else { GREEN }, text);
            }
        });
        if let Some(name) = load {
            self.load_preset(&name);
        }
    }

    fn preset_edit_row(&mut self, ui: &mut egui::Ui, current: Option<&str>) {
        let mut close = false;
        match &mut self.preset_ui.edit {
            Edit::None => {}
            Edit::SaveAs { name, include_cleanup } => {
                ui.horizontal(|ui| {
                    ui.label("Name");
                    let r = ui.text_edit_singleline(name);
                    let enter = r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
                    if ui.button("Save").clicked() || enter {
                        let preset = Preset::capture(name, &self.cfg.fx, *include_cleanup);
                        let saved_name = preset.name.clone();
                        match self.presets.save(preset, false) {
                            Ok(()) => {
                                self.preset_ui.message = Some((format!("Saved \"{saved_name}\"."), false));
                                self.cfg.preset = Some(saved_name);
                                self.dirty_since.get_or_insert_with(std::time::Instant::now);
                                close = true;
                            }
                            Err(e) => self.preset_ui.message = Some((e, true)),
                        }
                    }
                    if ui.button("Cancel").clicked() {
                        close = true;
                    }
                });
                ui.checkbox(include_cleanup, "Include mic cleanup (noise suppression & gate)")
                    .on_hover_text("Off: loading this preset keeps whatever noise settings suit your current room.");
            }
            Edit::Rename { name } => {
                ui.horizontal(|ui| {
                    ui.label("New name");
                    let r = ui.text_edit_singleline(name);
                    let enter = r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
                    if (ui.button("Rename").clicked() || enter)
                        && let Some(old) = current
                    {
                        let new = name.trim().to_string();
                        match self.presets.rename(old, &new) {
                            Ok(()) => {
                                self.preset_ui.message = Some((format!("Renamed to \"{new}\"."), false));
                                self.cfg.preset = Some(new);
                                self.dirty_since.get_or_insert_with(std::time::Instant::now);
                                close = true;
                            }
                            Err(e) => self.preset_ui.message = Some((e, true)),
                        }
                    }
                    if ui.button("Cancel").clicked() {
                        close = true;
                    }
                });
            }
            Edit::ConfirmDelete => {
                ui.horizontal(|ui| {
                    ui.label(format!("Delete \"{}\"? This can't be undone.", current.unwrap_or("")));
                    if ui.button(RichText::new("Delete").color(RED)).clicked()
                        && let Some(name) = current
                    {
                        match self.presets.delete(name) {
                            Ok(()) => {
                                self.preset_ui.message = Some((format!("Deleted \"{name}\"."), false));
                                // Settings stay as they are; they just no longer match a preset.
                                self.cfg.preset = None;
                                self.dirty_since.get_or_insert_with(std::time::Instant::now);
                            }
                            Err(e) => self.preset_ui.message = Some((e, true)),
                        }
                        close = true;
                    }
                    if ui.button("Cancel").clicked() {
                        close = true;
                    }
                });
            }
        }
        if close {
            self.preset_ui.edit = Edit::None;
        }
    }

    fn import_presets(&mut self) {
        let Some(files) = rfd::FileDialog::new()
            .set_title("Import presets")
            .add_filter("Voice Changer preset", &["json"])
            .pick_files()
        else {
            return;
        };
        let (mut ok, mut errors) = (Vec::new(), Vec::new());
        for f in files {
            match self.presets.import(&f) {
                Ok(name) => ok.push(name),
                Err(e) => errors.push(e),
            }
        }
        self.preset_ui.message = Some(if errors.is_empty() {
            (format!("Imported {}.", ok.join(", ")), false)
        } else {
            (format!("Imported {} preset(s); {} failed: {}", ok.len(), errors.len(), errors.join("; ")), true)
        });
    }

    fn export_preset(&mut self, name: &str) {
        let file =
            name.chars().map(|c| if c.is_alphanumeric() || c == ' ' || c == '-' { c } else { '_' }).collect::<String>();
        let Some(path) = rfd::FileDialog::new()
            .set_title("Export preset")
            .add_filter("Voice Changer preset", &["json"])
            .set_file_name(format!("{file}.json"))
            .save_file()
        else {
            return;
        };
        self.preset_ui.message = Some(match self.presets.export(name, &path) {
            Ok(()) => (format!("Exported to {}.", path.display()), false),
            Err(e) => (e, true),
        });
    }
}
