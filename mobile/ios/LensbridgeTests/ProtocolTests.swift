import CryptoKit
import XCTest

final class ProtocolTests: XCTestCase {
    private func hex(_ k: SymmetricKey) -> String { k.withUnsafeBytes { Data($0).hexString } }

    /// Same vector as desktop/crates/lb-proto/src/crypto.rs `known_vector`.
    func testKnownVectorMatchesRust() throws {
        let keys = SessionKeys.derive(psk: Data(repeating: 0x11, count: 32),
                                      nonceC: Data(repeating: 0x22, count: 16),
                                      nonceS: Data(repeating: 0x33, count: 16))
        XCTAssertEqual(hex(keys.c2s), "02dc43b962c49ee48a3e6b7c9405d4b945db4bb5e8d4ef08335798fb12b33a74")
        XCTAssertEqual(hex(keys.s2c), "29516e9412d6237340adba5144441e0f168b470a002b45da13899d9d1daffc1e")
        let ct = try CipherState(key: keys.c2s).seal(Data([0x00, 0x7b, 0x7d]))
        XCTAssertEqual(ct.hexString, "91a3a868663ddef2ed4d030bf67d25d7186a21")
    }

    func testSealOpenRoundtripAndCounterBinding() throws {
        let keys = SessionKeys.derive(psk: Data(repeating: 7, count: 32), nonceC: Data(repeating: 1, count: 16), nonceS: Data(repeating: 2, count: 16))
        let tx = CipherState(key: keys.s2c)
        let a = try tx.seal(Data("first".utf8))
        let b = try tx.seal(Data("second".utf8))
        let rx = CipherState(key: keys.s2c)
        XCTAssertEqual(try rx.open(a), Data("first".utf8))
        XCTAssertEqual(try rx.open(b), Data("second".utf8))
        XCTAssertThrowsError(try CipherState(key: keys.s2c).open(b))
    }

    func testMediaHeaderLayout() {
        let h = MediaHeader(codec: Lbp.codecH264, flags: Lbp.flagKey | Lbp.flagConfig, seq: 0x01020304, ptsUs: 0x1122334455667788)
        let d = h.encode(with: Data([9, 9]))
        XCTAssertEqual(Array(d), [2, 3, 1, 2, 3, 4, 0x11, 0x22, 0x33, 0x44, 0x55, 0x66, 0x77, 0x88, 9, 9])
    }

    func testPairingURI() {
        let key = Data((0..<32).map { UInt8($0) })
        let b64 = key.base64EncodedString().replacingOccurrences(of: "+", with: "-").replacingOccurrences(of: "/", with: "_").replacingOccurrences(of: "=", with: "")
        let uri = PairingURI("lensbridge://pair?v=1&k=\(b64)&n=%D0%9C%D0%BE%D0%B9%20%D0%9F%D0%9A")
        XCTAssertEqual(uri?.key, key)
        XCTAssertEqual(uri?.pcName, "Мой ПК")
        XCTAssertNil(PairingURI("lensbridge://pair?v=2&k=\(b64)"))
        XCTAssertNil(PairingURI("https://example.com"))
        XCTAssertEqual(lbpKeyId(key).count, 8)
    }

    func testReadBE() {
        var d = Data()
        d.appendBE(UInt32(0xDEADBEEF))
        XCTAssertEqual(d.readBE(UInt32.self, at: 0), 0xDEADBEEF)
    }
}
