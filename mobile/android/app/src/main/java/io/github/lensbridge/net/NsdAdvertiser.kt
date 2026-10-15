package io.github.lensbridge.net

import android.content.Context
import android.net.nsd.NsdManager
import android.net.nsd.NsdServiceInfo
import io.github.lensbridge.protocol.Lbp

/** Bonjour/mDNS advertisement `_lensbridge._tcp` so the PC finds the phone. */
class NsdAdvertiser(context: Context) {
    private val nsd = context.getSystemService(Context.NSD_SERVICE) as NsdManager
    private var listener: NsdManager.RegistrationListener? = null

    fun register(name: String, installId: String, onState: (String) -> Unit) {
        unregister()
        val info = NsdServiceInfo().apply {
            serviceName = name
            serviceType = Lbp.SERVICE_TYPE
            port = Lbp.PORT
            setAttribute("v", "1")
            setAttribute("name", name)
            setAttribute("id", installId)
        }
        val l = object : NsdManager.RegistrationListener {
            override fun onServiceRegistered(info: NsdServiceInfo) = onState("mDNS: «${info.serviceName}»")
            override fun onRegistrationFailed(info: NsdServiceInfo, errorCode: Int) = onState("mDNS: ошибка $errorCode")
            override fun onServiceUnregistered(info: NsdServiceInfo) {}
            override fun onUnregistrationFailed(info: NsdServiceInfo, errorCode: Int) {}
        }
        listener = l
        nsd.registerService(info, NsdManager.PROTOCOL_DNS_SD, l)
    }

    fun unregister() {
        listener?.let { runCatching { nsd.unregisterService(it) } }
        listener = null
    }
}
