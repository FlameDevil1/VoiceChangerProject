//! Stream management.
//!
//! Threads:
//! - **Capture callback** (cpal, MMCSS priority): downmix -> `EngineCore` -> push into one ring per
//!   output. This is the clock that drives processing.
//! - **Output callbacks** (one per output device): pull from their ring through a
//!   `DriftResampler`, so each device's clock can differ from the mic's.
//! - **Controller thread** (this file): owns the cpal streams, handles commands from the UI,
//!   rebuilds streams when a device disappears, and enumerates devices. Nothing here runs on the
//!   UI thread, so a stalled device call never freezes the window.
//!
//! Outputs are swapped in and out of a running capture stream through a lock-free queue, so
//! turning monitoring on/off or changing the cable device never interrupts the other output.

use super::devices::{self, DeviceList};
use super::shared::{Shared, SinkStats};
use crate::config::DeviceRef;
use crate::dsp::{self, Chain, CoreParams, DriftResampler, EffectKind, EngineCore, SmoothedValue};
use cpal::traits::{DeviceTrait, StreamTrait};
use cpal::{FromSample, SampleFormat, SizedSample};
use rtrb::{Consumer, Producer, RingBuffer};
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::atomic::Ordering::Relaxed;
use std::sync::mpsc::{self, RecvTimeoutError};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use crate::dsp::MAX_BLOCK;
/// Ring capacity per output, in seconds of audio.
const RING_SECONDS: f64 = 0.5;
const RETRY_INTERVAL: Duration = Duration::from_secs(1);
const BUILD_TIMEOUT: Option<Duration> = Some(Duration::from_secs(3));

#[derive(Clone, Debug, Default, PartialEq)]
pub struct EngineSettings {
    /// `None` = Windows default input.
    pub input: Option<DeviceRef>,
    /// `None` = no virtual mic output.
    pub cable: Option<DeviceRef>,
    /// `None` = Windows default output.
    pub monitor: Option<DeviceRef>,
    pub monitor_enabled: bool,
    /// Effect order for the chain built at start.
    pub chain_order: Vec<EffectKind>,
}

pub enum Command {
    Start(EngineSettings),
    Stop,
    SetCable(Option<DeviceRef>),
    SetMonitor {
        device: Option<DeviceRef>,
        enabled: bool,
    },
    /// Rebuild the effect chain in a new order; swapped in with a crossfade, no restart.
    SetChainOrder(Vec<EffectKind>),
    RefreshDevices,
    Shutdown,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub enum EngineState {
    #[default]
    Stopped,
    Running,
    /// The input device vanished; retrying once a second.
    Reconnecting,
    Error(String),
}

#[derive(Clone, Debug, Default)]
pub struct OutputInfo {
    pub name: String,
    pub sample_rate: u32,
    pub buffer_frames: u32,
    /// Waiting for this device to come back.
    pub lost: bool,
}

#[derive(Clone, Debug, Default)]
pub struct Status {
    pub state: EngineState,
    pub devices: DeviceList,
    pub input_name: String,
    pub sample_rate: u32,
    pub cable: Option<OutputInfo>,
    pub monitor: Option<OutputInfo>,
    /// Non-fatal problem worth showing (e.g. feedback-loop warning).
    pub warning: Option<String>,
}

/// UI-side handle. Dropping it stops audio and joins the controller thread.
pub struct EngineHandle {
    tx: mpsc::Sender<Command>,
    pub shared: Arc<Shared>,
    pub status: Arc<Mutex<Status>>,
    thread: Option<JoinHandle<()>>,
}

impl EngineHandle {
    /// `repaint` is called whenever `status` changes so the UI can redraw without polling.
    pub fn spawn(shared: Arc<Shared>, repaint: Box<dyn Fn() + Send>) -> Self {
        let (tx, rx) = mpsc::channel();
        let status = Arc::new(Mutex::new(Status::default()));
        let ctl = Controller {
            host: cpal::default_host(),
            shared: shared.clone(),
            status: status.clone(),
            repaint,
            settings: EngineSettings::default(),
            running: None,
            want_running: false,
            next_retry: Instant::now(),
            pending_close: Vec::new(),
        };
        let thread = std::thread::Builder::new()
            .name("engine-controller".into())
            .spawn(move || ctl.run(rx))
            .expect("spawn controller thread");
        let handle = Self { tx, shared, status, thread: Some(thread) };
        handle.send(Command::RefreshDevices);
        handle
    }

    pub fn send(&self, cmd: Command) {
        let _ = self.tx.send(cmd);
    }

    pub fn status(&self) -> Status {
        self.status.lock().map(|s| s.clone()).unwrap_or_default()
    }
}

impl Drop for EngineHandle {
    fn drop(&mut self) {
        self.send(Command::Shutdown);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

// ---------------------------------------------------------------------------------------------
// Controller thread
// ---------------------------------------------------------------------------------------------

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum SinkKind {
    Cable,
    Monitor,
}

/// Messages to the capture callback. Everything is allocated here, never on the audio thread.
enum SinkMsg {
    Set(SinkKind, Option<Producer<f32>>),
    Chain(Chain),
}

/// Objects the capture callback hands back so they are freed off the audio thread.
enum Retired {
    Producer(#[allow(dead_code)] Producer<f32>),
    Chain(#[allow(dead_code)] Chain),
}

struct OutputSink {
    stream: cpal::Stream,
    device_id: String,
}

struct Running {
    _input: cpal::Stream,
    engine_rate: u32,
    sink_tx: Producer<SinkMsg>,
    /// Old producers/chains handed back by the capture callback, dropped here.
    returned: Consumer<Retired>,
    cable: Option<OutputSink>,
    monitor: Option<OutputSink>,
    /// WASAPI flags a capture discontinuity while a stream spins up; we forget xruns from the
    /// first second so the glitch counter only shows real dropouts.
    started_at: Option<Instant>,
}

struct Controller {
    host: cpal::Host,
    shared: Arc<Shared>,
    status: Arc<Mutex<Status>>,
    repaint: Box<dyn Fn() + Send>,
    settings: EngineSettings,
    running: Option<Running>,
    /// The user asked for audio to run (so keep retrying after a disconnect).
    want_running: bool,
    next_retry: Instant,
    /// Monitor streams being faded out before they are closed.
    pending_close: Vec<(Instant, OutputSink)>,
}

impl Controller {
    fn run(mut self, rx: mpsc::Receiver<Command>) {
        loop {
            // Wake often only while something needs attention; otherwise stay idle.
            let busy = self.want_running && self.needs_retry() || !self.pending_close.is_empty();
            let timeout = if busy { Duration::from_millis(100) } else { Duration::from_millis(500) };
            match rx.recv_timeout(timeout) {
                Ok(Command::Shutdown) | Err(RecvTimeoutError::Disconnected) => break,
                Ok(cmd) => self.handle(cmd),
                Err(RecvTimeoutError::Timeout) => {}
            }
            self.housekeeping();
        }
        self.stop();
        log::info!("controller stopped");
    }

    fn handle(&mut self, cmd: Command) {
        match cmd {
            Command::Start(settings) => {
                self.settings = settings;
                self.want_running = true;
                self.stop_streams();
                if let Err(e) = self.start() {
                    log::error!("start failed: {e}");
                    self.want_running = false;
                    self.set_state(EngineState::Error(e));
                }
            }
            Command::Stop => self.stop(),
            Command::SetCable(dev) => {
                self.settings.cable = dev;
                if self.running.is_some() {
                    self.rebuild_sink(SinkKind::Cable);
                }
            }
            Command::SetMonitor { device, enabled } => {
                let device_changed = device != self.settings.monitor;
                self.settings.monitor = device;
                self.settings.monitor_enabled = enabled;
                if self.running.is_some() {
                    let open = self.running.as_ref().is_some_and(|r| r.monitor.is_some());
                    if enabled && (!open || device_changed) {
                        self.rebuild_sink(SinkKind::Monitor);
                    } else if !enabled && open {
                        // The output callback fades to silence; close once the fade is done.
                        self.detach_sink(SinkKind::Monitor, Duration::from_millis(80));
                    }
                }
            }
            Command::SetChainOrder(order) => {
                log::info!("effect order: {order:?}");
                self.settings.chain_order = order;
                if let Some(running) = &mut self.running {
                    let chain = Chain::build(
                        &self.settings.chain_order,
                        &self.shared.fx,
                        running.engine_rate as f32,
                        MAX_BLOCK,
                    );
                    if running.sink_tx.push(SinkMsg::Chain(chain)).is_err() {
                        log::warn!("chain queue full; order change dropped");
                    }
                }
            }
            Command::RefreshDevices => self.refresh_devices(),
            Command::Shutdown => unreachable!(),
        }
    }

    fn housekeeping(&mut self) {
        let now = Instant::now();
        self.pending_close.retain(|(at, _)| *at > now);

        let Some(running) = &mut self.running else {
            if self.want_running && now >= self.next_retry {
                self.next_retry = now + RETRY_INTERVAL;
                if self.start().is_ok() {
                    log::info!("reconnected");
                }
            }
            return;
        };
        while running.returned.pop().is_ok() {}
        if running.started_at.is_some_and(|t| t.elapsed() > Duration::from_secs(1)) {
            running.started_at = None;
            self.shared.capture_xruns.store(0, Relaxed);
        }

        if self.shared.input_failed.load(Relaxed) {
            log::warn!("input stream failed; reconnecting");
            self.stop_streams();
            self.set_state(EngineState::Reconnecting);
            self.next_retry = now + Duration::from_millis(300);
            return;
        }

        for kind in [SinkKind::Cable, SinkKind::Monitor] {
            let stats = self.stats(kind);
            let has_stream = self.sink(kind).is_some();
            if has_stream && stats.failed.load(Relaxed) {
                log::warn!("{kind:?} output failed; will retry");
                self.detach_sink(kind, Duration::ZERO);
                self.mark_lost(kind, true);
                self.next_retry = now;
            } else if !has_stream && self.sink_wanted(kind) && now >= self.next_retry {
                self.next_retry = now + RETRY_INTERVAL;
                self.rebuild_sink(kind);
            }
        }
    }

    fn needs_retry(&self) -> bool {
        match &self.running {
            None => true,
            Some(r) => {
                (self.sink_wanted(SinkKind::Cable) && r.cable.is_none())
                    || (self.sink_wanted(SinkKind::Monitor) && r.monitor.is_none())
            }
        }
    }

    fn sink_wanted(&self, kind: SinkKind) -> bool {
        match kind {
            SinkKind::Cable => self.settings.cable.is_some(),
            SinkKind::Monitor => self.settings.monitor_enabled,
        }
    }

    fn sink(&self, kind: SinkKind) -> Option<&OutputSink> {
        let r = self.running.as_ref()?;
        match kind {
            SinkKind::Cable => r.cable.as_ref(),
            SinkKind::Monitor => r.monitor.as_ref(),
        }
    }

    fn stats(&self, kind: SinkKind) -> &SinkStats {
        match kind {
            SinkKind::Cable => &self.shared.cable,
            SinkKind::Monitor => &self.shared.monitor,
        }
    }

    fn start(&mut self) -> Result<(), String> {
        let device = devices::find(&self.host, self.settings.input.as_ref(), true).ok_or_else(|| {
            match &self.settings.input {
                Some(d) => format!("Microphone \"{}\" not found", d.name),
                None => "No microphone found".to_string(),
            }
        })?;
        let name = devices::name_of(&device);
        let supported = device.default_input_config().map_err(|e| format!("{name}: {e}"))?;
        let engine_rate = supported.sample_rate();
        let mut config = supported.config();
        config.buffer_size = cpal::BufferSize::Default;

        self.shared.input_failed.store(false, Relaxed);
        let (sink_tx, sink_rx) = RingBuffer::new(8);
        let (ret_tx, returned) = RingBuffer::new(8);
        let state = CaptureState {
            shared: self.shared.clone(),
            core: EngineCore::with_chain(
                engine_rate as f32,
                MAX_BLOCK,
                Chain::build(&self.settings.chain_order, &self.shared.fx, engine_rate as f32, MAX_BLOCK),
            ),
            mono: vec![0.0; MAX_BLOCK],
            channels: config.channels as usize,
            rate: engine_rate as f32,
            cable: None,
            monitor: None,
            sink_rx,
            ret_tx,
            poisoned: false,
            needs_warm_up: true,
        };

        let stream = match supported.sample_format() {
            SampleFormat::F32 => build_capture::<f32>(&device, config, state, &self.shared),
            SampleFormat::I16 => build_capture::<i16>(&device, config, state, &self.shared),
            SampleFormat::I32 => build_capture::<i32>(&device, config, state, &self.shared),
            SampleFormat::F64 => build_capture::<f64>(&device, config, state, &self.shared),
            SampleFormat::U8 => build_capture::<u8>(&device, config, state, &self.shared),
            other => return Err(format!("{name}: unsupported sample format {other:?}")),
        }
        .map_err(|e| format!("{name}: {e}"))?;
        stream.play().map_err(|e| format!("{name}: {e}"))?;

        log::info!("capture: {name} @ {engine_rate} Hz, {} ch, {:?}", config.channels, supported.sample_format());
        let warning = devices::is_cable_capture(&name).then(|| {
            "Your microphone is set to the virtual cable's output. That feeds the voice changer back into \
             itself; pick your real microphone instead."
                .to_string()
        });

        self.running = Some(Running {
            _input: stream,
            engine_rate,
            sink_tx,
            returned,
            cable: None,
            monitor: None,
            started_at: Some(Instant::now()),
        });
        if let Ok(mut s) = self.status.lock() {
            s.input_name = name;
            s.sample_rate = engine_rate;
            s.warning = warning;
            s.cable = None;
            s.monitor = None;
        }
        for kind in [SinkKind::Cable, SinkKind::Monitor] {
            if self.sink_wanted(kind) {
                self.rebuild_sink(kind);
            }
        }
        self.set_state(EngineState::Running);
        Ok(())
    }

    /// (Re)build one output and hand its ring to the running capture callback.
    fn rebuild_sink(&mut self, kind: SinkKind) {
        self.detach_sink(kind, Duration::ZERO);
        let Some(running) = &self.running else { return };
        let engine_rate = running.engine_rate;
        let want = match kind {
            SinkKind::Cable => self.settings.cable.clone(),
            SinkKind::Monitor => self.settings.monitor.clone(),
        };
        if kind == SinkKind::Cable && want.is_none() {
            return;
        }
        let Some(device) = devices::find(&self.host, want.as_ref(), false) else {
            self.mark_lost(kind, true);
            return;
        };
        let device_id = devices::id_of(&device);
        if kind == SinkKind::Monitor && self.sink(SinkKind::Cable).is_some_and(|c| c.device_id == device_id) {
            self.set_warning("Monitoring is set to the virtual cable. Choose your headphones under \"Hear myself\".");
            return;
        }

        let stats = self.stats(kind);
        stats.failed.store(false, Relaxed);
        stats.underruns.store(0, Relaxed);
        match build_output(&device, engine_rate, kind, self.shared.clone()) {
            Ok((stream, producer, info)) => {
                log::info!("{kind:?} output: {} @ {} Hz, buffer {}", info.name, info.sample_rate, info.buffer_frames);
                let running = self.running.as_mut().expect("checked above");
                if running.sink_tx.push(SinkMsg::Set(kind, Some(producer))).is_err() {
                    log::error!("sink queue full");
                    return;
                }
                let sink = OutputSink { stream, device_id };
                match kind {
                    SinkKind::Cable => running.cable = Some(sink),
                    SinkKind::Monitor => running.monitor = Some(sink),
                }
                if let Ok(mut s) = self.status.lock() {
                    match kind {
                        SinkKind::Cable => s.cable = Some(info),
                        SinkKind::Monitor => s.monitor = Some(info),
                    }
                }
                (self.repaint)();
            }
            Err(e) => {
                log::warn!("{kind:?} output failed to open: {e}");
                self.mark_lost(kind, true);
            }
        }
    }

    /// Disconnect an output from the capture callback and close its stream after `delay`.
    fn detach_sink(&mut self, kind: SinkKind, delay: Duration) {
        let Some(running) = &mut self.running else { return };
        let sink = match kind {
            SinkKind::Cable => running.cable.take(),
            SinkKind::Monitor => running.monitor.take(),
        };
        if let Some(sink) = sink {
            let _ = running.sink_tx.push(SinkMsg::Set(kind, None));
            if delay.is_zero() {
                let _ = sink.stream.pause();
            } else {
                self.pending_close.push((Instant::now() + delay, sink));
            }
        }
        if let Ok(mut s) = self.status.lock() {
            match kind {
                SinkKind::Cable => s.cable = None,
                SinkKind::Monitor => s.monitor = None,
            }
        }
    }

    fn mark_lost(&mut self, kind: SinkKind, lost: bool) {
        let name = match kind {
            SinkKind::Cable => self.settings.cable.as_ref().map(|d| d.name.clone()),
            SinkKind::Monitor => {
                Some(self.settings.monitor.as_ref().map(|d| d.name.clone()).unwrap_or_else(|| "Default output".into()))
            }
        };
        if let (Ok(mut s), Some(name)) = (self.status.lock(), name) {
            let info = Some(OutputInfo { name, lost, ..Default::default() });
            match kind {
                SinkKind::Cable => s.cable = info,
                SinkKind::Monitor => s.monitor = info,
            }
        }
        (self.repaint)();
    }

    fn stop_streams(&mut self) {
        self.running = None;
        self.pending_close.clear();
    }

    fn stop(&mut self) {
        self.want_running = false;
        self.stop_streams();
        self.set_state(EngineState::Stopped);
    }

    fn refresh_devices(&mut self) {
        let list = devices::enumerate(&self.host);
        let changed = self.status.lock().map(|mut s| {
            let changed = s.devices != list;
            s.devices = list;
            changed
        });
        if changed.unwrap_or(false) {
            (self.repaint)();
        }
    }

    fn set_state(&mut self, state: EngineState) {
        if let Ok(mut s) = self.status.lock() {
            if s.state != state {
                log::info!("engine state: {state:?}");
            }
            s.state = state;
        }
        (self.repaint)();
    }

    fn set_warning(&mut self, msg: &str) {
        if let Ok(mut s) = self.status.lock() {
            s.warning = Some(msg.to_string());
        }
        (self.repaint)();
    }
}

// ---------------------------------------------------------------------------------------------
// Capture callback
// ---------------------------------------------------------------------------------------------

struct CaptureState {
    shared: Arc<Shared>,
    core: EngineCore,
    mono: Vec<f32>,
    channels: usize,
    rate: f32,
    cable: Option<Producer<f32>>,
    monitor: Option<Producer<f32>>,
    sink_rx: Consumer<SinkMsg>,
    ret_tx: Producer<Retired>,
    poisoned: bool,
    /// Per-thread library setup still to do (see `denoise::warm_up_thread`).
    needs_warm_up: bool,
}

impl CaptureState {
    fn process<T: Copy>(&mut self, data: &[T], to_f32: impl Fn(T) -> f32 + Copy) {
        if std::mem::take(&mut self.needs_warm_up) {
            // One-time, first callback of this thread: the only allocation the audio path makes.
            dsp::fx::denoise::warm_up_thread();
        }
        let start = Instant::now();
        // Anything replaced is handed back for deallocation off the audio thread (it is only
        // dropped here if the return queue is full, which would need a burst of changes).
        while let Ok(msg) = self.sink_rx.pop() {
            match msg {
                SinkMsg::Set(kind, new) => {
                    let slot = match kind {
                        SinkKind::Cable => &mut self.cable,
                        SinkKind::Monitor => &mut self.monitor,
                    };
                    if let Some(old) = std::mem::replace(slot, new) {
                        let _ = self.ret_tx.push(Retired::Producer(old));
                    }
                }
                SinkMsg::Chain(chain) => {
                    if let Some(old) = self.core.set_chain(chain) {
                        let _ = self.ret_tx.push(Retired::Chain(old));
                    }
                }
            }
        }

        let sh = &*self.shared;
        let channel = match sh.input_channel.load(Relaxed) {
            c if c >= 0 => Some(c as usize),
            _ => None,
        };
        let params = CoreParams {
            input_gain: sh.input_gain.load(),
            output_gain: sh.output_gain.load(),
            bypass: sh.bypass.load(Relaxed),
            mute: sh.mute.load(Relaxed),
        };

        let mut frames = 0;
        for chunk in data.chunks(self.channels * MAX_BLOCK) {
            let n = dsp::downmix(chunk, self.channels, channel, &mut self.mono, to_f32);
            let buf = &mut self.mono[..n];
            sh.in_peak.fetch_max(dsp::peak(buf));
            self.core.process(buf, params);
            sh.out_peak.fetch_max(dsp::peak(buf));
            for tx in [&mut self.cable, &mut self.monitor].into_iter().flatten() {
                // If an output stalls, its ring fills; drop the overflow rather than block.
                let _ = tx.push_partial_slice(buf);
            }
            frames += n;
        }

        if let Some(old) = self.core.take_retired() {
            let _ = self.ret_tx.push(Retired::Chain(old));
        }
        sh.dsp_latency.store(self.core.latency() as u32, Relaxed);

        if frames > 0 {
            sh.in_block.store(frames as u32, Relaxed);
            let block_secs = frames as f32 / self.rate;
            sh.load.fetch_max(start.elapsed().as_secs_f32() / block_secs);
        }
    }
}

fn build_capture<T>(
    device: &cpal::Device,
    config: cpal::StreamConfig,
    mut state: CaptureState,
    shared: &Arc<Shared>,
) -> Result<cpal::Stream, cpal::Error>
where
    T: SizedSample + Send + 'static,
    f32: FromSample<T>,
{
    let err_shared = shared.clone();
    device.build_input_stream::<T, _, _>(
        config,
        move |data: &[T], _| {
            if state.poisoned {
                return;
            }
            let res = catch_unwind(AssertUnwindSafe(|| state.process(data, f32::from_sample_)));
            if res.is_err() {
                state.poisoned = true;
                state.shared.input_failed.store(true, Relaxed);
            }
        },
        move |err: cpal::Error| match err.kind() {
            cpal::ErrorKind::Xrun => {
                err_shared.capture_xruns.fetch_add(1, Relaxed);
            }
            _ => err_shared.input_failed.store(true, Relaxed),
        },
        BUILD_TIMEOUT,
    )
}

// ---------------------------------------------------------------------------------------------
// Output callback
// ---------------------------------------------------------------------------------------------

struct PlaybackState {
    shared: Arc<Shared>,
    kind: SinkKind,
    rs: DriftResampler,
    mono: Vec<f32>,
    channels: usize,
    engine_rate: f32,
    gain: SmoothedValue,
    margin_gen: u32,
    poisoned: bool,
}

impl PlaybackState {
    fn stats(&self) -> &SinkStats {
        match self.kind {
            SinkKind::Cable => &self.shared.cable,
            SinkKind::Monitor => &self.shared.monitor,
        }
    }

    fn process(&mut self, data: &mut [f32]) {
        let generation = self.shared.margin_gen.load(Relaxed);
        if generation != self.margin_gen {
            self.margin_gen = generation;
            self.rs.set_margin(self.shared.margin.load() as f64);
        }
        let enabled = match self.kind {
            SinkKind::Cable => true,
            SinkKind::Monitor => self.shared.monitor_enabled.load(Relaxed),
        };
        self.gain.set_target(if enabled { 1.0 } else { 0.0 });
        let in_block = self.shared.in_block.load(Relaxed) as usize;

        let mut last = None;
        let mut peak = 0.0f32;
        for chunk in data.chunks_mut(self.channels * MAX_BLOCK) {
            let n = chunk.len() / self.channels;
            let buf = &mut self.mono[..n];
            last = Some(self.rs.process(buf, in_block));
            self.gain.apply(buf);
            peak = peak.max(dsp::peak(buf));
            for (frame, s) in chunk.chunks_exact_mut(self.channels).zip(buf.iter()) {
                frame.fill(*s);
            }
        }

        let underruns = self.rs.underruns();
        let margin_ms = self.rs.margin() as f32 * 1000.0;
        let st = self.stats();
        st.peak.fetch_max(peak);
        st.underruns.store(underruns, Relaxed);
        st.margin_ms.store(margin_ms);
        if let Some(d) = last {
            st.fill_ms.store(d.fill / self.engine_rate * 1000.0);
            st.target_ms.store(d.target / self.engine_rate * 1000.0);
            st.correction_ppm.store(d.correction * 1e6);
        }
    }
}

fn build_output(
    device: &cpal::Device,
    engine_rate: u32,
    kind: SinkKind,
    shared: Arc<Shared>,
) -> Result<(cpal::Stream, Producer<f32>, OutputInfo), String> {
    let name = devices::name_of(device);
    let default = device.default_output_config().map_err(|e| e.to_string())?;
    let channels = default.channels();

    // Ask for the engine rate first: Windows converts it in its own engine (AUTOCONVERTPCM), so
    // our resampler only has to track drift. Fall back to the device's native rate.
    let mut last_err = String::new();
    for rate in [engine_rate, default.sample_rate()] {
        let config = cpal::StreamConfig { channels, sample_rate: rate, buffer_size: cpal::BufferSize::Default };
        let (tx, rx) = RingBuffer::new((engine_rate as f64 * RING_SECONDS) as usize);
        let margin = shared.margin.load() as f64;
        let mut state = PlaybackState {
            shared: shared.clone(),
            kind,
            rs: DriftResampler::new(rx, engine_rate as f64, rate as f64, margin),
            mono: vec![0.0; MAX_BLOCK],
            channels: channels as usize,
            engine_rate: engine_rate as f32,
            gain: SmoothedValue::new(1.0, rate as f32, 0.020),
            margin_gen: shared.margin_gen.load(Relaxed),
            poisoned: false,
        };
        let err_shared = shared.clone();
        let result = device.build_output_stream::<f32, _, _>(
            config,
            move |data: &mut [f32], _| {
                if state.poisoned {
                    data.fill(0.0);
                    return;
                }
                if catch_unwind(AssertUnwindSafe(|| state.process(data))).is_err() {
                    state.poisoned = true;
                    data.fill(0.0);
                    state.stats().failed.store(true, Relaxed);
                }
            },
            move |err: cpal::Error| {
                if err.kind() != cpal::ErrorKind::Xrun {
                    let st = match kind {
                        SinkKind::Cable => &err_shared.cable,
                        SinkKind::Monitor => &err_shared.monitor,
                    };
                    st.failed.store(true, Relaxed);
                }
            },
            BUILD_TIMEOUT,
        );
        match result {
            Ok(stream) => {
                stream.play().map_err(|e| e.to_string())?;
                let buffer_frames = stream.buffer_size().unwrap_or(0);
                return Ok((stream, tx, OutputInfo { name, sample_rate: rate, buffer_frames, lost: false }));
            }
            Err(e) => last_err = e.to_string(),
        }
        if rate == default.sample_rate() {
            break;
        }
    }
    Err(format!("{name}: {last_err}"))
}
