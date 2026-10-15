//! Phone simulator: a `lbp/1` server producing an H.264 test pattern and a
//! sine tone. Used by integration tests and for developing the desktop app
//! without a phone.

use std::io::Write;
use std::net::{TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc, OnceLock};
use std::thread;
use std::time::{Duration, Instant};

use lb_proto::{
    server_handshake, FrameReader, FrameWriter, MediaHeader, PacketType, PairingKey, CODEC_H264, CODEC_PCM_S16LE, FLAG_CONFIG,
    FLAG_DISCONT, FLAG_KEY, PROTO_VERSION,
};
use openh264::encoder::{BitRate, Encoder, EncoderConfig, FrameRate, FrameType, IntraFramePeriod, RateControlMode, UsageType};
use openh264::formats::YUVBuffer;
use openh264::OpenH264API;
use serde_json::{json, Value};

/// Server clock: deliberately far from the client clock to exercise clock sync.
pub fn server_now_us() -> u64 {
    static START: OnceLock<Instant> = OnceLock::new();
    10_000_000_000 + START.get_or_init(Instant::now).elapsed().as_micros() as u64
}

#[derive(Clone)]
pub struct SimOptions {
    pub device_name: String,
}

impl Default for SimOptions {
    fn default() -> Self {
        Self {
            device_name: "Lensbridge Simulator".into(),
        }
    }
}

/// Accepts connections until `stop` is set. One active session at a time.
pub fn serve(listener: TcpListener, key: PairingKey, opts: SimOptions, stop: Arc<AtomicBool>) {
    listener.set_nonblocking(true).expect("nonblocking listener");
    let active = Arc::new(AtomicBool::new(false));
    while !stop.load(Ordering::Relaxed) {
        match listener.accept() {
            Ok((stream, peer)) => {
                let _ = stream.set_nonblocking(false);
                let key = key.clone();
                let opts = opts.clone();
                let active = active.clone();
                let stop = stop.clone();
                thread::spawn(move || {
                    if let Err(e) = handle(stream, key, opts, active, stop) {
                        log::info!("session with {peer} ended: {e}");
                    }
                });
            }
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => thread::sleep(Duration::from_millis(20)),
            Err(e) => {
                log::warn!("accept: {e}");
                thread::sleep(Duration::from_millis(100));
            }
        }
    }
}

struct ActiveGuard(Arc<AtomicBool>);
impl Drop for ActiveGuard {
    fn drop(&mut self) {
        self.0.store(false, Ordering::SeqCst);
    }
}

#[derive(Clone, Debug)]
struct State {
    camera: String,
    w: usize,
    h: usize,
    fps: u32,
    bitrate: u32,
    zoom: f64,
    torch: bool,
    mirror: bool,
    audio: bool,
    screen: String,
    ev: f64,
}

impl State {
    fn to_json(&self) -> Value {
        json!({"camera": self.camera, "w": self.w, "h": self.h, "fps": self.fps, "bitrate": self.bitrate,
               "zoom": self.zoom, "torch": self.torch, "mirror": self.mirror, "audio": self.audio,
               "screen": self.screen, "ev": self.ev, "focus_mode": "continuous"})
    }
}

fn caps() -> Value {
    json!({"t": "caps", "cameras": [
        {"id": "sim-back", "name": "Задняя (симулятор)", "position": "back",
         "formats": [{"w": 640, "h": 360, "min_fps": 1, "max_fps": 60}, {"w": 1280, "h": 720, "min_fps": 1, "max_fps": 60}, {"w": 1920, "h": 1080, "min_fps": 1, "max_fps": 30}],
         "zoom_min": 1.0, "zoom_max": 10.0, "torch": true, "focus_poi": true, "exposure_poi": true, "ev_min": -8.0, "ev_max": 8.0},
        {"id": "sim-front", "name": "Фронтальная (симулятор)", "position": "front",
         "formats": [{"w": 640, "h": 480, "min_fps": 1, "max_fps": 30}, {"w": 1280, "h": 720, "min_fps": 1, "max_fps": 30}],
         "zoom_min": 1.0, "zoom_max": 3.0, "torch": false, "focus_poi": false, "exposure_poi": true, "ev_min": -8.0, "ev_max": 8.0}],
        "audio": {"rate": 48000, "channels": 1}})
}

fn make_encoder(st: &State) -> Encoder {
    let cfg = EncoderConfig::new()
        .usage_type(UsageType::CameraVideoRealTime)
        .rate_control_mode(RateControlMode::Bitrate)
        .bitrate(BitRate::from_bps(st.bitrate))
        .max_frame_rate(FrameRate::from_hz(st.fps as f32))
        .intra_frame_period(IntraFramePeriod::from_num_frames(st.fps * 2))
        .skip_frames(false);
    Encoder::with_api_config(OpenH264API::from_source(), cfg).expect("openh264 encoder")
}

fn test_pattern(st: &State, n: u64) -> YUVBuffer {
    let (w, h) = (st.w, st.h);
    let (cw, ch) = (w / 2, h / 2);
    let mut buf = vec![0u8; w * h + 2 * cw * ch];
    let bar = (n as usize * 8) % w;
    for y in 0..h {
        for x in 0..w {
            let v = if x.abs_diff(bar) < 16 { 235 } else { 16 + ((x * 200) / w) as u8 };
            buf[y * w + x] = v;
        }
    }
    let u = (128.0 + 60.0 * ((n as f64) / 30.0).sin()) as u8;
    let v = (128.0 + 60.0 * ((n as f64) / 45.0).cos()) as u8;
    buf[w * h..w * h + cw * ch].fill(u);
    buf[w * h + cw * ch..].fill(v);
    YUVBuffer::from_vec(buf, w, h)
}

fn handle(
    mut stream: TcpStream,
    key: PairingKey,
    opts: SimOptions,
    active: Arc<AtomicBool>,
    stop: Arc<AtomicBool>,
) -> lb_proto::Result<()> {
    stream.set_nodelay(true)?;
    stream.set_read_timeout(Some(Duration::from_secs(5)))?;
    let busy = active.swap(true, Ordering::SeqCst);
    let keys = server_handshake(&mut stream, |id| (id == &key.key_id()).then(|| key.clone()), busy);
    if busy {
        // Do not clear the flag: it belongs to the other session.
        return keys.map(|_| ());
    }
    let _guard = ActiveGuard(active);
    let keys = keys?;

    let mut reader = FrameReader::new(stream.try_clone()?, &keys.c2s);
    let mut writer = FrameWriter::new(stream.try_clone()?, &keys.s2c);
    let (tx, rx) = mpsc::channel::<Value>();
    let closed = Arc::new(AtomicBool::new(false));
    {
        let closed = closed.clone();
        thread::spawn(move || {
            while let Ok(p) = reader.read_packet() {
                if p.ty == PacketType::Control {
                    if let Ok(v) = p.json() {
                        if tx.send(v).is_err() {
                            break;
                        }
                    }
                }
            }
            closed.store(true, Ordering::SeqCst);
        });
    }

    // The first message must be hello (proves the client owns the key).
    let hello = rx
        .recv_timeout(Duration::from_secs(5))
        .map_err(|_| lb_proto::Error::Malformed("no hello"))?;
    if hello["t"] != "hello" {
        return Err(lb_proto::Error::Malformed("first message is not hello"));
    }

    let mut st = State {
        camera: "sim-back".into(),
        w: 640,
        h: 360,
        fps: 30,
        bitrate: 2_000_000,
        zoom: 1.0,
        torch: false,
        mirror: false,
        audio: true,
        screen: "on".into(),
        ev: 0.0,
    };
    writer.write_json(
        &json!({"t": "welcome", "proto": PROTO_VERSION, "server": "lensbridge-sim", "version": env!("CARGO_PKG_VERSION"),
        "device": opts.device_name, "server_time_us": server_now_us(), "state": st.to_json()}),
    )?;
    send_stream_config(&mut writer, &st)?;

    let mut encoder = make_encoder(&st);
    let mut force_key = true;
    let mut discont = true;
    let (mut vseq, mut aseq, mut n) = (0u32, 0u32, 0u64);
    let mut next_video = Instant::now();
    let mut next_audio = Instant::now();
    let mut next_stats = Instant::now() + Duration::from_secs(1);
    let mut phase = 0f64;
    let mut sent_frames = 0u32;

    while !closed.load(Ordering::SeqCst) && !stop.load(Ordering::Relaxed) {
        while let Ok(msg) = rx.try_recv() {
            match msg["t"].as_str().unwrap_or("") {
                "ping" => writer.write_json(&json!({"t": "pong", "id": msg["id"], "c0": msg["c0"], "s1": server_now_us()}))?,
                "get_caps" => {
                    let mut c = caps();
                    c["id"] = msg["id"].clone();
                    writer.write_json(&c)?;
                }
                "request_keyframe" => force_key = true,
                "set" => {
                    let before = (st.w, st.h, st.fps, st.bitrate, st.camera.clone());
                    apply_set(&mut st, &msg["p"]);
                    if before != (st.w, st.h, st.fps, st.bitrate, st.camera.clone()) {
                        encoder = make_encoder(&st);
                        force_key = true;
                        discont = true;
                        send_stream_config(&mut writer, &st)?;
                    }
                    writer.write_json(&json!({"t": "ack", "id": msg["id"], "applied": st.to_json()}))?;
                }
                "bye" => return Ok(()),
                _ => writer.write_json(&json!({"t": "error", "id": msg["id"], "code": "bad_request", "message": "unknown type"}))?,
            }
        }

        let now = Instant::now();
        if now >= next_video {
            next_video += Duration::from_secs_f64(1.0 / st.fps as f64);
            if now > next_video + Duration::from_millis(200) {
                next_video = now; // fell behind: do not burst
            }
            let pts = server_now_us();
            if force_key {
                encoder.force_intra_frame();
            }
            let yuv = test_pattern(&st, n);
            n += 1;
            let bs = encoder
                .encode(&yuv)
                .map_err(|e| lb_proto::Error::Io(std::io::Error::other(e.to_string())))?;
            let ft = bs.frame_type();
            if !matches!(ft, FrameType::Skip | FrameType::Invalid) {
                let data = bs.to_vec();
                let key = matches!(ft, FrameType::IDR);
                let mut flags = 0;
                if key {
                    flags |= FLAG_KEY | FLAG_CONFIG;
                    force_key = false;
                }
                if discont {
                    flags |= FLAG_DISCONT;
                    discont = false;
                }
                let hdr = MediaHeader {
                    codec: CODEC_H264,
                    flags,
                    seq: vseq,
                    pts_us: pts,
                };
                vseq = vseq.wrapping_add(1);
                writer.write_packet(PacketType::Video, &hdr.encode_with(&data))?;
                sent_frames += 1;
            }
        }
        if st.audio && now >= next_audio {
            next_audio += Duration::from_millis(20);
            if now > next_audio + Duration::from_millis(200) {
                next_audio = now;
            }
            let mut pcm = Vec::with_capacity(960 * 2);
            for _ in 0..960 {
                let s = (phase.sin() * 6000.0) as i16;
                phase += 2.0 * std::f64::consts::PI * 440.0 / 48_000.0;
                pcm.extend_from_slice(&s.to_le_bytes());
            }
            let hdr = MediaHeader {
                codec: CODEC_PCM_S16LE,
                flags: 0,
                seq: aseq,
                pts_us: server_now_us(),
            };
            aseq = aseq.wrapping_add(1);
            writer.write_packet(PacketType::Audio, &hdr.encode_with(&pcm))?;
        }
        if now >= next_stats {
            next_stats += Duration::from_secs(1);
            writer.write_json(&json!({"t": "stats", "fps": sent_frames, "kbps": st.bitrate / 1000, "dropped": 0,
                "battery": 1.0, "charging": true, "thermal": "nominal"}))?;
            sent_frames = 0;
        }
        let wake = next_video.min(if st.audio { next_audio } else { next_video }).min(next_stats);
        let sleep = wake.saturating_duration_since(Instant::now()).min(Duration::from_millis(5));
        thread::sleep(sleep);
    }
    let _ = stream.flush();
    Ok(())
}

fn send_stream_config(writer: &mut FrameWriter<TcpStream>, st: &State) -> lb_proto::Result<()> {
    writer.write_json(&json!({"t": "stream_config",
        "video": {"codec": "h264", "w": st.w, "h": st.h, "fps": st.fps},
        "audio": {"codec": "pcm_s16le", "rate": 48000, "channels": 1}}))
}

fn apply_set(st: &mut State, p: &Value) {
    let formats = caps();
    if let Some(cam) = p["camera"].as_str() {
        if formats["cameras"].as_array().unwrap().iter().any(|c| c["id"] == cam) {
            st.camera = cam.into();
        }
    }
    if let (Some(w), Some(h)) = (p["w"].as_u64(), p["h"].as_u64()) {
        // Snap to a supported format of the current camera (even dimensions).
        let cams = formats["cameras"].as_array().unwrap();
        let cam = cams.iter().find(|c| c["id"] == st.camera.as_str()).unwrap();
        let best = cam["formats"]
            .as_array()
            .unwrap()
            .iter()
            .min_by_key(|f| f["w"].as_u64().unwrap().abs_diff(w) + f["h"].as_u64().unwrap().abs_diff(h))
            .unwrap();
        st.w = best["w"].as_u64().unwrap() as usize;
        st.h = best["h"].as_u64().unwrap() as usize;
    }
    if let Some(fps) = p["fps"].as_u64() {
        st.fps = fps.clamp(1, 60) as u32;
    }
    if let Some(b) = p["bitrate"].as_u64() {
        st.bitrate = b.clamp(200_000, 50_000_000) as u32;
    }
    if let Some(z) = p["zoom"].as_f64() {
        st.zoom = z.clamp(1.0, 10.0);
    }
    if let Some(t) = p["torch"].as_bool() {
        st.torch = t;
    }
    if let Some(m) = p["mirror"].as_bool() {
        st.mirror = m;
    }
    if let Some(a) = p["audio"].as_bool() {
        st.audio = a;
    }
    if let Some(s) = p["screen"].as_str() {
        st.screen = s.into();
    }
    if let Some(ev) = p["ev"].as_f64() {
        st.ev = ev.clamp(-8.0, 8.0);
    }
}
