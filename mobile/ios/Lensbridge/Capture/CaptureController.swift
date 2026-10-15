import AVFoundation
import UIKit

protocol CaptureControllerDelegate: AnyObject {
    func captureVideo(_ pixelBuffer: CVPixelBuffer, pts: CMTime)
    func captureAudio(_ sampleBuffer: CMSampleBuffer)
    func captureInterrupted(_ reason: String)
    func captureResumed()
}

/// AVCaptureSession wrapper: enumerates cameras/formats dynamically and applies
/// settings coming from the PC. All session work happens on `sessionQueue`.
final class CaptureController: NSObject {
    weak var delegate: CaptureControllerDelegate?

    let session = AVCaptureSession()
    private let sessionQueue = DispatchQueue(label: "capture.session")
    private let videoQueue = DispatchQueue(label: "capture.video", qos: .userInteractive)
    private let audioQueue = DispatchQueue(label: "capture.audio", qos: .userInitiated)
    private let videoOutput = AVCaptureVideoDataOutput()
    private let audioOutput = AVCaptureAudioDataOutput()
    private var videoInput: AVCaptureDeviceInput?
    private var audioInput: AVCaptureDeviceInput?
    private var configured = false

    // Current settings (sessionQueue only).
    private var cameraId = ""
    private var width = 1280
    private var height = 720
    private var fps = 30
    private var zoom: CGFloat = 1
    private var torch = false
    private var focusMode = "continuous"
    private var ev: Float = 0
    private var audioEnabled = true
    /// "auto" follows the device orientation, or "landscape" / "portrait".
    var orientationMode = "auto"
    private var orientation: AVCaptureVideoOrientation = .landscapeRight

    override init() {
        super.init()
        NotificationCenter.default.addObserver(self, selector: #selector(interrupted(_:)), name: .AVCaptureSessionWasInterrupted, object: session)
        NotificationCenter.default.addObserver(self, selector: #selector(interruptionEnded(_:)), name: .AVCaptureSessionInterruptionEnded, object: session)
        NotificationCenter.default.addObserver(self, selector: #selector(runtimeError(_:)), name: .AVCaptureSessionRuntimeError, object: session)
        NotificationCenter.default.addObserver(self, selector: #selector(deviceOrientationChanged), name: UIDevice.orientationDidChangeNotification, object: nil)
    }

    // MARK: - Devices and capabilities

    static func cameras() -> [AVCaptureDevice] {
        var types: [AVCaptureDevice.DeviceType] = [.builtInWideAngleCamera, .builtInUltraWideCamera, .builtInTelephotoCamera, .builtInTripleCamera, .builtInDualWideCamera]
        types.removeAll { $0 == .builtInDualWideCamera && !AVCaptureDevice.DiscoverySession(deviceTypes: [.builtInTripleCamera], mediaType: .video, position: .back).devices.isEmpty }
        return AVCaptureDevice.DiscoverySession(deviceTypes: types, mediaType: .video, position: .unspecified).devices
    }

    static func displayName(_ d: AVCaptureDevice) -> String {
        if d.position == .front { return "Фронтальная" }
        switch d.deviceType {
        case .builtInUltraWideCamera: return "Задняя сверхширокая (0.5×)"
        case .builtInTelephotoCamera: return "Задняя телевик"
        case .builtInTripleCamera, .builtInDualWideCamera: return "Задняя, все объективы (авто)"
        default: return "Задняя широкоугольная (1×)"
        }
    }

    /// Distinct (w, h) video formats with their frame-rate range, largest first.
    static func formats(of d: AVCaptureDevice) -> [(w: Int, h: Int, minFps: Int, maxFps: Int)] {
        var best: [String: (Int, Int, Int, Int)] = [:]
        for f in d.formats {
            let sub = CMFormatDescriptionGetMediaSubType(f.formatDescription)
            guard sub == kCVPixelFormatType_420YpCbCr8BiPlanarVideoRange || sub == kCVPixelFormatType_420YpCbCr8BiPlanarFullRange else { continue }
            let dims = CMVideoFormatDescriptionGetDimensions(f.formatDescription)
            let (w, h) = (Int(dims.width), Int(dims.height))
            guard w >= 320, w * h <= 3840 * 2160 else { continue }
            let maxFps = Int(f.videoSupportedFrameRateRanges.map(\.maxFrameRate).max() ?? 30)
            let minFps = max(1, Int(f.videoSupportedFrameRateRanges.map(\.minFrameRate).min() ?? 1))
            let key = "\(w)x\(h)"
            if let cur = best[key] {
                best[key] = (w, h, min(cur.2, minFps), max(cur.3, maxFps))
            } else {
                best[key] = (w, h, minFps, maxFps)
            }
        }
        return best.values.sorted { ($0.0 * $0.1, $0.3) > ($1.0 * $1.1, $1.3) }.map { (w: $0.0, h: $0.1, minFps: $0.2, maxFps: $0.3) }
    }

    private static func maxZoom(_ d: AVCaptureDevice) -> CGFloat {
        min(d.maxAvailableVideoZoomFactor, d.activeFormat.videoMaxZoomFactor, 15)
    }

    func capsJSON() -> [String: Any] {
        let cams: [[String: Any]] = Self.cameras().map { d in
            [
                "id": d.uniqueID,
                "name": Self.displayName(d),
                "position": d.position == .front ? "front" : "back",
                "formats": Self.formats(of: d).map { ["w": $0.w, "h": $0.h, "min_fps": $0.minFps, "max_fps": $0.maxFps] },
                "zoom_min": Double(d.minAvailableVideoZoomFactor),
                "zoom_max": Double(Self.maxZoom(d)),
                "torch": d.hasTorch,
                "focus_poi": d.isFocusPointOfInterestSupported,
                "exposure_poi": d.isExposurePointOfInterestSupported,
                "ev_min": Double(d.minExposureTargetBias),
                "ev_max": Double(d.maxExposureTargetBias),
            ]
        }
        return ["t": "caps", "cameras": cams, "audio": ["rate": 48000, "channels": 1]]
    }

    // MARK: - Lifecycle

    var isRunning: Bool { session.isRunning }

    func start(completion: @escaping (Bool) -> Void) {
        sessionQueue.async {
            if !self.configured { self.configure() }
            if !self.session.isRunning { self.session.startRunning() }
            completion(self.session.isRunning)
        }
    }

    func stop() {
        sessionQueue.async {
            if self.session.isRunning { self.session.stopRunning() }
            self.setTorchLocked(false)
        }
    }

    private func configure() {
        session.beginConfiguration()
        session.sessionPreset = .inputPriority
        session.automaticallyConfiguresApplicationAudioSession = true

        videoOutput.videoSettings = [kCVPixelBufferPixelFormatTypeKey as String: kCVPixelFormatType_420YpCbCr8BiPlanarVideoRange]
        videoOutput.alwaysDiscardsLateVideoFrames = true
        videoOutput.setSampleBufferDelegate(self, queue: videoQueue)
        if session.canAddOutput(videoOutput) { session.addOutput(videoOutput) }

        audioOutput.setSampleBufferDelegate(self, queue: audioQueue)
        if session.canAddOutput(audioOutput) { session.addOutput(audioOutput) }

        let initial = Self.cameras().first { $0.deviceType == .builtInWideAngleCamera && $0.position == .back } ?? Self.cameras().first
        if let d = initial { switchCamera(to: d) }
        setAudioLocked(audioEnabled)
        session.commitConfiguration()
        configured = true
        updateOrientationLocked()
    }

    // MARK: - Settings

    /// Applies a `set` request; calls back with the full applied state.
    func apply(_ p: [String: Any], completion: @escaping ([String: Any]) -> Void) {
        sessionQueue.async {
            if !self.configured { self.configure() }
            if let id = p["camera"] as? String, id != self.cameraId,
               let d = Self.cameras().first(where: { $0.uniqueID == id }) {
                self.session.beginConfiguration()
                self.switchCamera(to: d)
                self.session.commitConfiguration()
                self.updateOrientationLocked()
            }
            if let w = p["w"] as? Int, let h = p["h"] as? Int { self.width = w; self.height = h }
            if let f = p["fps"] as? Int { self.fps = f }
            if p["w"] != nil || p["fps"] != nil || p["camera"] != nil { self.applyFormatLocked() }
            if let z = p["zoom"] as? Double { self.zoom = CGFloat(z); self.applyZoomLocked() }
            if let t = p["torch"] as? Bool { self.torch = t; self.setTorchLocked(t) }
            if let e = p["ev"] as? Double { self.ev = Float(e); self.applyEVLocked() }
            if let m = p["focus_mode"] as? String { self.focusMode = m }
            let focus = p["focus"] as? [String: Any]
            let expo = p["exposure"] as? [String: Any]
            if focus != nil || expo != nil || p["focus_mode"] != nil {
                self.applyFocusLocked(focus: Self.point(focus), exposure: Self.point(expo))
            }
            if let a = p["audio"] as? Bool, a != self.audioEnabled {
                self.audioEnabled = a
                self.session.beginConfiguration()
                self.setAudioLocked(a)
                self.session.commitConfiguration()
            }
            completion(self.stateLocked())
        }
    }

    func state(completion: @escaping ([String: Any]) -> Void) {
        sessionQueue.async { completion(self.stateLocked()) }
    }

    private func stateLocked() -> [String: Any] {
        [
            "camera": cameraId, "w": width, "h": height, "fps": fps,
            "zoom": Double(zoom), "torch": torch, "focus_mode": focusMode, "ev": Double(ev), "audio": audioEnabled,
        ]
    }

    private static func point(_ d: [String: Any]?) -> CGPoint? {
        guard let d, let x = d["x"] as? Double, let y = d["y"] as? Double else { return nil }
        return CGPoint(x: min(max(x, 0), 1), y: min(max(y, 0), 1))
    }

    private func switchCamera(to d: AVCaptureDevice) {
        if let old = videoInput { session.removeInput(old) }
        guard let input = try? AVCaptureDeviceInput(device: d), session.canAddInput(input) else { return }
        session.addInput(input)
        videoInput = input
        cameraId = d.uniqueID
        torch = false
        if let conn = videoOutput.connection(with: .video) {
            conn.automaticallyAdjustsVideoMirroring = false
            conn.isVideoMirrored = false // mirroring is done on the PC
            if conn.isVideoStabilizationSupported { conn.preferredVideoStabilizationMode = .off } // latency
        }
        applyFormatLocked()
        zoom = d.position == .back && (d.deviceType == .builtInTripleCamera || d.deviceType == .builtInDualWideCamera) ? 2 : 1
        applyZoomLocked()
    }

    private func applyFormatLocked() {
        guard let d = videoInput?.device else { return }
        let candidates = d.formats.filter { f in
            let sub = CMFormatDescriptionGetMediaSubType(f.formatDescription)
            return sub == kCVPixelFormatType_420YpCbCr8BiPlanarVideoRange || sub == kCVPixelFormatType_420YpCbCr8BiPlanarFullRange
        }
        func dims(_ f: AVCaptureDevice.Format) -> (Int, Int) {
            let x = CMVideoFormatDescriptionGetDimensions(f.formatDescription)
            return (Int(x.width), Int(x.height))
        }
        func maxFps(_ f: AVCaptureDevice.Format) -> Double { f.videoSupportedFrameRateRanges.map(\.maxFrameRate).max() ?? 30 }
        // Exact size first, then the closest area; prefer formats reaching the fps.
        let target = width * height
        let sorted = candidates.sorted { a, b in
            let (aw, ah) = dims(a), (bw, bh) = dims(b)
            let ad = abs(aw * ah - target) + (aw == width && ah == height ? 0 : 1)
            let bd = abs(bw * bh - target) + (bw == width && bh == height ? 0 : 1)
            if ad != bd { return ad < bd }
            let aOk = maxFps(a) >= Double(fps), bOk = maxFps(b) >= Double(fps)
            if aOk != bOk { return aOk }
            return a.isVideoBinned && !b.isVideoBinned // binned formats are cheaper
        }
        guard let fmt = sorted.first else { return }
        let (w, h) = dims(fmt)
        width = w
        height = h
        let maxRate = maxFps(fmt)
        let minRate = fmt.videoSupportedFrameRateRanges.map(\.minFrameRate).min() ?? 1
        fps = Int(min(max(Double(fps), minRate), maxRate))
        do {
            try d.lockForConfiguration()
            d.activeFormat = fmt
            let dur = CMTime(value: 1, timescale: CMTimeScale(fps))
            d.activeVideoMinFrameDuration = dur
            d.activeVideoMaxFrameDuration = dur
            d.unlockForConfiguration()
        } catch {
            NSLog("format: \(error)")
        }
        applyZoomLocked()
    }

    private func applyZoomLocked() {
        guard let d = videoInput?.device else { return }
        let z = min(max(zoom, d.minAvailableVideoZoomFactor), Self.maxZoom(d))
        zoom = z
        guard (try? d.lockForConfiguration()) != nil else { return }
        d.videoZoomFactor = z
        d.unlockForConfiguration()
    }

    private func setTorchLocked(_ on: Bool) {
        guard let d = videoInput?.device, d.hasTorch, d.isTorchAvailable else { torch = false; return }
        guard (try? d.lockForConfiguration()) != nil else { return }
        if on { try? d.setTorchModeOn(level: AVCaptureDevice.maxAvailableTorchLevel) } else { d.torchMode = .off }
        d.unlockForConfiguration()
        torch = on
    }

    private func applyEVLocked() {
        guard let d = videoInput?.device else { return }
        let v = min(max(ev, d.minExposureTargetBias), d.maxExposureTargetBias)
        ev = v
        guard (try? d.lockForConfiguration()) != nil else { return }
        d.setExposureTargetBias(v, completionHandler: nil)
        d.unlockForConfiguration()
    }

    /// Converts a point in the streamed frame (top-left origin) to the device
    /// point-of-interest space (landscape-right sensor orientation).
    private func devicePoint(_ p: CGPoint) -> CGPoint {
        switch orientation {
        case .landscapeRight: return p
        case .landscapeLeft: return CGPoint(x: 1 - p.x, y: 1 - p.y)
        case .portrait: return CGPoint(x: p.y, y: 1 - p.x)
        case .portraitUpsideDown: return CGPoint(x: 1 - p.y, y: p.x)
        @unknown default: return p
        }
    }

    private func applyFocusLocked(focus: CGPoint?, exposure: CGPoint?) {
        guard let d = videoInput?.device, (try? d.lockForConfiguration()) != nil else { return }
        defer { d.unlockForConfiguration() }
        if let f = focus, d.isFocusPointOfInterestSupported {
            d.focusPointOfInterest = devicePoint(f)
        }
        switch focusMode {
        case "locked":
            if d.isFocusModeSupported(.locked) { d.focusMode = .locked }
        case "auto":
            if d.isFocusModeSupported(.autoFocus) { d.focusMode = .autoFocus }
        default:
            if d.isFocusModeSupported(.continuousAutoFocus) { d.focusMode = .continuousAutoFocus }
        }
        if let e = exposure, d.isExposurePointOfInterestSupported {
            d.exposurePointOfInterest = devicePoint(e)
            if d.isExposureModeSupported(.continuousAutoExposure) { d.exposureMode = .continuousAutoExposure }
        }
    }

    private func setAudioLocked(_ on: Bool) {
        if let a = audioInput {
            session.removeInput(a)
            audioInput = nil
        }
        guard on, AVCaptureDevice.authorizationStatus(for: .audio) == .authorized,
              let mic = AVCaptureDevice.default(for: .audio),
              let input = try? AVCaptureDeviceInput(device: mic), session.canAddInput(input) else { return }
        session.addInput(input)
        audioInput = input
    }

    // MARK: - Orientation

    @objc private func deviceOrientationChanged() {
        sessionQueue.async { self.updateOrientationLocked() }
    }

    func setOrientationMode(_ mode: String) {
        sessionQueue.async {
            self.orientationMode = mode
            self.updateOrientationLocked()
        }
    }

    private func updateOrientationLocked() {
        let wanted: AVCaptureVideoOrientation
        switch orientationMode {
        case "portrait": wanted = .portrait
        case "landscape": wanted = .landscapeRight
        default:
            switch UIDevice.current.orientation {
            case .portrait: wanted = .portrait
            case .portraitUpsideDown: wanted = .portraitUpsideDown
            case .landscapeLeft: wanted = .landscapeRight // home button on the right
            case .landscapeRight: wanted = .landscapeLeft
            default: wanted = orientation // face up/down/unknown: keep
            }
        }
        orientation = wanted
        if let conn = videoOutput.connection(with: .video), conn.isVideoOrientationSupported, conn.videoOrientation != wanted {
            conn.videoOrientation = wanted
        }
    }

    // MARK: - Notifications

    @objc private func interrupted(_ n: Notification) {
        var reason = "прервано"
        if let raw = n.userInfo?[AVCaptureSessionInterruptionReasonKey] as? Int,
           let r = AVCaptureSession.InterruptionReason(rawValue: raw) {
            switch r {
            case .videoDeviceNotAvailableInBackground: reason = "приложение свёрнуто (iOS не даёт камеру в фоне)"
            case .audioDeviceInUseByAnotherClient: reason = "микрофон занят (звонок?)"
            case .videoDeviceInUseByAnotherClient: reason = "камера занята другим приложением"
            case .videoDeviceNotAvailableWithMultipleForegroundApps: reason = "камера недоступна в режиме нескольких окон"
            case .videoDeviceNotAvailableDueToSystemPressure: reason = "перегрев: система отключила камеру"
            @unknown default: break
            }
        }
        delegate?.captureInterrupted(reason)
    }

    @objc private func interruptionEnded(_ n: Notification) {
        delegate?.captureResumed()
    }

    @objc private func runtimeError(_ n: Notification) {
        sessionQueue.asyncAfter(deadline: .now() + 0.5) {
            if !self.session.isRunning { self.session.startRunning() }
        }
    }
}

extension CaptureController: AVCaptureVideoDataOutputSampleBufferDelegate, AVCaptureAudioDataOutputSampleBufferDelegate {
    func captureOutput(_ output: AVCaptureOutput, didOutput sampleBuffer: CMSampleBuffer, from connection: AVCaptureConnection) {
        if output === videoOutput {
            guard let pb = CMSampleBufferGetImageBuffer(sampleBuffer) else { return }
            delegate?.captureVideo(pb, pts: CMSampleBufferGetPresentationTimeStamp(sampleBuffer))
        } else if output === audioOutput {
            delegate?.captureAudio(sampleBuffer)
        }
    }
}
