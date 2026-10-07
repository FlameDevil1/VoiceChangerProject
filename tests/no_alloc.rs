//! Real-time safety: the audio path must never touch the heap.
//!
//! A counting global allocator records every allocation and free made by this thread while
//! "counting" is on. Building chains happens outside the counted region (that is the
//! controller thread's job); everything the audio callback does happens inside it.

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;
use std::sync::atomic::{AtomicUsize, Ordering::Relaxed};
use voice_changer::dsp::{Chain, CoreParams, EffectKind, EngineCore, FxParams, FxSettings};
use voice_changer::offline::signals;

struct Counting;

static EVENTS: AtomicUsize = AtomicUsize::new(0);
thread_local! {
    // `const` init: reading it never allocates (which would recurse into the allocator).
    static COUNTING: Cell<bool> = const { Cell::new(false) };
}

fn note() {
    if COUNTING.with(|c| c.get()) {
        EVENTS.fetch_add(1, Relaxed);
    }
}

unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        note();
        unsafe { System.alloc(layout) }
    }
    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        note();
        unsafe { System.dealloc(ptr, layout) }
    }
    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        note();
        unsafe { System.realloc(ptr, layout, new_size) }
    }
}

#[global_allocator]
static ALLOC: Counting = Counting;

#[test]
fn audio_path_never_allocates() {
    let rate = 48_000.0;
    let mut settings = FxSettings::default()
        .with(EffectKind::Pitch, &[("semitones", -4.0), ("formant", -2.0)])
        .with(EffectKind::Reverb, &[("decay", 2.0)]);
    for kind in EffectKind::ALL {
        settings.set_enabled(kind, true);
    }
    let fx = FxParams::from_settings(&settings);
    let mut core = EngineCore::with_chain(rate, 480, Chain::build(&EffectKind::ALL, &fx, rate, 480));
    // Prepared off the "audio thread", like the controller does.
    let mut spare = Some(Chain::build(&[EffectKind::Pitch, EffectKind::Reverb], &fx, rate, 480));

    let mut input = signals::vowel_glide(48_000, 6.0, 110.0, 230.0);
    let noise = signals::brown_noise(48_000, 6.0, 0.05, 1);
    input.iter_mut().zip(&noise).for_each(|(a, b)| *a += b);
    let mut buf = vec![0.0f32; 480];
    let mut retired = Vec::with_capacity(4);
    // What the engine does in each capture thread's first callback.
    voice_changer::dsp::fx::denoise::warm_up_thread();

    EVENTS.store(0, Relaxed);
    for (i, chunk) in input.chunks(480).enumerate() {
        buf[..chunk.len()].copy_from_slice(chunk);
        let n = chunk.len();
        COUNTING.with(|c| c.set(true));
        // Everything the UI can do while audio runs: toggles, parameter moves, bypass, mute.
        let reverb = fx.get(EffectKind::Reverb);
        reverb.slot.set_enabled(i % 200 < 150);
        reverb.set(0, (i % 100) as f32);
        fx.get(EffectKind::Eq).set(2, ((i % 48) as f32 - 24.0) / 2.0);
        fx.get(EffectKind::Pitch).set(0, ((i / 50) % 13) as f32 - 6.0);
        let params =
            CoreParams { bypass: (300..320).contains(&i), mute: (400..410).contains(&i), ..Default::default() };
        if i == 250
            && let Some(old) = core.set_chain(spare.take().unwrap())
        {
            retired.push(old); // capacity reserved above: no allocation
        }
        core.process(&mut buf[..n], params);
        if let Some(old) = core.take_retired() {
            retired.push(old);
        }
        COUNTING.with(|c| c.set(false));
    }
    let events = EVENTS.load(Relaxed);
    assert_eq!(events, 0, "audio path made {events} heap allocations/frees");
    assert_eq!(retired.len(), 1, "old chain handed back for disposal off the audio thread");
}

/// Diagnostic: allocations per effect (`cargo test --release --test no_alloc -- --ignored --nocapture`).
#[test]
#[ignore]
fn allocations_per_effect() {
    voice_changer::dsp::fx::denoise::warm_up_thread();
    for kind in EffectKind::ALL {
        let fx = FxParams::from_settings(&FxSettings::default().with(kind, &[]));
        let mut chain = Chain::build(&[kind], &fx, 48_000.0, 480);
        let x = signals::vowel(48_000, 2.0, 150.0);
        let mut buf = vec![0.0f32; 480];
        EVENTS.store(0, Relaxed);
        let mut first = None;
        for (i, c) in x.chunks(480).enumerate() {
            buf.copy_from_slice(c);
            COUNTING.with(|f| f.set(true));
            chain.process(&mut buf);
            COUNTING.with(|f| f.set(false));
            if first.is_none() && EVENTS.load(Relaxed) > 0 {
                first = Some(i);
            }
            if i == 0 {
                println!("  {kind:?}: {} events in the first block", EVENTS.load(Relaxed));
            }
        }
        println!("{kind:?}: {} events, first in block {first:?}", EVENTS.load(Relaxed));
    }
}
