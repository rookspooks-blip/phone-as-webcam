import CoreMedia
import VideoToolbox

/// Hardware H.264 (VideoToolbox), low-latency rate control, no B-frames,
/// Annex B output with SPS/PPS before every IDR.
final class H264Encoder {
    /// (annexB, isKeyframe, pts) — called on a VideoToolbox thread.
    var onFrame: ((Data, Bool, CMTime) -> Void)?

    private var session: VTCompressionSession?
    private var width: Int32 = 0
    private var height: Int32 = 0
    private let lock = NSLock()
    private var forceKey = true
    private(set) var bitrate = 6_000_000
    private(set) var fps = 30
    private static let startCode: [UInt8] = [0, 0, 0, 1]

    func setBitrate(_ bps: Int) {
        lock.lock(); defer { lock.unlock() }
        bitrate = max(200_000, min(bps, 50_000_000))
        if let s = session {
            VTSessionSetProperty(s, key: kVTCompressionPropertyKey_AverageBitRate, value: bitrate as CFNumber)
        }
    }

    func setFps(_ f: Int) {
        lock.lock(); defer { lock.unlock() }
        fps = max(1, f)
        if let s = session {
            VTSessionSetProperty(s, key: kVTCompressionPropertyKey_ExpectedFrameRate, value: fps as CFNumber)
        }
    }

    func requestKeyframe() {
        lock.lock(); forceKey = true; lock.unlock()
    }

    func invalidate() {
        lock.lock(); defer { lock.unlock() }
        if let s = session {
            VTCompressionSessionInvalidate(s)
        }
        session = nil
        forceKey = true
    }

    private func makeSession(width w: Int32, height h: Int32) -> VTCompressionSession? {
        var s: VTCompressionSession?
        let spec: [CFString: Any] = [kVTVideoEncoderSpecification_EnableLowLatencyRateControl: true]
        let st = VTCompressionSessionCreate(allocator: nil, width: w, height: h, codecType: kCMVideoCodecType_H264,
                                            encoderSpecification: spec as CFDictionary, imageBufferAttributes: nil,
                                            compressedDataAllocator: nil, outputCallback: nil, refcon: nil, compressionSessionOut: &s)
        guard st == noErr, let s else { return nil }
        VTSessionSetProperty(s, key: kVTCompressionPropertyKey_RealTime, value: kCFBooleanTrue)
        // Constrained Baseline: decodable by every software decoder on the PC side.
        VTSessionSetProperty(s, key: kVTCompressionPropertyKey_ProfileLevel, value: kVTProfileLevel_H264_ConstrainedBaseline_AutoLevel)
        VTSessionSetProperty(s, key: kVTCompressionPropertyKey_AllowFrameReordering, value: kCFBooleanFalse)
        VTSessionSetProperty(s, key: kVTCompressionPropertyKey_AverageBitRate, value: bitrate as CFNumber)
        VTSessionSetProperty(s, key: kVTCompressionPropertyKey_ExpectedFrameRate, value: fps as CFNumber)
        VTSessionSetProperty(s, key: kVTCompressionPropertyKey_MaxKeyFrameIntervalDuration, value: 2 as CFNumber)
        VTCompressionSessionPrepareToEncodeFrames(s)
        return s
    }

    func encode(_ pb: CVPixelBuffer, pts: CMTime) {
        let w = Int32(CVPixelBufferGetWidth(pb))
        let h = Int32(CVPixelBufferGetHeight(pb))
        lock.lock()
        if session == nil || w != width || h != height {
            if let s = session { VTCompressionSessionInvalidate(s) }
            session = makeSession(width: w, height: h)
            width = w
            height = h
            forceKey = true
        }
        let key = forceKey
        forceKey = false
        let s = session
        lock.unlock()
        guard let s else { return }
        let props: CFDictionary? = key ? [kVTEncodeFrameOptionKey_ForceKeyFrame: kCFBooleanTrue] as CFDictionary : nil
        VTCompressionSessionEncodeFrame(s, imageBuffer: pb, presentationTimeStamp: pts, duration: .invalid,
                                        frameProperties: props, infoFlagsOut: nil) { [weak self] status, _, sample in
            guard status == noErr, let sample else { return }
            self?.emit(sample)
        }
    }

    private func emit(_ sample: CMSampleBuffer) {
        guard let block = CMSampleBufferGetDataBuffer(sample) else { return }
        var isKey = true
        if let arr = CMSampleBufferGetSampleAttachmentsArray(sample, createIfNecessary: false) as? [[CFString: Any]],
           let notSync = arr.first?[kCMSampleAttachmentKey_NotSync] as? Bool {
            isKey = !notSync
        }
        var out = Data()
        if isKey, let fmt = CMSampleBufferGetFormatDescription(sample) {
            var count = 0
            CMVideoFormatDescriptionGetH264ParameterSetAtIndex(fmt, parameterSetIndex: 0, parameterSetPointerOut: nil,
                                                               parameterSetSizeOut: nil, parameterSetCountOut: &count, nalUnitHeaderLengthOut: nil)
            for i in 0..<count {
                var ptr: UnsafePointer<UInt8>?
                var size = 0
                if CMVideoFormatDescriptionGetH264ParameterSetAtIndex(fmt, parameterSetIndex: i, parameterSetPointerOut: &ptr,
                                                                      parameterSetSizeOut: &size, parameterSetCountOut: nil,
                                                                      nalUnitHeaderLengthOut: nil) == noErr, let ptr {
                    out.append(contentsOf: Self.startCode)
                    out.append(ptr, count: size)
                }
            }
        }
        let total = CMBlockBufferGetDataLength(block)
        var avcc = Data(count: total)
        let st = avcc.withUnsafeMutableBytes { raw -> OSStatus in
            CMBlockBufferCopyDataBytes(block, atOffset: 0, dataLength: total, destination: raw.baseAddress!)
        }
        guard st == noErr else { return }
        // AVCC (4-byte big-endian lengths) → Annex B.
        var i = 0
        while i + 4 <= avcc.count {
            let n = Int(avcc[i]) << 24 | Int(avcc[i + 1]) << 16 | Int(avcc[i + 2]) << 8 | Int(avcc[i + 3])
            i += 4
            guard n > 0, i + n <= avcc.count else { break }
            out.append(contentsOf: Self.startCode)
            out.append(avcc.subdata(in: i ..< i + n))
            i += n
        }
        onFrame?(out, isKey, CMSampleBufferGetPresentationTimeStamp(sample))
    }
}
