//! Windows virtual camera writer: publishes frames into shared memory for the
//! "Lensbridge Camera" DirectShow filter, plus filter install helpers.

use std::path::PathBuf;
use std::sync::atomic::{fence, AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::Arc;
use std::thread::{self, JoinHandle};
use std::time::Duration;

use lb_client::LatestFrame;
use windows_sys::Win32::Foundation::{CloseHandle, HANDLE, INVALID_HANDLE_VALUE};
use windows_sys::Win32::System::Memory::{
    CreateFileMappingW, MapViewOfFile, UnmapViewOfFile, FILE_MAP_ALL_ACCESS, MEMORY_MAPPED_VIEW_ADDRESS, PAGE_READWRITE,
};
use windows_sys::Win32::System::Registry::{RegCloseKey, RegOpenKeyExW, HKEY, HKEY_CLASSES_ROOT, KEY_READ};
use windows_sys::Win32::System::SystemInformation::GetTickCount64;
use windows_sys::Win32::UI::Shell::ShellExecuteW;
use windows_sys::Win32::UI::WindowsAndMessaging::SW_HIDE;

use super::{layout::*, prepare_i420};

const SHM_NAME: &str = "Local\\LensbridgeVirtualCamera1";
const CLSID: &str = "{CB4E2A03-21DA-4355-8213-E8E2E58EAF82}";

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

struct Mapping {
    handle: HANDLE,
    view: MEMORY_MAPPED_VIEW_ADDRESS,
}

// SAFETY: the mapping is only accessed from the writer thread after creation.
unsafe impl Send for Mapping {}

impl Mapping {
    fn create() -> Result<Self, String> {
        let name = wide(SHM_NAME);
        // SAFETY: plain Win32 calls with valid arguments; errors are checked.
        unsafe {
            let handle = CreateFileMappingW(
                INVALID_HANDLE_VALUE,
                std::ptr::null(),
                PAGE_READWRITE,
                0,
                TOTAL_SIZE as u32,
                name.as_ptr(),
            );
            if handle.is_null() {
                return Err(format!("CreateFileMapping: {}", std::io::Error::last_os_error()));
            }
            let view = MapViewOfFile(handle, FILE_MAP_ALL_ACCESS, 0, 0, TOTAL_SIZE);
            if view.Value.is_null() {
                CloseHandle(handle);
                return Err(format!("MapViewOfFile: {}", std::io::Error::last_os_error()));
            }
            Ok(Self { handle, view })
        }
    }

    fn base(&self) -> *mut u8 {
        self.view.Value as *mut u8
    }

    fn u32_at(&self, off: usize) -> &AtomicU32 {
        // SAFETY: offset is inside the mapping and 4-byte aligned (page-aligned base).
        unsafe { &*(self.base().add(off) as *const AtomicU32) }
    }

    fn u64_at(&self, off: usize) -> &AtomicU64 {
        // SAFETY: offset is inside the mapping and 8-byte aligned.
        unsafe { &*(self.base().add(off) as *const AtomicU64) }
    }

    fn init_header(&self) {
        self.u32_at(OFF_VERSION).store(VERSION, Ordering::Relaxed);
        self.u32_at(OFF_MAX_WIDTH).store(MAX_WIDTH as u32, Ordering::Relaxed);
        self.u32_at(OFF_MAX_HEIGHT).store(MAX_HEIGHT as u32, Ordering::Relaxed);
        self.u32_at(OFF_SLOT_SIZE).store(SLOT_SIZE as u32, Ordering::Relaxed);
        self.u32_at(OFF_FORMAT).store(FORMAT_I420, Ordering::Relaxed);
        fence(Ordering::Release);
        self.u32_at(OFF_MAGIC).store(MAGIC, Ordering::Release);
    }

    fn publish(&self, width: usize, height: usize, data: &[u8], frame_id: u64, pts_us: u64) {
        let latest = self.u32_at(OFF_LATEST_SLOT).load(Ordering::Acquire) & 1;
        let slot = (latest ^ 1) as usize;
        let slot_off = HEADER_SIZE + slot * SLOT_SIZE;
        let seq = self.u64_at(slot_off + SLOT_OFF_SEQ);
        let s = seq.load(Ordering::Relaxed);
        seq.store(s | 1, Ordering::Relaxed); // odd: writing
        fence(Ordering::Release);
        self.u32_at(slot_off + SLOT_OFF_WIDTH).store(width as u32, Ordering::Relaxed);
        self.u32_at(slot_off + SLOT_OFF_HEIGHT).store(height as u32, Ordering::Relaxed);
        self.u64_at(slot_off + SLOT_OFF_FRAME_ID).store(frame_id, Ordering::Relaxed);
        self.u64_at(slot_off + SLOT_OFF_PTS).store(pts_us, Ordering::Relaxed);
        let n = data.len().min(SLOT_SIZE - SLOT_HEADER_SIZE);
        // SAFETY: destination range lies within the slot's data area.
        unsafe { std::ptr::copy_nonoverlapping(data.as_ptr(), self.base().add(slot_off + SLOT_HEADER_SIZE), n) };
        fence(Ordering::Release);
        seq.store((s | 1) + 1, Ordering::Release); // even: stable
        self.u32_at(OFF_LATEST_SLOT).store(slot as u32, Ordering::Release);
        // SAFETY: trivial Win32 call.
        self.u64_at(OFF_HEARTBEAT).store(unsafe { GetTickCount64() }, Ordering::Release);
    }
}

impl Drop for Mapping {
    fn drop(&mut self) {
        // SAFETY: handles were created by us and are released once.
        unsafe {
            UnmapViewOfFile(self.view);
            CloseHandle(self.handle);
        }
    }
}

pub struct VcamWriter {
    stop: Arc<AtomicBool>,
    written: Arc<AtomicU64>,
    thread: Option<JoinHandle<()>>,
}

impl VcamWriter {
    pub fn start(frames: Arc<LatestFrame>, mirror: Arc<AtomicBool>) -> Result<Self, String> {
        let mapping = Mapping::create()?;
        mapping.init_header();
        let stop = Arc::new(AtomicBool::new(false));
        let written = Arc::new(AtomicU64::new(0));
        let (stop2, written2) = (stop.clone(), written.clone());
        let thread = thread::Builder::new()
            .name("vcam-writer".into())
            .spawn(move || {
                let mut seen = frames.generation();
                let mut id = 0u64;
                while !stop2.load(Ordering::Relaxed) {
                    if let Some((gen, frame)) = frames.wait_newer(seen, Duration::from_millis(200)) {
                        seen = gen;
                        let (w, h, data) = prepare_i420(&frame, mirror.load(Ordering::Relaxed));
                        id += 1;
                        mapping.publish(w, h, &data, id, frame.pts_us);
                        written2.fetch_add(1, Ordering::Relaxed);
                    }
                }
            })
            .map_err(|e| e.to_string())?;
        Ok(Self {
            stop,
            written,
            thread: Some(thread),
        })
    }

    pub fn frames_written(&self) -> u64 {
        self.written.load(Ordering::Relaxed)
    }
}

impl Drop for VcamWriter {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

/// True when the filter's COM class is registered (64-bit view).
pub fn filter_installed() -> bool {
    let key = wide(&format!("CLSID\\{CLSID}\\InprocServer32"));
    let mut h: HKEY = std::ptr::null_mut();
    // SAFETY: valid key path, handle closed below.
    let rc = unsafe { RegOpenKeyExW(HKEY_CLASSES_ROOT, key.as_ptr(), 0, KEY_READ, &mut h) };
    if rc == 0 {
        unsafe { RegCloseKey(h) };
        true
    } else {
        false
    }
}

fn filter_dlls() -> Vec<PathBuf> {
    let dir = std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|d| d.to_path_buf()))
        .unwrap_or_default();
    ["lensbridge_vcam.dll", "lensbridge_vcam_x86.dll"]
        .iter()
        .flat_map(|n| [dir.join("vcam").join(n), dir.join(n)])
        .filter(|p| p.is_file())
        .collect()
}

/// Registers (or unregisters) the filter DLLs with an elevated regsvr32 (UAC prompt).
pub fn install_filter(install: bool) -> Result<(), String> {
    let dlls = filter_dlls();
    if dlls.is_empty() {
        return Err("lensbridge_vcam.dll не найден рядом с программой (папка vcam\\)".into());
    }
    let mut seen_names = Vec::new();
    for dll in dlls {
        let name = dll.file_name().map(|n| n.to_owned());
        if seen_names.contains(&name) {
            continue;
        }
        seen_names.push(name);
        // 32-bit DLLs must be registered by the 32-bit regsvr32 (SysWOW64).
        let is_x86 = dll.to_string_lossy().ends_with("_x86.dll");
        let windir = std::env::var("WINDIR").unwrap_or_else(|_| "C:\\Windows".into());
        let regsvr = if is_x86 {
            format!("{windir}\\SysWOW64\\regsvr32.exe")
        } else {
            format!("{windir}\\System32\\regsvr32.exe")
        };
        let params = format!("/s {}\"{}\"", if install { "" } else { "/u " }, dll.display());
        let (verb, file, params) = (wide("runas"), wide(&regsvr), wide(&params));
        // SAFETY: valid null-terminated wide strings.
        let rc = unsafe {
            ShellExecuteW(
                std::ptr::null_mut(),
                verb.as_ptr(),
                file.as_ptr(),
                params.as_ptr(),
                std::ptr::null(),
                SW_HIDE,
            )
        };
        if (rc as isize) <= 32 {
            return Err(format!(
                "не удалось запустить regsvr32 (код {}). Запустите от администратора вручную",
                rc as isize
            ));
        }
    }
    Ok(())
}
