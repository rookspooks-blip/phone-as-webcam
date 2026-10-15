import Foundation
import Network

/// Optional plain-HTTP server on 4747: `/` page, `/video` MJPEG, `/info`.
/// Unencrypted by design (for a browser on the LAN), off by default, LAN peers only.
final class HttpServer {
    private let queue = DispatchQueue(label: "http.server")
    private var listener: NWListener?
    private var viewers: [UUID: Viewer] = [:]
    private let lock = NSLock()
    private var viewerCountValue = 0
    var deviceName = "iPhone"
    var onViewersChanged: ((Int) -> Void)?
    var onStateChanged: ((String) -> Void)?

    private final class Viewer {
        let id = UUID()
        let conn: NWConnection
        var inFlight = 0
        init(conn: NWConnection) { self.conn = conn }
    }

    var viewerCount: Int {
        lock.lock(); defer { lock.unlock() }
        return viewerCountValue
    }

    var isRunning: Bool { queue.sync { listener != nil } }

    func start() {
        queue.async {
            guard self.listener == nil else { return }
            do {
                let params = NWParameters.tcp
                params.allowLocalEndpointReuse = true
                let l = try NWListener(using: params, on: NWEndpoint.Port(rawValue: Lbp.httpPort)!)
                l.newConnectionHandler = { [weak self] c in self?.accept(c) }
                l.stateUpdateHandler = { [weak self] st in
                    if case let .failed(e) = st { self?.onStateChanged?("HTTP: ошибка \(e)") }
                    if case .ready = st { self?.onStateChanged?("HTTP: порт \(Lbp.httpPort)") }
                }
                l.start(queue: self.queue)
                self.listener = l
            } catch {
                self.onStateChanged?("HTTP: не удалось открыть порт \(Lbp.httpPort): \(error)")
            }
        }
    }

    func stop() {
        queue.async {
            self.listener?.cancel()
            self.listener = nil
            self.viewers.values.forEach { $0.conn.cancel() }
            self.viewers.removeAll()
            self.updateCount()
        }
    }

    private func updateCount() {
        lock.lock()
        viewerCountValue = viewers.count
        lock.unlock()
        onViewersChanged?(viewers.count)
    }

    private func accept(_ c: NWConnection) {
        guard NetUtil.isLocalPeer(c.endpoint) else { c.cancel(); return }
        c.start(queue: queue)
        readRequest(c, buffer: Data())
    }

    private func readRequest(_ c: NWConnection, buffer: Data) {
        c.receive(minimumIncompleteLength: 1, maximumLength: 8192) { [weak self] data, _, done, err in
            guard let self else { return }
            var buf = buffer
            if let data { buf.append(data) }
            if let end = buf.range(of: Data("\r\n\r\n".utf8)) {
                let head = String(decoding: buf[..<end.lowerBound], as: UTF8.self)
                self.route(c, requestLine: head.components(separatedBy: "\r\n").first ?? "")
            } else if err != nil || done || buf.count > 8192 {
                c.cancel()
            } else {
                self.readRequest(c, buffer: buf)
            }
        }
    }

    private func route(_ c: NWConnection, requestLine: String) {
        let parts = requestLine.split(separator: " ")
        guard parts.count >= 2, parts[0] == "GET" else { respond(c, status: "405 Method Not Allowed", type: "text/plain", body: Data("method".utf8)); return }
        let path = parts[1].split(separator: "?").first.map(String.init) ?? "/"
        switch path {
        case "/":
            let html = """
            <!doctype html><html><head><meta charset="utf-8"><meta name="viewport" content="width=device-width">
            <title>Lensbridge — \(deviceName)</title>
            <style>body{margin:0;background:#111;color:#ccc;font-family:sans-serif}img{width:100vw;height:calc(100vh - 2em);object-fit:contain}p{margin:.3em}</style>
            </head><body><p>Lensbridge · \(deviceName) · MJPEG без шифрования (только локальная сеть)</p><img src="/video"></body></html>
            """
            respond(c, status: "200 OK", type: "text/html; charset=utf-8", body: Data(html.utf8))
        case "/info":
            let obj: [String: Any] = ["proto": Lbp.protoVersion, "device": deviceName, "name": deviceName]
            respond(c, status: "200 OK", type: "application/json", body: (try? JSONSerialization.data(withJSONObject: obj)) ?? Data())
        case "/video":
            if viewers.count >= 3 {
                respond(c, status: "503 Service Unavailable", type: "text/plain", body: Data("too many viewers".utf8))
                return
            }
            let header = "HTTP/1.1 200 OK\r\nContent-Type: multipart/x-mixed-replace; boundary=lbframe\r\nCache-Control: no-store\r\nConnection: close\r\n\r\n"
            c.send(content: Data(header.utf8), completion: .contentProcessed { _ in })
            let v = Viewer(conn: c)
            viewers[v.id] = v
            c.stateUpdateHandler = { [weak self] st in
                switch st {
                case .failed, .cancelled:
                    self?.queue.async {
                        self?.viewers[v.id] = nil
                        self?.updateCount()
                    }
                default: break
                }
            }
            updateCount()
        default:
            respond(c, status: "404 Not Found", type: "text/plain", body: Data("not found".utf8))
        }
    }

    private func respond(_ c: NWConnection, status: String, type: String, body: Data) {
        var d = Data("HTTP/1.1 \(status)\r\nContent-Type: \(type)\r\nContent-Length: \(body.count)\r\nConnection: close\r\n\r\n".utf8)
        d.append(body)
        c.send(content: d, completion: .contentProcessed { _ in c.cancel() })
    }

    /// Sends one JPEG to every viewer; slow viewers skip frames.
    func broadcast(jpeg: Data, seq: UInt32, ptsUs: UInt64) {
        queue.async {
            for v in self.viewers.values where v.inFlight < 2 {
                var part = Data("--lbframe\r\nContent-Type: image/jpeg\r\nContent-Length: \(jpeg.count)\r\nX-LB-Seq: \(seq)\r\nX-LB-PTS: \(ptsUs)\r\n\r\n".utf8)
                part.append(jpeg)
                part.append(Data("\r\n".utf8))
                v.inFlight += 1
                v.conn.send(content: part, completion: .contentProcessed { [weak self] err in
                    self?.queue.async {
                        v.inFlight -= 1
                        if err != nil { v.conn.cancel() }
                    }
                })
            }
        }
    }
}
