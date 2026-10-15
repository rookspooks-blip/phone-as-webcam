package io.github.lensbridge.net

import io.github.lensbridge.protocol.Lbp
import java.io.IOException
import java.io.OutputStream
import java.net.InetSocketAddress
import java.net.ServerSocket
import java.net.Socket
import java.util.concurrent.CopyOnWriteArrayList
import kotlin.concurrent.thread

/**
 * Optional plain-HTTP server on 4747: `/` page, `/video` MJPEG, `/info`.
 * Unencrypted by design (browser on the LAN), off by default, LAN peers only.
 */
class HttpServer(private val deviceName: () -> String, private val onViewers: (Int) -> Unit) {
    private class Viewer(val socket: Socket) {
        private val lock = Object()
        private var frame: Triple<ByteArray, Int, Long>? = null
        @Volatile var alive = true

        fun offer(jpeg: ByteArray, seq: Int, pts: Long) = synchronized(lock) {
            frame = Triple(jpeg, seq, pts) // newest wins: slow viewers skip frames
            lock.notifyAll()
        }

        fun take(): Triple<ByteArray, Int, Long>? = synchronized(lock) {
            while (alive && frame == null) lock.wait(1000)
            frame.also { frame = null }
        }

        fun stop() = synchronized(lock) {
            alive = false
            lock.notifyAll()
        }
    }

    private val viewers = CopyOnWriteArrayList<Viewer>()
    @Volatile private var running = false
    private var ss: ServerSocket? = null

    val viewerCount: Int get() = viewers.size

    fun start() {
        if (running) return
        running = true
        thread(name = "http-accept", isDaemon = true) {
            try {
                val s = ServerSocket()
                s.reuseAddress = true
                s.bind(InetSocketAddress(Lbp.HTTP_PORT))
                ss = s
                while (running) {
                    val c = s.accept()
                    if (!LbpServer.isLocal(c.inetAddress)) {
                        c.close()
                        continue
                    }
                    thread(name = "http-conn", isDaemon = true) { handle(c) }
                }
            } catch (_: IOException) {
            }
        }
    }

    fun stop() {
        running = false
        try {
            ss?.close()
        } catch (_: IOException) {
        }
        viewers.forEach { it.stop(); runCatching { it.socket.close() } }
        viewers.clear()
        onViewers(0)
    }

    fun broadcast(jpeg: ByteArray, seq: Int, ptsUs: Long) {
        viewers.forEach { it.offer(jpeg, seq, ptsUs) }
    }

    private fun handle(c: Socket) {
        try {
            c.soTimeout = 10_000
            val reader = c.getInputStream().bufferedReader()
            val requestLine = reader.readLine() ?: return
            while (true) {
                val l = reader.readLine() ?: break
                if (l.isEmpty()) break
            }
            val parts = requestLine.split(" ")
            val path = parts.getOrNull(1)?.substringBefore('?') ?: "/"
            val out = c.getOutputStream()
            when {
                parts.firstOrNull() != "GET" -> respond(out, "405 Method Not Allowed", "text/plain", "method".toByteArray())
                path == "/" -> respond(out, "200 OK", "text/html; charset=utf-8", page().toByteArray())
                path == "/info" -> respond(out, "200 OK", "application/json", """{"proto":${Lbp.PROTO},"device":"${deviceName()}"}""".toByteArray())
                path == "/video" -> stream(c, out)
                else -> respond(out, "404 Not Found", "text/plain", "not found".toByteArray())
            }
        } catch (_: IOException) {
        } finally {
            runCatching { c.close() }
        }
    }

    private fun page() = """<!doctype html><html><head><meta charset="utf-8"><meta name="viewport" content="width=device-width">
<title>Lensbridge</title><style>body{margin:0;background:#111;color:#ccc;font-family:sans-serif}img{width:100vw;height:calc(100vh - 2em);object-fit:contain}p{margin:.3em}</style>
</head><body><p>Lensbridge · ${deviceName()} · MJPEG без шифрования (только локальная сеть)</p><img src="/video"></body></html>"""

    private fun respond(out: OutputStream, status: String, type: String, body: ByteArray) {
        out.write("HTTP/1.1 $status\r\nContent-Type: $type\r\nContent-Length: ${body.size}\r\nConnection: close\r\n\r\n".toByteArray())
        out.write(body)
        out.flush()
    }

    private fun stream(c: Socket, out: OutputStream) {
        if (viewers.size >= 3) {
            respond(out, "503 Service Unavailable", "text/plain", "too many viewers".toByteArray())
            return
        }
        out.write("HTTP/1.1 200 OK\r\nContent-Type: multipart/x-mixed-replace; boundary=lbframe\r\nCache-Control: no-store\r\nConnection: close\r\n\r\n".toByteArray())
        out.flush()
        c.soTimeout = 0
        val v = Viewer(c)
        viewers.add(v)
        onViewers(viewers.size)
        try {
            while (running && v.alive) {
                val (jpeg, seq, pts) = v.take() ?: continue
                out.write("--lbframe\r\nContent-Type: image/jpeg\r\nContent-Length: ${jpeg.size}\r\nX-LB-Seq: $seq\r\nX-LB-PTS: $pts\r\n\r\n".toByteArray())
                out.write(jpeg)
                out.write("\r\n".toByteArray())
                out.flush()
            }
        } finally {
            v.stop()
            viewers.remove(v)
            onViewers(viewers.size)
        }
    }
}
