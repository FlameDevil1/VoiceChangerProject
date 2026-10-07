//! One-shot playback of a rendered clip (the Test button), on its own output stream so it never
//! goes to the virtual microphone.

use super::devices;
use crate::config::DeviceRef;
use cpal::traits::{DeviceTrait, StreamTrait};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering::Relaxed};
use std::time::Duration;

/// Shared progress of a playback, for the UI.
#[derive(Debug, Default)]
pub struct Playback {
    /// Samples played so far.
    pub position: AtomicUsize,
    pub total: AtomicUsize,
    pub stop: AtomicBool,
    pub finished: AtomicBool,
}

/// Render a clip with `render` and play it at `rate` on `device` (`None` = Windows default
/// output). Returns at once: a worker thread renders (so the UI never stalls), owns the stream,
/// and ends when the clip finishes or `stop` is set.
pub fn play(render: impl FnOnce() -> Vec<f32> + Send + 'static, rate: u32, device: Option<DeviceRef>) -> Arc<Playback> {
    let state = Arc::new(Playback::default());
    let st = state.clone();
    let _ = std::thread::Builder::new().name("test-playback".into()).spawn(move || {
        let samples = Arc::new(render());
        st.total.store(samples.len(), Relaxed);
        if !st.stop.load(Relaxed)
            && let Err(e) = run(&samples, rate, device.as_ref(), &st)
        {
            log::warn!("test playback failed: {e}");
        }
        st.finished.store(true, Relaxed);
    });
    state
}

fn run(samples: &Arc<Vec<f32>>, rate: u32, device: Option<&DeviceRef>, st: &Arc<Playback>) -> Result<(), String> {
    let host = cpal::default_host();
    let dev = devices::find(&host, device, false).ok_or("output device not found")?;
    let channels = dev.default_output_config().map_err(|e| e.to_string())?.channels() as usize;
    let config =
        cpal::StreamConfig { channels: channels as u16, sample_rate: rate, buffer_size: cpal::BufferSize::Default };
    let (data, cb_state) = (samples.clone(), st.clone());
    let stream = dev
        .build_output_stream::<f32, _, _>(
            config,
            move |out: &mut [f32], _| {
                let mut pos = cb_state.position.load(Relaxed);
                for frame in out.chunks_exact_mut(channels) {
                    let s = if cb_state.stop.load(Relaxed) { 0.0 } else { data.get(pos).copied().unwrap_or(0.0) };
                    frame.fill(s);
                    pos += 1;
                }
                cb_state.position.store(pos.min(data.len()), Relaxed);
            },
            |e| log::warn!("test playback stream error: {e}"),
            Some(Duration::from_secs(3)),
        )
        .map_err(|e| e.to_string())?;
    stream.play().map_err(|e| e.to_string())?;
    while !st.stop.load(Relaxed) && st.position.load(Relaxed) < samples.len() {
        std::thread::sleep(Duration::from_millis(20));
    }
    // Let the device drain its last buffer before closing.
    std::thread::sleep(Duration::from_millis(80));
    Ok(())
}
