import Foundation
import Network

protocol LbpServerDelegate: AnyObject {
    /// Called after the client proved its key with `hello`.
    func server(_ server: LbpServer, sessionStarted session: LbpSession, hello: [String: Any])
    func server(_ server: LbpServer, session: LbpSession, control: [String: Any])
    func server(_ server: LbpServer, sessionEnded session: LbpSession, reason: String)
    func server(_ server: LbpServer, listenerState: String)
}

/// TCP listener on 4748 with Bonjour advertisement. One active session.
final class LbpServer {
    weak var delegate: LbpServerDelegate?
    private let queue = DispatchQueue(label: "lbp.server")
    private var listener: NWListener?
    private var sessions: [UUID: LbpSession] = [:]
    private(set) var active: LbpSession?
    private let keyLookup: (Data) -> Data?
    private var restartWork: DispatchWorkItem?

    init(keyLookup: @escaping (Data) -> Data?) {
        self.keyLookup = keyLookup
    }

    func start(deviceName: String, installId: String) {
        queue.async { self.startLocked(deviceName: deviceName, installId: installId) }
    }

    private func startLocked(deviceName: String, installId: String) {
        guard listener == nil else { return }
        let tcp = NWProtocolTCP.Options()
        tcp.noDelay = true
        tcp.enableKeepalive = true
        tcp.keepaliveIdle = 5
        let params = NWParameters(tls: nil, tcp: tcp)
        params.allowLocalEndpointReuse = true
        params.includePeerToPeer = false
        do {
            let l = try NWListener(using: params, on: NWEndpoint.Port(rawValue: Lbp.port)!)
            let txt = NWTXTRecord(["v": "1", "name": deviceName, "id": installId])
            l.service = NWListener.Service(name: deviceName, type: Lbp.serviceType, domain: nil, txtRecord: txt)
            l.stateUpdateHandler = { [weak self] state in
                guard let self else { return }
                switch state {
                case .ready:
                    self.delegate?.server(self, listenerState: "ожидание ПК на порту \(Lbp.port)")
                case let .failed(err):
                    self.delegate?.server(self, listenerState: "ошибка сервера: \(err)")
                    self.scheduleRestart(deviceName: deviceName, installId: installId)
                case let .waiting(err):
                    self.delegate?.server(self, listenerState: "сеть недоступна: \(err)")
                default:
                    break
                }
            }
            l.newConnectionHandler = { [weak self] conn in self?.accept(conn) }
            l.start(queue: queue)
            listener = l
        } catch {
            delegate?.server(self, listenerState: "не удалось открыть порт \(Lbp.port): \(error)")
            scheduleRestart(deviceName: deviceName, installId: installId)
        }
    }

    private func scheduleRestart(deviceName: String, installId: String) {
        listener?.cancel()
        listener = nil
        restartWork?.cancel()
        let work = DispatchWorkItem { [weak self] in self?.startLocked(deviceName: deviceName, installId: installId) }
        restartWork = work
        queue.asyncAfter(deadline: .now() + 2, execute: work)
    }

    func stop() {
        queue.async {
            self.restartWork?.cancel()
            self.listener?.cancel()
            self.listener = nil
            self.sessions.values.forEach { $0.close(reason: "сервер остановлен") }
        }
    }

    /// Restart (e.g. after returning from background, where sockets may die).
    func restart(deviceName: String, installId: String) {
        queue.async {
            self.listener?.cancel()
            self.listener = nil
            self.startLocked(deviceName: deviceName, installId: installId)
        }
    }

    private func accept(_ conn: NWConnection) {
        guard NetUtil.isLocalPeer(conn.endpoint) else {
            conn.cancel()
            return
        }
        let s = LbpSession(connection: conn)
        sessions[s.id] = s
        s.onHello = { [weak self] session, hello in
            guard let self else { return }
            self.queue.async {
                self.active = session
                self.delegate?.server(self, sessionStarted: session, hello: hello)
            }
        }
        s.onControl = { [weak self] session, msg in
            guard let self else { return }
            self.delegate?.server(self, session: session, control: msg)
        }
        s.onClosed = { [weak self] session, reason in
            guard let self else { return }
            self.queue.async {
                self.sessions[session.id] = nil
                let wasActive = self.active?.id == session.id
                if wasActive { self.active = nil }
                if wasActive { self.delegate?.server(self, sessionEnded: session, reason: reason) }
            }
        }
        s.start(lookup: keyLookup, isBusy: { [weak self] keyId in
            guard let self else { return false }
            return self.queue.sync {
                guard let current = self.active else { return false }
                if current.keyId == keyId {
                    // Same PC reconnecting (e.g. after a network blip): replace the stale session.
                    self.active = nil
                    current.close(reason: "переподключение того же ПК")
                    return false
                }
                return true
            }
        })
    }
}
