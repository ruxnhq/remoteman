package com.ruxn.remoteman

import android.app.Activity
import android.net.nsd.NsdManager
import android.net.nsd.NsdServiceInfo
import android.os.Bundle
import android.util.Log

/**
 * Debug builds only: advertise a pretend host, as a real one does (docs/PROTOCOL.md, "Finding
 * a host"), for as long as the app's process lives. Nothing listens on its port.
 *
 *   adb shell am start -n com.ruxn.remoteman/.FakeHostActivity --es name "Test desk" \
 *       --es fp 0123456789abcdef --es os linux --es mac aa:bb:cc:dd:ee:ff --ei port 7800
 *   adb shell am start -n com.ruxn.remoteman/.FakeHostActivity --es stop "Test desk"
 */
class FakeHostActivity : Activity() {
    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        val nsd = getSystemService(NsdManager::class.java)
        intent.getStringExtra("stop")?.let { name ->
            registered.remove(name)?.let { runCatching { nsd.unregisterService(it) } }
            Log.i(TAG, "fake host: withdrew $name")
            finish()
            return
        }
        val name = intent.getStringExtra("name") ?: "Fake host"
        val info = NsdServiceInfo().apply {
            serviceName = name
            serviceType = Advert.TYPE
            port = intent.getIntExtra("port", 7800)
            setAttribute("v", "0")
            intent.getStringExtra("fp")?.let { setAttribute("fp", it) }
            setAttribute("os", intent.getStringExtra("os") ?: "linux")
            intent.getStringExtra("mac")?.let { setAttribute("mac", it) }
        }
        val l = object : NsdManager.RegistrationListener {
            override fun onServiceRegistered(info: NsdServiceInfo) { Log.i(TAG, "fake host: advertising ${info.serviceName}") }
            override fun onRegistrationFailed(info: NsdServiceInfo, error: Int) { Log.w(TAG, "fake host: failed ($error)") }
            override fun onServiceUnregistered(info: NsdServiceInfo) {}
            override fun onUnregistrationFailed(info: NsdServiceInfo, error: Int) {}
        }
        registered.remove(name)?.let { runCatching { nsd.unregisterService(it) } }
        registered[name] = l
        nsd.registerService(info, NsdManager.PROTOCOL_DNS_SD, l)
        finish()
    }

    companion object {
        const val TAG = "remoteman"
        private val registered = mutableMapOf<String, NsdManager.RegistrationListener>()
    }
}
