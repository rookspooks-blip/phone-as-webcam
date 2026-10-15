// Lensbridge protocol lbp/1 (see docs/PROTOCOL.md). Pure Foundation/CryptoKit,
// shared by the app and the unit tests.

import CoreMedia
import CryptoKit
import Foundation

enum Lbp {
    static let magic = Data("LBP1".utf8)
    static let port: UInt16 = 4748
    static let httpPort: UInt16 = 4747
    static let serviceType = "_lensbridge._tcp"
    static let protoVersion = 1
    static let maxFrameLen = 8 * 1024 * 1024
    static let clientHelloLen = 28
    static let serverHelloLen = 21
    static let tagLen = 16

    static let codecH264: UInt8 = 2
    static let codecPCM: UInt8 = 16
    static let flagKey: UInt8 = 1 << 0
    static let flagConfig: UInt8 = 1 << 1
    static let flagDiscont: UInt8 = 1 << 2
}

enum LbpError: Error {
    case malformed(String)
    case badMagic
    case decrypt
}

enum HandshakeStatus: UInt8 {
    case ok = 0
    case unknownKey = 1
    case busy = 2
    case unsupported = 3
}

enum PacketType: UInt8 {
    case control = 0
    case video = 1
    case audio = 2
}

/// First 8 bytes of SHA-256(K).
func lbpKeyId(_ key: Data) -> Data {
    Data(SHA256.hash(data: key).prefix(8))
}

extension Data {
    var hexString: String { map { String(format: "%02x", $0) }.joined() }

    init?(base64URL s: String) {
        var b = s.replacingOccurrences(of: "-", with: "+").replacingOccurrences(of: "_", with: "/")
        while b.count % 4 != 0 { b += "=" }
        self.init(base64Encoded: b)
    }

    mutating func appendBE<T: FixedWidthInteger>(_ v: T) {
        var be = v.bigEndian
        Swift.withUnsafeBytes(of: &be) { append(contentsOf: $0) }
    }

    func readBE<T: FixedWidthInteger>(_: T.Type, at offset: Int) -> T {
        var v: T = 0
        for i in 0..<MemoryLayout<T>.size {
            v = (v << 8) | T(self[self.startIndex + offset + i])
        }
        return v
    }
}

struct SessionKeys {
    let c2s: SymmetricKey
    let s2c: SymmetricKey

    static func derive(psk: Data, nonceC: Data, nonceS: Data) -> SessionKeys {
        let salt = nonceC + nonceS
        let ikm = SymmetricKey(data: psk)
        let c2s = HKDF<SHA256>.deriveKey(inputKeyMaterial: ikm, salt: salt, info: Data("lbp1 c2s".utf8), outputByteCount: 32)
        let s2c = HKDF<SHA256>.deriveKey(inputKeyMaterial: ikm, salt: salt, info: Data("lbp1 s2c".utf8), outputByteCount: 32)
        return SessionKeys(c2s: c2s, s2c: s2c)
    }
}

/// ChaCha20-Poly1305 for one direction; nonce = 0u32 || counter u64 BE.
final class CipherState {
    private let key: SymmetricKey
    private var counter: UInt64 = 0

    init(key: SymmetricKey) { self.key = key }

    private func nextNonce() throws -> ChaChaPoly.Nonce {
        var n = Data(count: 4)
        n.appendBE(counter)
        counter += 1
        return try ChaChaPoly.Nonce(data: n)
    }

    func seal(_ plaintext: Data) throws -> Data {
        let box = try ChaChaPoly.seal(plaintext, using: key, nonce: try nextNonce())
        return box.ciphertext + box.tag
    }

    func open(_ data: Data) throws -> Data {
        guard data.count >= Lbp.tagLen else { throw LbpError.malformed("frame shorter than tag") }
        let nonce = try nextNonce()
        let ct = data.prefix(data.count - Lbp.tagLen)
        let tag = data.suffix(Lbp.tagLen)
        do {
            let box = try ChaChaPoly.SealedBox(nonce: nonce, ciphertext: ct, tag: tag)
            return try ChaChaPoly.open(box, using: key)
        } catch {
            throw LbpError.decrypt
        }
    }
}

struct MediaHeader {
    var codec: UInt8
    var flags: UInt8
    var seq: UInt32
    var ptsUs: UInt64

    func encode(with payload: Data) -> Data {
        var d = Data(capacity: 14 + payload.count)
        d.append(codec)
        d.append(flags)
        d.appendBE(seq)
        d.appendBE(ptsUs)
        d.append(payload)
        return d
    }
}

enum LbpFrame {
    /// Builds `len u32 BE | seal(type | payload)`.
    static func build(type: PacketType, payload: Data, cipher: CipherState) throws -> Data {
        var pt = Data(capacity: payload.count + 1)
        pt.append(type.rawValue)
        pt.append(payload)
        let ct = try cipher.seal(pt)
        var out = Data(capacity: ct.count + 4)
        out.appendBE(UInt32(ct.count))
        out.append(ct)
        return out
    }

    static func serverHello(status: HandshakeStatus, nonceS: Data) -> Data {
        var d = Lbp.magic
        d.append(status.rawValue)
        d.append(nonceS.count == 16 ? nonceS : Data(count: 16))
        return d
    }
}

struct PairingURI {
    let key: Data
    let pcName: String

    /// `lensbridge://pair?v=1&k=<base64url(K)>&n=<name>`
    init?(_ string: String) {
        guard let comps = URLComponents(string: string.trimmingCharacters(in: .whitespacesAndNewlines)),
              comps.scheme == "lensbridge", comps.host == "pair" else { return nil }
        let items = comps.queryItems ?? []
        guard items.first(where: { $0.name == "v" })?.value == "1",
              let k = items.first(where: { $0.name == "k" })?.value,
              let key = Data(base64URL: k), key.count == 32 else { return nil }
        self.key = key
        self.pcName = items.first(where: { $0.name == "n" })?.value ?? "ПК"
    }
}

/// Host time clock in microseconds: the same clock as AVCaptureSession timestamps.
func hostTimeMicros() -> UInt64 {
    let t = CMClockGetTime(CMClockGetHostTimeClock())
    return cmTimeMicros(t)
}

func cmTimeMicros(_ t: CMTime) -> UInt64 {
    guard t.isValid, t.timescale > 0 else { return 0 }
    let us = CMTimeConvertScale(t, timescale: 1_000_000, method: .roundHalfAwayFromZero).value
    return us > 0 ? UInt64(us) : 0
}
