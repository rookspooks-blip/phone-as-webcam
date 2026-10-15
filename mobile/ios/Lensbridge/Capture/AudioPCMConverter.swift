import AVFoundation

/// Converts microphone sample buffers to PCM s16le, 48 kHz, mono.
final class AudioPCMConverter {
    private var converter: AVAudioConverter?
    private var inFormat: AVAudioFormat?
    private var inDesc = AudioStreamBasicDescription()
    let outFormat = AVAudioFormat(commonFormat: .pcmFormatInt16, sampleRate: 48_000, channels: 1, interleaved: true)!

    private static func same(_ a: AudioStreamBasicDescription, _ b: AudioStreamBasicDescription) -> Bool {
        a.mSampleRate == b.mSampleRate && a.mFormatID == b.mFormatID && a.mFormatFlags == b.mFormatFlags
            && a.mChannelsPerFrame == b.mChannelsPerFrame && a.mBitsPerChannel == b.mBitsPerChannel
    }

    func convert(_ sb: CMSampleBuffer) -> Data? {
        guard let fd = CMSampleBufferGetFormatDescription(sb),
              let asbdPtr = CMAudioFormatDescriptionGetStreamBasicDescription(fd) else { return nil }
        var asbd = asbdPtr.pointee
        if converter == nil || !Self.same(asbd, inDesc) {
            guard let f = AVAudioFormat(streamDescription: &asbd) else { return nil }
            inFormat = f
            inDesc = asbd
            converter = AVAudioConverter(from: f, to: outFormat)
        }
        guard let inFormat, let converter else { return nil }
        let frames = AVAudioFrameCount(CMSampleBufferGetNumSamples(sb))
        guard frames > 0, let inBuf = AVAudioPCMBuffer(pcmFormat: inFormat, frameCapacity: frames) else { return nil }
        inBuf.frameLength = frames
        guard CMSampleBufferCopyPCMDataIntoAudioBufferList(sb, at: 0, frameCount: Int32(frames), into: inBuf.mutableAudioBufferList) == noErr else {
            return nil
        }
        let capacity = AVAudioFrameCount(Double(frames) * outFormat.sampleRate / inFormat.sampleRate) + 64
        guard let outBuf = AVAudioPCMBuffer(pcmFormat: outFormat, frameCapacity: capacity) else { return nil }
        var fed = false
        var error: NSError?
        let status = converter.convert(to: outBuf, error: &error) { _, outStatus in
            if fed {
                outStatus.pointee = .noDataNow
                return nil
            }
            fed = true
            outStatus.pointee = .haveData
            return inBuf
        }
        guard status != .error, let ch = outBuf.int16ChannelData, outBuf.frameLength > 0 else { return nil }
        return Data(bytes: ch[0], count: Int(outBuf.frameLength) * MemoryLayout<Int16>.size)
    }
}
