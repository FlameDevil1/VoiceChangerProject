//! Pitch and formant shifting by causal TD-PSOLA.
//!
//! **Tracking.** A YIN pitch tracker runs every 5 ms on a ~16 kHz decimated copy of the input
//! (about 9x cheaper than full rate and plenty for voice pitch).
//!
//! **Analysis marks** are spaced one pitch period apart (a fixed 5 ms in unvoiced sections).
//! Each mark stores the period estimate current when it was created and is never revised.
//!
//! **Synthesis marks** are spaced `period / pitch_ratio` apart. Each synthesis mark `p` takes a
//! Hann-windowed grain from the latest analysis mark `a <= p` and overlap-adds it at `p`.
//! Placing grains closer together raises the pitch; further apart lowers it. The grain's
//! content (one glottal pulse plus its formant ringing) is kept, so formants are preserved.
//! Reading each grain time-scaled by `formant_ratio` moves the formants independently of pitch.
//!
//! **Causality / latency.** Classic PSOLA waits for a full grain (two pitch periods, ~25 ms for a
//! male voice) before outputting anything. Here every output sample is evaluated on demand from
//! the active grains. Because each grain reads from an analysis mark at or before its synthesis
//! position, an output sample only ever needs input up to (nearly) the same instant. The only
//! algorithmic delay is the 3 samples the cubic interpolator needs, i.e. ~0.06 ms. (Pitch
//! *detection* still lags ~5–20 ms, which only affects how quickly grain sizes follow the voice.)
//!
//! Unvoiced sounds (s, f, breaths) are passed through with pitch ratio 1, so noise is not
//! warped, but formant shifting still applies so the vocal character stays consistent.

use super::util::HannTable;

pub const MAX_SHIFT_SEMITONES: f32 = 12.0;
const FMIN: f32 = 60.0;
const FMAX: f32 = 900.0;
/// Interpolator lookahead: the read position always trails the input by this many samples.
const PAD: f64 = 3.0;
const MAX_GRAINS: usize = 24;

// -------------------------------------------------------------------------------------------
// Pitch tracker
// -------------------------------------------------------------------------------------------

/// Real-time YIN pitch tracker on a decimated signal. All buffers allocated up front.
pub struct PitchTracker {
    pub(crate) decim: usize,
    acc: f32,
    acc_n: usize,
    ring: Vec<f32>,
    ring_pos: usize,
    filled: usize,
    lin: Vec<f32>,
    diff: Vec<f32>,
    sq_diff: super::simd::SqDiff,
    win: usize,
    tau_min: usize,
    tau_max: usize,
    hop: usize,
    since_hop: usize,
    /// Current period in full-rate samples, `None` when unvoiced.
    period: Option<f32>,
    unvoiced_hops: u32,
    /// Decimated samples pushed so far (absolute index of the next one).
    dec_count: u64,
    /// Absolute full-rate position of the most recent glottal pulse (waveform maximum within the
    /// last period), used to phase-lock analysis marks.
    pulse: Option<f64>,
}

impl PitchTracker {
    pub fn new(sample_rate: f32) -> Self {
        let decim = ((sample_rate / 16_000.0).round() as usize).max(1);
        let dr = sample_rate / decim as f32;
        let tau_min = (dr / FMAX).floor().max(2.0) as usize;
        let tau_max = (dr / FMIN).ceil() as usize;
        let win = (dr * 0.020).ceil() as usize; // 20 ms window
        let need = win + tau_max + 2;
        Self {
            decim,
            acc: 0.0,
            acc_n: 0,
            ring: vec![0.0; need],
            ring_pos: 0,
            filled: 0,
            lin: vec![0.0; need],
            diff: vec![0.0; tau_max + 2],
            sq_diff: super::simd::sq_diff_fn(),
            win,
            tau_min,
            tau_max,
            hop: ((dr * 0.005) as usize).max(1),
            since_hop: 0,
            period: None,
            unvoiced_hops: 0,
            dec_count: 0,
            pulse: None,
        }
    }

    pub fn period(&self) -> Option<f32> {
        self.period
    }

    pub fn pulse(&self) -> Option<f64> {
        self.pulse
    }

    pub fn reset(&mut self) {
        self.acc = 0.0;
        self.acc_n = 0;
        self.ring.fill(0.0);
        self.ring_pos = 0;
        self.filled = 0;
        self.since_hop = 0;
        self.period = None;
        self.unvoiced_hops = 0;
        self.dec_count = 0;
        self.pulse = None;
    }

    #[inline]
    pub fn push(&mut self, x: f32) {
        // Box-filter decimation: crude, but aliasing above ~5 kHz doesn't affect voice pitch.
        self.acc += x;
        self.acc_n += 1;
        if self.acc_n < self.decim {
            return;
        }
        let v = self.acc / self.decim as f32;
        self.acc = 0.0;
        self.acc_n = 0;
        self.ring[self.ring_pos] = v;
        self.ring_pos = (self.ring_pos + 1) % self.ring.len();
        self.dec_count += 1;
        self.filled = (self.filled + 1).min(self.ring.len());
        self.since_hop += 1;
        if self.since_hop >= self.hop {
            self.since_hop = 0;
            if self.filled == self.ring.len() {
                self.analyze();
            }
        }
    }

    fn analyze(&mut self) {
        // Unroll the ring into a contiguous buffer, oldest first.
        let n = self.ring.len();
        let (a, b) = self.ring.split_at(self.ring_pos);
        self.lin[..b.len()].copy_from_slice(b);
        self.lin[b.len()..n].copy_from_slice(a);

        let w = self.win;
        let x = &self.lin;
        let energy: f32 = x[..w].iter().map(|s| s * s).sum::<f32>() / w as f32;
        let voiced = if energy < 1e-6 {
            None // below about -60 dBFS: silence
        } else {
            self.diff[0] = 1.0;
            let mut running = 0.0f32;
            let mut found = None;
            for tau in 1..=self.tau_max {
                let d = (self.sq_diff)(&x[..w], &x[tau..tau + w]);
                running += d;
                self.diff[tau] = if running > 0.0 { d * tau as f32 / running } else { 1.0 };
            }
            // First dip below the strict threshold (YIN's rule, avoids octave errors). Failing
            // that, accept the global minimum if it is still clearly periodic: breathy vowels
            // and pitch glides often sit there, and calling them unvoiced would briefly switch
            // the shift off mid-word.
            const THRESHOLD: f32 = 0.2;
            const FALLBACK: f32 = 0.35;
            let mut tau = self.tau_min;
            while tau < self.tau_max {
                if self.diff[tau] < THRESHOLD {
                    while tau < self.tau_max && self.diff[tau + 1] < self.diff[tau] {
                        tau += 1;
                    }
                    found = Some(tau);
                    break;
                }
                tau += 1;
            }
            if found.is_none() {
                let (t, v) = (self.tau_min..self.tau_max)
                    .map(|t| (t, self.diff[t]))
                    .fold((0, f32::MAX), |b, c| if c.1 < b.1 { c } else { b });
                if v < FALLBACK {
                    found = Some(t);
                }
            }
            found.map(|t| {
                let (l, c, r) = (self.diff[t - 1], self.diff[t], self.diff[t + 1]);
                let den = l - 2.0 * c + r;
                let off = if den.abs() > 1e-12 { (0.5 * (l - r) / den).clamp(-0.5, 0.5) } else { 0.0 };
                (t as f32 + off) * self.decim as f32
            })
        };

        match voiced {
            Some(p) => {
                self.period = Some(p);
                self.unvoiced_hops = 0;
                // Pulse = waveform maximum within the most recent period. Always the maximum
                // (not |x|) so the reference doesn't flip polarity between frames.
                let tau = ((p / self.decim as f32).ceil() as usize).clamp(1, n);
                let (i, _) = x[n - tau..n]
                    .iter()
                    .enumerate()
                    .fold((0, f32::MIN), |best, (i, v)| if *v > best.1 { (i, *v) } else { best });
                let abs = self.dec_count - tau as u64 + i as u64;
                // Centre of the box-filtered decimation window.
                self.pulse = Some(abs as f64 * self.decim as f64 + (self.decim as f64 - 1.0) * 0.5);
            }
            // Hold the last pitch for up to 40 ms through brief dropouts inside a vowel, but let
            // go at once when the signal goes quiet (end of a word).
            None => {
                self.unvoiced_hops += 1;
                if self.unvoiced_hops > 8 || energy < 1e-5 {
                    self.period = None;
                }
            }
        }
    }
}

// -------------------------------------------------------------------------------------------
// PSOLA shifter
// -------------------------------------------------------------------------------------------

#[derive(Clone, Copy, Default)]
struct Grain {
    /// Synthesis centre (absolute sample index).
    p: f64,
    /// Input position read at the grain centre.
    read_centre: f64,
    /// Window half-lengths before/after the centre (and reciprocals). The left half always
    /// matches the previous grain's right half, so adjacent halves sum to exactly 1.
    half_l: f64,
    half_r: f64,
    inv_l: f64,
    inv_r: f64,
    /// Input samples advanced per output sample (formant ratio).
    step: f64,
    /// Distance to the next synthesis mark.
    spacing: f64,
    /// Amplitude compensation for sparse grains (pitch down / formant up).
    gain: f32,
}

/// Per-block controls for the shifter.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PsolaControls {
    /// Pitch ratio (2.0 = one octave up).
    pub ratio: f64,
    /// Formant ratio (> 1 = smaller vocal tract).
    pub formant: f64,
    /// 0 = natural intonation, 1 = every period forced to `target_hz` (robot monotone).
    pub monotone: f64,
    pub target_hz: f64,
    /// Pitch movement around your running average pitch: 1 = natural, 0 = flat (monotone at
    /// your own average), 2 = exaggerated twice as much.
    pub intonation: f64,
    /// Auto-tune: 0 = off, 1 = every period snapped to the nearest note of the scale.
    pub autotune: f64,
    /// Scale root, 0 = C ... 11 = B.
    pub key: i32,
    pub scale: Scale,
    /// Vibrato depth in cents (0 = off) and rate in Hz.
    pub vibrato_cents: f64,
    pub vibrato_hz: f64,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Scale {
    #[default]
    Chromatic,
    Major,
    Minor,
    Pentatonic,
}

impl Scale {
    pub const LABELS: &'static [&'static str] = &["Chromatic", "Major", "Minor", "Pentatonic"];

    pub fn from_index(i: f32) -> Self {
        match i.round() as i32 {
            1 => Scale::Major,
            2 => Scale::Minor,
            3 => Scale::Pentatonic,
            _ => Scale::Chromatic,
        }
    }

    fn degrees(self) -> &'static [i32] {
        match self {
            Scale::Chromatic => &[0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11],
            Scale::Major => &[0, 2, 4, 5, 7, 9, 11],
            Scale::Minor => &[0, 2, 3, 5, 7, 8, 10],
            Scale::Pentatonic => &[0, 2, 4, 7, 9],
        }
    }
}

/// Frequency of the scale note nearest to `hz`.
pub fn snap_to_scale(hz: f64, key: i32, scale: Scale) -> f64 {
    let midi = 69.0 + 12.0 * (hz / 440.0).log2();
    let degrees = scale.degrees();
    let center = midi.round() as i32;
    let best = (center - 6..=center + 6)
        .filter(|n| degrees.contains(&(n - key).rem_euclid(12)))
        .min_by(|a, b| (*a as f64 - midi).abs().total_cmp(&(*b as f64 - midi).abs()))
        .unwrap_or(center);
    440.0 * 2f64.powf((best as f64 - 69.0) / 12.0)
}

impl Default for PsolaControls {
    fn default() -> Self {
        Self {
            ratio: 1.0,
            formant: 1.0,
            monotone: 0.0,
            target_hz: 110.0,
            intonation: 1.0,
            autotune: 0.0,
            key: 0,
            scale: Scale::Chromatic,
            vibrato_cents: 0.0,
            vibrato_hz: 5.5,
        }
    }
}

impl PsolaControls {
    pub fn from_semitones(pitch: f32, formant: f32) -> Self {
        let st = pitch.clamp(-MAX_SHIFT_SEMITONES, MAX_SHIFT_SEMITONES) as f64;
        let fm = formant.clamp(-MAX_SHIFT_SEMITONES, MAX_SHIFT_SEMITONES) as f64;
        Self { ratio: 2f64.powf(st / 12.0), formant: 2f64.powf(fm / 12.0), ..Default::default() }
    }
}

pub struct PitchShifter {
    controls: PsolaControls,
    rate: f32,
    hist: Vec<f32>,
    mask: usize,
    n: u64,
    tracker: PitchTracker,
    hann: HannTable,
    grains: [Grain; MAX_GRAINS],
    active: usize,
    pending: Option<Grain>,
    next_p: f64,
    // Latest analysis mark and the period/voicing it was created with.
    mark: f64,
    mark_period: f64,
    mark_voiced: bool,
    unvoiced_period: f64,
    max_period: f64,
    /// Right half-length of the most recently created grain.
    last_half_r: f64,
    /// Running average of the voiced pitch, as log2(Hz), for the intonation control.
    avg_log_f0: Option<f64>,
    /// Last coarse period from the tracker and its full-rate refinement.
    refined: (f32, f64),
}

impl PitchShifter {
    pub fn new() -> Self {
        Self {
            controls: PsolaControls::default(),
            rate: 0.0,
            hist: Vec::new(),
            mask: 0,
            n: 0,
            tracker: PitchTracker::new(48_000.0),
            hann: HannTable::new(),
            grains: [Grain::default(); MAX_GRAINS],
            active: 0,
            pending: None,
            next_p: 0.0,
            mark: 0.0,
            mark_period: 240.0,
            mark_voiced: false,
            unvoiced_period: 240.0,
            max_period: 800.0,
            last_half_r: 240.0,
            avg_log_f0: None,
            refined: (0.0, 0.0),
        }
    }

    /// Latest analysis mark at or before `p`, extending the mark sequence as needed.
    ///
    /// `mark_period` is the actual distance to the next mark. In voiced sections each new mark is
    /// nudged (by at most 1/8 period) towards the predicted glottal pulse, so grains end up
    /// centred on pulses. Formant shifting resamples each grain, and that only stays clean when
    /// a grain holds one pulse at its centre.
    fn analysis_mark(&mut self, p: f64) -> (f64, f64, bool) {
        while self.mark + self.mark_period <= p {
            self.mark += self.mark_period;
            match self.tracker.period() {
                Some(t) => {
                    let t = self.refine_period(t).min(self.max_period);
                    let mut next = self.mark + t;
                    if let Some(pulse) = self.tracker.pulse() {
                        // Phase error of the next mark against the pulse grid, wrapped to ±T/2.
                        let phase = (next - pulse) / t;
                        let err = (phase - phase.round()) * t;
                        next -= err.clamp(-t / 8.0, t / 8.0);
                    }
                    self.mark_period = next - self.mark;
                    self.mark_voiced = true;
                }
                None => {
                    self.mark_period = self.unvoiced_period;
                    self.mark_voiced = false;
                }
            }
        }
        (self.mark, self.mark_period, self.mark_voiced)
    }

    fn make_grain(&mut self, p: f64) -> Grain {
        let (a, period, voiced) = self.analysis_mark(p);
        let c = self.controls;
        // Pitch decisions use the measured period, not the phase-corrected mark spacing.
        let ratio = if voiced { self.voiced_ratio(p, self.refined.1.min(self.max_period)) } else { 1.0 };
        let step = c.formant.clamp(0.5, 2.0);
        let half_r = period / step;
        let half_l = self.last_half_r;
        self.last_half_r = half_r;
        // With formants raised, the grain's right edge would read ahead of real time; shift
        // the whole read back so it never does (a constant per-grain offset is inaudible).
        let back = (period - half_r).max(0.0);
        let spacing = period / ratio;
        // Grains spaced wider than their windows leave gaps; restore the lost power.
        let gain = (spacing / half_r).max(1.0).sqrt() as f32;
        Grain {
            p,
            read_centre: a - back - PAD,
            half_l,
            half_r,
            inv_l: 1.0 / half_l,
            inv_r: 1.0 / half_r,
            step,
            spacing,
            gain,
        }
    }

    /// Refine the tracker's period (measured on a ~16 kHz copy, so only ~1 % accurate for high
    /// voices) to sub-sample accuracy at full rate: search a few samples around it with the
    /// squared-difference function on the latest audio, then interpolate the minimum. Cached
    /// until the tracker's estimate changes (every 5 ms at most).
    fn refine_period(&mut self, coarse: f32) -> f64 {
        if coarse == self.refined.0 {
            return self.refined.1;
        }
        let span = self.tracker.decim as i64 + 1;
        let c = coarse.round() as i64;
        let win = (coarse as i64 * 2).clamp(256, 2048);
        let end = self.n as i64 - PAD as i64;
        let (m, h) = (self.mask as i64, &self.hist);
        let diff = |tau: i64| -> f32 {
            (end - win..end).map(|i| h[(i & m) as usize] - h[((i - tau) & m) as usize]).map(|d| d * d).sum()
        };
        let lo = (c - span).max(2);
        let (best, _) = (lo..=c + span).map(|t| (t, diff(t))).fold((c, f32::MAX), |b, x| if x.1 < b.1 { x } else { b });
        let (a, b, cc) = (diff(best - 1), diff(best), diff(best + 1));
        let den = a - 2.0 * b + cc;
        let off = if den.abs() > 1e-12 { (0.5 * (a - cc) / den).clamp(-0.5, 0.5) } else { 0.0 };
        let fine = best as f64 + off as f64;
        // Only trust the refinement if history covers the window (not right after a reset).
        let fine = if (self.n as i64) > win + c + span + 8 { fine } else { coarse as f64 };
        self.refined = (coarse, fine);
        fine
    }

    /// Pitch ratio for a voiced grain at output position `p` whose input period is `period`.
    /// Everything depends only on absolute sample positions, so it is block-size invariant.
    fn voiced_ratio(&mut self, p: f64, period: f64) -> f64 {
        let c = self.controls;
        let sr = self.rate as f64;
        let f_in = sr / period;
        // Monotone pulls each period towards the target pitch: ratio = target / input.
        let mut ratio = c.ratio * (c.target_hz / f_in).powf(c.monotone);

        // Intonation: scale the distance from your running average pitch (in octaves).
        let log_f = f_in.log2();
        let avg = *self.avg_log_f0.get_or_insert(log_f);
        // 1.5 s time constant, advanced by one period per grain.
        let alpha = 1.0 - (-period / (1.5 * sr)).exp();
        self.avg_log_f0 = Some(avg + alpha * (log_f - avg));
        if c.intonation != 1.0 {
            ratio *= 2f64.powf((avg - log_f) * (1.0 - c.intonation));
        }

        // Auto-tune: pull the output pitch to the nearest note of the scale.
        if c.autotune > 0.0 {
            let f_out = f_in * ratio;
            let target = snap_to_scale(f_out, c.key, c.scale);
            ratio *= (target / f_out).powf(c.autotune.min(1.0));
        }

        // Vibrato on top (after auto-tune, so a hard-tuned voice can still wobble on purpose).
        if c.vibrato_cents > 0.0 {
            let phase = std::f64::consts::TAU * c.vibrato_hz * p / sr;
            ratio *= 2f64.powf(c.vibrato_cents * phase.sin() / 1200.0);
        }
        ratio.clamp(0.25, 4.0)
    }

    #[inline]
    fn read(&self, pos: f64) -> f32 {
        let i = pos.floor();
        let t = (pos - i) as f32;
        let i = i as i64;
        let m = self.mask as i64;
        let h = &self.hist;
        let xm1 = h[((i - 1) & m) as usize];
        let x0 = h[(i & m) as usize];
        let x1 = h[((i + 1) & m) as usize];
        let x2 = h[((i + 2) & m) as usize];
        let c1 = 0.5 * (x1 - xm1);
        let c2 = xm1 - 2.5 * x0 + 2.0 * x1 - 0.5 * x2;
        let c3 = 0.5 * (x2 - xm1) + 1.5 * (x0 - x1);
        ((c3 * t + c2) * t + c1) * t + x0
    }
}

impl Default for PitchShifter {
    fn default() -> Self {
        Self::new()
    }
}

impl PitchShifter {
    pub fn set_controls(&mut self, c: PsolaControls) {
        self.controls = c;
    }

    pub fn prepare(&mut self, sample_rate: f32) {
        self.rate = sample_rate;
        self.max_period = (sample_rate / FMIN) as f64;
        self.unvoiced_period = (sample_rate * 0.005) as f64;
        // Reads reach back at most ~4 max periods (grain span + mark lag + formant offset).
        let size = ((self.max_period * 8.0) as usize + 64).next_power_of_two();
        self.hist = vec![0.0; size];
        self.mask = size - 1;
        self.tracker = PitchTracker::new(sample_rate);
        self.reset();
    }

    pub fn process(&mut self, buf: &mut [f32]) {
        for s in buf.iter_mut() {
            let n = self.n as f64;
            self.hist[(self.n as usize) & self.mask] = *s;
            self.tracker.push(*s);

            // Start every grain whose window now begins.
            loop {
                let g = match self.pending {
                    Some(g) => g,
                    None => {
                        let g = self.make_grain(self.next_p);
                        self.pending = Some(g);
                        g
                    }
                };
                if g.p - g.half_l > n || self.active == MAX_GRAINS {
                    break;
                }
                self.grains[self.active] = g;
                self.active += 1;
                self.next_p = g.p + g.spacing;
                self.pending = None;
            }

            // Overlap-add the active grains at this output sample.
            let (mut sum, mut wsum) = (0.0f32, 0.0f32);
            let mut i = 0;
            while i < self.active {
                let g = self.grains[i];
                let d = n - g.p;
                if d >= g.half_r {
                    self.active -= 1;
                    self.grains[i] = self.grains[self.active];
                    continue;
                }
                let w = self.hann.at((d * if d < 0.0 { g.inv_l } else { g.inv_r }) as f32);
                sum += w * g.gain * self.read(g.read_centre + d * g.step);
                wsum += w;
                i += 1;
            }
            // Where grains pile up (pitch up / formant down) they add with differing phases, so
            // normalise by sqrt(window sum) to preserve power; where windows sum to 1 or less
            // (zero shift, PSOLA's natural gaps on pitch down) leave the signal alone.
            *s = if wsum > 1.0 { sum / wsum.sqrt() } else { sum };
            self.n += 1;
        }
    }

    /// Algorithmic latency in samples (the interpolator pad).
    pub fn latency(&self) -> usize {
        PAD as usize
    }

    /// Clear state without reallocating (real-time safe).
    pub fn reset(&mut self) {
        self.hist.fill(0.0);
        self.n = 0;
        self.tracker.reset();
        self.active = 0;
        self.pending = None;
        self.next_p = 0.0;
        self.mark = 0.0;
        self.mark_period = self.unvoiced_period;
        self.mark_voiced = false;
        self.last_half_r = self.unvoiced_period;
        self.avg_log_f0 = None;
        self.refined = (0.0, 0.0);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::offline::{analysis, signals};

    fn shift(x: &[f32], st: f32, fm: f32, block: usize) -> Vec<f32> {
        run(x, PsolaControls::from_semitones(st, fm), block)
    }

    fn run(x: &[f32], c: PsolaControls, block: usize) -> Vec<f32> {
        let mut p = PitchShifter::new();
        p.prepare(48_000.0);
        p.set_controls(c);
        let mut y = x.to_vec();
        for c in y.chunks_mut(block) {
            p.process(c);
        }
        y
    }

    /// Steady part of a signal (skip onset while the tracker locks, and the fade-out).
    fn steady(x: &[f32]) -> &[f32] {
        &x[9600..x.len() - 2400]
    }

    #[test]
    fn tracker_follows_vowel_pitch() {
        for f0 in [80.0, 140.0, 300.0] {
            let x = signals::vowel(48_000, 0.4, f0);
            let mut t = PitchTracker::new(48_000.0);
            x.iter().take(12_000).for_each(|s| t.push(*s));
            let est = 48_000.0 / t.period().expect("voiced");
            assert!((est - f0 as f32).abs() / (f0 as f32) < 0.01, "f0 {f0} -> {est}");
        }
        let mut t = PitchTracker::new(48_000.0);
        signals::noise(48_000, 0.2, 0.3, 3).iter().for_each(|s| t.push(*s));
        assert_eq!(t.period(), None, "noise is unvoiced");
    }

    #[test]
    fn zero_shift_is_transparent() {
        let x = signals::vowel(48_000, 0.6, 140.0);
        let y = shift(&x, 0.0, 0.0, 480);
        // Output = input delayed by the interpolator pad.
        let d = PAD as usize;
        let snr = analysis::snr_db(steady(&x[..x.len() - d]), steady(&y[d..]));
        assert!(snr > 60.0, "SNR {snr:.1} dB");
    }

    #[test]
    fn pitch_shift_hits_target_and_keeps_formants() {
        let x = signals::vowel(48_000, 0.8, 140.0);
        let centroid_in = analysis::spectral_centroid(steady(&x), 48_000);
        for st in [-12.0f32, -5.0, 4.0, 7.0, 12.0] {
            let y = shift(&x, st, 0.0, 480);
            let want = 140.0 * 2f32.powf(st / 12.0);
            let got = analysis::estimate_f0(steady(&y), 48_000, 50.0, 800.0).expect("voiced output");
            assert!((got - want).abs() / want < 0.02, "{st} st: want {want:.1}, got {got:.1}");
            // Formants preserved: the spectral centroid barely moves (resampling would move it
            // by the full pitch ratio, e.g. 1.5x at +7 st).
            let c = analysis::spectral_centroid(steady(&y), 48_000) / centroid_in;
            assert!((0.92..1.12).contains(&c), "{st} st: centroid ratio {c:.2}");
            // Loudness stays roughly constant.
            let level = analysis::rms(steady(&y)) / analysis::rms(steady(&x));
            assert!((0.6..1.3).contains(&level), "{st} st: level ratio {level:.2}");
        }
    }

    #[test]
    fn formant_shift_moves_spectrum_not_pitch() {
        let x = signals::vowel(48_000, 0.8, 140.0);
        let centroid_in = analysis::spectral_centroid(steady(&x), 48_000);
        // A +/-4 st formant shift moves the envelope by 1.26x / 0.79x.
        for (fm, lo, hi) in [(4.0f32, 1.1, 1.4), (-4.0, 0.65, 0.9)] {
            let y = shift(&x, 0.0, fm, 480);
            let f0 = analysis::estimate_f0(steady(&y), 48_000, 50.0, 800.0).unwrap();
            assert!((f0 - 140.0).abs() < 3.0, "formant {fm}: pitch moved to {f0}");
            let c = analysis::spectral_centroid(steady(&y), 48_000) / centroid_in;
            assert!((lo..hi).contains(&c), "formant {fm}: centroid ratio {c:.2}");
        }
    }

    #[test]
    fn monotone_flattens_intonation_to_target() {
        // A vowel gliding 120 -> 220 Hz comes out flat at 150 Hz.
        let x = signals::vowel_glide(48_000, 1.0, 120.0, 220.0);
        let c = PsolaControls { monotone: 1.0, target_hz: 150.0, ..Default::default() };
        let y = run(&x, c, 480);
        for start in [12_000, 24_000, 36_000] {
            let f = analysis::estimate_f0(&y[start..start + 4800], 48_000, 50.0, 800.0).unwrap();
            assert!((f - 150.0).abs() < 4.0, "at {start}: {f}");
        }
    }

    /// f0 measured in consecutive windows of `win` samples, skipping the first `skip`.
    fn f0_track(y: &[f32], skip: usize, win: usize) -> Vec<f32> {
        y[skip..]
            .chunks(win)
            .filter(|c| c.len() == win)
            .filter_map(|c| analysis::estimate_f0(c, 48_000, 50.0, 800.0))
            .collect()
    }

    #[test]
    fn snap_to_scale_picks_nearest_allowed_note() {
        assert!((snap_to_scale(450.0, 0, Scale::Chromatic) - 440.0).abs() < 0.01); // A4
        assert!((snap_to_scale(290.0, 0, Scale::Major) - 293.66).abs() < 0.05); // D4 in C major
        // C#4 (277 Hz) is not in C major: snaps to C4 or D4, whichever is nearer.
        let s = snap_to_scale(272.0, 0, Scale::Major);
        assert!((s - 261.63).abs() < 0.05, "{s}");
        // A (major) pentatonic is A, B, C#, E, F#: 300 Hz (near D) snaps to C#4.
        assert!((snap_to_scale(300.0, 9, Scale::Pentatonic) - 277.18).abs() < 0.05);
    }

    #[test]
    fn autotune_snaps_a_detuned_voice() {
        let x = signals::vowel(48_000, 1.0, 452.0); // between A4 (440) and A#4 (466), nearer A4
        let c = PsolaControls { autotune: 1.0, ..Default::default() };
        let y = run(&x, c, 480);
        for f in f0_track(&y, 9600, 4800) {
            assert!((f - 440.0).abs() < 3.0, "{f}");
        }
    }

    #[test]
    fn intonation_flattens_or_exaggerates_a_glide() {
        // Speech-like: pitch swinging +/-20 % around 160 Hz once a second.
        let x = signals::vowel_wobble(48_000, 4.0, 160.0, 0.2, 1.0);
        let span = |y: &[f32]| {
            let t = f0_track(y, 48_000, 4800);
            t.iter().cloned().fold(f32::MIN, f32::max) / t.iter().cloned().fold(f32::MAX, f32::min)
        };
        let natural = span(&run(&x, PsolaControls::default(), 480));
        let flat = span(&run(&x, PsolaControls { intonation: 0.0, ..Default::default() }, 480));
        let big = span(&run(&x, PsolaControls { intonation: 2.0, ..Default::default() }, 480));
        assert!(flat < 1.0 + (natural - 1.0) * 0.35, "flat span {flat} vs natural {natural}");
        assert!(big > natural * 1.05, "exaggerated {big} vs natural {natural}");
    }

    #[test]
    fn vibrato_wobbles_pitch_by_its_depth() {
        let x = signals::vowel(48_000, 2.0, 200.0);
        let c = PsolaControls { vibrato_cents: 50.0, vibrato_hz: 2.0, ..Default::default() };
        let t = f0_track(&run(&x, c, 480), 9600, 2400);
        let (lo, hi) = (t.iter().cloned().fold(f32::MAX, f32::min), t.iter().cloned().fold(f32::MIN, f32::max));
        let cents = 1200.0 * (hi / lo).log2();
        assert!((60.0..130.0).contains(&cents), "peak-to-peak {cents} cents (expected ~100)");
    }

    #[test]
    fn block_size_invariant() {
        let x = signals::vowel(48_000, 0.4, 170.0);
        let reference = shift(&x, 5.0, -3.0, 480);
        for block in [1, 37, 4096] {
            assert_eq!(shift(&x, 5.0, -3.0, block), reference, "block {block}");
        }
    }

    #[test]
    fn silence_and_noise_stay_sane() {
        let y = shift(&signals::silence(48_000, 0.2), 7.0, 3.0, 480);
        assert!(y.iter().all(|s| *s == 0.0));
        let x = signals::noise(48_000, 0.5, 0.3, 9);
        let y = shift(&x, -7.0, 0.0, 480);
        assert!(y.iter().all(|s| s.is_finite()));
        // Unvoiced input passes at its original level (no pitch warping of noise).
        let r = analysis::rms(steady(&y)) / analysis::rms(steady(&x));
        assert!((0.7..1.3).contains(&r), "noise level ratio {r:.2}");
    }

    #[test]
    fn long_run_is_stable() {
        // 60 s: positions are f64 and indices wrap; check no drift, NaN or silence.
        let x = signals::vowel(48_000, 60.0, 120.0);
        let y = shift(&x, -4.0, 2.0, 480);
        let tail = &y[y.len() - 48_000..y.len() - 4800];
        assert!(tail.iter().all(|s| s.is_finite()));
        assert!(analysis::rms(tail) > 0.05);
        let f0 = analysis::estimate_f0(tail, 48_000, 50.0, 800.0).unwrap();
        assert!((f0 - 120.0 * 2f32.powf(-4.0 / 12.0)).abs() < 2.0, "{f0}");
    }
}
