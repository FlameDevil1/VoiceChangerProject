//! Live spectrum: your voice before effects (outline) and after (filled bars).

use eframe::egui::{self, Color32, Stroke};
use voice_changer::audio::shared::Scope;
use voice_changer::dsp::fft;

const FFT_SIZE: usize = 2048;
const BANDS: usize = 40;
const FLOOR_DB: f32 = -80.0;
/// Bars fall at this rate (dB per second) so the display is readable, not jittery.
const FALL_DB_PER_S: f32 = 45.0;

pub struct Spectrum {
    input: [f32; BANDS],
    output: [f32; BANDS],
}

impl Default for Spectrum {
    fn default() -> Self {
        Self { input: [FLOOR_DB; BANDS], output: [FLOOR_DB; BANDS] }
    }
}

/// Log-spaced band edges between 60 Hz and 16 kHz (or Nyquist).
fn band_edges(rate: u32) -> [f32; BANDS + 1] {
    let (lo, hi) = (60.0f32, 16_000f32.min(rate as f32 * 0.45));
    std::array::from_fn(|i| lo * (hi / lo).powf(i as f32 / BANDS as f32))
}

fn bands(mags: &[f32], rate: u32) -> [f32; BANDS] {
    let edges = band_edges(rate);
    let bin_hz = rate as f32 / FFT_SIZE as f32;
    std::array::from_fn(|b| {
        let (a, z) = ((edges[b] / bin_hz) as usize, ((edges[b + 1] / bin_hz).ceil() as usize).max(1));
        let peak = mags[a.min(mags.len() - 1)..z.min(mags.len())].iter().fold(0.0f32, |m, v| m.max(*v));
        (20.0 * peak.max(1e-6).log10()).max(FLOOR_DB)
    })
}

impl Spectrum {
    pub fn reset(&mut self) {
        *self = Self::default();
    }

    pub fn update(&mut self, scope: &Scope, rate: u32, dt: f32) {
        if rate == 0 {
            return;
        }
        let (inp, out) = scope.snapshot(FFT_SIZE);
        let fall = FALL_DB_PER_S * dt;
        for (cur, new) in [
            (&mut self.input, bands(&fft::spectrum(&inp), rate)),
            (&mut self.output, bands(&fft::spectrum(&out), rate)),
        ] {
            for (c, n) in cur.iter_mut().zip(new) {
                *c = n.max(*c - fall);
            }
        }
    }

    pub fn show(&self, ui: &mut egui::Ui) {
        let (rect, _) = ui.allocate_exact_size(egui::vec2(ui.available_width(), 64.0), egui::Sense::hover());
        let p = ui.painter();
        let v = ui.visuals();
        p.rect_filled(rect, 3.0, v.extreme_bg_color);
        let h = |db: f32| rect.bottom() - rect.height() * ((db - FLOOR_DB) / -FLOOR_DB).clamp(0.0, 1.0);
        let w = rect.width() / BANDS as f32;
        let accent = v.selection.bg_fill;
        for b in 0..BANDS {
            let x0 = rect.left() + b as f32 * w + 1.0;
            let x1 = x0 + w - 2.0;
            p.rect_filled(
                egui::Rect::from_min_max(egui::pos2(x0, h(self.output[b])), egui::pos2(x1, rect.bottom())),
                1.0,
                accent,
            );
            let y = h(self.input[b]);
            p.line_segment(
                [egui::pos2(x0, y), egui::pos2(x1, y)],
                Stroke::new(1.5, v.strong_text_color().gamma_multiply(0.7)),
            );
        }
        let label_color = Color32::from_gray(140);
        for (hz, text) in [(100.0, "100"), (1000.0, "1k"), (10_000.0, "10k")] {
            let x = rect.left() + rect.width() * ((hz as f32 / 60.0).ln() / (16_000.0f32 / 60.0).ln());
            p.text(
                egui::pos2(x, rect.bottom() - 2.0),
                egui::Align2::CENTER_BOTTOM,
                text,
                egui::FontId::proportional(10.0),
                label_color,
            );
        }
    }
}
