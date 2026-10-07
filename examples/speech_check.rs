//! Measures pitch accuracy of the shifter on a real speech file, frame by frame.
//!   cargo run --release --example speech_check -- speech.wav [out_dir]
use voice_changer::dsp::{CoreParams, FxSettings};
use voice_changer::offline::{self, analysis, WavFormat};

fn main() {
    let mut args = std::env::args().skip(1);
    let path = std::path::PathBuf::from(args.next().expect("speech.wav"));
    let out_dir = args.next().map(std::path::PathBuf::from);
    let a = offline::load(&path).unwrap();
    let x = offline::to_mono(&a, None);
    let rate = a.rate;
    let frame = (rate as f32 * 0.06) as usize;
    println!("{}: {:.1} s @ {} Hz", path.display(), a.duration_secs(), rate);
    println!("{:>12} {:>7} {:>9} {:>9} {:>8} {:>7}", "setting", "voiced", "median", "within3%", "level", "peak");
    for (st, fm) in [(0.0f32, 0.0f32), (-5.0, -3.0), (-12.0, 0.0), (5.0, 3.0), (7.0, 0.0), (12.0, 0.0), (0.0, 5.0)] {
        let mut fx = FxSettings::default();
        fx.pitch.enabled = true; fx.pitch.semitones = st; fx.pitch.formant = fm;
        let y = offline::render(&x, rate, CoreParams::default(), &fx, 480);
        let want = 2f32.powf(st / 12.0);
        let mut ratios = Vec::new();
        let mut voiced = 0;
        for i in (0..x.len().saturating_sub(frame)).step_by(frame) {
            let (fi, fo) = (analysis::estimate_f0(&x[i..i + frame], rate, 50.0, 900.0), analysis::estimate_f0(&y[i..i + frame], rate, 40.0, 1000.0));
            if let Some(fi) = fi {
                voiced += 1;
                if let Some(fo) = fo { ratios.push(fo / fi / want); }
            }
        }
        ratios.sort_by(|a, b| a.partial_cmp(b).unwrap());
        let median = ratios.get(ratios.len() / 2).copied().unwrap_or(0.0);
        let within = ratios.iter().filter(|r| (**r - 1.0).abs() < 0.03).count() as f32 / voiced.max(1) as f32;
        println!("{:>5}/{:>4}st {:>7} {:>9.3} {:>8.0}% {:>8.2} {:>7.2}", st, fm, voiced, median, within * 100.0,
            analysis::rms(&y) / analysis::rms(&x), analysis::peak(&y));
        if std::env::var_os("VC_DIST").is_some() {
            let pct = |q: f32| ratios.get(((ratios.len() as f32 - 1.0) * q) as usize).copied().unwrap_or(0.0);
            let oct = ratios.iter().filter(|r| (**r - 2.0).abs() < 0.1 || (**r - 0.5).abs() < 0.05).count();
            let near = ratios.iter().filter(|r| (**r - 1.0).abs() < 0.08).count();
            println!("      p10 {:.3} p25 {:.3} p75 {:.3} p90 {:.3} | within 8%: {} / octave errors: {} / no f0: {}",
                pct(0.1), pct(0.25), pct(0.75), pct(0.9), near, oct, voiced - ratios.len());
        }
        if let Some(d) = &out_dir {
            let name = format!("{}_p{}_f{}.wav", path.file_stem().unwrap().to_str().unwrap(), st, fm);
            offline::save_wav(&d.join(name), &y, rate, WavFormat::Pcm16).unwrap();
        }
    }
}
