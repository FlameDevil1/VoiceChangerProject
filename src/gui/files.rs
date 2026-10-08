//! "Files & recording": record the virtual mic, and give audio files your current voice
//! (batch, speed with the pitch kept, WAV or MP3, preview, drag and drop).

use super::App;
use super::recorder::reveal;
use super::widgets::{AMBER, GREEN, RED, slider_row};
use eframe::egui::{self, RichText};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering::Relaxed};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use voice_changer::audio::playback::{self, Playback};
use voice_changer::dsp::CoreParams;
use voice_changer::offline::{self, export};

/// Audio formats the file picker offers (anything Symphonia decodes also works when dropped).
const EXTENSIONS: [&str; 5] = ["wav", "mp3", "flac", "ogg", "oga"];

#[derive(Clone, Debug, PartialEq)]
enum Status {
    Waiting,
    Working(f32),
    Done,
    Failed(String),
    Cancelled,
}

struct Item {
    input: PathBuf,
    output: Option<PathBuf>,
    status: Status,
}

struct Run {
    cancel: Arc<AtomicBool>,
    shared: Arc<Mutex<Vec<Status>>>,
    /// Indices into `items` this run processes, in the same order as `shared`.
    indices: Vec<usize>,
    worker: std::thread::JoinHandle<()>,
}

#[derive(Default)]
pub struct FileBatch {
    items: Vec<Item>,
    run: Option<Run>,
    preview: Option<(usize, Arc<Playback>)>,
    /// Files were just dropped: open the section so you see them.
    reveal: bool,
    last_output_dir: Option<PathBuf>,
}

impl FileBatch {
    pub fn add(&mut self, paths: Vec<PathBuf>) {
        for p in paths {
            if p.is_file() && !self.items.iter().any(|i| i.input == p) {
                self.items.push(Item { input: p, output: None, status: Status::Waiting });
            }
        }
        self.reveal = true;
    }

    fn running(&self) -> bool {
        self.run.is_some()
    }

    /// Copy progress from the worker; finish the run when it's done.
    fn poll(&mut self) {
        let Some(run) = &self.run else { return };
        if let Ok(shared) = run.shared.lock() {
            for (&i, s) in run.indices.iter().zip(shared.iter()) {
                self.items[i].status = s.clone();
            }
        }
        if run.worker.is_finished()
            && let Some(run) = self.run.take()
        {
            let _ = run.worker.join();
        }
    }

    fn stop_preview(&mut self) {
        if let Some((_, p)) = self.preview.take() {
            p.stop.store(true, Relaxed);
        }
    }
}

impl App {
    /// Files dropped on the window go to the batch list; show a hint while dragging over it.
    pub(super) fn handle_dropped_files(&mut self, ctx: &egui::Context) {
        let dropped: Vec<PathBuf> = ctx.input(|i| i.raw.dropped_files.iter().map(|f| f.path().to_path_buf()).collect());
        if !dropped.is_empty() {
            self.files.add(dropped);
        }
        if ctx.input(|i| !i.raw.hovered_files.is_empty()) {
            let painter = ctx.layer_painter(egui::LayerId::new(egui::Order::Foreground, egui::Id::new("drop-overlay")));
            let rect = ctx.content_rect();
            painter.rect_filled(rect, 0.0, egui::Color32::from_black_alpha(170));
            painter.text(
                rect.center(),
                egui::Align2::CENTER_CENTER,
                "Drop audio files to give them your voice",
                egui::FontId::proportional(18.0),
                egui::Color32::WHITE,
            );
        }
    }

    pub(super) fn files_section(&mut self, ui: &mut egui::Ui) {
        self.files.poll();
        if self.files.running() || self.is_recording() {
            ui.ctx().request_repaint_after(Duration::from_millis(100));
        }
        let mut title = "Files & recording".to_string();
        if let Some(t) = self.recorder.elapsed() {
            title += &format!(": recording {}:{:02}", t.as_secs() / 60, t.as_secs() % 60);
        } else if self.files.running() {
            title += ": processing";
        }
        ui.add_space(4.0);
        let id = ui.make_persistent_id("files-section");
        if std::mem::take(&mut self.files.reveal) {
            let mut state = egui::collapsing_header::CollapsingState::load_with_default_open(ui.ctx(), id, false);
            state.set_open(true);
            state.store(ui.ctx());
        }
        egui::Frame::group(ui.style()).show(ui, |ui| {
            ui.set_width(ui.available_width());
            egui::CollapsingHeader::new(RichText::new(title).strong()).id_salt(id).show(ui, |ui| {
                self.recording_ui(ui);
                ui.separator();
                self.batch_ui(ui);
            });
        });
    }

    fn recording_ui(&mut self, ui: &mut egui::Ui) {
        ui.label(RichText::new("Record").strong());
        ui.horizontal_wrapped(|ui| {
            self.record_button(ui);
            let mut format = self.cfg.record_format;
            egui::ComboBox::from_id_salt("record-format").selected_text(format.label()).show_ui(ui, |ui| {
                for f in export::Format::ALL {
                    ui.selectable_value(&mut format, f, f.label());
                }
            });
            if format != self.cfg.record_format {
                self.cfg.record_format = format;
                self.mark_dirty();
            }
            if ui.button("Open folder").clicked() {
                let dir = self.recordings_dir();
                let _ = std::fs::create_dir_all(&dir);
                reveal(&dir);
            }
            if ui.button("Change…").on_hover_text(self.recordings_dir().display().to_string()).clicked()
                && let Some(dir) = rfd::FileDialog::new().set_directory(self.recordings_dir()).pick_folder()
            {
                self.cfg.recordings_dir = Some(dir);
                self.mark_dirty();
            }
        });
        match &self.recorder.saved {
            Some(Ok(path)) => {
                let path = path.clone();
                ui.horizontal(|ui| {
                    let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
                    ui.label(RichText::new(format!("Saved {name}")).small().color(GREEN));
                    if ui.small_button("Show").clicked() {
                        reveal(&path);
                    }
                });
            }
            Some(Err(e)) => {
                ui.label(RichText::new(e).small().color(RED));
            }
            None => {
                ui.label(RichText::new("Saves exactly what others hear from the virtual mic.").small().weak());
            }
        }
    }

    fn batch_ui(&mut self, ui: &mut egui::Ui) {
        ui.label(RichText::new("Process audio files").strong());
        ui.label(
            RichText::new("Gives recordings your current voice and effects. You can also drop files on the window.")
                .small()
                .weak(),
        );
        let running = self.files.running();
        ui.horizontal(|ui| {
            if ui.add_enabled(!running, egui::Button::new("Add files…")).clicked()
                && let Some(paths) = rfd::FileDialog::new().add_filter("Audio", &EXTENSIONS).pick_files()
            {
                self.files.add(paths);
            }
            if !self.files.items.is_empty() && ui.add_enabled(!running, egui::Button::new("Clear list")).clicked() {
                self.files.stop_preview();
                self.files.items.clear();
            }
        });

        // The list.
        let mut remove = None;
        let mut preview = None;
        let playing = self.files.preview.as_ref().filter(|(_, p)| !p.finished.load(Relaxed)).map(|(i, _)| *i);
        if !self.files.items.is_empty() {
            egui::Grid::new("file-list").num_columns(3).striped(true).spacing([8.0, 4.0]).show(ui, |ui| {
                for (i, item) in self.files.items.iter().enumerate() {
                    let name = item.input.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
                    ui.label(&name).on_hover_text(item.input.display().to_string());
                    match &item.status {
                        Status::Waiting => ui.label(RichText::new("waiting").weak()),
                        Status::Working(p) => ui.add(egui::ProgressBar::new(*p).desired_width(110.0).show_percentage()),
                        Status::Done => ui.label(RichText::new("done").color(GREEN)),
                        Status::Failed(e) => ui.label(RichText::new("failed").color(RED)).on_hover_text(e),
                        Status::Cancelled => ui.label(RichText::new("cancelled").color(AMBER)),
                    };
                    ui.horizontal(|ui| {
                        if item.status == Status::Done {
                            let label = if playing == Some(i) { "■" } else { "▶" };
                            if ui.small_button(label).on_hover_text("Listen (on your Hear myself device)").clicked() {
                                preview = Some(i);
                            }
                            if let Some(out) = &item.output
                                && ui.small_button("Show").clicked()
                            {
                                reveal(out);
                            }
                        }
                        if !running && ui.small_button("✖").on_hover_text("Remove from the list").clicked() {
                            remove = Some(i);
                        }
                    });
                    ui.end_row();
                }
            });
        }
        if let Some(i) = remove {
            self.files.stop_preview();
            self.files.items.remove(i);
        }
        if let Some(i) = preview {
            let again = playing == Some(i);
            self.files.stop_preview();
            if !again && let Some(out) = self.files.items[i].output.clone() {
                let load = move || {
                    let a = offline::load(&out)?;
                    Ok((offline::to_mono(&a, None), a.rate))
                };
                self.files.preview = Some((i, playback::play_clip(load, self.cfg.monitor.clone())));
            }
        }

        // Options.
        egui::Grid::new("file-options").num_columns(3).spacing([8.0, 6.0]).show(ui, |ui| {
            let mut pct = self.cfg.export_speed * 100.0;
            let r = slider_row(ui, "Speed", &mut pct, 50.0..=200.0, " %", 5.0, 100.0)
                .on_hover_text("Faster or slower without changing the pitch.");
            if r.changed() {
                self.cfg.export_speed = pct / 100.0;
                self.mark_dirty();
            }
            ui.label("Format");
            let mut format = self.cfg.export_format;
            egui::ComboBox::from_id_salt("export-format").selected_text(format.label()).show_ui(ui, |ui| {
                for f in export::Format::ALL {
                    ui.selectable_value(&mut format, f, f.label());
                }
            });
            if format != self.cfg.export_format {
                self.cfg.export_format = format;
                self.mark_dirty();
            }
            ui.end_row();
            ui.label("Save to");
            let target = match &self.cfg.export_dir {
                Some(d) => {
                    d.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| d.display().to_string())
                }
                None => "next to the originals".to_string(),
            };
            ui.horizontal(|ui| {
                ui.label(target)
                    .on_hover_text(self.cfg.export_dir.as_ref().map(|d| d.display().to_string()).unwrap_or_default());
                if ui.small_button("Change…").clicked()
                    && let Some(dir) = rfd::FileDialog::new().pick_folder()
                {
                    self.cfg.export_dir = Some(dir);
                    self.mark_dirty();
                }
                if self.cfg.export_dir.is_some() && ui.small_button("Reset").clicked() {
                    self.cfg.export_dir = None;
                    self.mark_dirty();
                }
            });
            ui.end_row();
        });

        let waiting = self.files.items.iter().filter(|i| i.status != Status::Done).count();
        ui.horizontal(|ui| {
            if let Some(run) = &self.files.run {
                if ui.button("Cancel").clicked() {
                    run.cancel.store(true, Relaxed);
                }
            } else if ui
                .add_enabled(
                    waiting > 0,
                    egui::Button::new(match waiting {
                        0 => "Process files".to_string(),
                        1 => "Process 1 file".to_string(),
                        n => format!("Process {n} files"),
                    }),
                )
                .clicked()
            {
                self.start_batch();
            }
            if !running
                && let Some(dir) = self.files.last_output_dir.clone()
                && ui.button("Open output folder").clicked()
            {
                reveal(&dir);
            }
        });
    }

    fn start_batch(&mut self) {
        let opts = export::ExportOptions {
            // Mic gain is for your microphone, not for files.
            params: CoreParams::default(),
            fx: self.cfg.fx.clone(),
            channel: None,
            speed: self.cfg.export_speed,
            format: self.cfg.export_format,
        };
        let mut taken = Vec::new();
        let mut jobs = Vec::new();
        for (i, item) in self.files.items.iter_mut().enumerate() {
            if item.status == Status::Done {
                continue;
            }
            let out = export::output_path(&item.input, self.cfg.export_dir.as_deref(), opts.format, &taken);
            taken.push(out.clone());
            item.output = Some(out.clone());
            item.status = Status::Waiting;
            jobs.push((i, item.input.clone(), out));
        }
        if jobs.is_empty() {
            return;
        }
        self.files.last_output_dir = jobs[0].2.parent().map(|p| p.to_path_buf());
        let shared = Arc::new(Mutex::new(vec![Status::Waiting; jobs.len()]));
        let cancel = Arc::new(AtomicBool::new(false));
        let indices = jobs.iter().map(|j| j.0).collect();
        // Half the cores: fast, while leaving room for live audio and the UI.
        let workers = std::thread::available_parallelism().map_or(2, |n| n.get() / 2).clamp(1, jobs.len());
        let (sh, c) = (shared.clone(), cancel.clone());
        let ctx = self.ctx.clone();
        let worker = std::thread::spawn(move || {
            let next = AtomicUsize::new(0);
            std::thread::scope(|s| {
                for _ in 0..workers {
                    s.spawn(|| {
                        loop {
                            let k = next.fetch_add(1, Relaxed);
                            let Some((_, input, output)) = jobs.get(k) else { break };
                            let set = |st: Status| {
                                if let Ok(mut v) = sh.lock() {
                                    v[k] = st;
                                }
                            };
                            if c.load(Relaxed) {
                                set(Status::Cancelled);
                                continue;
                            }
                            set(Status::Working(0.0));
                            let progress = |p: f32| set(Status::Working(p));
                            let result = export::process_file(input, output, &opts, &progress, &c);
                            set(match result {
                                Ok(_) => Status::Done,
                                Err(e) if c.load(Relaxed) => {
                                    let _ = std::fs::remove_file(output);
                                    log::info!("{}: {e}", input.display());
                                    Status::Cancelled
                                }
                                Err(e) => {
                                    log::warn!("processing {} failed: {e}", input.display());
                                    Status::Failed(e)
                                }
                            });
                            ctx.request_repaint();
                        }
                    });
                }
            });
            ctx.request_repaint();
        });
        self.files.run = Some(Run { cancel, shared, indices, worker });
    }
}
