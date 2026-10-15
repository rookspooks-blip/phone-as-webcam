#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]
//! Lensbridge desktop app: receives the phone camera/microphone and exposes
//! them as a virtual camera and an audio stream.

mod app;
mod config;
mod discovery;
mod qr;
mod vcam;

use eframe::egui;

fn options(renderer: eframe::Renderer) -> eframe::NativeOptions {
    eframe::NativeOptions {
        renderer,
        viewport: egui::ViewportBuilder::default()
            .with_title("Lensbridge")
            .with_inner_size([1200.0, 740.0])
            .with_min_inner_size([900.0, 560.0]),
        ..Default::default()
    }
}

fn run(renderer: eframe::Renderer) -> eframe::Result {
    eframe::run_native(
        "Lensbridge",
        options(renderer),
        Box::new(|cc| Ok(Box::new(app::LensbridgeApp::new(cc)))),
    )
}

fn main() -> eframe::Result {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).init();
    // wgpu (Direct3D 12 / Vulkan / Metal) by default; OpenGL as a fallback for
    // old GPUs, remote desktops and VMs. LENSBRIDGE_RENDERER=glow forces it.
    if std::env::var("LENSBRIDGE_RENDERER").as_deref() == Ok("glow") {
        return run(eframe::Renderer::Glow);
    }
    match run(eframe::Renderer::Wgpu) {
        Err(e) => {
            log::warn!("wgpu renderer failed ({e}), falling back to OpenGL");
            run(eframe::Renderer::Glow)
        }
        ok => ok,
    }
}
