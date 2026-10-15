//! Client session: connect → handshake → stream, with automatic reconnection.
//!
//! Threads per connection:
//! - session thread: owns the writer, sends pings / commands, supervises;
//! - reader thread: decrypts, decodes H.264, publishes frames and audio.

use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use crossbeam_channel::{select, tick, unbounded, Receiver, Sender};
use lb_proto::{
    client_handshake, Error as ProtoError, FrameReader, FrameWriter, HandshakeStatus, MediaHeader, PacketType, PairingKey, CODEC_H264,
    CODEC_PCM_S16LE, PROTO_VERSION,
};
use serde_json::{json, Map, Value};

use crate::clock::ClockSync;
use crate::decode::{H264Decoder, VideoFrame};
use crate::frame_slot::LatestFrame;
use crate::now_us;
use crate::transport::{self, Duplex, Target};

const CONNECT_TIMEOUT: Duration = Duration::from_secs(3);
const READ_TIMEOUT: Duration = Duration::from_secs(5);
const BACKOFF_MIN: Duration = Duration::from_millis(500);
const BACKOFF_MAX: Duration = Duration::from_secs(5);
const STABLE_SESSION: Duration = Duration::from_secs(10);

pub type AudioSink = Arc<dyn Fn(AudioChunk) + Send + Sync>;

#[derive(Clone)]
pub struct ClientConfig {
    pub target: Target,
    pub key: PairingKey,
    pub client_name: String,
    /// Called from the reader thread for every audio chunk (keep it fast).
    pub audio_sink: Option<AudioSink>,
    /// Frame slot to publish into; a fresh one is created when `None`. Passing
    /// a long-lived slot lets consumers (virtual camera) survive reconnects.
    pub frames: Option<Arc<LatestFrame>>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum ConnState {
    Connecting {
        attempt: u32,
    },
    Connected {
        device: String,
        server: String,
    },
    Disconnected {
        reason: String,
        retry_in_ms: u64,
    },
    /// The phone does not know our key: scan the QR code on the phone.
    NotPaired,
    /// Another computer is streaming from this phone.
    Busy,
    Stopped,
}

#[derive(Debug, Clone)]
pub struct AudioChunk {
    pub pts_us: u64,
    pub rate: u32,
    pub channels: u16,
    pub samples: Vec<i16>,
    pub discont: bool,
}

#[derive(Debug, Clone, Default)]
pub struct ClientStats {
    pub fps: f32,
    pub kbps: f32,
    /// Capture → decoded on the PC (needs clock sync).
    pub latency_ms: Option<f32>,
    pub rtt_ms: Option<f32>,
    pub width: usize,
    pub height: usize,
    pub frames_total: u64,
    pub frames_dropped: u64,
    pub decode_errors: u64,
    pub audio_chunks: u64,
    /// Last `stats` message from the phone.
    pub phone: Option<Value>,
}

#[derive(Debug, Clone)]
pub enum Event {
    State(ConnState),
    /// Any control message from the phone (`welcome`, `caps`, `ack`, `event`, `error`, ...).
    Control(Value),
    Stats(ClientStats),
}

enum Command {
    Send(Value),
    Set(Map<String, Value>),
    Stop,
}

enum Internal {
    ReaderDone(String),
    NeedKeyframe,
}

pub struct ClientHandle {
    cmd_tx: Sender<Command>,
    pub events: Receiver<Event>,
    pub frames: Arc<LatestFrame>,
    next_id: AtomicU32,
    thread: Option<JoinHandle<()>>,
}

impl ClientHandle {
    fn id(&self) -> u32 {
        self.next_id.fetch_add(1, Ordering::Relaxed)
    }

    /// `set` with the given parameters; they are also remembered and
    /// re-applied after every reconnect.
    pub fn set(&self, params: Value) {
        if let Value::Object(map) = params {
            let _ = self.cmd_tx.send(Command::Set(map));
        }
    }

    pub fn get_caps(&self) {
        let id = self.id();
        let _ = self.cmd_tx.send(Command::Send(json!({"t": "get_caps", "id": id})));
    }

    pub fn request_keyframe(&self) {
        let _ = self.cmd_tx.send(Command::Send(json!({"t": "request_keyframe"})));
    }

    pub fn send_raw(&self, v: Value) {
        let _ = self.cmd_tx.send(Command::Send(v));
    }

    pub fn stop(mut self) {
        self.shutdown();
    }

    fn shutdown(&mut self) {
        let _ = self.cmd_tx.send(Command::Stop);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

impl Drop for ClientHandle {
    fn drop(&mut self) {
        self.shutdown();
    }
}

/// Starts the client in the background.
pub fn spawn(cfg: ClientConfig) -> ClientHandle {
    let (cmd_tx, cmd_rx) = unbounded();
    let (ev_tx, ev_rx) = unbounded();
    let frames = cfg.frames.clone().unwrap_or_default();
    let frames2 = frames.clone();
    let thread = thread::Builder::new()
        .name("lb-session".into())
        .spawn(move || supervisor(cfg, cmd_rx, ev_tx, frames2))
        .expect("spawn session thread");
    ClientHandle {
        cmd_tx,
        events: ev_rx,
        frames,
        next_id: AtomicU32::new(1),
        thread: Some(thread),
    }
}

enum SessionEnd {
    Stopped,
    Failed(ProtoError),
}

fn supervisor(cfg: ClientConfig, cmd_rx: Receiver<Command>, ev_tx: Sender<Event>, frames: Arc<LatestFrame>) {
    let mut desired = Map::new();
    let mut backoff = BACKOFF_MIN;
    let mut attempt = 0u32;
    loop {
        attempt += 1;
        let _ = ev_tx.send(Event::State(ConnState::Connecting { attempt }));
        let started = Instant::now();
        let end = run_session(&cfg, &cmd_rx, &ev_tx, &frames, &mut desired);
        let (state, wait) = match end {
            SessionEnd::Stopped => break,
            SessionEnd::Failed(ProtoError::Rejected(HandshakeStatus::UnknownKey)) => (ConnState::NotPaired, Duration::from_secs(3)),
            SessionEnd::Failed(ProtoError::Rejected(HandshakeStatus::Busy)) => (ConnState::Busy, Duration::from_secs(2)),
            SessionEnd::Failed(e) => {
                if started.elapsed() > STABLE_SESSION {
                    backoff = BACKOFF_MIN;
                    attempt = 0;
                }
                let wait = backoff;
                backoff = (backoff * 2).min(BACKOFF_MAX);
                (
                    ConnState::Disconnected {
                        reason: describe(&e),
                        retry_in_ms: wait.as_millis() as u64,
                    },
                    wait,
                )
            }
        };
        let _ = ev_tx.send(Event::State(state));
        // Wait, but stay responsive to Stop and remember settings meanwhile.
        let deadline = Instant::now() + wait;
        loop {
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                break;
            }
            match cmd_rx.recv_timeout(left) {
                Ok(Command::Stop) | Err(crossbeam_channel::RecvTimeoutError::Disconnected) => {
                    let _ = ev_tx.send(Event::State(ConnState::Stopped));
                    return;
                }
                Ok(Command::Set(p)) => desired.extend(p),
                Ok(Command::Send(_)) => {}
                Err(crossbeam_channel::RecvTimeoutError::Timeout) => break,
            }
        }
    }
    let _ = ev_tx.send(Event::State(ConnState::Stopped));
}

fn describe(e: &ProtoError) -> String {
    match e {
        ProtoError::Io(io) if io.kind() == std::io::ErrorKind::ConnectionRefused => {
            "соединение отклонено (приложение на телефоне не запущено?)".into()
        }
        ProtoError::Io(io) if matches!(io.kind(), std::io::ErrorKind::TimedOut | std::io::ErrorKind::WouldBlock) => {
            "таймаут (телефон не отвечает)".into()
        }
        ProtoError::Io(io) if io.kind() == std::io::ErrorKind::UnexpectedEof => "соединение закрыто телефоном".into(),
        ProtoError::Decrypt => "ошибка расшифровки (неверный ключ?)".into(),
        other => other.to_string(),
    }
}

struct Shared {
    clock: Mutex<ClockSync>,
    stats: Mutex<StatsAccum>,
}

#[derive(Default)]
struct StatsAccum {
    frames: u32,
    bytes: u64,
    latency_sum_ms: f64,
    latency_n: u32,
    totals: ClientStats,
}

fn run_session(
    cfg: &ClientConfig,
    cmd_rx: &Receiver<Command>,
    ev_tx: &Sender<Event>,
    frames: &Arc<LatestFrame>,
    desired: &mut Map<String, Value>,
) -> SessionEnd {
    match run_session_inner(cfg, cmd_rx, ev_tx, frames, desired) {
        Ok(()) => SessionEnd::Stopped,
        Err(e) => SessionEnd::Failed(e),
    }
}

fn run_session_inner(
    cfg: &ClientConfig,
    cmd_rx: &Receiver<Command>,
    ev_tx: &Sender<Event>,
    frames: &Arc<LatestFrame>,
    desired: &mut Map<String, Value>,
) -> Result<(), ProtoError> {
    let mut stream = transport::connect(&cfg.target, CONNECT_TIMEOUT)?;
    stream.set_read_timeout_duplex(Some(READ_TIMEOUT))?;
    let keys = client_handshake(&mut stream, &cfg.key)?;

    let control = stream.try_clone_duplex()?;
    let reader_stream = stream.try_clone_duplex()?;
    let mut writer = FrameWriter::new(stream, &keys.c2s);
    let reader = FrameReader::new(reader_stream, &keys.s2c);

    let shared = Arc::new(Shared {
        clock: Mutex::new(ClockSync::new()),
        stats: Mutex::new(StatsAccum::default()),
    });
    let (int_tx, int_rx) = unbounded();
    let reader_thread = {
        let shared = shared.clone();
        let ev_tx = ev_tx.clone();
        let frames = frames.clone();
        let sink = cfg.audio_sink.clone();
        thread::Builder::new()
            .name("lb-reader".into())
            .spawn(move || {
                let reason = match reader_loop(reader, &shared, &ev_tx, &frames, sink, &int_tx) {
                    Ok(()) => "закрыто".to_string(),
                    Err(e) => describe(&e),
                };
                let _ = int_tx.send(Internal::ReaderDone(reason));
            })
            .expect("spawn reader")
    };

    let result = (|| -> Result<(), ProtoError> {
        writer.write_json(&json!({
            "t": "hello", "proto": PROTO_VERSION, "client": "lensbridge-desktop",
            "version": env!("CARGO_PKG_VERSION"), "name": cfg.client_name,
        }))?;
        writer.write_json(&json!({"t": "get_caps", "id": 0}))?;
        if !desired.is_empty() {
            writer.write_json(&json!({"t": "set", "id": 0, "p": Value::Object(desired.clone())}))?;
        }
        writer.write_json(&json!({"t": "request_keyframe"}))?;

        let ticker = tick(Duration::from_secs(1));
        let mut ping_id = 1_000_000u32;
        let mut last_kf_request = Instant::now();
        send_ping(&mut writer, &mut ping_id)?;
        loop {
            select! {
                recv(cmd_rx) -> cmd => match cmd {
                    Ok(Command::Send(v)) => writer.write_json(&v)?,
                    Ok(Command::Set(p)) => {
                        desired.extend(p.clone());
                        writer.write_json(&json!({"t": "set", "id": 0, "p": Value::Object(p)}))?;
                    }
                    Ok(Command::Stop) | Err(_) => {
                        let _ = writer.write_json(&json!({"t": "bye"}));
                        return Ok(());
                    }
                },
                recv(int_rx) -> msg => match msg {
                    Ok(Internal::ReaderDone(reason)) => return Err(ProtoError::Io(std::io::Error::other(reason))),
                    Err(_) => return Err(ProtoError::Io(std::io::Error::other("reader stopped"))),
                    Ok(Internal::NeedKeyframe) => {
                        if last_kf_request.elapsed() > Duration::from_millis(500) {
                            last_kf_request = Instant::now();
                            writer.write_json(&json!({"t": "request_keyframe"}))?;
                        }
                    }
                },
                recv(ticker) -> _ => {
                    send_ping(&mut writer, &mut ping_id)?;
                    let _ = ev_tx.send(Event::Stats(snapshot_stats(&shared)));
                }
            }
        }
    })();

    control.shutdown_duplex();
    let _ = reader_thread.join();
    result
}

fn send_ping(writer: &mut FrameWriter<Box<dyn Duplex>>, id: &mut u32) -> Result<(), ProtoError> {
    *id = id.wrapping_add(1);
    writer.write_json(&json!({"t": "ping", "id": *id, "c0": now_us()}))
}

fn snapshot_stats(shared: &Shared) -> ClientStats {
    let mut acc = shared.stats.lock().unwrap();
    let clock = shared.clock.lock().unwrap();
    let mut s = acc.totals.clone();
    s.fps = acc.frames as f32;
    s.kbps = (acc.bytes * 8) as f32 / 1000.0;
    s.latency_ms = (acc.latency_n > 0).then(|| (acc.latency_sum_ms / acc.latency_n as f64) as f32);
    s.rtt_ms = clock.last_rtt_us().map(|r| r as f32 / 1000.0);
    acc.frames = 0;
    acc.bytes = 0;
    acc.latency_sum_ms = 0.0;
    acc.latency_n = 0;
    s
}

fn reader_loop(
    mut reader: FrameReader<Box<dyn Duplex>>,
    shared: &Shared,
    ev_tx: &Sender<Event>,
    frames: &LatestFrame,
    audio_sink: Option<AudioSink>,
    int_tx: &Sender<Internal>,
) -> Result<(), ProtoError> {
    let mut decoder = H264Decoder::new().map_err(|e| ProtoError::Io(std::io::Error::other(e)))?;
    let mut waiting_key = true;
    let mut last_video_seq: Option<u32> = None;
    let mut audio_cfg = (48_000u32, 1u16);
    loop {
        let packet = reader.read_packet()?;
        let recv_us = now_us();
        shared.stats.lock().unwrap().bytes += packet.payload.len() as u64;
        match packet.ty {
            PacketType::Control => {
                let Ok(v) = packet.json() else { continue };
                match v["t"].as_str() {
                    Some("pong") => {
                        if let (Some(c0), Some(s1)) = (v["c0"].as_u64(), v["s1"].as_u64()) {
                            shared.clock.lock().unwrap().add(c0, s1, recv_us);
                        }
                        continue;
                    }
                    Some("welcome") => {
                        let device = v["device"].as_str().unwrap_or("телефон").to_string();
                        let server = v["server"].as_str().unwrap_or("?").to_string();
                        let _ = ev_tx.send(Event::State(ConnState::Connected { device, server }));
                    }
                    Some("stats") => shared.stats.lock().unwrap().totals.phone = Some(v.clone()),
                    Some("stream_config") => {
                        let a = &v["audio"];
                        if let (Some(rate), Some(ch)) = (a["rate"].as_u64(), a["channels"].as_u64()) {
                            audio_cfg = (rate as u32, ch as u16);
                        }
                    }
                    _ => {}
                }
                let _ = ev_tx.send(Event::Control(v));
            }
            PacketType::Video => {
                let (hdr, au) = MediaHeader::parse(&packet.payload)?;
                if hdr.codec != CODEC_H264 {
                    continue;
                }
                if let Some(prev) = last_video_seq {
                    let gap = hdr.seq.wrapping_sub(prev).wrapping_sub(1);
                    if gap > 0 && gap < 1_000_000 {
                        shared.stats.lock().unwrap().totals.frames_dropped += gap as u64;
                    }
                }
                last_video_seq = Some(hdr.seq);
                if hdr.is_discont() && !hdr.is_key() {
                    waiting_key = true;
                }
                if waiting_key && !hdr.is_key() {
                    let _ = int_tx.send(Internal::NeedKeyframe);
                    continue;
                }
                match decoder.decode(au) {
                    Ok(Some((width, height, i420))) => {
                        waiting_key = false;
                        let latency = shared
                            .clock
                            .lock()
                            .unwrap()
                            .to_client(hdr.pts_us)
                            .map(|cap| recv_us.saturating_sub(cap));
                        {
                            let mut st = shared.stats.lock().unwrap();
                            st.frames += 1;
                            st.totals.frames_total += 1;
                            st.totals.width = width;
                            st.totals.height = height;
                            if let Some(l) = latency {
                                st.latency_sum_ms += l as f64 / 1000.0;
                                st.latency_n += 1;
                            }
                        }
                        frames.publish(VideoFrame {
                            width,
                            height,
                            i420,
                            pts_us: hdr.pts_us,
                            seq: hdr.seq,
                            recv_us,
                        });
                    }
                    Ok(None) => {}
                    Err(e) => {
                        log::debug!("decode error: {e}");
                        shared.stats.lock().unwrap().totals.decode_errors += 1;
                        // Fresh decoder: corrupted reference state otherwise lingers.
                        decoder = H264Decoder::new().map_err(|e| ProtoError::Io(std::io::Error::other(e)))?;
                        waiting_key = true;
                        let _ = int_tx.send(Internal::NeedKeyframe);
                    }
                }
            }
            PacketType::Audio => {
                let (hdr, data) = MediaHeader::parse(&packet.payload)?;
                if hdr.codec != CODEC_PCM_S16LE {
                    continue;
                }
                shared.stats.lock().unwrap().totals.audio_chunks += 1;
                if let Some(sink) = &audio_sink {
                    let samples = data.as_chunks::<2>().0.iter().map(|b| i16::from_le_bytes(*b)).collect();
                    sink(AudioChunk {
                        pts_us: hdr.pts_us,
                        rate: audio_cfg.0,
                        channels: audio_cfg.1,
                        samples,
                        discont: hdr.is_discont(),
                    });
                }
            }
            PacketType::Unknown(_) => {}
        }
    }
}
