#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]
//! Lensbridge desktop app: receives the phone camera/microphone and exposes
//! them as a virtual camera and an audio stream.

mod app;
mod config;
mod discovery;
mod qr;
mod vcam;

use eframe::egui;

fn main() -> eframe::Result {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).init();
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("Lensbridge")
            .with_inner_size([1200.0, 740.0])
            .with_min_inner_size([900.0, 560.0]),
        ..Default::default()
    };
    eframe::run_native("Lensbridge", options, Box::new(|cc| Ok(Box::new(app::LensbridgeApp::new(cc)))))
}
