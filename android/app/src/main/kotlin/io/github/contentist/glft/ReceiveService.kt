package io.github.contentist.glft

import android.app.Service
import android.content.Intent
import android.content.pm.ServiceInfo
import android.net.ConnectivityManager
import android.net.LinkProperties
import android.net.Network
import android.net.wifi.WifiManager
import android.os.IBinder
import android.util.Log
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.SupervisorJob
import kotlinx.coroutines.cancel
import kotlinx.coroutines.launch
import uniffi.glft_ffi.GlftException

/**
 * Receiving in the background: a dataSync foreground service whose notification is
 * the visible sign that glft can receive. Stopping the service closes the socket.
 *
 * Android allows dataSync services 6 hours in any 24; [onTimeout] ends receiving
 * cleanly and says why.
 */
class ReceiveService : Service() {
    private val scope = CoroutineScope(SupervisorJob() + Dispatchers.IO)
    private var multicast: WifiManager.MulticastLock? = null
    private var started = false
    private var watcher: ConnectivityManager.NetworkCallback? = null
    /** The addresses receiving started with; a change means devices can't find this phone. */
    @Volatile private var addresses: Set<String>? = null

    override fun onBind(intent: Intent?): IBinder? = null

    override fun onStartCommand(intent: Intent?, flags: Int, startId: Int): Int {
        if (intent?.action == ACTION_STOP) {
            stopSelf()
            return START_NOT_STICKY
        }
        val app = glft
        if (started) return START_NOT_STICKY
        started = true
        startForeground(
            Notifications.RECEIVING_ID,
            Notifications.receiving(this, app.saveTo.value.label),
            ServiceInfo.FOREGROUND_SERVICE_TYPE_DATA_SYNC,
        )
        // mDNS needs multicast, which Wi-Fi drivers filter unless an app holds this lock.
        multicast = getSystemService(WifiManager::class.java).createMulticastLock("glft-mdns").apply {
            setReferenceCounted(false)
            acquire()
        }
        app.markReceiving(true)
        scope.launch { start() }
        watchNetwork()
        return START_NOT_STICKY
    }

    private fun start() {
        try {
            glft.node.startReceiving(glft.storageChoice(), glft.listener)
        } catch (e: GlftException.Failed) {
            glft.fail(e)
            stopSelf()
        }
    }

    /**
     * When Wi-Fi reconnects, GrapheneOS's per-connection random MAC address usually gets
     * the phone a new IP address, and the network sockets (and mDNS announcements) of the
     * old one are dead. Receiving restarts on the new address; a transfer in progress
     * resumes from where it was.
     */
    private fun watchNetwork() {
        val cm = getSystemService(ConnectivityManager::class.java)
        val callback = object : ConnectivityManager.NetworkCallback() {
            override fun onLinkPropertiesChanged(network: Network, lp: LinkProperties) {
                val now = lp.linkAddresses.map { it.address.hostAddress ?: "" }.filter { !it.contains(':') }.toSet()
                if (BuildConfig.DEBUG) Log.d("glft", "default network $network (${lp.interfaceName}): $now")
                if (now.isEmpty()) return
                val before = addresses
                addresses = now
                if (before != null && before != now) {
                    glft.note("This phone's network address changed to ${now.joinToString()}; restarting receiving so paired devices can find it")
                    scope.launch { start() }
                }
            }
        }
        cm.registerDefaultNetworkCallback(callback)
        watcher = callback
    }

    override fun onTimeout(startId: Int, fgsType: Int) {
        glft.fail("receive-time-limit", "onTimeout for the dataSync foreground service")
        stopSelf()
    }

    override fun onDestroy() {
        val app = glft
        Thread {
            app.node.stop()
            app.refreshDevices()
        }.start()
        app.markReceiving(false)
        watcher?.let { getSystemService(ConnectivityManager::class.java).unregisterNetworkCallback(it) }
        multicast?.release()
        scope.cancel()
        super.onDestroy()
    }

    companion object {
        const val ACTION_STOP = "stop"
    }
}
