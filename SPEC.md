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
| Pitch / formant | **In-house causal TD-PSOLA** (step 3) | ~0.06 ms algorithmic latency vs 20–60 ms for FFT/stretcher approaches; formants preserved by design; no third-party licence. |
| Noise suppression | `nnnoiseless` (pure-Rust RNNoise, BSD) | 10 ms frames, 48 kHz native, CPU-only. 40+ dB on fan/rumble noise; weak on flat white hiss (use the gate for that). |
| Internal sample rate | The mic's native rate (48 kHz on almost every Windows device) | WASAPI shared-mode capture only accepts the native format; outputs are opened at the same rate and Windows converts if needed. |
| Target latency | **≤ 40 ms app-side** in Balanced mode, measured and shown in the UI | See latency budget below. |
| Target hardware | Ryzen 7 7800X3D + RTX 4070 Super (dev laptop: Ryzen 7 7735HS) | Builds run on any x86-64 CPU; the hot loop switches to AVX2 at runtime when available, with bit-identical output. The GPU makes AI voice conversion realistic as a stretch goal. |
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
- **Single instance**: a named mutex keeps one copy running; launching again signals the running
  copy (named event) to show its window, even from the tray, and exits before touching the log.
- **Monitoring safety**: "Hear myself" warns when the output is a speaker (Windows form factor),
  and reminds you to use headphones when the device type is unknown (common for HDMI monitors).
- **Per-stream recovery**: if the headphones are unplugged only the monitor stream is retried;
  the virtual mic keeps working. If the mic disappears the engine retries once a second.

### Real-time rules (enforced in review)

- No allocation, locks, file I/O or logging in any audio callback. Scratch buffers are sized
  up front (`MAX_BLOCK`); larger callbacks are split.
- UI → audio: atomics only (`Shared`). Audio → UI: atomics (peaks, load, ring stats).
- Every parameter change is smoothed (20 ms linear ramp): no zipper noise, no clicks on toggles.
- Effect chains are built and prepared off-thread, handed to the capture callback through the
  lock-free queue, and crossfaded in over 20 ms; the old chain is handed back and freed on the
  controller thread.
- Effects with latency report it via `Processor::latency()`. Each slot delays its dry signal by
  that amount, so a partial wet/dry mix never comb-filters.
- **Disabled effects are removed from the signal path**: zero CPU and zero added latency.
  On/off and bypass toggles are 20 ms crossfades against the undelayed input. A brief latency
  mismatch inside a 20 ms fade is inaudible, and it keeps "normal voice" at minimum latency.
- The first block after creation uses the requested parameter values directly (no ramp from
  defaults), so files rendered at -6 dB are at -6 dB from the first sample.
- Denormals are flushed to zero (MXCSR FTZ/DAZ) at the top of every callback and offline render.
- The always-on limiter also replaces NaN/inf with silence, so a DSP bug can't reach the cable.
- **Enforced by a test** (`tests/no_alloc.rs`): a counting global allocator runs the full engine
  with every effect on while toggling effects, moving parameters, bypassing, muting and swapping
  chains, and asserts zero heap allocations or frees. It caught one: RNNoise's FFT library builds
  a thread-local planner on first use, so each capture thread now warms it up in its first
  callback, before any audio is sent.
- Audio callbacks are wrapped in `catch_unwind`: a DSP bug silences that stream and triggers a
  rebuild instead of killing the app.

### Latency budget (Balanced mode, 48 kHz, 10 ms device periods)

| Stage | Typical |
|---|---|
| WASAPI capture period | 10 ms |
| Ring target: ½ input block + output block + margin (6 ms) | 21 ms |
| WASAPI render buffer | 10 ms |
| Limiter lookahead (always on) | 1 ms |
| Pitch & formant / robot (when on) | 0.06 ms |
| Noise suppression (when on) | 10 ms (RNNoise overlap; frames processed as they complete). Falls back to 20 ms if audio ever arrives in uneven blocks |
| **App-side total** | **≈ 42 ms** (Low mode ≈ 38 ms) |
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

Default chain order: noise suppression → gate → pitch/formant → robot → EQ → compressor →
reverb → radio → (bad-mic/connection, step 6) → **limiter (always last)**. Noise suppression
runs before the gate so the gate sees a clean signal. The order is stored per config/preset, and
configs from older versions get new effects inserted at their default position.

### Core effects: **done (step 4)**

Every effect is declared by a spec table (`src/dsp/fx/*.rs`): keys, ranges, defaults, units,
help text and presets. The GUI panel, config/preset serialisation and the `vcrender --fx` CLI are
all generated from it, so adding an effect is one module plus one line in `chain.rs`.

| Effect | Implementation | Cost* |
|---|---|---|
| Noise suppression | RNNoise; 10 ms in direct mode (20 ms fallback); 48 kHz only (passes through and warns otherwise); voice-probability readout | 0.67 % |
| Noise gate | Peak detector, 4 dB hysteresis, hold, fades linear in dB (release = time to close fully) | 0.07 % |
| Pitch & formant | Causal PSOLA (step 3), presets: deeper, higher, male↔female, child, monster, chipmunk | 0.55 % |
| Robot | PSOLA *monotone* mode (every period forced to one note) + ring mod + tuned comb | 0.54 % |
| Equalizer | 5 RBJ biquads (100 Hz shelf, 350 Hz, 1 kHz, 3 kHz, 8 kHz shelf); flat bands are skipped (bit-exact) | 0.12 % |
| Compressor | Feed-forward, 6 dB soft knee, gain-reduction readout | 0.23 % |
| Reverb | 8-line FDN with Hadamard mixing, per-line damping, RT60-accurate decay, slewed size | 0.37 % |
| Radio / telephone | 24 dB/oct band-pass, tanh drive, seeded static; telephone / AM / walkie presets | 0.19 % |

\*Share of one Ryzen 7 7735HS laptop core, live. All eight together: 2.3 %; in the live engine,
worst callback 4.5 % of its time budget.

Verified by `tests/effects.rs`: every effect and preset is finite, bounded, silent on silence and
block-size invariant; the gate removes ≥ 35 dB of background without clicks; the compressor's
gain reduction matches theory within 1 dB; EQ bands hit their gain within 0.8 dB; reverb RT60
within 35 % of the setting and stable at maximum settings; robot output stays within 4 Hz of its
note over a 120→220 Hz glide; radio rejects ≥ 30 dB outside its band; RNNoise removes ≥ 12 dB
of fan-like noise while keeping a vowel within 6 dB.

### Pitch & formant: **done (step 3)**

Causal TD-PSOLA (`src/dsp/pitch.rs`):

- **Tracking:** YIN every 5 ms on a ~16 kHz decimated copy (60–900 Hz). Falls back to the global
  minimum for breathy frames, and holds the pitch 40 ms through dropouts unless the signal goes
  quiet. Real-time voicing agrees with offline analysis on 80–95 % of voiced frames of TTS speech.
- **Marks:** analysis marks one period apart, phase-locked (≤ 1/8 period per mark) to the latest
  glottal pulse, so grains are centred on pulses. That is what keeps formant shifting clean.
- **Synthesis:** grains overlap-added at `period / pitch_ratio`, read time-scaled by the formant
  ratio. Asymmetric windows (left half = previous grain's right half) sum to exactly 1, so zero
  shift is transparent (> 60 dB SNR). Power normalisation keeps loudness within about ±1.5 dB
  over ±7 st (−3 dB at +12 st).
- **Causal evaluation:** every output sample is computed on demand from grains that read only
  past input, so the algorithmic latency is the 3-sample interpolator pad instead of the classic
  two pitch periods (~25 ms).
- **Unvoiced** sounds pass at their original pitch (no buzz on "s"/"f"), but are formant-shifted.
- **Measured:** pitch exact to < 0.5 % on synthetic vowels from −12 to +12 st. Formant peaks track
  within one harmonic for ±8 st. Spectral centroid moves ≤ 6 % under pure pitch shift (vs 50 %
  for resampling). Cost ≈ 0.5 % of one laptop core (Ryzen 7 7735HS).
- **Known limits:** large upward shifts (+7..+12) reuse grains, which adds slight roughness on
  real voices (inherent to PSOLA). Onsets take ~5–20 ms before the tracker locks. A future
  "quality" option could add a spectral-envelope (LPC) path for very large formant shifts.

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

## 4. Presets: **done (step 5)**

- 14 built-ins: Normal, Deep voice, Monster, Female, Male, Child, Chipmunk, Robot, Alien,
  Telephone, Walkie-talkie, Cave, Announcer, Podcast. Bad-connection scenarios join in step 9.
- User presets: one JSON file each in `%APPDATA%\VoiceChanger\presets` (`schema_version`,
  unknown fields ignored, missing ones defaulted). Save, Save as, rename, delete (with
  confirmation), import (several files at once; name clashes get " (2)"), export.
- **Mic cleanup is kept separate from voice character**: loading a preset leaves noise
  suppression and the gate as they are, unless it was saved with "include mic cleanup".
- The loaded preset is remembered; "*" marks it as modified (parameters, on/off or order).
- Effect order is adjustable (arrows per effect, "Reset order"); reordering swaps the chain live
  with a crossfade.

## 5. Hotkeys and quick controls: **done (step 6)**

- **Global hotkeys** (customisable, conflicts resolved by moving the key): effects on/off
  (Ctrl+Alt+V), effects while held (unset by default), panic / normal voice (Ctrl+Alt+N),
  next/previous preset (Ctrl+Alt+Page Down / Page Up), mute (Ctrl+Alt+M), show window (unset).
- **Two hooks, one matcher**: a passive low-level hook (`WH_KEYBOARD_LL`) sees keys while other
  apps are focused, including press *and* release for hold-to-use; it never blocks keys. Windows
  doesn't call it while our own window is focused, so a thread-local `WH_KEYBOARD` hook on the
  window thread covers that. Both feed one state machine: each press is handled once, auto-repeat
  is ignored, and a key pressed in our window and released in a game still pairs up. (An earlier
  egui-based fallback was dropped: egui turns Ctrl+C/X/V into clipboard commands and loses keys.)
- Audio-state actions (toggle, hold, panic, mute) are applied on the hook thread through the
  shared atomics, so they're instant even if the UI is busy or hidden.
- Recording a binding: click it, press the combination (Esc cancels, Backspace clears).
  Assigning a hold key switches effects off at once (push-to-talk semantics).
- **Tray icon** (drawn in code, also the window icon): left click shows the window; menu with
  Show, Effects on, Mute, Voice → presets, Quit; tooltip shows the current state.
- **Close to tray** (default on, first time shows a hint), **start hidden in the tray**.
- **Toasts**: a small Win32 overlay (no focus, click-through, sized to its text) shows the preset
  or state after hotkey/tray actions, even while the window is hidden. Not visible over
  exclusive-fullscreen games (an OS limitation); fine over borderless/windowed.
- Limitation: hooks don't see keys while an app running as administrator is focused.

## 6. User interface

Done: single window, dark/light/system theme, device and level panels (step 1).
**Simple / Advanced modes (step 5)**: Simple shows devices, levels, a preset button grid, bypass,
mute, quick noise-suppression/gate toggles and volume; Advanced adds input gain, mic channel,
preset management, every effect panel with ordering, and latency/performance details.
Tray icon, close to tray and start hidden: done (step 6). Later: Test button, spectrum visualizer (FFT on the UI side from a ring copy, capped at the meter
frame rate), launch on Windows startup (with the installer).

## 7. File processing (offline)

**Engine done (step 2).** `offline::render` runs the same `EngineCore` over files with the live
block size, so files sound identical to the virtual mic. One code path serves file export, the
future Test button and the DSP test harness.

- Decoding: Symphonia (WAV, MP3, FLAC, OGG/Vorbis). Output: WAV, 32-bit float or 16-bit with
  TPDF dither (`hound`). MP3 export (LAME) comes with the file-processing UI (step 10).
- `vcrender` CLI: single or batch files, rendered in parallel across all cores; output names are
  de-duplicated and inputs are never overwritten.
- Test harness: deterministic generated signals (synthetic vowel with exact pitch/formants,
  sweep, seeded noise), analysis helpers (RMS, SNR, YIN pitch estimate), golden WAVs in
  `tests/golden/`, a bypass null test, and a block-size invariance test that every new effect
  must pass.

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

As specified. Release profile: thin LTO, stripped, ~9 MB `.exe`, no console window. Installer
should check for VB-CABLE and link to it, not bundle it.

- **CPU compatibility (done):** builds target baseline x86-64 so they run on any 64-bit PC; the
  pitch tracker's hot loop picks an AVX2 version at runtime (`src/dsp/simd.rs`). Output is
  bit-identical either way (verified by running the golden tests on both paths, also in CI).
  Cost of portability vs an AVX2-only build: ~0.3 % of one core with every effect on.
- **SmartScreen:** unsigned downloads show "Windows protected your PC". Options, cheapest first:
  publish through the Microsoft Store (Store-signed, no warning); Microsoft's Azure-based
  signing service (low monthly fee, identity validation, availability depends on country);
  a traditional code-signing certificate (yearly, hardware key required). Signing alone no
  longer guarantees no warning: SmartScreen also builds reputation per publisher over downloads.
- **CI:** `.github/workflows/ci.yml` (clippy with warnings as errors, tests on the AVX2 and
  baseline paths, release artifacts) runs once the repo is pushed to GitHub.

## Build order

1. ✅ Audio I/O, device selection, pass-through, meters, virtual cable output, drift
   compensation, reconnect, settings, GUI shell.
2. ✅ Offline renderer (`EngineCore` over files), `vcrender` CLI, golden-file test harness.
3. ✅ Effect chain (slots, crossfaded hot-swap, latency-compensated mix), always-on limiter,
   pitch/formant shifter, Effects panel, `vcrender --pitch/--formant`.
4. ✅ Core effects: noise suppression, gate, EQ, compressor, reverb, robot, radio; spec-driven
   effect panels; zero-allocation audio path enforced by a test.
5. ✅ Presets (built-in + user, import/export), Simple/Advanced modes, effect reordering,
   runtime CPU dispatch, GUI split into modules, CI workflow.
6. ✅ Global hotkeys (hold-to-use, panic, presets, mute), tray icon, toasts, close to tray,
   single instance, monitoring safety warning, 10 ms noise suppression.
7. Test button (record 5 s, play back processed), visualizer, other UI polish.
8. Modulation sliders (3A).
9. Bad mic / bad connection (3B), scenario presets, monitor tap point.
10. File processing UI (batch, MP3 export, record-with-effects).
11. Installer, signing, startup options, stretch features.

Steps 6–7 were moved ahead of 3A/3B: they make the app usable day to day, and the Test button
is the quickest way to judge effect quality by ear.

## Stretch / future

Unchanged from rev 1 (AI voice conversion, soundboard, ambience, per-app routing, VST plugins,
pitch-adaptive effects, per-app profiles). Heavy effects such as AI conversion should run on a
worker thread with larger blocks, fed by its own ring, so they can't stall the capture callback.

### Own virtual audio device (instead of VB-CABLE)

Feasible, but a large, separate project. Replacing VB-CABLE means shipping a **kernel-mode audio
driver** (e.g. based on Microsoft's SysVAD / SimpleAudioSample): a render endpoint we write to
and a capture endpoint other apps record from.

- **Signing:** Windows 10/11 loads only Microsoft-signed drivers. That needs an EV code-signing
  certificate (yearly cost) plus attestation signing through the Partner Center hardware
  program. Test-signing mode works for development only. Many anti-cheat systems refuse to run
  with it on, so it is not an option for users.
- **Risk:** driver bugs blue-screen the machine; Windows updates need re-testing.
- **Effort:** roughly 1–3 months to a reliable, signed v1 for someone new to drivers, plus
  ongoing maintenance.
- **Middle grounds:** license VB-CABLE for redistribution from VB-Audio; or an APO (audio
  processing object) on the real mic, which avoids a virtual device but has its own signing
  and install hurdles.

Decision: keep VB-CABLE for v1. Outputs are already abstract (`OutputSink`), so a custom driver
can be swapped in later without touching the DSP. Revisit if the app becomes a paid product.
