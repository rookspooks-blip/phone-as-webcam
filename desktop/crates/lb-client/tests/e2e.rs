//! End-to-end: lb-client ↔ lb-sim over real TCP on localhost.

use std::net::TcpListener;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use lb_client::{spawn, ClientConfig, ConnState, Event, Target};
use lb_proto::PairingKey;
use serde_json::json;

struct Sim {
    port: u16,
    stop: Arc<AtomicBool>,
}

impl Drop for Sim {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
    }
}

fn start_sim(key: &PairingKey) -> Sim {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let stop = Arc::new(AtomicBool::new(false));
    let (k, s) = (key.clone(), stop.clone());
    thread::spawn(move || lb_sim::serve(listener, k, Default::default(), s));
    Sim { port, stop }
}

fn wait_for<F: FnMut(&Event) -> bool>(rx: &crossbeam_channel::Receiver<Event>, timeout: Duration, mut f: F) -> bool {
    let deadline = Instant::now() + timeout;
    while let Some(left) = deadline.checked_duration_since(Instant::now()) {
        match rx.recv_timeout(left) {
            Ok(ev) if f(&ev) => return true,
            Ok(_) => {}
            Err(_) => return false,
        }
    }
    false
}

#[test]
fn streams_video_audio_and_applies_settings() {
    let key = PairingKey::generate();
    let sim = start_sim(&key);
    let audio = Arc::new(Mutex::new(0usize));
    let audio2 = audio.clone();
    let client = spawn(ClientConfig {
        target: Target::Tcp { host: "127.0.0.1".into(), port: sim.port },
        key,
        client_name: "test".into(),
        audio_sink: Some(Arc::new(move |c| *audio2.lock().unwrap() += c.samples.len())),
    });

    assert!(wait_for(&client.events, Duration::from_secs(10), |e| matches!(e, Event::State(ConnState::Connected { .. }))));
    assert!(wait_for(&client.events, Duration::from_secs(5), |e| matches!(e, Event::Control(v) if v["t"] == "caps")));

    // Frames arrive and decode at the default 640x360.
    let (gen, f) = client.frames.wait_newer(0, Duration::from_secs(5)).expect("first frame");
    assert_eq!((f.width, f.height), (640, 360));
    assert_eq!(f.i420.len(), 640 * 360 * 3 / 2);

    // Switch resolution: ack reports it and frames follow.
    client.set(json!({"w": 1280, "h": 720, "fps": 30}));
    assert!(wait_for(&client.events, Duration::from_secs(5), |e| matches!(e, Event::Control(v) if v["t"] == "ack" && v["applied"]["w"] == 1280)));
    let deadline = Instant::now() + Duration::from_secs(5);
    let mut seen = gen;
    let mut switched = false;
    while Instant::now() < deadline {
        if let Some((g, f)) = client.frames.wait_newer(seen, Duration::from_millis(500)) {
            seen = g;
            if (f.width, f.height) == (1280, 720) {
                switched = true;
                break;
            }
        }
    }
    assert!(switched, "resolution switch not observed");

    // Stats with clock-synced latency (sim clock is offset by 10 000 s).
    assert!(wait_for(&client.events, Duration::from_secs(5), |e| matches!(e, Event::Stats(s) if s.fps > 5.0 && s.latency_ms.is_some_and(|l| (0.0..1000.0).contains(&l)))));
    assert!(*audio.lock().unwrap() >= 48_000 / 2, "expected at least 0.5 s of audio");
    client.stop();
}

#[test]
fn wrong_key_reports_not_paired() {
    let sim = start_sim(&PairingKey::generate());
    let client = spawn(ClientConfig {
        target: Target::Tcp { host: "127.0.0.1".into(), port: sim.port },
        key: PairingKey::generate(),
        client_name: "test".into(),
        audio_sink: None,
    });
    assert!(wait_for(&client.events, Duration::from_secs(10), |e| matches!(e, Event::State(ConnState::NotPaired))));
}

#[test]
fn reconnects_after_server_restart() {
    let key = PairingKey::generate();
    let sim = start_sim(&key);
    let port = sim.port;
    let client = spawn(ClientConfig {
        target: Target::Tcp { host: "127.0.0.1".into(), port },
        key: key.clone(),
        client_name: "test".into(),
        audio_sink: None,
    });
    assert!(wait_for(&client.events, Duration::from_secs(10), |e| matches!(e, Event::State(ConnState::Connected { .. }))));
    drop(sim); // stops accepting; existing session ends via stop flag
    assert!(wait_for(&client.events, Duration::from_secs(10), |e| matches!(e, Event::State(ConnState::Disconnected { .. }))));
    // Same port again.
    thread::sleep(Duration::from_millis(300));
    let listener = TcpListener::bind(("127.0.0.1", port)).unwrap();
    let stop = Arc::new(AtomicBool::new(false));
    let s = stop.clone();
    thread::spawn(move || lb_sim::serve(listener, key, Default::default(), s));
    assert!(wait_for(&client.events, Duration::from_secs(15), |e| matches!(e, Event::State(ConnState::Connected { .. }))));
    stop.store(true, Ordering::Relaxed);
}
