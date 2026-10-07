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

    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("Voice Changer")
            .with_inner_size([460.0, 860.0])
            .with_min_inner_size([380.0, 420.0])
            .with_icon(egui::IconData { rgba: gui::icon::rgba(64), width: 64, height: 64 })
            .with_visible(!cfg.start_minimized),
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
            guard.listen(move || gui::bring_to_front(&ctx));
            Ok(Box::new(gui::App::new(cc, cfg, guard)))
        }),
    );
    log::info!("exit");
    result
}
