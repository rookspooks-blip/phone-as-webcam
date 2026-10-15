import Foundation
import Security

struct PairedPC: Codable, Identifiable, Equatable {
    var id: String { keyIdHex }
    let keyIdHex: String
    let key: Data
    var name: String
    let added: Date
}

/// Paired PCs (their pre-shared keys) stored in the Keychain, this device only.
final class PairingStore: ObservableObject {
    @Published private(set) var pcs: [PairedPC] = []
    private let lock = NSLock()
    private var byKeyId: [Data: Data] = [:]
    private static let service = "io.github.lensbridge.pairing"
    private static let account = "paired-pcs"

    init() {
        load()
    }

    /// Thread-safe lookup used by the network layer.
    func key(forKeyId id: Data) -> Data? {
        lock.lock(); defer { lock.unlock() }
        return byKeyId[id]
    }

    func add(key: Data, name: String) {
        let idHex = lbpKeyId(key).hexString
        var list = pcs.filter { $0.keyIdHex != idHex }
        list.append(PairedPC(keyIdHex: idHex, key: key, name: name, added: Date()))
        save(list)
    }

    func remove(_ pc: PairedPC) {
        save(pcs.filter { $0 != pc })
    }

    private func reindex() {
        lock.lock()
        byKeyId = Dictionary(pcs.map { (lbpKeyId($0.key), $0.key) }, uniquingKeysWith: { a, _ in a })
        lock.unlock()
    }

    private func load() {
        let q: [String: Any] = [
            kSecClass as String: kSecClassGenericPassword,
            kSecAttrService as String: Self.service,
            kSecAttrAccount as String: Self.account,
            kSecReturnData as String: true,
            kSecMatchLimit as String: kSecMatchLimitOne,
        ]
        var out: CFTypeRef?
        if SecItemCopyMatching(q as CFDictionary, &out) == errSecSuccess, let data = out as? Data,
           let list = try? JSONDecoder().decode([PairedPC].self, from: data) {
            pcs = list
        }
        reindex()
    }

    private func save(_ list: [PairedPC]) {
        guard let data = try? JSONEncoder().encode(list) else { return }
        let base: [String: Any] = [
            kSecClass as String: kSecClassGenericPassword,
            kSecAttrService as String: Self.service,
            kSecAttrAccount as String: Self.account,
        ]
        SecItemDelete(base as CFDictionary)
        var add = base
        add[kSecValueData as String] = data
        add[kSecAttrAccessible as String] = kSecAttrAccessibleAfterFirstUnlockThisDeviceOnly
        SecItemAdd(add as CFDictionary, nil)
        pcs = list
        reindex()
    }
}
