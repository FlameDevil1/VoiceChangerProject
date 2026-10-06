# Voice Changer

Real-time voice changer for Windows 10/11 that feeds a virtual microphone (VB-CABLE), so
Discord, games, OBS and Zoom hear the processed voice.

**Status: steps 1–2 of the [build order](SPEC.md#build-order).** The audio pipeline, device
handling, virtual cable output, GUI shell and offline file renderer work; voice effects come next.

## Requirements

- Windows 10/11 x64
- [VB-CABLE](https://vb-audio.com/Cable/) (free) for the virtual microphone. The app works
  without it but other programs won't hear the changed voice.

## Build

Needs Rust (MSVC toolchain) and the Visual Studio 2022 Build Tools (C++ workload). Builds target
AVX2 CPUs (`.cargo/config.toml`); remove that file for a generic x86-64 build.

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
```

## Tests

`cargo test` runs unit tests plus golden-file regression tests (`tests/golden.rs`). Test inputs
are generated in code, so no recordings are needed. When a golden check fails, the test writes
`tests/golden/<case>.actual.wav` next to the golden so you can listen to both. If the change is
intended, re-bless with:

```bash
VC_UPDATE_GOLDEN=1 cargo test --test golden
```

Render speed: `cargo test --release --test golden -- --ignored --nocapture`.

Layout:

| Path | What |
|---|---|
| `src/audio/engine.rs` | Controller thread, capture/output callbacks, reconnect |
| `src/audio/shared.rs` | Lock-free state shared between audio, controller and UI |
| `src/audio/devices.rs` | Enumeration, device lookup, virtual cable detection |
| `src/dsp/drift.rs` | Clock-drift-compensating resampler |
| `src/dsp/mod.rs` | `EngineCore` (per-block processing) and the `Processor` trait for effects |
| `src/offline/` | File I/O, offline render, test signals, analysis (RMS, SNR, pitch) |
| `src/bin/vcrender.rs` | Command-line file renderer |
| `tests/golden.rs` | Golden-file regression harness |
| `src/gui.rs` | egui window |
| `SPEC.md` | Full spec, architecture and latency budget |
