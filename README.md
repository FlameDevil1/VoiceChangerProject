# Voice Changer

Change your voice live in Discord, games, OBS or Zoom. Free, for Windows 10 and 11.

**[⬇ Download the latest version](https://github.com/FlameDevil1/VoiceChangerProject/releases/latest)**

## Get started

1. **Install [VB-CABLE](https://vb-audio.com/Cable/)** (free). It's the virtual microphone that
   other apps listen to. If it's missing, Voice Changer shows you how to set it up.
2. **Download and run `VoiceChanger-Setup-…exe`** from the link above. No administrator rights
   needed.
   - Windows may say *"Windows protected your PC"* because the app isn't code-signed yet.
     Click **More info**, then **Run anyway**.
3. **Open Voice Changer**, pick your microphone and press **Start**.
4. **In Discord, your game or OBS**, choose **CABLE Output** as your microphone.
5. **Click a voice.** To hear yourself, tick **Hear myself** (use headphones).

Prefer not to install? Download the **portable zip** instead, unzip it anywhere and run
`voicechanger.exe`.

## What it can do

- **17 ready-made voices**: deep, female, child, robot, monster, alien, ghost, auto-tune and more.
  Fine-tune any of them, roll random ones, and save your own.
- **Clean up your mic**: noise suppression and a noise gate.
- **Fake a bad mic or connection**: cheap headset, laggy Wi-Fi, tunnel, broken cable. Press
  **Glitch now** for an instant stutter. Only others hear the lag; you don't.
- **Record** what others hear, or **give audio files your voice** (WAV or MP3, with an optional
  speed change).
- **Hotkeys that work in games** (keyboard or mouse side buttons), a tray icon and small
  on-screen messages.
- **Light on your PC**: every effect at once uses about 3 % of one CPU core.

## Handy tips

| Hotkey | Does |
|---|---|
| Ctrl+Alt+V | Voice effects on/off |
| Ctrl+Alt+Page Down / Page Up | Next / previous voice |
| Ctrl+Alt+N | Your normal voice |
| Ctrl+Alt+M | Mute the virtual mic |

- Change hotkeys, add a key you hold for the changed voice, or turn on **Start with Windows** under **Advanced →
  Hotkeys, tray & startup**.
- Closing the window keeps the app running in the tray. Right-click the tray icon to quit.
- **Test my voice** records 5 seconds and plays them back with your current voice, so you can
  tweak before anyone hears it.
- Ctrl+Z undoes the last voice click or Randomize.

## Problems?

- **Others can't hear the changed voice**: in Voice Changer, **Virtual mic** should be
  **CABLE Input**; in the other app, the microphone should be **CABLE Output**.
- **Hotkeys don't work in a game**: the game probably runs as administrator. Voice Changer
  tells you when this happens; run Voice Changer as administrator too, or use the tray icon.
- **Mic is silent**: it may be muted, or Windows may be blocking microphone access. The app
  shows a warning with a button that opens the right settings page.
- **Something else**: go to **Advanced → Help & diagnostics → Copy diagnostics** and paste it
  into a [new issue](https://github.com/FlameDevil1/VoiceChangerProject/issues/new).

To uninstall, use Windows **Settings → Apps**. It asks whether to also delete your settings and
saved voices. Recordings in your Music folder are kept.

---

## For developers

Everything below is for building and working on the code.

### Build

Needs Rust (MSVC toolchain) and the Visual Studio 2022 Build Tools (C++ workload). Builds run on
any 64-bit CPU and use AVX2 automatically where available (see `.cargo/config.toml` for a
machine-tuned build).

```bash
cargo build --release
```

The app is `target/release/voicechanger.exe`. Settings, presets and logs live in
`%APPDATA%\VoiceChanger\`.

### Command-line file renderer

```bash
cargo run --release --bin vcrender -- voice.mp3              # -> voice_vc.wav
cargo run --release --bin vcrender -- *.wav -o out --pcm16   # batch, parallel
cargo run --release --bin vcrender -- in.wav --pitch -5 --formant -3   # deeper voice
cargo run --release --bin vcrender -- in.wav --fx denoise --fx reverb.decay=2.5 --fx radio
cargo run --release --bin vcrender -- talk.wav --speed 1.25 --mp3   # faster, same pitch, MP3
cargo run --release --bin vcrender -- --list-fx                        # effects and parameters
```

### Tests

```bash
cargo test
cargo clippy --all-targets
```

`cargo test` runs unit tests, behavioural tests for every effect (`tests/effects.rs`), a
real-time safety test that fails if the audio path ever allocates (`tests/no_alloc.rs`), and
golden-file regression tests (`tests/golden.rs`). Test inputs are generated in code, so no
recordings are needed. When a golden check fails, the test writes
`tests/golden/<case>.actual.wav` next to the golden so you can listen to both. If the change is
intended, re-bless with:

```bash
VC_UPDATE_GOLDEN=1 cargo test --test golden
```

More tools:

- Render speed: `cargo test --release --test golden -- --ignored --nocapture`.
- Pitch diagnostics: `cargo run --release --example pitch_diag` (pitch accuracy, formant
  movement, loudness), `--example formant_diag` (formant peak table), and
  `--example speech_check -- speech.wav [out_dir]` (frame-by-frame accuracy on a real recording).
- Loudness per voice: `--example preset_levels`. CPU cost per effect: `--example effect_cost`.
- `cargo run --release --example smoke` lists devices; `smoke 10` runs the engine headless for
  10 s against real hardware (`smoke 10 Speakers` uses a real output with the voice muted).

### Releases

CI (`.github/workflows/ci.yml`) checks formatting, runs clippy and the tests on both CPU paths,
and builds and test-installs the installer on every push. To publish a release, set the version
in `Cargo.toml`, add notes in `.github/release-notes/vX.Y.Z.md`, then tag and push:

```bash
git tag v0.2.0
```

```bash
git push origin v0.2.0
```

GitHub Actions publishes `VoiceChanger-Setup-X.Y.Z.exe` (Inno Setup, `installer/`) and a
portable zip.

### Code layout

| Path | What |
|---|---|
| `src/audio/engine.rs` | Controller thread, capture/output callbacks, reconnect |
| `src/audio/shared.rs` | Lock-free state shared between audio, controller and UI |
| `src/audio/devices.rs` | Enumeration, device lookup, virtual cable detection |
| `src/dsp/mod.rs` | `EngineCore` (gain, chain, bypass, limiter) and the `Processor` trait |
| `src/dsp/chain.rs` | Effect slots, chain, `FxSettings` (serialisable) / `FxParams` (atomic) |
| `src/dsp/fx/` | One file per effect: its spec table (controls, presets) and processor |
| `src/dsp/pitch.rs` | YIN tracker and causal PSOLA engine (pitch, formant, auto-tune) |
| `src/dsp/drift.rs` | Clock-drift-compensating resampler |
| `src/offline/` | File I/O, export pipeline, MP3, test signals, analysis |
| `src/presets.rs` | Built-in voices and scenarios, user preset store |
| `src/hotkeys.rs` | Hotkey bindings, key matcher, Windows keyboard and mouse hooks |
| `src/autostart.rs`, `src/elevation.rs` | Start with Windows; admin-app hotkey warning |
| `src/gui/` | egui window, one file per section; tray, toasts, recorder, file batch |
| `src/bin/vcrender.rs` | Command-line file renderer |
| `installer/` | Inno Setup script, build and silent install test |
| `SPEC.md` | Full spec, architecture, latency budget and build order |

## License

MIT. See [LICENSE](LICENSE).
