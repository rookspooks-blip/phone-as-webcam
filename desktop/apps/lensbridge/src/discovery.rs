//! Finds phones: Bonjour/mDNS on the LAN, iPhones over USB (usbmuxd) and
//! Android phones over USB (adb). Everything runs in background threads.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use lb_client::{adb, usbmux, Target};
use lb_proto::{DEFAULT_PORT, SERVICE_TYPE};
use mdns_sd::{ServiceDaemon, ServiceEvent};

#[derive(Debug, Clone, PartialEq)]
pub struct Found {
    pub name: String,
    pub detail: String,
    pub target: Target,
}

#[derive(Default)]
struct State {
    wifi: BTreeMap<String, Found>,
    usb: Vec<Found>,
    mdns_error: Option<String>,
    usbmux_error: Option<String>,
    adb_error: Option<String>,
}

pub struct Discovery {
    state: Arc<Mutex<State>>,
}

impl Discovery {
    pub fn start(ctx: eframe::egui::Context) -> Self {
        let state = Arc::new(Mutex::new(State::default()));
        {
            let state = state.clone();
            let ctx = ctx.clone();
            thread::Builder::new().name("mdns".into()).spawn(move || mdns_loop(state, ctx)).ok();
        }
        {
            let state = state.clone();
            thread::Builder::new()
                .name("usb-scan".into())
                .spawn(move || usb_loop(state, ctx))
                .ok();
        }
        Self { state }
    }

    pub fn devices(&self) -> Vec<Found> {
        let s = self.state.lock().unwrap();
        s.usb.iter().cloned().chain(s.wifi.values().cloned()).collect()
    }

    pub fn problems(&self) -> Vec<String> {
        let s = self.state.lock().unwrap();
        [&s.mdns_error, &s.usbmux_error, &s.adb_error]
            .into_iter()
            .flatten()
            .cloned()
            .collect()
    }
}

fn mdns_loop(state: Arc<Mutex<State>>, ctx: eframe::egui::Context) {
    let daemon = match ServiceDaemon::new() {
        Ok(d) => d,
        Err(e) => {
            state.lock().unwrap().mdns_error = Some(format!("mDNS недоступен: {e}"));
            return;
        }
    };
    let rx = match daemon.browse(SERVICE_TYPE) {
        Ok(rx) => rx,
        Err(e) => {
            state.lock().unwrap().mdns_error = Some(format!("mDNS browse: {e}"));
            return;
        }
    };
    while let Ok(event) = rx.recv() {
        match event {
            ServiceEvent::ServiceResolved(info) => {
                let mut v4: Vec<_> = info.get_addresses_v4().into_iter().collect();
                v4.sort();
                let host = match v4.first() {
                    Some(ip) => ip.to_string(),
                    None => info.get_hostname().trim_end_matches('.').to_string(),
                };
                let name = info
                    .get_property_val_str("name")
                    .map(str::to_string)
                    .unwrap_or_else(|| info.get_fullname().split('.').next().unwrap_or("Телефон").to_string());
                let port = if info.get_port() == 0 { DEFAULT_PORT } else { info.get_port() };
                let found = Found {
                    name,
                    detail: format!("Wi-Fi {host}:{port}"),
                    target: Target::Tcp { host, port },
                };
                state.lock().unwrap().wifi.insert(info.get_fullname().to_string(), found);
                ctx.request_repaint();
            }
            ServiceEvent::ServiceRemoved(_, fullname) => {
                state.lock().unwrap().wifi.remove(&fullname);
                ctx.request_repaint();
            }
            _ => {}
        }
    }
}

fn usb_loop(state: Arc<Mutex<State>>, ctx: eframe::egui::Context) {
    loop {
        let mut usb = Vec::new();
        let usbmux_error = match usbmux::list_devices() {
            Ok(devs) => {
                for d in devs {
                    usb.push(Found {
                        name: "iPhone (USB)".into(),
                        detail: format!("USB · {}", &d.udid[..d.udid.len().min(12)]),
                        target: Target::Usbmux {
                            udid: d.udid,
                            port: DEFAULT_PORT,
                        },
                    });
                }
                None
            }
            Err(e) => Some(format!("iPhone по USB: {e}")),
        };
        let adb_error = if adb::find_adb().is_none() {
            None // adb not installed: Android USB simply unavailable, not an error
        } else {
            match adb::list_devices() {
                Ok(devs) => {
                    for d in devs {
                        if d.state == "device" {
                            usb.push(Found {
                                name: d.model.clone().unwrap_or_else(|| "Android (USB)".into()),
                                detail: format!("USB · adb {}", d.serial),
                                target: Target::Adb {
                                    serial: Some(d.serial),
                                    port: DEFAULT_PORT,
                                },
                            });
                        } else if d.state == "unauthorized" {
                            usb.push(Found {
                                name: "Android: разрешите отладку на телефоне".into(),
                                detail: format!("adb {} (unauthorized)", d.serial),
                                target: Target::Adb {
                                    serial: Some(d.serial),
                                    port: DEFAULT_PORT,
                                },
                            });
                        }
                    }
                    None
                }
                Err(e) => Some(format!("adb: {e}")),
            }
        };
        {
            let mut s = state.lock().unwrap();
            let changed = s.usb != usb;
            s.usb = usb;
            s.usbmux_error = usbmux_error;
            s.adb_error = adb_error;
            if changed {
                ctx.request_repaint();
            }
        }
        thread::sleep(Duration::from_secs(2));
    }
}
