import Foundation
import Network

/// One client connection speaking lbp/1: handshake, encrypted frames,
/// send-queue backpressure and idle timeout.
final class LbpSession {
    let id = UUID()
    let connection: NWConnection
    private let queue: DispatchQueue

    private var sendCipher: CipherState?
    private var recvCipher: CipherState?
    private var gotHello = false
    private var closed = false
    private var lastRx = Date()
    private var idleTimer: DispatchSourceTimer?

    private let lock = NSLock()
    private var pendingBytes = 0
    private var bytesSentWindow = 0

    private(set) var clientName = "ПК"
    private(set) var keyId = Data()
    var peerDescription: String { "\(connection.endpoint)" }

    /// Called on the session queue.
    var onHello: ((LbpSession, [String: Any]) -> Void)?
    var onControl: ((LbpSession, [String: Any]) -> Void)?
    var onClosed: ((LbpSession, String) -> Void)?

    init(connection: NWConnection) {
        self.connection = connection
        self.queue = DispatchQueue(label: "lbp.session.\(UUID().uuidString.prefix(8))", qos: .userInitiated)
    }

    /// `lookup` returns the pairing key for a key id; `isBusy` tells if another session streams.
    func start(lookup: @escaping (Data) -> Data?, isBusy: @escaping (Data) -> Bool) {
        connection.stateUpdateHandler = { [weak self] state in
            switch state {
            case let .failed(err): self?.close(reason: "сбой соединения: \(err)")
            case .cancelled: self?.close(reason: "соединение закрыто")
            default: break
            }
        }
        connection.start(queue: queue)
        receiveExactly(Lbp.clientHelloLen) { [weak self] data in
            guard let self, let data else { self?.close(reason: "нет handshake"); return }
            self.handleClientHello(data, lookup: lookup, isBusy: isBusy)
        }
        let timer = DispatchSource.makeTimerSource(queue: queue)
        timer.schedule(deadline: .now() + 1, repeating: 1)
        timer.setEventHandler { [weak self] in
            guard let self else { return }
            if Date().timeIntervalSince(self.lastRx) > 6 {
                self.close(reason: "клиент не отвечает")
            }
        }
        timer.resume()
        idleTimer = timer
    }

    private func handleClientHello(_ d: Data, lookup: (Data) -> Data?, isBusy: (Data) -> Bool) {
        guard d.prefix(4) == Lbp.magic else {
            send(raw: LbpFrame.serverHello(status: .unsupported, nonceS: Data()))
            close(reason: "неизвестный протокол")
            return
        }
        let keyId = d.subdata(in: d.startIndex + 4 ..< d.startIndex + 12)
        let nonceC = d.subdata(in: d.startIndex + 12 ..< d.startIndex + 28)
        guard let psk = lookup(keyId) else {
            send(raw: LbpFrame.serverHello(status: .unknownKey, nonceS: Data()))
            close(reason: "ПК не сопряжён (ключ \(keyId.hexString))")
            return
        }
        self.keyId = keyId
        if isBusy(keyId) {
            send(raw: LbpFrame.serverHello(status: .busy, nonceS: Data()))
            close(reason: "занято другим ПК")
            return
        }
        var nonceS = Data(count: 16)
        _ = nonceS.withUnsafeMutableBytes { SecRandomCopyBytes(kSecRandomDefault, 16, $0.baseAddress!) }
        send(raw: LbpFrame.serverHello(status: .ok, nonceS: nonceS))
        let keys = SessionKeys.derive(psk: psk, nonceC: nonceC, nonceS: nonceS)
        sendCipher = CipherState(key: keys.s2c)
        recvCipher = CipherState(key: keys.c2s)
        readFrame()
    }

    private func readFrame() {
        receiveExactly(4) { [weak self] lenData in
            guard let self, let lenData else { self?.close(reason: "соединение закрыто клиентом"); return }
            let len = Int(lenData.readBE(UInt32.self, at: 0))
            guard len >= 1 + Lbp.tagLen, len <= Lbp.maxFrameLen else { self.close(reason: "неверная длина кадра"); return }
            self.receiveExactly(len) { [weak self] body in
                guard let self, let body, let cipher = self.recvCipher else { self?.close(reason: "обрыв кадра"); return }
                do {
                    let pt = try cipher.open(body)
                    self.lastRx = Date()
                    self.handlePacket(pt)
                    if !self.closed { self.readFrame() }
                } catch {
                    self.close(reason: "ошибка расшифровки")
                }
            }
        }
    }

    private func handlePacket(_ pt: Data) {
        guard let typeByte = pt.first, PacketType(rawValue: typeByte) == .control else { return }
        let payload = pt.dropFirst()
        guard let obj = (try? JSONSerialization.jsonObject(with: payload)) as? [String: Any] else { return }
        if !gotHello {
            guard obj["t"] as? String == "hello" else { close(reason: "первое сообщение не hello"); return }
            gotHello = true
            clientName = obj["name"] as? String ?? "ПК"
            onHello?(self, obj)
            return
        }
        onControl?(self, obj)
    }

    private func receiveExactly(_ n: Int, _ done: @escaping (Data?) -> Void) {
        connection.receive(minimumIncompleteLength: n, maximumLength: n) { data, _, _, error in
            if let data, data.count == n, error == nil {
                done(data)
            } else {
                done(nil)
            }
        }
    }

    private func send(raw: Data) {
        connection.send(content: raw, completion: .contentProcessed { _ in })
    }

    // MARK: - Sending (thread-safe)

    var queuedBytes: Int {
        lock.lock(); defer { lock.unlock() }
        return pendingBytes
    }

    /// Takes and resets the number of bytes handed to the network since the last call.
    func takeSentBytes() -> Int {
        lock.lock(); defer { lock.unlock() }
        let b = bytesSentWindow
        bytesSentWindow = 0
        return b
    }

    func sendJSON(_ obj: [String: Any]) {
        guard let data = try? JSONSerialization.data(withJSONObject: obj) else { return }
        enqueue(type: .control, payload: data)
    }

    /// Returns false (and sends nothing) when `droppable` and the queue is over `limit` bytes.
    @discardableResult
    func sendMedia(type: PacketType, header: MediaHeader, payload: Data, droppable: Bool, limit: Int) -> Bool {
        if droppable && queuedBytes > limit { return false }
        enqueue(type: type, payload: header.encode(with: payload))
        return true
    }

    private func enqueue(type: PacketType, payload: Data) {
        lock.lock()
        pendingBytes += payload.count
        lock.unlock()
        queue.async { [weak self] in
            guard let self, !self.closed, let cipher = self.sendCipher else { return }
            guard let frame = try? LbpFrame.build(type: type, payload: payload, cipher: cipher) else { return }
            let size = payload.count
            self.connection.send(content: frame, completion: .contentProcessed { [weak self] error in
                guard let self else { return }
                self.lock.lock()
                self.pendingBytes -= size
                self.bytesSentWindow += frame.count
                self.lock.unlock()
                if let error { self.close(reason: "ошибка отправки: \(error)") }
            })
        }
    }

    func close(reason: String) {
        queue.async { [weak self] in
            guard let self, !self.closed else { return }
            self.closed = true
            self.idleTimer?.cancel()
            self.connection.cancel()
            self.onClosed?(self, reason)
        }
    }
}
