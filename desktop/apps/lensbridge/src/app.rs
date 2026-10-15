//! Main window: devices + pairing (left), preview (center), camera controls
//! and outputs (right), status bar and diagnostics.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use eframe::egui::{self, Color32, ColorImage, RichText, Sense, TextureHandle, TextureOptions, Vec2};
use lb_client::audio_out::{find_virtual_cable, list_output_devices, AudioOutput};
use lb_client::session::AudioSink;
use lb_client::{now_us, ClientConfig, ClientHandle, ClientStats, ConnState, Event, LatestFrame, Target};
use lb_proto::{PairingKey, DEFAULT_PORT};
use serde_json::{json, Value};

use crate::config::Config;
use crate::discovery::Discovery;
use crate::{qr, vcam};

const FPS_CHOICES: [u32; 12] = [5, 10, 15, 20, 24, 25, 30, 48, 50, 60, 90, 120];
const STALE_US: u64 = 1_500_000;

#[derive(Debug, Clone, Default)]
struct CameraCaps {
    id: String,
    name: String,
    /// (w, h, min_fps, max_fps), largest first.
    formats: Vec<(u32, u32, u32, u32)>,
    zoom: (f32, f32),
    torch: bool,
    focus_poi: bool,
    exposure_poi: bool,
    ev: (f32, f32),
}

fn parse_caps(v: &Value) -> Vec<CameraCaps> {
    let mut out = Vec::new();
    for c in v["cameras"].as_array().into_iter().flatten() {
        let mut formats: Vec<_> = c["formats"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|f| {
                Some((
                    f["w"].as_u64()? as u32,
                    f["h"].as_u64()? as u32,
                    f["min_fps"].as_u64().unwrap_or(1) as u32,
                    f["max_fps"].as_u64()? as u32,
                ))
            })
            .collect();
        formats.sort_by_key(|f| std::cmp::Reverse((f.0 * f.1, f.3)));
        out.push(CameraCaps {
            id: c["id"].as_str().unwrap_or_default().to_string(),
            name: c["name"].as_str().unwrap_or("Камера").to_string(),
            formats,
            zoom: (
                c["zoom_min"].as_f64().unwrap_or(1.0) as f32,
                c["zoom_max"].as_f64().unwrap_or(1.0) as f32,
            ),
            torch: c["torch"].as_bool().unwrap_or(false),
            focus_poi: c["focus_poi"].as_bool().unwrap_or(false),
            exposure_poi: c["exposure_poi"].as_bool().unwrap_or(false),
            ev: (
                c["ev_min"].as_f64().unwrap_or(0.0) as f32,
                c["ev_max"].as_f64().unwrap_or(0.0) as f32,
            ),
        });
    }
    out
}

/// UI copies of continuous controls (so sliders stay smooth while dragging).
#[derive(Default)]
struct Sliders {
    bitrate_mbps: f32,
    zoom: f32,
    ev: f32,
}

pub struct LensbridgeApp {
    cfg: Config,
    key: PairingKey,
    discovery: Discovery,
    client: Option<ClientHandle>,
    target: Option<Target>,
    conn: Option<ConnState>,
    device_name: String,
    frames: Arc<LatestFrame>,
    preview: Option<TextureHandle>,
    preview_gen: u64,
    preview_recv_us: u64,
    preview_size: (usize, usize),
    cameras: Vec<CameraCaps>,
    state: Value,
    sliders: Sliders,
    stats: ClientStats,
    mirror: Arc<AtomicBool>,
    vcam: Option<vcam::VcamWriter>,
    vcam_error: Option<String>,
    audio_devices: Vec<String>,
    audio_out: Option<AudioOutput>,
    audio_error: Option<String>,
    audio_route: Arc<Mutex<Option<AudioSink>>>,
    show_pairing: bool,
    qr_texture: Option<TextureHandle>,
    confirm_new_key: bool,
    show_diag: bool,
    log: VecDeque<String>,
}

fn pc_name() -> String {
    std::env::var("COMPUTERNAME")
        .or_else(|_| std::env::var("HOSTNAME"))
        .unwrap_or_else(|_| "ПК".into())
}

impl LensbridgeApp {
    pub fn new(cc: &eframe::CreationContext<'_>) -> Self {
        let mut cfg = Config::load();
        let key = cfg.pairing_key();
        let mirror = Arc::new(AtomicBool::new(cfg.mirror));
        let mut app = Self {
            discovery: Discovery::start(cc.egui_ctx.clone()),
            key,
            client: None,
            target: None,
            conn: None,
            device_name: String::new(),
            frames: LatestFrame::new(),
            preview: None,
            preview_gen: 0,
            preview_recv_us: 0,
            preview_size: (0, 0),
            cameras: Vec::new(),
            state: json!({}),
            sliders: Sliders {
                bitrate_mbps: 6.0,
                zoom: 1.0,
                ev: 0.0,
            },
            stats: ClientStats::default(),
            mirror,
            vcam: None,
            vcam_error: None,
            audio_devices: list_output_devices(),
            audio_out: None,
            audio_error: None,
            audio_route: Arc::new(Mutex::new(None)),
            show_pairing: false,
            qr_texture: None,
            confirm_new_key: false,
            show_diag: false,
            log: VecDeque::new(),
            cfg,
        };
        if app.cfg.vcam_enabled {
            app.set_vcam(true);
        }
        let dev = app.cfg.audio_device.clone();
        app.set_audio_device(dev);
        if app.cfg.auto_connect {
            if let Some(t) = app.cfg.last_target.as_ref().map(Target::from) {
                app.connect(t);
            }
        }
        app
    }

    fn note(&mut self, msg: impl Into<String>) {
        let msg = msg.into();
        log::info!("{msg}");
        self.log.push_back(msg);
        while self.log.len() > 50 {
            self.log.pop_front();
        }
    }

    // ------------------------------------------------------------ actions

    fn connect(&mut self, target: Target) {
        self.disconnect();
        let route = self.audio_route.clone();
        let sink: AudioSink = Arc::new(move |chunk| {
            if let Some(s) = route.lock().unwrap().as_ref() {
                s(chunk);
            }
        });
        let handle = lb_client::spawn(ClientConfig {
            target: target.clone(),
            key: self.key.clone(),
            client_name: pc_name(),
            audio_sink: Some(sink),
            frames: Some(self.frames.clone()),
        });
        handle.set(json!({"mirror": self.cfg.mirror, "audio": self.cfg.audio_device.is_some()}));
        self.note(format!("Подключение: {}", target.label()));
        self.cfg.last_target = Some((&target).into());
        self.cfg.save();
        self.client = Some(handle);
        self.target = Some(target);
    }

    fn disconnect(&mut self) {
        if let Some(c) = self.client.take() {
            c.stop();
            self.note("Отключено");
        }
        self.target = None;
        self.conn = None;
        self.cameras.clear();
        self.state = json!({});
    }

    fn set(&mut self, p: Value) {
        if let (Some(c), Value::Object(map)) = (&self.client, &p) {
            c.set(p.clone());
            if let Value::Object(st) = &mut self.state {
                for (k, v) in map {
                    st.insert(k.clone(), v.clone());
                }
            }
        }
    }

    fn set_vcam(&mut self, enabled: bool) {
        self.vcam = None;
        self.vcam_error = None;
        if enabled {
            match vcam::VcamWriter::start(self.frames.clone(), self.mirror.clone()) {
                Ok(w) => self.vcam = Some(w),
                Err(e) => self.vcam_error = Some(e),
            }
        }
        self.cfg.vcam_enabled = enabled;
        self.cfg.save();
    }

    fn set_audio_device(&mut self, device: Option<String>) {
        *self.audio_route.lock().unwrap() = None;
        self.audio_out = None;
        self.audio_error = None;
        if let Some(name) = &device {
            match AudioOutput::open(Some(name)) {
                Ok(out) => {
                    *self.audio_route.lock().unwrap() = Some(out.sink());
                    self.audio_out = Some(out);
                }
                Err(e) => self.audio_error = Some(e),
            }
        }
        if let Some(c) = &self.client {
            c.set(json!({"audio": device.is_some()}));
        }
        self.cfg.audio_device = device;
        self.cfg.save();
    }

    fn new_key(&mut self) {
        self.key = PairingKey::generate();
        self.cfg.set_key(&self.key);
        self.qr_texture = None;
        self.note("Создан новый ключ пейринга: телефоны нужно сопрячь заново");
        if let Some(t) = self.target.clone() {
            self.connect(t);
        }
    }

    // ------------------------------------------------------------ events

    fn pump_events(&mut self) {
        let Some(client) = &self.client else { return };
        let events: Vec<Event> = client.events.try_iter().collect();
        for ev in events {
            match ev {
                Event::State(st) => {
                    match &st {
                        ConnState::Connected { device, .. } => {
                            self.device_name = device.clone();
                            self.note(format!("Подключено: {device}"));
                        }
                        ConnState::Disconnected { reason, .. } => self.note(format!("Связь потеряна: {reason}")),
                        ConnState::NotPaired => self.note("Телефон не сопряжён с этим ПК"),
                        ConnState::Busy => self.note("Телефон занят другим компьютером"),
                        _ => {}
                    }
                    self.conn = Some(st);
                }
                Event::Control(v) => self.on_control(v),
                Event::Stats(s) => self.stats = s,
            }
        }
    }

    fn on_control(&mut self, v: Value) {
        match v["t"].as_str().unwrap_or("") {
            "caps" => self.cameras = parse_caps(&v),
            "welcome" => self.apply_state(&v["state"]),
            "ack" => self.apply_state(&v["applied"]),
            "event" => {
                let kind = v["kind"].as_str().unwrap_or("?");
                let text = match kind {
                    "interrupted" => "Камера телефона прервана (звонок, другое приложение или приложение свёрнуто)".to_string(),
                    "resumed" => "Камера телефона снова работает".to_string(),
                    other => format!("Событие телефона: {other} {}", v["detail"].as_str().unwrap_or("")),
                };
                self.note(text);
            }
            "error" => {
                let m = format!(
                    "Ошибка телефона: {} {}",
                    v["code"].as_str().unwrap_or(""),
                    v["message"].as_str().unwrap_or("")
                );
                self.note(m);
            }
            _ => {}
        }
    }

    fn apply_state(&mut self, st: &Value) {
        if !st.is_object() {
            return;
        }
        self.state = st.clone();
        if let Some(b) = st["bitrate"].as_f64() {
            self.sliders.bitrate_mbps = (b / 1e6) as f32;
        }
        if let Some(z) = st["zoom"].as_f64() {
            self.sliders.zoom = z as f32;
        }
        if let Some(e) = st["ev"].as_f64() {
            self.sliders.ev = e as f32;
        }
    }

    fn update_preview(&mut self, ctx: &egui::Context) {
        if let Some((gen, f)) = self.frames.get_newer(self.preview_gen) {
            self.preview_gen = gen;
            self.preview_recv_us = f.recv_us;
            self.preview_size = (f.width, f.height);
            let rgba = f.to_rgba(self.mirror.load(Ordering::Relaxed));
            let img = ColorImage::from_rgba_unmultiplied([f.width, f.height], &rgba);
            match &mut self.preview {
                Some(t) => t.set(img, TextureOptions::LINEAR),
                None => self.preview = Some(ctx.load_texture("preview", img, TextureOptions::LINEAR)),
            }
        }
    }

    fn current_camera(&self) -> Option<&CameraCaps> {
        let id = self.state["camera"].as_str();
        self.cameras.iter().find(|c| Some(c.id.as_str()) == id).or(self.cameras.first())
    }

    // ------------------------------------------------------------ UI

    fn ui_left(&mut self, ui: &mut egui::Ui) {
        ui.heading("Телефоны");
        ui.add_space(4.0);
        let devices = self.discovery.devices();
        if devices.is_empty() {
            ui.label(RichText::new("Не найдено. Запустите Lensbridge на телефоне (та же Wi-Fi сеть) или подключите по USB.").weak());
        }
        let mut connect_to = None;
        for d in &devices {
            let selected = self.target.as_ref() == Some(&d.target);
            let r = ui.selectable_label(selected, format!("{}\n{}", d.name, d.detail));
            if r.clicked() && !selected {
                connect_to = Some(d.target.clone());
            }
        }
        if let Some(t) = connect_to {
            self.connect(t);
        }

        ui.separator();
        ui.label("Вручную (IP или IP:порт):");
        let resp = ui.text_edit_singleline(&mut self.cfg.manual_host);
        let enter = resp.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
        if ui.button("Подключить").clicked() || enter {
            let s = self.cfg.manual_host.trim().to_string();
            if !s.is_empty() {
                let (host, port) = match s.rsplit_once(':') {
                    Some((h, p)) if !h.contains(':') || h.ends_with(']') => (h.to_string(), p.parse().unwrap_or(DEFAULT_PORT)),
                    _ => (s.clone(), DEFAULT_PORT),
                };
                self.connect(Target::Tcp { host, port });
            }
        }

        if self.client.is_some() {
            ui.add_space(6.0);
            if ui.button("Отключиться").clicked() {
                self.disconnect();
                self.cfg.last_target = None;
                self.cfg.save();
            }
        }

        ui.separator();
        ui.heading("Пейринг");
        ui.label(RichText::new(format!("Ключ этого ПК: {}", self.key.key_id_hex())).small().weak());
        if ui.button("Показать QR для телефона").clicked() {
            self.show_pairing = true;
        }
        if ui.checkbox(&mut self.cfg.auto_connect, "Подключаться автоматически").changed() {
            self.cfg.save();
        }

        let problems = self.discovery.problems();
        if !problems.is_empty() {
            ui.separator();
            egui::CollapsingHeader::new("Поиск: замечания").default_open(false).show(ui, |ui| {
                for p in problems {
                    ui.label(RichText::new(p).small().weak());
                }
            });
        }
    }

    fn ui_preview(&mut self, ui: &mut egui::Ui) {
        let avail = ui.available_size();
        let stale = now_us().saturating_sub(self.preview_recv_us) > STALE_US;
        if let (Some(tex), (w, h)) = (&self.preview, self.preview_size) {
            let aspect = w as f32 / h.max(1) as f32;
            let mut size = Vec2::new(avail.x, avail.x / aspect);
            if size.y > avail.y {
                size = Vec2::new(avail.y * aspect, avail.y);
            }
            let resp = ui
                .centered_and_justified(|ui| ui.add(egui::Image::new(tex).fit_to_exact_size(size).sense(Sense::click())))
                .inner;
            if stale {
                ui.painter().rect_filled(resp.rect, 0.0, Color32::from_black_alpha(150));
                ui.painter().text(
                    resp.rect.center(),
                    egui::Align2::CENTER_CENTER,
                    "нет сигнала",
                    egui::FontId::proportional(28.0),
                    Color32::WHITE,
                );
            }
            if let Some(pos) = resp.interact_pointer_pos().filter(|_| resp.clicked()) {
                let mut x = ((pos.x - resp.rect.left()) / resp.rect.width()).clamp(0.0, 1.0);
                let y = ((pos.y - resp.rect.top()) / resp.rect.height()).clamp(0.0, 1.0);
                if self.mirror.load(Ordering::Relaxed) {
                    x = 1.0 - x;
                }
                let cam = self.current_camera().cloned().unwrap_or_default();
                let mut p = serde_json::Map::new();
                if cam.focus_poi {
                    p.insert("focus".into(), json!({"x": x, "y": y}));
                    p.insert("focus_mode".into(), json!("auto"));
                }
                if cam.exposure_poi {
                    p.insert("exposure".into(), json!({"x": x, "y": y}));
                }
                if !p.is_empty() {
                    self.set(Value::Object(p));
                }
            }
        } else {
            ui.centered_and_justified(|ui| {
                ui.label(RichText::new(self.connection_hint()).size(18.0).weak());
            });
        }
    }

    fn connection_hint(&self) -> String {
        match &self.conn {
            None => "Выберите телефон слева или введите IP".into(),
            Some(ConnState::Connecting { attempt }) => format!("Подключение… (попытка {attempt})"),
            Some(ConnState::NotPaired) => {
                "Телефон не знает этот ПК.\nНажмите «Показать QR для телефона» и отсканируйте его в приложении Lensbridge.".into()
            }
            Some(ConnState::Busy) => "Телефон уже передаёт видео на другой компьютер".into(),
            Some(ConnState::Disconnected { reason, retry_in_ms }) => {
                format!("Нет связи: {reason}\nПовтор через {:.1} с", *retry_in_ms as f32 / 1000.0)
            }
            Some(ConnState::Connected { .. }) => "Ожидание первого кадра…".into(),
            Some(ConnState::Stopped) => "Остановлено".into(),
        }
    }

    fn ui_right(&mut self, ui: &mut egui::Ui) {
        egui::ScrollArea::vertical().show(ui, |ui| {
            ui.heading("Камера");
            let connected = matches!(self.conn, Some(ConnState::Connected { .. }));
            ui.add_enabled_ui(connected && !self.cameras.is_empty(), |ui| self.ui_camera_controls(ui));
            ui.separator();
            self.ui_outputs(ui);
        });
    }

    fn ui_camera_controls(&mut self, ui: &mut egui::Ui) {
        let cam = self.current_camera().cloned().unwrap_or_default();

        // Lens / camera
        let mut cam_id = cam.id.clone();
        egui::ComboBox::from_label("Объектив")
            .selected_text(cam.name.clone())
            .width(200.0)
            .show_ui(ui, |ui| {
                for c in &self.cameras {
                    ui.selectable_value(&mut cam_id, c.id.clone(), &c.name);
                }
            });
        if cam_id != cam.id {
            self.set(json!({"camera": cam_id}));
        }

        // Resolution
        let (w, h) = (
            self.state["w"].as_u64().unwrap_or(0) as u32,
            self.state["h"].as_u64().unwrap_or(0) as u32,
        );
        let mut sel = (w, h);
        egui::ComboBox::from_label("Разрешение")
            .selected_text(format!("{w}×{h}"))
            .width(200.0)
            .show_ui(ui, |ui| {
                for f in &cam.formats {
                    ui.selectable_value(&mut sel, (f.0, f.1), format!("{}×{}  (до {} к/с)", f.0, f.1, f.3));
                }
            });
        if sel != (w, h) {
            self.set(json!({"w": sel.0, "h": sel.1}));
        }

        // FPS limited by the selected format
        let fmt = cam.formats.iter().find(|f| (f.0, f.1) == (w, h)).copied();
        let (min_fps, max_fps) = fmt.map(|f| (f.2, f.3)).unwrap_or((1, 30));
        let fps = self.state["fps"].as_u64().unwrap_or(30) as u32;
        let mut new_fps = fps;
        egui::ComboBox::from_label("Кадров/с")
            .selected_text(fps.to_string())
            .width(200.0)
            .show_ui(ui, |ui| {
                for f in FPS_CHOICES.iter().copied().filter(|f| *f >= min_fps && *f <= max_fps) {
                    ui.selectable_value(&mut new_fps, f, f.to_string());
                }
            });
        if new_fps != fps {
            self.set(json!({"fps": new_fps}));
        }

        // Bitrate
        let r = ui.add(
            egui::Slider::new(&mut self.sliders.bitrate_mbps, 0.5..=30.0)
                .text("Мбит/с")
                .fixed_decimals(1),
        );
        if r.drag_stopped() || (r.changed() && !r.dragged()) {
            let b = (self.sliders.bitrate_mbps * 1e6) as u64;
            self.set(json!({"bitrate": b}));
        }

        // Zoom
        if cam.zoom.1 > cam.zoom.0 {
            let r = ui.add(
                egui::Slider::new(&mut self.sliders.zoom, cam.zoom.0..=cam.zoom.1)
                    .text("Зум")
                    .logarithmic(true)
                    .fixed_decimals(1),
            );
            if r.changed() {
                let z = self.sliders.zoom;
                self.set(json!({"zoom": z}));
            }
        }

        // Exposure compensation
        if cam.ev.1 > cam.ev.0 {
            let r = ui.add(
                egui::Slider::new(&mut self.sliders.ev, cam.ev.0..=cam.ev.1)
                    .text("Экспозиция (EV)")
                    .fixed_decimals(1),
            );
            if r.drag_stopped() || (r.changed() && !r.dragged()) {
                let e = self.sliders.ev;
                self.set(json!({"ev": e}));
            }
        }

        // Focus mode
        let fm = self.state["focus_mode"].as_str().unwrap_or("continuous").to_string();
        let mut new_fm = fm.clone();
        egui::ComboBox::from_label("Фокус")
            .selected_text(match fm.as_str() {
                "locked" => "зафиксирован",
                "auto" => "по точке",
                _ => "непрерывный",
            })
            .width(200.0)
            .show_ui(ui, |ui| {
                ui.selectable_value(&mut new_fm, "continuous".to_string(), "непрерывный");
                ui.selectable_value(&mut new_fm, "locked".to_string(), "зафиксирован");
            });
        if new_fm != fm {
            self.set(json!({"focus_mode": new_fm}));
        }
        if cam.focus_poi || cam.exposure_poi {
            ui.label(RichText::new("Клик по превью — фокус и экспозиция по точке").small().weak());
        }

        let mut torch = self.state["torch"].as_bool().unwrap_or(false);
        if cam.torch && ui.checkbox(&mut torch, "Фонарик").changed() {
            self.set(json!({"torch": torch}));
        }

        let mut mirror = self.cfg.mirror;
        if ui.checkbox(&mut mirror, "Зеркалить изображение").changed() {
            self.cfg.mirror = mirror;
            self.mirror.store(mirror, Ordering::Relaxed);
            self.preview_gen = 0; // re-render the current frame
            self.cfg.save();
            self.set(json!({"mirror": mirror}));
        }

        let mut dim = self.state["screen"].as_str() == Some("dim");
        if ui.checkbox(&mut dim, "Затемнить экран телефона").changed() {
            self.set(json!({"screen": if dim { "dim" } else { "on" }}));
        }
    }

    fn ui_outputs(&mut self, ui: &mut egui::Ui) {
        ui.heading("Выход");

        // Virtual camera
        let mut vcam_on = self.vcam.is_some();
        if ui.checkbox(&mut vcam_on, "Виртуальная камера «Lensbridge Camera»").changed() {
            self.set_vcam(vcam_on);
        }
        if cfg!(windows) {
            if vcam::filter_installed() {
                ui.label(
                    RichText::new("Драйвер камеры установлен")
                        .small()
                        .color(Color32::from_rgb(90, 170, 90)),
                );
            } else {
                ui.label(
                    RichText::new("Драйвер камеры не установлен: Zoom/OBS её не увидят")
                        .small()
                        .color(Color32::from_rgb(220, 160, 60)),
                );
                if ui.button("Установить (нужны права администратора)").clicked() {
                    match vcam::install_filter(true) {
                        Ok(()) => self.note("Запрошена установка виртуальной камеры"),
                        Err(e) => self.note(format!("Установка камеры: {e}")),
                    }
                }
            }
        }
        if let Some(e) = &self.vcam_error {
            ui.label(RichText::new(e).small().color(Color32::from_rgb(220, 120, 80)));
        }

        ui.add_space(8.0);
        // Audio
        let current = self.cfg.audio_device.clone();
        let mut sel = current.clone();
        let cable = find_virtual_cable(&self.audio_devices);
        egui::ComboBox::from_label("Звук телефона")
            .selected_text(current.clone().unwrap_or_else(|| "не выводить".into()))
            .width(220.0)
            .show_ui(ui, |ui| {
                ui.selectable_value(&mut sel, None, "не выводить");
                for d in &self.audio_devices {
                    let label = if Some(d) == cable.as_ref() {
                        format!("{d}  ← микрофон для звонков")
                    } else {
                        d.clone()
                    };
                    ui.selectable_value(&mut sel, Some(d.clone()), label);
                }
            });
        if sel != current {
            self.set_audio_device(sel);
        }
        if ui.small_button("Обновить список устройств").clicked() {
            self.audio_devices = list_output_devices();
        }
        if cable.is_none() {
            ui.label(
                RichText::new(if cfg!(windows) {
                    "Чтобы звук стал микрофоном в Zoom/Discord, установите VB-Cable (vb-audio.com/Cable) и выберите «CABLE Input» здесь, а в звонке — микрофон «CABLE Output»."
                } else {
                    "Для микрофона в звонках установите виртуальный кабель (macOS: BlackHole)."
                })
                .small()
                .weak(),
            );
        }
        if let Some(e) = &self.audio_error {
            ui.label(RichText::new(e).small().color(Color32::from_rgb(220, 120, 80)));
        }
    }

    fn ui_status(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            let (text, color) = match &self.conn {
                Some(ConnState::Connected { device, .. }) => (device.clone(), Color32::from_rgb(90, 180, 90)),
                Some(ConnState::Connecting { .. }) => ("подключение…".into(), Color32::from_rgb(200, 170, 60)),
                Some(ConnState::NotPaired) => ("не сопряжено".into(), Color32::from_rgb(220, 120, 80)),
                Some(ConnState::Busy) => ("занят".into(), Color32::from_rgb(220, 120, 80)),
                Some(ConnState::Disconnected { .. }) => ("нет связи".into(), Color32::from_rgb(220, 120, 80)),
                _ => ("не подключено".into(), Color32::GRAY),
            };
            let (dot, _) = ui.allocate_exact_size(Vec2::splat(12.0), Sense::hover());
            ui.painter().circle_filled(dot.center(), 5.0, color);
            ui.label(RichText::new(text).color(color));
            if let Some(t) = &self.target {
                ui.label(RichText::new(t.label()).weak());
            }
            ui.separator();
            let s = &self.stats;
            ui.label(format!("{}×{}  {:.0} к/с  {:.1} Мбит/с", s.width, s.height, s.fps, s.kbps / 1000.0));
            if let Some(l) = s.latency_ms {
                ui.label(format!("задержка ≈{l:.0} мс"));
            }
            if let Some(phone) = &s.phone {
                if let Some(b) = phone["battery"].as_f64() {
                    let ch = if phone["charging"].as_bool().unwrap_or(false) {
                        " (заряжается)"
                    } else {
                        ""
                    };
                    ui.label(format!("батарея {:.0}%{ch}", b * 100.0));
                }
                if let Some(t) = phone["thermal"].as_str().filter(|t| *t != "nominal") {
                    ui.label(RichText::new(format!("нагрев: {t}")).color(Color32::from_rgb(220, 140, 60)));
                }
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui.button("Диагностика").clicked() {
                    self.show_diag = true;
                }
            });
        });
    }

    fn diagnostics_report(&self) -> String {
        let s = &self.stats;
        let mut r = String::new();
        r += &format!(
            "Lensbridge {} ({} {})\n",
            env!("CARGO_PKG_VERSION"),
            std::env::consts::OS,
            std::env::consts::ARCH
        );
        r += &format!("Цель: {}\n", self.target.as_ref().map(|t| t.label()).unwrap_or_else(|| "-".into()));
        r += &format!("Состояние: {:?}\n", self.conn);
        r += &format!("Ключ (id): {}\n", self.key.key_id_hex());
        r += &format!(
            "Видео: {}x{} {:.1} к/с, {:.0} кбит/с, задержка {:?} мс, RTT {:?} мс\n",
            s.width, s.height, s.fps, s.kbps, s.latency_ms, s.rtt_ms
        );
        r += &format!(
            "Кадров всего {}, пропущено {}, ошибок декодера {}, аудио-пакетов {}\n",
            s.frames_total, s.frames_dropped, s.decode_errors, s.audio_chunks
        );
        r += &format!(
            "Телефон: {}\n",
            s.phone.as_ref().map(|p| p.to_string()).unwrap_or_else(|| "-".into())
        );
        r += &format!("Состояние камеры: {}\n", self.state);
        r += &format!(
            "Виртуальная камера: {} (драйвер {}, записано кадров {})\n",
            if self.vcam.is_some() { "вкл" } else { "выкл" },
            if vcam::filter_installed() {
                "установлен"
            } else {
                "не установлен"
            },
            self.vcam.as_ref().map(|v| v.frames_written()).unwrap_or(0)
        );
        if let Some(e) = &self.vcam_error {
            r += &format!("  ошибка: {e}\n");
        }
        match &self.audio_out {
            Some(a) => {
                r += &format!(
                    "Звук: {} (буфер {:.0} мс, опустошений {})\n",
                    a.device_name,
                    a.buffered_ms(),
                    a.underruns()
                )
            }
            None => r += &format!("Звук: выкл {}\n", self.audio_error.clone().unwrap_or_default()),
        }
        for p in self.discovery.problems() {
            r += &format!("Поиск: {p}\n");
        }
        r += "Журнал:\n";
        for l in &self.log {
            r += &format!("  {l}\n");
        }
        r
    }

    fn ui_windows(&mut self, ctx: &egui::Context) {
        if self.show_pairing {
            let mut open = true;
            let uri = self.key.to_uri(&pc_name());
            if self.qr_texture.is_none() {
                if let Some(img) = qr::render(&uri, 6) {
                    self.qr_texture = Some(ctx.load_texture("pairing-qr", img, TextureOptions::NEAREST));
                }
            }
            egui::Window::new("Пейринг с телефоном")
                .open(&mut open)
                .resizable(false)
                .collapsible(false)
                .show(ctx, |ui| {
                    ui.label("1. Откройте Lensbridge на телефоне → «Сопрячь с ПК».");
                    ui.label("2. Наведите камеру телефона на этот код.");
                    ui.label(
                        RichText::new("Код содержит секретный ключ: не показывайте его посторонним.")
                            .small()
                            .weak(),
                    );
                    if let Some(t) = &self.qr_texture {
                        ui.add(egui::Image::new(t).fit_to_exact_size(Vec2::splat(300.0)));
                    }
                    ui.horizontal(|ui| {
                        if ui
                            .button("Скопировать ссылку")
                            .on_hover_text("Для симулятора lb-sim --uri или ручного ввода")
                            .clicked()
                        {
                            ui.ctx().copy_text(uri.clone());
                        }
                        if ui.button("Новый ключ…").clicked() {
                            self.confirm_new_key = true;
                        }
                    });
                    if self.confirm_new_key {
                        ui.separator();
                        ui.label("Новый ключ отменит доступ всем уже сопряжённым телефонам. Продолжить?");
                        ui.horizontal(|ui| {
                            if ui.button("Да, создать").clicked() {
                                self.confirm_new_key = false;
                                self.new_key();
                            }
                            if ui.button("Отмена").clicked() {
                                self.confirm_new_key = false;
                            }
                        });
                    }
                });
            self.show_pairing = open;
        }

        if self.show_diag {
            let mut open = true;
            let report = self.diagnostics_report();
            egui::Window::new("Диагностика")
                .open(&mut open)
                .default_width(640.0)
                .show(ctx, |ui| {
                    if ui.button("Скопировать отчёт").clicked() {
                        ui.ctx().copy_text(report.clone());
                    }
                    egui::ScrollArea::vertical().max_height(420.0).show(ui, |ui| {
                        ui.add(egui::Label::new(RichText::new(&report).monospace()).wrap());
                    });
                });
            self.show_diag = open;
        }
    }
}

impl eframe::App for LensbridgeApp {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        self.pump_events();
        self.update_preview(&ctx);

        egui::Panel::bottom("status").show(ui, |ui| self.ui_status(ui));
        egui::Panel::left("devices")
            .resizable(true)
            .default_size(250.0)
            .show(ui, |ui| self.ui_left(ui));
        egui::Panel::right("controls")
            .resizable(true)
            .default_size(320.0)
            .show(ui, |ui| self.ui_right(ui));
        egui::CentralPanel::default().show(ui, |ui| self.ui_preview(ui));
        self.ui_windows(&ctx);

        let interval = if self.client.is_some() {
            Duration::from_millis(15)
        } else {
            Duration::from_millis(250)
        };
        ctx.request_repaint_after(interval);
    }
}
