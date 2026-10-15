//! Minimal usbmuxd client (iPhone over USB).
//!
//! usbmuxd is Apple's USB multiplexer: on macOS it is built in
//! (`/var/run/usbmuxd`), on Windows it is the "Apple Mobile Device Service"
//! installed with the Apple Devices app or iTunes (TCP 127.0.0.1:27015), on
//! Linux it is the `usbmuxd` package. After a successful `Connect` the socket
//! becomes a raw TCP tunnel to a port on the phone.
//!
//! Wire format: 16-byte little-endian header (length incl. header, version=1,
//! message=8 "plist", tag) followed by an XML plist.

use std::io;

use plist::{Dictionary, Value};

use crate::transport::Duplex;

const VERSION_PLIST: u32 = 1;
const MESSAGE_PLIST: u32 = 8;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UsbDevice {
    pub device_id: u64,
    pub udid: String,
    pub product_id: Option<u64>,
}

fn open_socket() -> io::Result<Box<dyn Duplex>> {
    #[cfg(windows)]
    {
        let s = std::net::TcpStream::connect(("127.0.0.1", 27015)).map_err(|e| {
            io::Error::new(
                e.kind(),
                format!("служба Apple Mobile Device недоступна ({e}). Установите приложение «Устройства Apple» (Microsoft Store) или iTunes"),
            )
        })?;
        s.set_nodelay(true)?;
        Ok(Box::new(s))
    }
    #[cfg(unix)]
    {
        let path = std::env::var("USBMUXD_SOCKET_ADDRESS").unwrap_or_else(|_| "/var/run/usbmuxd".into());
        let s = std::os::unix::net::UnixStream::connect(&path)
            .map_err(|e| io::Error::new(e.kind(), format!("usbmuxd недоступен по {path} ({e}). На Linux установите пакет usbmuxd")))?;
        Ok(Box::new(s))
    }
}

fn request(kind: &str) -> Dictionary {
    let mut d = Dictionary::new();
    d.insert("MessageType".into(), Value::String(kind.into()));
    d.insert("ClientVersionString".into(), Value::String(concat!("lensbridge ", env!("CARGO_PKG_VERSION")).into()));
    d.insert("ProgName".into(), Value::String("lensbridge".into()));
    d
}

fn send(sock: &mut dyn Duplex, tag: u32, dict: Dictionary) -> io::Result<()> {
    let mut body = Vec::new();
    plist::to_writer_xml(&mut body, &Value::Dictionary(dict)).map_err(io::Error::other)?;
    let mut msg = Vec::with_capacity(16 + body.len());
    msg.extend_from_slice(&((16 + body.len()) as u32).to_le_bytes());
    msg.extend_from_slice(&VERSION_PLIST.to_le_bytes());
    msg.extend_from_slice(&MESSAGE_PLIST.to_le_bytes());
    msg.extend_from_slice(&tag.to_le_bytes());
    msg.extend_from_slice(&body);
    sock.write_all(&msg)?;
    sock.flush()
}

fn recv(sock: &mut dyn Duplex) -> io::Result<Dictionary> {
    let mut hdr = [0u8; 16];
    sock.read_exact(&mut hdr)?;
    let len = u32::from_le_bytes(hdr[0..4].try_into().unwrap()) as usize;
    if !(16..=1 << 20).contains(&len) {
        return Err(io::Error::new(io::ErrorKind::InvalidData, "usbmuxd: bad message length"));
    }
    let mut body = vec![0u8; len - 16];
    sock.read_exact(&mut body)?;
    match plist::from_bytes::<Value>(&body).map_err(io::Error::other)? {
        Value::Dictionary(d) => Ok(d),
        _ => Err(io::Error::new(io::ErrorKind::InvalidData, "usbmuxd: reply is not a dictionary")),
    }
}

/// Lists iPhones/iPads currently attached over USB.
pub fn list_devices() -> io::Result<Vec<UsbDevice>> {
    let mut sock = open_socket()?;
    send(sock.as_mut(), 1, request("ListDevices"))?;
    let reply = recv(sock.as_mut())?;
    let mut out = Vec::new();
    if let Some(Value::Array(list)) = reply.get("DeviceList") {
        for entry in list {
            let Some(d) = entry.as_dictionary() else { continue };
            let Some(props) = d.get("Properties").and_then(Value::as_dictionary) else { continue };
            if props.get("ConnectionType").and_then(Value::as_string) != Some("USB") {
                continue;
            }
            let device_id = d
                .get("DeviceID")
                .and_then(Value::as_unsigned_integer)
                .or_else(|| props.get("DeviceID").and_then(Value::as_unsigned_integer));
            let udid = props.get("SerialNumber").and_then(Value::as_string);
            if let (Some(device_id), Some(udid)) = (device_id, udid) {
                out.push(UsbDevice {
                    device_id,
                    udid: udid.to_string(),
                    product_id: props.get("ProductID").and_then(Value::as_unsigned_integer),
                });
            }
        }
    }
    Ok(out)
}

/// Opens a tunnel to `port` on the device with the given UDID.
pub fn connect(udid: &str, port: u16) -> io::Result<Box<dyn Duplex>> {
    let dev = list_devices()?
        .into_iter()
        .find(|d| d.udid == udid)
        .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "iPhone не подключён по USB (или не нажато «Доверять этому компьютеру»)"))?;
    let mut sock = open_socket()?;
    let mut req = request("Connect");
    req.insert("DeviceID".into(), Value::Integer(dev.device_id.into()));
    // usbmuxd expects the port in network byte order inside a host integer.
    req.insert("PortNumber".into(), Value::Integer((port.swap_bytes() as u64).into()));
    send(sock.as_mut(), 2, req)?;
    let reply = recv(sock.as_mut())?;
    match reply.get("Number").and_then(Value::as_unsigned_integer) {
        Some(0) => Ok(sock),
        Some(3) => Err(io::Error::new(io::ErrorKind::ConnectionRefused, "приложение Lensbridge на iPhone не запущено (порт закрыт)")),
        Some(n) => Err(io::Error::other(format!("usbmuxd Connect вернул код {n}"))),
        None => Err(io::Error::new(io::ErrorKind::InvalidData, "usbmuxd: ответ без кода")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn port_is_network_order() {
        assert_eq!(4748u16.swap_bytes(), 0x8c12);
    }

    #[test]
    fn request_serializes_as_xml_plist() {
        let mut body = Vec::new();
        plist::to_writer_xml(&mut body, &Value::Dictionary(request("ListDevices"))).unwrap();
        let s = String::from_utf8(body).unwrap();
        assert!(s.contains("<key>MessageType</key>"));
        assert!(s.contains("<string>ListDevices</string>"));
    }
}
