//! "Record": saves exactly what the virtual mic sends (your changed voice, with any bad mic or
//! connection, mute and bypass) to a file. A writer thread streams it to disk as it comes, so
//! long recordings use no extra memory; MP3 is encoded when you stop.

use super::App;
use rtrb::{Consumer, RingBuffer};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering::Relaxed};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};
use voice_changer::audio::Command;
use voice_changer::config;
use voice_changer::offline::export::Format;

/// Ring between the audio thread and the writer: generous, so a slow disk never drops audio.
const RING_SECONDS: usize = 10;

#[derive(Default)]
pub struct Recorder {
    active: Option<Active>,
    /// Result of the last recording, shown until the next one starts.
    pub saved: Option<Result<PathBuf, String>>,
}

struct Active {
    started: Instant,
    stop: Arc<AtomicBool>,
    worker: JoinHandle<Result<PathBuf, String>>,
}

impl Recorder {
    pub fn elapsed(&self) -> Option<Duration> {
        self.active.as_ref().map(|a| a.started.elapsed())
    }
}

/// "Voice Changer 2026-10-08 14-03-27" in local time.
fn timestamp_name() -> String {
    #[cfg(windows)]
    {
        let t = unsafe { windows::Win32::System::SystemInformation::GetLocalTime() };
        format!(
            "Voice Changer {:04}-{:02}-{:02} {:02}-{:02}-{:02}",
            t.wYear, t.wMonth, t.wDay, t.wHour, t.wMinute, t.wSecond
        )
    }
    #[cfg(not(windows))]
    {
        let secs = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs());
        format!("Voice Changer {secs}")
    }
}

/// Stream the ring to `wav` until stopped, then (for MP3) encode `final_path` from it.
fn write(
    mut rx: Consumer<f32>,
    rate: u32,
    wav: PathBuf,
    final_path: PathBuf,
    format: Format,
    stop: Arc<AtomicBool>,
) -> Result<PathBuf, String> {
    let spec = match format {
        Format::Wav16 => hound::WavSpec {
            channels: 1,
            sample_rate: rate,
            bits_per_sample: 16,
            sample_format: hound::SampleFormat::Int,
        },
        _ => hound::WavSpec {
            channels: 1,
            sample_rate: rate,
            bits_per_sample: 32,
            sample_format: hound::SampleFormat::Float,
        },
    };
    let err = |e: hound::Error| format!("{}: {e}", wav.display());
    let mut writer = hound::WavWriter::create(&wav, spec).map_err(err)?;
    let mut stop_seen: Option<Instant> = None;
    loop {
        let stopping = stop.load(Relaxed);
        if stopping && stop_seen.is_none() {
            stop_seen = Some(Instant::now());
        }
        let mut wrote = false;
        while let Ok(s) = rx.pop() {
            wrote = true;
            match format {
                Format::Wav16 => writer.write_sample((s.clamp(-1.0, 1.0) * 32767.0).round() as i16).map_err(err)?,
                _ => writer.write_sample(s).map_err(err)?,
            }
        }
        // Done once the tap has gone quiet (or, if the stop never reached the audio thread, soon after).
        if stopping && (!wrote || stop_seen.is_some_and(|t| t.elapsed() > Duration::from_secs(1))) {
            break;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    writer.finalize().map_err(err)?;
    if format == Format::Mp3 {
        let audio = voice_changer::offline::load(&wav)?;
        let result = voice_changer::offline::mp3::encode(&final_path, &audio.samples, audio.rate, 160);
        let _ = std::fs::remove_file(&wav);
        result?;
    }
    Ok(final_path)
}

/// Show a file selected in Explorer (or open the folder if it's gone).
pub fn reveal(path: &Path) {
    let mut cmd = std::process::Command::new("explorer");
    if path.is_file() {
        cmd.arg(format!("/select,{}", path.display()));
    } else {
        cmd.arg(path);
    }
    let _ = cmd.spawn();
}

impl App {
    pub(super) fn recordings_dir(&self) -> PathBuf {
        self.cfg.recordings_dir.clone().unwrap_or_else(config::default_recordings_dir)
    }

    pub(super) fn is_recording(&self) -> bool {
        self.recorder.active.is_some()
    }

    pub(super) fn start_recording(&mut self) {
        if self.is_recording() || !self.is_active() {
            return;
        }
        let rate = self.status.sample_rate.max(8000);
        let dir = self.recordings_dir();
        if let Err(e) = std::fs::create_dir_all(&dir) {
            self.recorder.saved = Some(Err(format!("{}: {e}", dir.display())));
            return;
        }
        let format = self.cfg.record_format;
        let name = timestamp_name();
        let final_path = dir.join(format!("{name}.{}", format.extension()));
        let wav = if format == Format::Mp3 { dir.join(format!("{name}.recording.wav")) } else { final_path.clone() };
        let (tx, rx) = RingBuffer::new(rate as usize * RING_SECONDS);
        let stop = Arc::new(AtomicBool::new(false));
        let st = stop.clone();
        let worker = std::thread::Builder::new()
            .name("recorder".into())
            .spawn(move || write(rx, rate, wav, final_path, format, st));
        match worker {
            Ok(worker) => {
                self.engine.send(Command::SetRecordTap(Some(tx)));
                self.recorder.active = Some(Active { started: Instant::now(), stop, worker });
                self.recorder.saved = None;
                log::info!("recording started");
            }
            Err(e) => self.recorder.saved = Some(Err(e.to_string())),
        }
    }

    pub(super) fn stop_recording(&mut self) {
        if let Some(a) = &self.recorder.active {
            self.engine.send(Command::SetRecordTap(None));
            a.stop.store(true, Relaxed);
        }
    }

    /// Finish a stopped recording, and stop one whose audio went away. Runs while hidden too.
    pub(super) fn recorder_tick(&mut self, ctx: &eframe::egui::Context) {
        let Some(active) = &self.recorder.active else { return };
        if !self.is_active() && !active.stop.load(Relaxed) {
            self.stop_recording();
        }
        let Some(active) = self.recorder.active.take_if(|a| a.worker.is_finished()) else {
            ctx.request_repaint_after(Duration::from_millis(250));
            return;
        };
        let result = active.worker.join().unwrap_or_else(|_| Err("recorder crashed".into()));
        match &result {
            Ok(path) => {
                log::info!("recording saved: {}", path.display());
                let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
                self.toast(&format!("Recording saved: {name}"));
            }
            Err(e) => {
                log::warn!("recording failed: {e}");
                self.toast("Recording failed");
            }
        }
        self.recorder.saved = Some(result);
    }

    /// "⏺ Record" / "■ Stop 0:12" button for the controls row.
    pub(super) fn record_button(&mut self, ui: &mut eframe::egui::Ui) {
        use eframe::egui;
        match self.recorder.elapsed() {
            Some(t) => {
                let s = t.as_secs();
                let label = egui::RichText::new(format!("■ Stop  {}:{:02}", s / 60, s % 60)).color(super::widgets::RED);
                if ui.button(label).on_hover_text("Stop and save the recording").clicked() {
                    self.stop_recording();
                }
            }
            None => {
                let r = ui.add_enabled(self.is_active(), egui::Button::new("⏺ Record"));
                let tip = if self.is_active() {
                    format!("Record what the virtual mic sends to {}", self.recordings_dir().display())
                } else {
                    "Press Start first".to_string()
                };
                if r.on_hover_text(tip).on_disabled_hover_text("Press Start first").clicked() {
                    self.start_recording();
                }
            }
        }
    }
}
