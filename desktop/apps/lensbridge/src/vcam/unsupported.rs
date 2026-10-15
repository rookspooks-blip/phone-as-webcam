use std::sync::atomic::AtomicBool;
use std::sync::Arc;

use lb_client::LatestFrame;

pub struct VcamWriter;

impl VcamWriter {
    pub fn start(_frames: Arc<LatestFrame>, _mirror: Arc<AtomicBool>) -> Result<Self, String> {
        Err(if cfg!(target_os = "macos") {
            "Своя виртуальная камера на macOS требует подписи Developer ID. Используйте «Камеру Continuity» или OBS (см. README)".into()
        } else {
            "Виртуальная камера в этой сборке реализована только для Windows".into()
        })
    }

    pub fn frames_written(&self) -> u64 {
        0
    }
}

pub fn filter_installed() -> bool {
    false
}

pub fn install_filter(_install: bool) -> Result<(), String> {
    Err("Только для Windows".into())
}
