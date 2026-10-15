//! Byte-stream transports to the phone. All of them end up as a plain
//! bidirectional stream carrying `lbp/1`.

use std::io::{self, Read, Write};
use std::net::{Shutdown, TcpStream, ToSocketAddrs};
use std::time::Duration;

/// A bidirectional stream that can be split into a reader and a writer.
pub trait Duplex: Read + Write + Send {
    fn try_clone_duplex(&self) -> io::Result<Box<dyn Duplex>>;
    fn set_read_timeout_duplex(&self, d: Option<Duration>) -> io::Result<()>;
    fn shutdown_duplex(&self);
}

impl Duplex for TcpStream {
    fn try_clone_duplex(&self) -> io::Result<Box<dyn Duplex>> {
        Ok(Box::new(self.try_clone()?))
    }
    fn set_read_timeout_duplex(&self, d: Option<Duration>) -> io::Result<()> {
        self.set_read_timeout(d)
    }
    fn shutdown_duplex(&self) {
        let _ = self.shutdown(Shutdown::Both);
    }
}

#[cfg(unix)]
impl Duplex for std::os::unix::net::UnixStream {
    fn try_clone_duplex(&self) -> io::Result<Box<dyn Duplex>> {
        Ok(Box::new(self.try_clone()?))
    }
    fn set_read_timeout_duplex(&self, d: Option<Duration>) -> io::Result<()> {
        self.set_read_timeout(d)
    }
    fn shutdown_duplex(&self) {
        let _ = self.shutdown(Shutdown::Both);
    }
}

/// Where the phone is.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Target {
    /// Wi-Fi / LAN: host name or IP and port.
    Tcp { host: String, port: u16 },
    /// iPhone over USB via usbmuxd (Apple Mobile Device Service on Windows).
    Usbmux { udid: String, port: u16 },
    /// Android over USB via `adb forward`.
    Adb { serial: Option<String>, port: u16 },
}

impl Target {
    pub fn label(&self) -> String {
        match self {
            Target::Tcp { host, port } => format!("Wi-Fi {host}:{port}"),
            Target::Usbmux { udid, .. } => format!("USB iPhone {}", short(udid)),
            Target::Adb { serial, .. } => format!("USB Android {}", serial.as_deref().unwrap_or("(любой)")),
        }
    }

    pub fn is_usb(&self) -> bool {
        !matches!(self, Target::Tcp { .. })
    }
}

fn short(s: &str) -> &str {
    &s[..s.len().min(8)]
}

pub fn connect(target: &Target, timeout: Duration) -> io::Result<Box<dyn Duplex>> {
    match target {
        Target::Tcp { host, port } => connect_tcp(host, *port, timeout),
        Target::Usbmux { udid, port } => crate::usbmux::connect(udid, *port),
        Target::Adb { serial, port } => crate::adb::connect(serial.as_deref(), *port, timeout),
    }
}

pub fn connect_tcp(host: &str, port: u16, timeout: Duration) -> io::Result<Box<dyn Duplex>> {
    let host = host.trim().trim_start_matches('[').trim_end_matches(']');
    let addrs: Vec<_> = (host, port).to_socket_addrs()?.collect();
    let mut last_err = io::Error::new(io::ErrorKind::NotFound, "адрес не найден");
    for addr in addrs {
        match TcpStream::connect_timeout(&addr, timeout) {
            Ok(s) => {
                s.set_nodelay(true)?;
                return Ok(Box::new(s));
            }
            Err(e) => last_err = e,
        }
    }
    Err(last_err)
}
