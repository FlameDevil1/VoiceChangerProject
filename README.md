# Voice Changer

Real-time voice changer for Windows 10/11 that feeds a virtual microphone (VB-CABLE), so
Discord, games, OBS and Zoom hear the processed voice.

**Status: steps 1–7 of the [build order](SPEC.md#build-order).** Audio pipeline, virtual cable
output, offline renderer, limiter, eight effects (noise suppression, noise gate, pitch & formant,
robot, equalizer, compressor, reverb, radio/telephone), presets, Simple/Advanced modes, global
hotkeys (keyboard or mouse buttons), a tray icon, a Test button and a live spectrum.

## Requirements

- Windows 10/11 x64
- [VB-CABLE](https://vb-audio.com/Cable/) (free) for the virtual microphone. The app works
  without it but other programs won't hear the changed voice.

## Build

Needs Rust (MSVC toolchain) and the Visual Studio 2022 Build Tools (C++ workload). Builds run on
any 64-bit CPU and use AVX2 automatically where available (see `.cargo/config.toml` for a
machine-tuned build).

```bash
cargo build --release
```

The binary is `target/release/voicechanger.exe`.

## Use

1. Start the app and pick your microphone.
2. Virtual mic: select **CABLE Input** (auto-selected if VB-CABLE is installed).
3. Press **Start**.
4. In Discord/OBS/your game, select **CABLE Output** as the microphone.
5. Optional: tick **Hear myself** to monitor through your headphones.
6. Pick a voice in **Simple** mode, or switch to **Advanced** to tweak effects and save your
   own presets (stored in `%APPDATA%\VoiceChanger\presets`, shareable via Export/Import).
7. Hotkeys work while gaming: **Ctrl+Alt+V** effects on/off, **Ctrl+Alt+Page Down/Up** next or
   previous preset, **Ctrl+Alt+N** normal voice, **Ctrl+Alt+M** mute. Change them, or add a
   hold-to-use key, under Advanced → Hotkeys & tray. Closing the window keeps it in the tray.
8. **Test my voice** records 5 seconds (the virtual mic is muted meanwhile); then play it back as
   the changed voice (re-rendered with your current settings, so tweak and replay) or the original.
9. Problems? Advanced → Help & diagnostics → **Copy diagnostics**, and paste it into an issue.
   **Back up settings** saves your settings and presets to one file.

## Releases

Tag a version that matches `Cargo.toml` and push the tag; GitHub Actions tests, builds and
publishes a zip with `voicechanger.exe` and `vcrender.exe`:

```bash
git tag v0.1.0
```

```bash
git push origin v0.1.0
```

Settings and logs live in `%APPDATA%\VoiceChanger\`.

## Develop

```bash
cargo test
cargo clippy --all-targets
cargo run --release --example smoke
```

The `smoke` example lists devices, and with arguments runs the engine headless against real
hardware: `smoke 10` sends the default mic to the cable for 10 s; `smoke 10 Speakers` uses a
real output with the voice muted.

## Render files

```bash
cargo run --release --bin vcrender -- voice.mp3              # -> voice_vc.wav
cargo run --release --bin vcrender -- *.wav -o out --pcm16   # batch, parallel
cargo run --release --bin vcrender -- in.wav --pitch -5 --formant -3   # deeper voice
cargo run --release --bin vcrender -- in.wav --fx denoise --fx reverb.decay=2.5 --fx radio
cargo run --release --bin vcrender -- --list-fx                        # effects and parameters
```

## Tests

`cargo test` runs unit tests, behavioural tests for every effect (`tests/effects.rs`), a
real-time safety test that fails if the audio path ever allocates (`tests/no_alloc.rs`), and
golden-file regression tests (`tests/golden.rs`). Test inputs
are generated in code, so no recordings are needed. When a golden check fails, the test writes
`tests/golden/<case>.actual.wav` next to the golden so you can listen to both. If the change is
intended, re-bless with:

```bash
VC_UPDATE_GOLDEN=1 cargo test --test golden
```

Render speed: `cargo test --release --test golden -- --ignored --nocapture`.

Pitch diagnostics: `cargo run --release --example pitch_diag` (pitch accuracy, formant movement,
loudness), `--example formant_diag` (formant peak table), and
`--example speech_check -- speech.wav [out_dir]` (frame-by-frame accuracy on a real recording).

Layout:

| Path | What |
|---|---|
| `src/audio/engine.rs` | Controller thread, capture/output callbacks, reconnect |
| `src/audio/shared.rs` | Lock-free state shared between audio, controller and UI |
| `src/audio/devices.rs` | Enumeration, device lookup, virtual cable detection |
| `src/dsp/drift.rs` | Clock-drift-compensating resampler |
| `src/dsp/mod.rs` | `EngineCore` (gain, chain, bypass, limiter) and the `Processor` trait |
| `src/dsp/chain.rs` | Effect slots, chain, `FxSettings` (serialisable) / `FxParams` (atomic) |
| `src/dsp/pitch.rs` | YIN tracker and causal PSOLA engine (pitch, formant, monotone) |
| `src/dsp/fx/` | One file per effect: its spec table (controls, presets) and processor |
| `src/dsp/params.rs` | Spec types, atomic `EffectParams`, serialisable `EffectSettings` |
| `src/dsp/biquad.rs` | RBJ cookbook filters |
| `src/dsp/limiter.rs` | Lookahead brickwall limiter |
| `src/offline/` | File I/O, offline render, test signals, analysis (RMS, SNR, pitch) |
| `src/bin/vcrender.rs` | Command-line file renderer |
| `tests/golden.rs` | Golden-file regression harness |
| `src/gui/` | egui window: `mod.rs` (layout, sections), `effects.rs`, `presets.rs`, `widgets.rs` |
| `src/presets.rs` | Built-in presets, user preset store (save/rename/delete/import/export) |
| `src/hotkeys.rs` | Hotkey bindings, key matcher, Windows keyboard hooks |
| `src/toast.rs`, `src/single_instance.rs` | On-screen toast (Win32), one-instance guard |
| `src/gui/tray.rs`, `src/gui/system.rs` | Tray icon/menu; hotkey/tray handling and settings |
| `src/gui/test_voice.rs`, `src/gui/spectrum.rs` | Test button; live spectrum |
| `src/gui/help.rs`, `src/backup.rs` | Diagnostics report, settings backup/restore |
| `src/audio/playback.rs`, `src/dsp/fft.rs` | One-shot clip playback; FFT for the spectrum |
| `src/dsp/simd.rs` | Runtime CPU feature dispatch (AVX2 when available) |
| `SPEC.md` | Full spec, architecture and latency budget |

## License

MIT. See [LICENSE](LICENSE).
