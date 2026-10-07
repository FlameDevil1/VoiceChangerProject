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
    decim: usize,
    acc: f32,
    acc_n: usize,
    ring: Vec<f32>,
    ring_pos: usize,
    filled: usize,
    lin: Vec<f32>,
    diff: Vec<f32>,
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
                let d = sq_diff(&x[..w], &x[tau..tau + w]);
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

/// Sum of squared differences, written with 8 independent accumulators so LLVM vectorises it
/// (a single float accumulator can't be reordered, which blocks SIMD).
#[inline]
fn sq_diff(a: &[f32], b: &[f32]) -> f32 {
    let mut acc = [0.0f32; 8];
    let ((ca, ra), (cb, rb)) = (a.as_chunks::<8>(), b.as_chunks::<8>());
    for (x, y) in ca.iter().zip(cb) {
        for i in 0..8 {
            let d = x[i] - y[i];
            acc[i] += d * d;
        }
    }
    let mut s: f32 = acc.iter().sum();
    for (x, y) in ra.iter().zip(rb) {
        s += (x - y) * (x - y);
    }
    s
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
}

impl Default for PsolaControls {
    fn default() -> Self {
        Self { ratio: 1.0, formant: 1.0, monotone: 0.0, target_hz: 110.0 }
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
                    let t = (t as f64).min(self.max_period);
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
        let ratio = if voiced {
            // Monotone pulls each period towards the target pitch: ratio = target / input.
            let towards = c.target_hz * period / self.rate as f64;
            (c.ratio * towards.powf(c.monotone)).clamp(0.25, 4.0)
        } else {
            1.0
        };
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
