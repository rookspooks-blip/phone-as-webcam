//! Persistent settings. The pairing key is protected with DPAPI on Windows
//! (bound to the Windows user account); elsewhere the file is created 0600.

use std::path::PathBuf;

use lb_client::Target;
use lb_proto::PairingKey;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum SavedTarget {
    Tcp { host: String, port: u16 },
    Usbmux { udid: String, port: u16 },
    Adb { serial: Option<String>, port: u16 },
}

impl From<&Target> for SavedTarget {
    fn from(t: &Target) -> Self {
        match t {
            Target::Tcp { host, port } => SavedTarget::Tcp {
                host: host.clone(),
                port: *port,
            },
            Target::Usbmux { udid, port } => SavedTarget::Usbmux {
                udid: udid.clone(),
                port: *port,
            },
            Target::Adb { serial, port } => SavedTarget::Adb {
                serial: serial.clone(),
                port: *port,
            },
        }
    }
}

impl From<&SavedTarget> for Target {
    fn from(t: &SavedTarget) -> Self {
        match t {
            SavedTarget::Tcp { host, port } => Target::Tcp {
                host: host.clone(),
                port: *port,
            },
            SavedTarget::Usbmux { udid, port } => Target::Usbmux {
                udid: udid.clone(),
                port: *port,
            },
            SavedTarget::Adb { serial, port } => Target::Adb {
                serial: serial.clone(),
                port: *port,
            },
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    /// Pairing key: "dpapi:<base64>" on Windows, "raw:<base64url>" elsewhere.
    pub key: Option<String>,
    pub last_target: Option<SavedTarget>,
    pub manual_host: String,
    /// `None` = do not play phone audio.
    pub audio_device: Option<String>,
    pub vcam_enabled: bool,
    pub mirror: bool,
    pub auto_connect: bool,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            key: None,
            last_target: None,
            manual_host: String::new(),
            audio_device: None,
            vcam_enabled: true,
            mirror: false,
            auto_connect: true,
        }
    }
}

pub fn config_dir() -> PathBuf {
    dirs::config_dir().unwrap_or_else(std::env::temp_dir).join("Lensbridge")
}

fn config_path() -> PathBuf {
    config_dir().join("config.json")
}

impl Config {
    pub fn load() -> Self {
        std::fs::read(config_path())
            .ok()
            .and_then(|b| serde_json::from_slice(&b).ok())
            .unwrap_or_default()
    }

    pub fn save(&self) {
        let dir = config_dir();
        if let Err(e) = std::fs::create_dir_all(&dir) {
            log::warn!("config dir: {e}");
            return;
        }
        let path = config_path();
        let data = serde_json::to_vec_pretty(self).expect("serialize config");
        if let Err(e) = write_private(&path, &data) {
            log::warn!("save config: {e}");
        }
    }

    /// Returns the pairing key, creating (and saving) one on first use.
    pub fn pairing_key(&mut self) -> PairingKey {
        if let Some(k) = self.key.as_deref().and_then(decode_key) {
            return k;
        }
        let k = PairingKey::generate();
        self.set_key(&k);
        k
    }

    pub fn set_key(&mut self, k: &PairingKey) {
        self.key = Some(encode_key(k));
        self.save();
    }
}

fn write_private(path: &std::path::Path, data: &[u8]) -> std::io::Result<()> {
    #[cfg(unix)]
    {
        use std::io::Write;
        use std::os::unix::fs::OpenOptionsExt;
        let mut f = std::fs::OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(true)
            .mode(0o600)
            .open(path)?;
        f.write_all(data)
    }
    #[cfg(not(unix))]
    {
        std::fs::write(path, data)
    }
}

fn encode_key(k: &PairingKey) -> String {
    #[cfg(windows)]
    {
        use base64::Engine;
        if let Some(blob) = dpapi::protect(k.bytes()) {
            return format!("dpapi:{}", base64::engine::general_purpose::STANDARD.encode(blob));
        }
    }
    format!("raw:{}", k.to_base64())
}

fn decode_key(s: &str) -> Option<PairingKey> {
    if let Some(raw) = s.strip_prefix("raw:") {
        return PairingKey::from_base64(raw).ok();
    }
    #[cfg(windows)]
    if let Some(b64) = s.strip_prefix("dpapi:") {
        use base64::Engine;
        let blob = base64::engine::general_purpose::STANDARD.decode(b64).ok()?;
        let bytes: [u8; 32] = dpapi::unprotect(&blob)?.try_into().ok()?;
        return Some(PairingKey::from_bytes(bytes));
    }
    None
}

#[cfg(windows)]
mod dpapi {
    use windows_sys::Win32::Foundation::LocalFree;
    use windows_sys::Win32::Security::Cryptography::{CryptProtectData, CryptUnprotectData, CRYPTPROTECT_UI_FORBIDDEN, CRYPT_INTEGER_BLOB};

    fn call(input: &[u8], protect: bool) -> Option<Vec<u8>> {
        let in_blob = CRYPT_INTEGER_BLOB {
            cbData: input.len() as u32,
            pbData: input.as_ptr() as *mut u8,
        };
        let mut out = CRYPT_INTEGER_BLOB {
            cbData: 0,
            pbData: std::ptr::null_mut(),
        };
        // SAFETY: valid input blob, out blob is filled by the API and freed with LocalFree.
        let ok = unsafe {
            if protect {
                CryptProtectData(
                    &in_blob,
                    std::ptr::null(),
                    std::ptr::null(),
                    std::ptr::null(),
                    std::ptr::null(),
                    CRYPTPROTECT_UI_FORBIDDEN,
                    &mut out,
                )
            } else {
                CryptUnprotectData(
                    &in_blob,
                    std::ptr::null_mut(),
                    std::ptr::null(),
                    std::ptr::null(),
                    std::ptr::null(),
                    CRYPTPROTECT_UI_FORBIDDEN,
                    &mut out,
                )
            }
        };
        if ok == 0 || out.pbData.is_null() {
            return None;
        }
        // SAFETY: the API returned a buffer of cbData bytes.
        let v = unsafe { std::slice::from_raw_parts(out.pbData, out.cbData as usize).to_vec() };
        unsafe { LocalFree(out.pbData as _) };
        Some(v)
    }

    pub fn protect(data: &[u8]) -> Option<Vec<u8>> {
        call(data, true)
    }

    pub fn unprotect(data: &[u8]) -> Option<Vec<u8>> {
        call(data, false)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn key_roundtrip() {
        let k = PairingKey::generate();
        assert_eq!(decode_key(&encode_key(&k)), Some(k));
    }

    #[test]
    fn target_roundtrip() {
        let t = Target::Usbmux {
            udid: "abc".into(),
            port: 4748,
        };
        let s = SavedTarget::from(&t);
        let json = serde_json::to_string(&s).unwrap();
        assert!(json.contains("\"kind\":\"usbmux\""));
        let back: SavedTarget = serde_json::from_str(&json).unwrap();
        assert_eq!(Target::from(&back), t);
    }
}
