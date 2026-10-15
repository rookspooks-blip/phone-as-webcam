import AVFoundation
import Combine
import SwiftUI
import UIKit

/// Glue between capture, encoder, network servers, pairing and the UI.
final class AppModel: ObservableObject {
    // MARK: UI state (main thread)
    @Published var status = "Запуск…"
    @Published var connectedPC: String?
    @Published var isCapturing = false
    @Published var dimmed = false {
        didSet { lock.lock(); dimFlag = dimmed; lock.unlock() }
    }
    @Published var httpViewers = 0
    @Published var fps = 0
    @Published var kbps = 0
    @Published var addresses: [String] = []
    @Published var events: [String] = []
    @Published var cameraDenied = false
    @Published var micDenied = false
    @Published var showScanner = false
    @Published var httpEnabled: Bool {
        didSet {
            UserDefaults.standard.set(httpEnabled, forKey: "httpEnabled")
            httpEnabled ? http.start() : http.stop()
        }
    }
    @Published var orientationMode: String {
        didSet {
            UserDefaults.standard.set(orientationMode, forKey: "orientationMode")
            capture.setOrientationMode(orientationMode)
        }
    }
    @Published var deviceName: String {
        didSet { UserDefaults.standard.set(deviceName, forKey: "deviceName") }
    }

    let pairing = PairingStore()
    let capture = CaptureController()
    private let encoder = H264Encoder()
    private let audioConverter = AudioPCMConverter()
    private let jpeg = JpegEncoder()
    private let jpegQueue = DispatchQueue(label: "jpeg", qos: .utility)
    private var server: LbpServer!
    private let http = HttpServer()
    private let installId: String

    // MARK: Streaming state (guarded by `lock`)
    private let lock = NSLock()
    private var session: LbpSession?
    private var videoSeq: UInt32 = 0
    private var audioSeq: UInt32 = 0
    private var pendingDiscont = true
    private var framesThisSecond = 0
    private var droppedTotal = 0
    private var jpegBusy = false
    private var jpegSeq: UInt32 = 0
    private var frameCounter = 0
    private var mirror = false
    private var audioWanted = true
    private var dimFlag = false
    private var statsTimer: Timer?
    private var wasBackgrounded = false

    init() {
        let defaults = UserDefaults.standard
        httpEnabled = defaults.bool(forKey: "httpEnabled")
        orientationMode = defaults.string(forKey: "orientationMode") ?? "auto"
        deviceName = defaults.string(forKey: "deviceName") ?? UIDevice.current.name
        if let id = defaults.string(forKey: "installId") {
            installId = id
        } else {
            installId = String(UUID().uuidString.replacingOccurrences(of: "-", with: "").prefix(16)).lowercased()
            defaults.set(installId, forKey: "installId")
        }

        UIDevice.current.isBatteryMonitoringEnabled = true
        UIDevice.current.beginGeneratingDeviceOrientationNotifications()
        capture.delegate = self
        capture.setOrientationMode(orientationMode)
        encoder.onFrame = { [weak self] data, key, pts in self?.sendVideo(data, key: key, pts: pts) }

        server = LbpServer(keyLookup: { [weak self] id in self?.pairing.key(forKeyId: id) })
        server.delegate = self
        http.deviceName = deviceName
        http.onViewersChanged = { [weak self] n in
            DispatchQueue.main.async {
                self?.httpViewers = n
                self?.updateCaptureNeeded()
            }
        }
        http.onStateChanged = { [weak self] s in DispatchQueue.main.async { self?.note(s) } }

        requestPermissions()
        server.start(deviceName: deviceName, installId: installId)
        if httpEnabled { http.start() }
        refreshAddresses()
        statsTimer = Timer.scheduledTimer(withTimeInterval: 1, repeats: true) { [weak self] _ in self?.tick() }
    }

    // MARK: - Helpers

    func note(_ s: String) {
        let f = DateFormatter()
        f.dateFormat = "HH:mm:ss"
        events.append("\(f.string(from: Date()))  \(s)")
        if events.count > 60 { events.removeFirst(events.count - 60) }
    }

    func refreshAddresses() {
        addresses = NetUtil.localIPv4Addresses().map { "\($0.ip)  (\($0.iface))" }
    }

    private func requestPermissions() {
        AVCaptureDevice.requestAccess(for: .video) { ok in
            DispatchQueue.main.async { self.cameraDenied = !ok }
            AVCaptureDevice.requestAccess(for: .audio) { ok in
                DispatchQueue.main.async { self.micDenied = !ok }
            }
        }
    }

    private var currentSession: LbpSession? {
        lock.lock(); defer { lock.unlock() }
        return session
    }

    /// Capture runs only while somebody consumes it (PC session or browser viewer).
    private func updateCaptureNeeded() {
        let needed = currentSession != nil || httpViewers > 0
        if needed && !isCapturing {
            isCapturing = true
            capture.start { ok in
                DispatchQueue.main.async {
                    self.isCapturing = ok
                    if !ok { self.note("Не удалось запустить камеру") }
                }
            }
        } else if !needed && isCapturing {
            capture.stop()
            encoder.invalidate()
            isCapturing = false
        }
    }

    func scenePhaseChanged(_ phase: ScenePhase) {
        switch phase {
        case .active:
            UIApplication.shared.isIdleTimerDisabled = true // screen lock would stop the camera
            refreshAddresses()
            if wasBackgrounded {
                wasBackgrounded = false
                server.restart(deviceName: deviceName, installId: installId)
                if httpEnabled { http.stop(); http.start() }
            }
        case .background:
            wasBackgrounded = true
            note("Приложение в фоне: iOS остановила камеру")
        default:
            break
        }
    }

    func startScanning() {
        if currentSession != nil || httpViewers > 0 {
            note("Сначала отключите ПК: камера занята трансляцией")
            return
        }
        showScanner = true
    }

    func handleScanned(_ text: String) {
        showScanner = false
        guard let uri = PairingURI(text) else {
            note("Это не QR-код Lensbridge")
            return
        }
        pairing.add(key: uri.key, name: uri.pcName)
        note("Сопряжено с «\(uri.pcName)»")
    }

    func setDimmed(_ on: Bool) {
        dimmed = on
    }

    private func tick() {
        lock.lock()
        let frames = framesThisSecond
        framesThisSecond = 0
        let dropped = droppedTotal
        let s = session
        lock.unlock()
        fps = frames
        let sent = s?.takeSentBytes() ?? 0
        kbps = sent * 8 / 1000
        guard let s else { return }
        let dev = UIDevice.current
        let thermal: String
        switch ProcessInfo.processInfo.thermalState {
        case .nominal: thermal = "nominal"
        case .fair: thermal = "fair"
        case .serious: thermal = "serious"
        case .critical: thermal = "critical"
        @unknown default: thermal = "nominal"
        }
        s.sendJSON([
            "t": "stats", "fps": frames, "kbps": kbps, "dropped": dropped,
            "battery": dev.batteryLevel >= 0 ? Double(dev.batteryLevel) as Any : NSNull(),
            "charging": dev.batteryState == .charging || dev.batteryState == .full,
            "thermal": thermal,
        ])
    }

    // MARK: - Sending

    private func sendVideo(_ data: Data, key: Bool, pts: CMTime) {
        lock.lock()
        guard let s = session else { lock.unlock(); return }
        var flags: UInt8 = key ? (Lbp.flagKey | Lbp.flagConfig) : 0
        if pendingDiscont {
            if !key {
                lock.unlock()
                encoder.requestKeyframe()
                return
            }
            flags |= Lbp.flagDiscont
            pendingDiscont = false
        }
        let header = MediaHeader(codec: Lbp.codecH264, flags: flags, seq: videoSeq, ptsUs: cmTimeMicros(pts))
        // ~150 ms of video may be queued; beyond that drop until the next IDR.
        let limit = max(256 * 1024, encoder.bitrate / 8 * 150 / 1000)
        if s.sendMedia(type: .video, header: header, payload: data, droppable: true, limit: limit) {
            videoSeq &+= 1
            lock.unlock()
        } else {
            droppedTotal += 1
            pendingDiscont = true
            lock.unlock()
            encoder.requestKeyframe()
        }
    }

    private func sendStreamConfig(_ s: LbpSession, state: [String: Any]) {
        s.sendJSON([
            "t": "stream_config",
            "video": ["codec": "h264", "w": state["w"] ?? 0, "h": state["h"] ?? 0, "fps": state["fps"] ?? 30],
            "audio": ["codec": "pcm_s16le", "rate": 48000, "channels": 1],
        ])
    }

    private func fullState(_ captureState: [String: Any]) -> [String: Any] {
        var st = captureState
        st["bitrate"] = encoder.bitrate
        lock.lock()
        st["mirror"] = mirror
        st["screen"] = dimFlag ? "dim" : "on"
        lock.unlock()
        return st
    }

    private func handleSet(_ msg: [String: Any], session s: LbpSession) {
        let p = msg["p"] as? [String: Any] ?? [:]
        if let b = p["bitrate"] as? Int { encoder.setBitrate(b) }
        if let m = p["mirror"] as? Bool { lock.lock(); mirror = m; lock.unlock() }
        if let a = p["audio"] as? Bool { lock.lock(); audioWanted = a; lock.unlock() }
        if let scr = p["screen"] as? String {
            let on = scr == "dim"
            lock.lock(); dimFlag = on; lock.unlock()
            DispatchQueue.main.async { self.dimmed = on }
        }
        let reformat = p["w"] != nil || p["fps"] != nil || p["camera"] != nil
        capture.apply(p) { [weak self] applied in
            guard let self else { return }
            if let f = applied["fps"] as? Int { self.encoder.setFps(f) }
            if reformat {
                self.lock.lock(); self.pendingDiscont = true; self.lock.unlock()
                self.encoder.requestKeyframe()
                self.sendStreamConfig(s, state: applied)
            }
            s.sendJSON(["t": "ack", "id": msg["id"] ?? 0, "applied": self.fullState(applied)])
        }
    }
}

// MARK: - Server delegate

extension AppModel: LbpServerDelegate {
    func server(_ server: LbpServer, listenerState: String) {
        DispatchQueue.main.async { self.status = listenerState }
    }

    func server(_ server: LbpServer, sessionStarted s: LbpSession, hello: [String: Any]) {
        lock.lock()
        session = s
        videoSeq = 0
        audioSeq = 0
        pendingDiscont = true
        droppedTotal = 0
        lock.unlock()
        encoder.requestKeyframe()
        DispatchQueue.main.async {
            self.connectedPC = s.clientName
            self.note("Подключён ПК «\(s.clientName)» (\(s.peerDescription))")
            self.updateCaptureNeeded()
        }
        capture.state { [weak self] st in
            guard let self else { return }
            let full = self.fullState(st)
            s.sendJSON([
                "t": "welcome", "proto": Lbp.protoVersion, "server": "lensbridge-ios",
                "version": Bundle.main.infoDictionary?["CFBundleShortVersionString"] as? String ?? "0",
                "device": self.deviceNameSnapshot, "server_time_us": hostTimeMicros(), "state": full,
            ])
            self.sendStreamConfig(s, state: st)
        }
    }

    private var deviceNameSnapshot: String {
        UserDefaults.standard.string(forKey: "deviceName") ?? UIDevice.current.name
    }

    func server(_ server: LbpServer, session s: LbpSession, control msg: [String: Any]) {
        switch msg["t"] as? String ?? "" {
        case "ping":
            s.sendJSON(["t": "pong", "id": msg["id"] ?? 0, "c0": msg["c0"] ?? 0, "s1": hostTimeMicros()])
        case "get_caps":
            var caps = capture.capsJSON()
            caps["id"] = msg["id"] ?? 0
            s.sendJSON(caps)
        case "set":
            handleSet(msg, session: s)
        case "request_keyframe":
            encoder.requestKeyframe()
        case "bye":
            s.close(reason: "ПК отключился")
        default:
            s.sendJSON(["t": "error", "id": msg["id"] ?? 0, "code": "bad_request", "message": "unknown type"])
        }
    }

    func server(_ server: LbpServer, sessionEnded s: LbpSession, reason: String) {
        lock.lock()
        if session?.id == s.id { session = nil }
        lock.unlock()
        DispatchQueue.main.async {
            self.connectedPC = nil
            self.note("ПК отключён: \(reason)")
            self.updateCaptureNeeded()
        }
    }
}

// MARK: - Capture delegate

extension AppModel: CaptureControllerDelegate {
    func captureVideo(_ pixelBuffer: CVPixelBuffer, pts: CMTime) {
        lock.lock()
        framesThisSecond += 1
        frameCounter += 1
        let streaming = session != nil
        let wantJpeg = http.viewerCount > 0 && frameCounter % 2 == 0 && !jpegBusy
        if wantJpeg { jpegBusy = true }
        lock.unlock()

        if streaming {
            encoder.encode(pixelBuffer, pts: pts)
        }
        if wantJpeg {
            let ptsUs = cmTimeMicros(pts)
            jpegQueue.async { [weak self] in
                guard let self else { return }
                if let data = self.jpeg.encode(pixelBuffer) {
                    self.lock.lock()
                    let seq = self.jpegSeq
                    self.jpegSeq &+= 1
                    self.lock.unlock()
                    self.http.broadcast(jpeg: data, seq: seq, ptsUs: ptsUs)
                }
                self.lock.lock(); self.jpegBusy = false; self.lock.unlock()
            }
        }
    }

    func captureAudio(_ sampleBuffer: CMSampleBuffer) {
        lock.lock()
        let s = session
        let wanted = audioWanted
        lock.unlock()
        guard let s, wanted, let pcm = audioConverter.convert(sampleBuffer) else { return }
        lock.lock()
        let header = MediaHeader(codec: Lbp.codecPCM, flags: 0, seq: audioSeq, ptsUs: cmTimeMicros(CMSampleBufferGetPresentationTimeStamp(sampleBuffer)))
        audioSeq &+= 1
        lock.unlock()
        s.sendMedia(type: .audio, header: header, payload: pcm, droppable: true, limit: 4 * 1024 * 1024)
    }

    func captureInterrupted(_ reason: String) {
        currentSession?.sendJSON(["t": "event", "kind": "interrupted", "detail": reason])
        DispatchQueue.main.async { self.note("Камера прервана: \(reason)") }
    }

    func captureResumed() {
        lock.lock(); pendingDiscont = true; lock.unlock()
        encoder.requestKeyframe()
        currentSession?.sendJSON(["t": "event", "kind": "resumed", "detail": ""])
        DispatchQueue.main.async { self.note("Камера снова работает") }
    }
}
