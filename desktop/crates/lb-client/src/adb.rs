//! Android over USB through `adb forward`.
//!
//! Requires Android platform-tools (`adb`) and "USB debugging" enabled on the
//! phone. We look for adb in `LENSBRIDGE_ADB`, next to our executable
//! (`platform-tools/adb`), in `ANDROID_HOME`/`ANDROID_SDK_ROOT`, then `PATH`.

use std::io;
use std::path::PathBuf;
use std::process::{Command, Output};
use std::time::Duration;

use crate::transport::{connect_tcp, Duplex};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AdbDevice {
    pub serial: String,
    pub state: String,
    pub model: Option<String>,
}

fn exe_name() -> &'static str {
    if cfg!(windows) {
        "adb.exe"
    } else {
        "adb"
    }
}

pub fn find_adb() -> Option<PathBuf> {
    if let Ok(p) = std::env::var("LENSBRIDGE_ADB") {
        return Some(PathBuf::from(p));
    }
    let mut candidates = Vec::new();
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            candidates.push(dir.join("platform-tools").join(exe_name()));
            candidates.push(dir.join(exe_name()));
        }
    }
    for var in ["ANDROID_HOME", "ANDROID_SDK_ROOT"] {
        if let Ok(root) = std::env::var(var) {
            candidates.push(PathBuf::from(root).join("platform-tools").join(exe_name()));
        }
    }
    if let Some(paths) = std::env::var_os("PATH") {
        candidates.extend(std::env::split_paths(&paths).map(|p| p.join(exe_name())));
    }
    candidates.into_iter().find(|p| p.is_file())
}

fn run(args: &[&str]) -> io::Result<Output> {
    let adb = find_adb().ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::NotFound,
            "adb не найден: установите Android platform-tools и добавьте в PATH",
        )
    })?;
    let mut cmd = Command::new(adb);
    cmd.args(args);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }
    let out = cmd.output()?;
    if !out.status.success() {
        let msg = String::from_utf8_lossy(&out.stderr).trim().to_string();
        return Err(io::Error::other(format!("adb {}: {msg}", args.join(" "))));
    }
    Ok(out)
}

pub fn parse_devices(text: &str) -> Vec<AdbDevice> {
    text.lines()
        .skip_while(|l| !l.starts_with("List of devices"))
        .skip(1)
        .filter_map(|line| {
            let mut parts = line.split_whitespace();
            let serial = parts.next()?.to_string();
            let state = parts.next()?.to_string();
            let model = parts.find_map(|p| p.strip_prefix("model:")).map(|m| m.replace('_', " "));
            Some(AdbDevice { serial, state, model })
        })
        .collect()
}

pub fn list_devices() -> io::Result<Vec<AdbDevice>> {
    let out = run(&["devices", "-l"])?;
    Ok(parse_devices(&String::from_utf8_lossy(&out.stdout)))
}

fn with_serial<'a>(serial: Option<&'a str>, rest: &[&'a str]) -> Vec<&'a str> {
    let mut v = Vec::new();
    if let Some(s) = serial {
        v.push("-s");
        v.push(s);
    }
    v.extend_from_slice(rest);
    v
}

/// Ensures `tcp:<local> → tcp:<remote_port>` exists; returns the local port.
pub fn forward(serial: Option<&str>, remote_port: u16) -> io::Result<u16> {
    let remote = format!("tcp:{remote_port}");
    // Reuse an existing rule instead of piling up new ones on every reconnect.
    if let Ok(out) = run(&with_serial(serial, &["forward", "--list"])) {
        for line in String::from_utf8_lossy(&out.stdout).lines() {
            let parts: Vec<_> = line.split_whitespace().collect();
            if parts.len() == 3 && parts[2] == remote && serial.is_none_or(|s| s == parts[0]) {
                if let Some(p) = parts[1].strip_prefix("tcp:").and_then(|p| p.parse().ok()) {
                    return Ok(p);
                }
            }
        }
    }
    let out = run(&with_serial(serial, &["forward", "tcp:0", &remote]))?;
    String::from_utf8_lossy(&out.stdout)
        .trim()
        .parse()
        .map_err(|_| io::Error::other("adb forward не вернул номер порта"))
}

pub fn connect(serial: Option<&str>, remote_port: u16, timeout: Duration) -> io::Result<Box<dyn Duplex>> {
    let local = forward(serial, remote_port)?;
    connect_tcp("127.0.0.1", local, timeout)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_devices_output() {
        let text = "* daemon started successfully\nList of devices attached\nR58M123ABC     device usb:1-1 product:beyond1 model:SM_G973F device:beyond1 transport_id:1\nemulator-5554  offline\n\n";
        let d = parse_devices(text);
        assert_eq!(d.len(), 2);
        assert_eq!(d[0].serial, "R58M123ABC");
        assert_eq!(d[0].state, "device");
        assert_eq!(d[0].model.as_deref(), Some("SM G973F"));
        assert_eq!(d[1].state, "offline");
    }
}
