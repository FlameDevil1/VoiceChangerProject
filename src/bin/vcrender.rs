//! Command-line file renderer: applies the voice changer chain to audio files.
//!
//!   vcrender voice.mp3                       -> voice_vc.wav next to the input
//!   vcrender a.wav b.flac -o out\            -> batch, rendered in parallel
//!   vcrender in.wav -o out.wav --in-gain -3 --pcm16

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Instant;
use voice_changer::dsp::{CoreParams, EffectKind, FxSettings, db_to_gain};
use voice_changer::offline::{self, WavFormat};

const USAGE: &str = "\
Usage: vcrender [options] <input>...

Applies the voice changer chain to audio files (WAV, MP3, FLAC, OGG).

Options:
  -o, --out <path>      Output file (one input) or directory (several inputs).
                        Default: <input>_vc.wav next to each input.
      --in-gain <dB>    Input gain (default 0)
      --out-gain <dB>   Output gain (default 0)
      --pitch <st>      Pitch shift in semitones, -12..12 (enables pitch & formant)
      --formant <st>    Formant shift in semitones, -12..12 (enables pitch & formant)
      --fx <effect>[.<param>=<value>]
                        Enable an effect, optionally setting one parameter or its mix.
                        Repeatable. Examples: --fx reverb  --fx reverb.decay=3
                        --fx reverb.mix=0.4  --fx robot  --fx denoise
      --list-fx         List effects and their parameters
      --bypass          Skip effects (gains still apply)
      --channel <c>     mix | left | right (default mix)
      --pcm16           Write 16-bit PCM instead of 32-bit float
      --block <frames>  Processing block size (default 480, same as live)
  -j, --jobs <n>        Parallel files (default: all cores)
  -h, --help            Show this help";

struct Opts {
    inputs: Vec<PathBuf>,
    out: Option<PathBuf>,
    params: CoreParams,
    fx: FxSettings,
    channel: Option<usize>,
    format: WavFormat,
    block: usize,
    jobs: usize,
}

fn parse() -> Result<Opts, String> {
    let mut o = Opts {
        inputs: Vec::new(),
        out: None,
        params: CoreParams::default(),
        fx: FxSettings::default(),
        channel: None,
        format: WavFormat::Float32,
        block: offline::DEFAULT_BLOCK,
        jobs: std::thread::available_parallelism().map(|n| n.get()).unwrap_or(4),
    };
    let mut args = std::env::args().skip(1);
    let num = |v: Option<String>, flag: &str| -> Result<f32, String> {
        v.and_then(|s| s.parse().ok()).ok_or_else(|| format!("{flag} needs a number"))
    };
    while let Some(a) = args.next() {
        match a.as_str() {
            "-h" | "--help" => return Err(String::new()),
            "-o" | "--out" => o.out = Some(args.next().ok_or("--out needs a path")?.into()),
            "--in-gain" => o.params.input_gain = db_to_gain(num(args.next(), &a)?),
            "--out-gain" => o.params.output_gain = db_to_gain(num(args.next(), &a)?),
            "--pitch" => {
                let v = num(args.next(), &a)?;
                o.fx = std::mem::take(&mut o.fx).with(EffectKind::Pitch, &[("semitones", v)]);
            }
            "--formant" => {
                let v = num(args.next(), &a)?;
                o.fx = std::mem::take(&mut o.fx).with(EffectKind::Pitch, &[("formant", v)]);
            }
            "--fx" => parse_fx(&mut o.fx, &args.next().ok_or("--fx needs an effect")?)?,
            "--list-fx" => {
                list_fx();
                std::process::exit(0);
            }
            "--bypass" => o.params.bypass = true,
            "--pcm16" => o.format = WavFormat::Pcm16,
            "--block" => o.block = num(args.next(), &a)?.max(1.0) as usize,
            "-j" | "--jobs" => o.jobs = num(args.next(), &a)?.max(1.0) as usize,
            "--channel" => {
                o.channel = match args.next().as_deref() {
                    Some("mix") => None,
                    Some("left") => Some(0),
                    Some("right") => Some(1),
                    _ => return Err("--channel must be mix, left or right".into()),
                }
            }
            s if s.starts_with('-') => return Err(format!("unknown option {s}")),
            _ => o.inputs.push(a.into()),
        }
    }
    if o.inputs.is_empty() {
        return Err("no input files".into());
    }
    Ok(o)
}

/// `reverb`, `reverb.decay=3` or `reverb.mix=0.4`.
fn parse_fx(fx: &mut FxSettings, arg: &str) -> Result<(), String> {
    let (name, assign) = match arg.split_once('.') {
        Some((n, rest)) => (n, Some(rest)),
        None => (arg, None),
    };
    let kind = EffectKind::from_key(name).ok_or_else(|| format!("unknown effect '{name}' (see --list-fx)"))?;
    fx.set_enabled(kind, true);
    if let Some(assign) = assign {
        let (key, value) = assign.split_once('=').ok_or_else(|| format!("expected {name}.<param>=<value>"))?;
        let v: f32 = value.parse().map_err(|_| format!("{arg}: '{value}' is not a number"))?;
        if key == "mix" {
            fx.set_mix(kind, v);
        } else if !fx.set(kind, key, v) {
            return Err(format!("{name} has no parameter '{key}' (see --list-fx)"));
        }
    }
    Ok(())
}

fn list_fx() {
    for kind in EffectKind::ALL {
        let spec = kind.spec();
        println!("{}  ({}; mix default {})", kind.key(), spec.label, spec.default_mix);
        for p in spec.params {
            println!("    {:<10} {:>7} .. {:<7} default {:<6} {}", p.key, p.min, p.max, p.default, p.unit.trim());
        }
    }
}

fn output_path(input: &Path, out: Option<&Path>, many: bool, with_ext: bool) -> PathBuf {
    let stem = input.file_stem().and_then(|s| s.to_str()).unwrap_or("output");
    let name = match input.extension().and_then(|e| e.to_str()) {
        Some(ext) if with_ext => format!("{stem}_{ext}_vc.wav"),
        _ => format!("{stem}_vc.wav"),
    };
    match out {
        Some(o) if many || o.is_dir() => o.join(name),
        Some(o) => o.to_path_buf(),
        None => input.with_file_name(name),
    }
}

/// Output path for every input. Inputs sharing a name (voice.wav, voice.mp3) get the extension
/// added so parallel workers never write the same file; an output may never overwrite an input.
fn plan_outputs(o: &Opts) -> Result<Vec<PathBuf>, String> {
    let many = o.inputs.len() > 1;
    let plain: Vec<PathBuf> = o.inputs.iter().map(|i| output_path(i, o.out.as_deref(), many, false)).collect();
    let outputs: Vec<PathBuf> = o
        .inputs
        .iter()
        .zip(&plain)
        .map(|(i, p)| {
            let clash = plain.iter().filter(|q| *q == p).count() > 1;
            if clash { output_path(i, o.out.as_deref(), many, true) } else { p.clone() }
        })
        .collect();
    let key = |p: &Path| std::fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf());
    for (k, out) in outputs.iter().enumerate() {
        if outputs[..k].iter().any(|prev| prev == out) {
            return Err(format!("{} is listed twice", o.inputs[k].display()));
        }
        if o.inputs.iter().any(|i| key(i) == key(out)) {
            return Err(format!("refusing to overwrite input {}", out.display()));
        }
    }
    Ok(outputs)
}

/// Returns seconds of audio rendered.
fn render_one(input: &Path, output: &Path, o: &Opts) -> Result<f64, String> {
    let t = Instant::now();
    let audio = offline::load(input)?;
    let mono = offline::to_mono(&audio, o.channel);
    let decoded = t.elapsed();
    let t = Instant::now();
    let out = offline::render(&mono, audio.rate, o.params, &o.fx, o.block);
    let rendered = t.elapsed();
    offline::save_wav(output, &out, audio.rate, o.format)?;
    let secs = audio.duration_secs();
    println!(
        "{} -> {}  ({secs:.1} s, {} Hz, {} ch; decode {:.0} ms, process {:.1} ms = {:.0}x real time)",
        input.display(),
        output.display(),
        audio.rate,
        audio.channels,
        decoded.as_secs_f64() * 1000.0,
        rendered.as_secs_f64() * 1000.0,
        secs / rendered.as_secs_f64().max(1e-9),
    );
    Ok(secs)
}

fn main() {
    let o = match parse() {
        Ok(o) => o,
        Err(e) => {
            if !e.is_empty() {
                eprintln!("error: {e}\n");
            }
            eprintln!("{USAGE}");
            std::process::exit(if e.is_empty() { 0 } else { 2 });
        }
    };
    let many = o.inputs.len() > 1;
    if let (true, Some(dir)) = (many, &o.out)
        && let Err(e) = std::fs::create_dir_all(dir)
    {
        eprintln!("error: {}: {e}", dir.display());
        std::process::exit(1);
    }
    let outputs = match plan_outputs(&o) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("error: {e}");
            std::process::exit(2);
        }
    };

    // Simple work queue: each worker takes the next file. Files are independent, so a batch
    // scales across all cores.
    let start = Instant::now();
    let next = AtomicUsize::new(0);
    let failed = AtomicUsize::new(0);
    let total_audio = std::sync::Mutex::new(0.0f64);
    std::thread::scope(|s| {
        for _ in 0..o.jobs.min(o.inputs.len()) {
            s.spawn(|| {
                loop {
                    let i = next.fetch_add(1, Ordering::Relaxed);
                    let Some(input) = o.inputs.get(i) else { break };
                    match render_one(input, &outputs[i], &o) {
                        Ok(secs) => *total_audio.lock().unwrap() += secs,
                        Err(e) => {
                            eprintln!("error: {e}");
                            failed.fetch_add(1, Ordering::Relaxed);
                        }
                    }
                }
            });
        }
    });

    let failed = failed.into_inner();
    if many {
        let wall = start.elapsed().as_secs_f64();
        let audio = total_audio.into_inner().unwrap();
        println!(
            "{} of {} files, {audio:.1} s of audio in {wall:.2} s ({:.0}x real time overall)",
            o.inputs.len() - failed,
            o.inputs.len(),
            audio / wall.max(1e-9)
        );
    }
    std::process::exit(if failed > 0 { 1 } else { 0 });
}
