package io.github.lensbridge.net

import io.github.lensbridge.protocol.Lbp
import org.json.JSONObject
import java.io.IOException
import java.net.Inet6Address
import java.net.InetAddress
import java.net.InetSocketAddress
import java.net.ServerSocket
import kotlin.concurrent.thread

/** TCP server on 4748 (all interfaces: Wi-Fi, hotspot, and localhost for `adb forward`). */
class LbpServer(private val lookup: (ByteArray) -> ByteArray?, private val listener: Listener) : LbpSession.Listener {
    interface Listener {
        fun onSessionStarted(session: LbpSession, hello: JSONObject)
        fun onSessionControl(session: LbpSession, msg: JSONObject)
        fun onSessionEnded(session: LbpSession, reason: String)
        fun onServerState(text: String)
    }

    @Volatile private var running = false
    private var serverSocket: ServerSocket? = null
    private var active: LbpSession? = null

    val activeSession: LbpSession? get() = synchronized(this) { active }

    fun start() {
        if (running) return
        running = true
        thread(name = "lbp-accept", isDaemon = true) {
            while (running) {
                try {
                    val ss = ServerSocket()
                    ss.reuseAddress = true
                    ss.bind(InetSocketAddress(Lbp.PORT))
                    serverSocket = ss
                    listener.onServerState("ожидание ПК на порту ${Lbp.PORT}")
                    while (running) {
                        val s = ss.accept()
                        if (!isLocal(s.inetAddress)) {
                            s.close()
                            continue
                        }
                        LbpSession(s, lookup, ::isBusy, this).start()
                    }
                } catch (e: IOException) {
                    if (running) {
                        listener.onServerState("сервер: ${e.message}, повтор…")
                        Thread.sleep(2000)
                    }
                }
            }
        }
    }

    fun stop() {
        running = false
        try {
            serverSocket?.close()
        } catch (_: IOException) {
        }
        activeSession?.close("сервер остановлен")
    }

    private fun isBusy(keyId: ByteArray, session: LbpSession): Boolean = synchronized(this) {
        val cur = active ?: return false
        if (cur.keyId.contentEquals(keyId)) {
            // Same PC reconnecting after a network blip: drop the stale session.
            active = null
            cur.close("переподключение того же ПК")
            return false
        }
        true
    }

    override fun onHello(session: LbpSession, hello: JSONObject) {
        synchronized(this) { active = session }
        listener.onSessionStarted(session, hello)
    }

    override fun onControl(session: LbpSession, msg: JSONObject) = listener.onSessionControl(session, msg)

    override fun onClosed(session: LbpSession, reason: String) {
        val wasActive = synchronized(this) {
            if (active === session) {
                active = null
                true
            } else false
        }
        if (wasActive) listener.onSessionEnded(session, reason)
    }

    companion object {
        /** LAN, link-local, loopback (adb/USB) and CGNAT hotspot ranges only. */
        fun isLocal(a: InetAddress?): Boolean {
            if (a == null) return false
            if (a.isLoopbackAddress || a.isSiteLocalAddress || a.isLinkLocalAddress) return true
            val b = a.address
            if (a is Inet6Address) return (b[0].toInt() and 0xfe) == 0xfc
            return b.size == 4 && (b[0].toInt() and 0xff) == 100 && (b[1].toInt() and 0xc0) == 64
        }
    }
}
