// No console window in release builds.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod gui;

use eframe::egui;
use voice_changer::config::Config;

fn main() -> eframe::Result {
    voice_changer::logging::init();
    log::info!("Voice Changer {} starting", env!("CARGO_PKG_VERSION"));
    let cfg = Config::load();

    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("Voice Changer")
            .with_inner_size([460.0, 700.0])
            .with_min_inner_size([380.0, 420.0]),
        // Measured: glow uses ~25% of the memory of wgpu for the same CPU cost.
        renderer: eframe::Renderer::Glow,
        multisampling: 0,
        depth_buffer: 0,
        stencil_buffer: 0,
        ..Default::default()
    };
    let result = eframe::run_native("Voice Changer", options, Box::new(|cc| Ok(Box::new(gui::App::new(cc, cfg)))));
    log::info!("exit");
    result
}
