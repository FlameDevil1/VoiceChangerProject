// No console window in release builds.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod gui;
mod single_instance;
mod toast;

use eframe::egui;
use single_instance::Instance;
use voice_changer::config::Config;

fn main() -> eframe::Result {
    // Before anything else (even logging, which rotates the log file): if the app is already
    // running, ask it to show its window and quit.
    let guard = match single_instance::acquire() {
        Instance::Secondary => return Ok(()),
        Instance::Primary(guard) => guard,
    };

    voice_changer::logging::init();
    log::info!("Voice Changer {} starting", env!("CARGO_PKG_VERSION"));
    let cfg = Config::load();
    // Launched by "Start with Windows": straight to the tray.
    let start_hidden = cfg.start_minimized || std::env::args().any(|a| a == voice_changer::autostart::ARG);
    voice_changer::autostart::repair();

    let options = eframe::NativeOptions {
        viewport: initial_viewport()
            .with_title("Voice Changer")
            .with_min_inner_size([380.0, 420.0])
            .with_icon(egui::IconData { rgba: gui::icon::rgba(64), width: 64, height: 64 })
            .with_visible(!start_hidden),
        // Measured: glow uses ~25% of the memory of wgpu for the same CPU cost.
        renderer: eframe::Renderer::Glow,
        multisampling: 0,
        depth_buffer: 0,
        stencil_buffer: 0,
        ..Default::default()
    };
    let result = eframe::run_native(
        "Voice Changer",
        options,
        Box::new(|cc| {
            let ctx = cc.egui_ctx.clone();
            let quit_ctx = ctx.clone();
            guard.listen(move || gui::bring_to_front(&ctx), move || gui::request_quit(&quit_ctx));
            Ok(Box::new(gui::App::new(cc, cfg, start_hidden, guard)))
        }),
    );
    log::info!("exit");
    result
}

/// 460 x 860 points, centred in the screen's work area and shortened on short screens (e.g.
/// 1366x768 laptops): the content scrolls, but a window running past the taskbar hides its bottom.
fn initial_viewport() -> egui::ViewportBuilder {
    use windows::Win32::Foundation::RECT;
    use windows::Win32::UI::WindowsAndMessaging::{
        SPI_GETWORKAREA, SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS, SystemParametersInfoW,
    };
    const SIZE: [f32; 2] = [460.0, 860.0];
    const TITLE_BAR: f32 = 32.0;
    let mut work = RECT::default();
    // SAFETY: SPI_GETWORKAREA writes one RECT. Before winit sets DPI awareness this is in
    // logical (96-DPI) pixels, the same units as the window size.
    let ok = unsafe {
        SystemParametersInfoW(SPI_GETWORKAREA, 0, Some((&raw mut work).cast()), SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS(0))
    };
    let (w, h) = ((work.right - work.left) as f32, (work.bottom - work.top) as f32);
    let builder = egui::ViewportBuilder::default();
    if ok.is_err() || w < SIZE[0] || h < 420.0 + TITLE_BAR {
        return builder.with_inner_size(SIZE);
    }
    let height = SIZE[1].min(h - TITLE_BAR - 16.0);
    let x = work.left as f32 + (w - SIZE[0]) / 2.0;
    let y = work.top as f32 + (h - height - TITLE_BAR) / 2.0;
    builder.with_inner_size([SIZE[0], height]).with_position([x, y])
}
