# Voice Changer for Windows: Project Spec (rev 2)

Revision 2 folds in the architecture review: decisions filled in, real-time rules made explicit,
overlapping effects merged into shared building blocks, and the build order changed so offline
rendering (and therefore DSP testing) comes early.

## Decisions

| Decision | Choice | Why |
|---|---|---|
| Language / framework | **Rust**; `cpal` (WASAPI) for audio, `egui`/`eframe` (OpenGL) for the UI | No GC pauses in the audio thread, memory-safe, single small `.exe`. |
| UI renderer | `glow` (OpenGL), not `wgpu` | Measured: same CPU cost, ~25 % of the memory (92 MB vs 375 MB private). |
| Licensing | **Undecided**; keep every dependency permissive (MIT/BSD/Apache) until it is | Leaves both free and paid open. Rules out GPL Rubber Band unless licensed. |
| Pitch / formant | Signalsmith Stretch (MIT) or an in-house WSOLA/PSOLA + LPC envelope | Low latency (time-domain); no GPL. |
| Noise suppression | `nnnoiseless` (pure-Rust RNNoise, BSD) | 10 ms frames, 48 kHz native, CPU-only. |
| Internal sample rate | The mic's native rate (48 kHz on almost every Windows device) | WASAPI shared-mode capture only accepts the native format; outputs are opened at the same rate and Windows converts if needed. |
| Target latency | **≤ 40 ms app-side** in Balanced mode, measured and shown in the UI | See latency budget below. |
| AI voice conversion in v1 | **No** | Needs a GPU and adds 100–300 ms. Architecture leaves room for it later. |
| v1 must-haves | I/O + VB-CABLE, pitch/formant, gate + noise suppression, EQ, compressor/limiter, reverb, robot, radio, presets, hotkeys | |

## Architecture

```
 mic ──WASAPI──► capture callback (MMCSS "Pro Audio")
                 downmix → EngineCore (gain, effect chain, limiter)
                    │                         │
                 ring (lock-free)          ring (lock-free)
                    ▼                         ▼
   cable callback: DriftResampler    monitor callback: DriftResampler
        │                                     │
   "CABLE Input" (VB-CABLE)            headphones
```

- **The capture callback is the master clock.** Each output reads its own ring through a
  `DriftResampler`: a 4-point cubic interpolator whose ratio is steered by a PI controller that
  holds the ring at a target fill. This absorbs the clock drift between independent devices
  (mic, headphones, virtual cable) that otherwise causes periodic clicks or growing delay.
  Corrections are capped at ±0.3 % (inaudible).
- **Controller thread** owns all cpal streams, handles UI commands, enumerates devices, and
  rebuilds streams when a device disappears. The UI thread never touches a device.
- **Outputs hot-swap**: turning monitoring on/off or changing the cable device sends a new ring
  producer to the running capture callback through a lock-free queue. The other output never
  glitches. Old producers are returned and freed off the audio thread.
- **Per-stream recovery**: if the headphones are unplugged only the monitor stream is retried;
  the virtual mic keeps working. If the mic disappears the engine retries once a second.

### Real-time rules (enforced in review)

- No allocation, locks, file I/O or logging in any audio callback. Scratch buffers are sized
  up front (`MAX_BLOCK`); larger callbacks are split.
- UI → audio: atomics only (`Shared`). Audio → UI: atomics (peaks, load, ring stats).
- Every parameter change is smoothed (20 ms linear ramp): no zipper noise, no clicks on toggles.
- Effect-chain changes are built off-thread and swapped in atomically (step 3).
- Effects with latency report it via `Processor::latency()`. The chain delays the dry path by the
  same amount so wet/dry mixing doesn't comb-filter, including while bypassed.
- Audio callbacks are wrapped in `catch_unwind`: a DSP bug silences that stream and triggers a
  rebuild instead of killing the app.

### Latency budget (Balanced mode, 48 kHz, 10 ms device periods)

| Stage | Typical |
|---|---|
| WASAPI capture period | 10 ms |
| Ring target: ½ input block + output block + margin (6 ms) | 21 ms |
| WASAPI render buffer | 10 ms |
| **App-side total** | **≈ 41 ms** (Low mode ≈ 37 ms) |
| VB-CABLE + receiving app | outside our control |

Margins grow by 1 ms automatically after each underrun (capped at 50 ms), so "Low" is safe to
try. Future reduction: IAudioClient3 low-latency shared-mode periods (2.67–5 ms) or exclusive
mode. Both need WASAPI code beyond what cpal provides.

### Resource policy

- Audio engine: measured ~0.1–0.3 % of one core for pass-through.
- UI redraws only on input, status changes, or while running: **30 fps focused, 10 fps in the
  background, 0 when minimized or covered**. "Low-power UI" lowers this to 12/5 fps. Measured on a
  laptop iGPU: ~0.45 % of one core per fps, 0 % when stopped.
- Device list refreshes when the window regains focus and every 5 s while focused, not in the
  background. Reconnect polling (1 s) runs only while a device is missing.

## 1. Core audio pipeline (MVP): **done (step 1)**

- Input device dropdown with "Windows default", auto-refresh on focus, remembered by stable ID
  with name fallback (IDs can change when USB devices move ports).
- Mic channel selector (mix / left / right) for interfaces that put the mic on one channel.
- Monitor output selector + "Hear myself" toggle (click-free fade, stream closed when off).
- Latency mode Low / Balanced / Safe (replaces a raw buffer-size setting: in WASAPI shared mode
  the callback period is fixed by the device, so the buffer setting only ever changed our margin).
- Input/output peak meters with peak-hold and clip indicator; DSP load; glitch counter;
  measured path latency.
- Bypass (hear original) and Mute (virtual mic) toggles, both crossfaded.

## 2. Virtual microphone output: **done (step 1)**

- Writes to VB-CABLE's playback side ("CABLE Input"); auto-selected on first run if installed.
  Also recognises Hi-Fi Cable, CABLE-A/B and Voicemeeter inputs.
- Setup helper when no cable is found: steps, download link, Rescan. VB-CABLE is linked, never
  bundled (redistribution needs a licence from VB-Audio).
- Indicator: "Feeding CABLE Input (…)" plus which device to pick in Discord ("CABLE Output").
- Warns if the selected mic is the cable's own output (feedback loop) or if monitoring is routed
  into the cable.

## 3. Effects, as building blocks

Named effects in the original spec are mostly presets over a smaller set of DSP blocks:

| Block | Covers |
|---|---|
| Pitch + formant | Pitch shift, Formant, Monster/Deep, Chipmunk/Helium, Girl/Boy |
| Parametric EQ (with macro sliders) | 5-band EQ, Tone/Brightness, Warmth, Presence, Nasality |
| Band-pass | Radio/Telephone, cheap-mic bandwidth limiting |
| Reverb (room/hall/cave) | Reverb presets, room echo / hollow sound, speakerphone |
| Delay | Echo, chorus/flanger/doubling (modulated), stereo width |
| Waveshaper | Distortion/overdrive, clipping, roughness/growl |
| Noise excitation mix | Whisper, breathiness, hiss/pink noise |
| Vocoder / ring mod | Robot |
| Pitch tracker + snap | Auto-tune, intonation, vibrato |
| Dynamics | Noise gate, compressor, limiter (pinned last), auto-gain |
| Noise suppression | RNNoise |
| Network buffer (modes) | Lag spikes, stutter/packet loss, freeze/loop, choppy cut-outs, dropout, jitter |
| Bit-crush / downsample | Low-bitrate codec |
| Event generators | Hum, crackle, handling thumps, wind, plosive pops, gain drift/pumping |

Fixed chain order: gate → noise suppression → pitch/formant → character → EQ → compressor →
bad-mic/connection → **limiter (always last)**.

### 3A. Modulation sliders

As specified (value display, typed input, reset per slider, randomize with locks). Changes:
- **Speed / time stretch is file-only.** Real-time speed-up is impossible (audio can't play
  faster than it arrives) and slow-down accumulates delay without bound.
- Every random effect uses a **seeded RNG** so presets and tests are reproducible.

### 3B. Bad mic & bad connection

As specified, plus:
- **Monitor tap point: pre or post bad-connection effects, default pre.** Hearing your own voice
  delayed by 0.2–2 s (delayed auditory feedback) makes it very hard to keep talking.
- Delay build-up/catch-up reuses the pitch engine's time-stretcher.
- Buffers allocated only while enabled; full bypass when off; crossfaded switching (unchanged).

## 4. Presets

As specified. Preset JSON carries a `schemaVersion` so older presets keep loading after updates.

## 5. Hotkeys and quick controls

As specified. Push-to-talk "effect while held" needs key-up events, so it uses a low-level
keyboard hook (`WH_KEYBOARD_LL`); toggles can use `RegisterHotKey`.

## 6. User interface

Done in step 1: single window, dark/light/system theme, simple device and level panels.
Later: simple vs advanced mode, spectrum visualizer (FFT on the UI side from a ring copy, capped
at the meter frame rate), Test button, tray icon, start minimized, launch on startup.

## 7. File processing (offline)

**Moved up to step 2.** Runs the same `EngineCore` over files, which gives deterministic DSP unit
tests (golden files, null tests), the Test button, and file export from one code path.
Decoding via Symphonia (WAV/MP3/FLAC/OGG), WAV via `hound`, MP3 export via LAME (patents expired).

## 8. Settings

- Stored in `%APPDATA%\VoiceChanger\config.json`, written atomically (temp file + rename),
  missing fields fall back to defaults.
- Remembers devices, gains, latency mode, theme, monitoring, and whether audio was running.
- Auto-reconnect when a device is unplugged and replugged: done.
- **Removed from v1:** ASIO (rare among voice-chat users, extra SDK licensing) and WASAPI
  exclusive mode (cpal is shared-mode only; revisit together with low-latency periods).

## 9. Quality and reliability

- Engine on its own threads; controller separate from UI; panics in callbacks are contained.
- Log file `%APPDATA%\VoiceChanger\voicechanger.log` (+ previous run), panics included.
- Unit tests for DSP, drift control (simulated drifting clocks, 2 min, ±0.05 %), config, and
  device-name detection. `examples/smoke.rs` drives the real engine headless against real devices.

## 10. Packaging

As specified. Release profile: thin LTO, stripped, ~8 MB `.exe`, no console window. Installer
should check for VB-CABLE and link to it, not bundle it.

## Build order

1. ✅ Audio I/O, device selection, pass-through, meters, virtual cable output, drift
   compensation, reconnect, settings, GUI shell.
2. Offline renderer (`EngineCore` over files) + golden-file test harness.
3. Effect chain infrastructure (atomic chain swap, latency compensation, limiter) + pitch/formant.
4. Core effects: gate, RNNoise, EQ, compressor, reverb, robot, radio.
5. Modulation sliders (3A).
6. Bad mic / bad connection (3B), monitor tap point.
7. Presets (save/load/import/export, scenario presets).
8. Global hotkeys, tray icon, toasts.
9. UI polish: visualizer, simple/advanced mode, Test button.
10. File processing UI (batch, MP3 export, record-with-effects).
11. Installer, startup options, stretch features.

## Stretch / future

Unchanged from rev 1 (AI voice conversion, soundboard, ambience, per-app routing, VST plugins,
pitch-adaptive effects, per-app profiles). Heavy effects such as AI conversion should run on a
worker thread with larger blocks, fed by its own ring, so they can't stall the capture callback.
