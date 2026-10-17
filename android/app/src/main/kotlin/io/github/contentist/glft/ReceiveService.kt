package io.github.contentist.glft

import android.app.Service
import android.content.Intent
import android.content.pm.ServiceInfo
import android.net.wifi.WifiManager
import android.os.IBinder
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
        scope.launch {
            try {
                app.node.startReceiving(app.storageChoice(), app.listener)
            } catch (e: GlftException.Failed) {
                app.fail(e)
                stopSelf()
            }
        }
        return START_NOT_STICKY
    }

    override fun onTimeout(startId: Int, fgsType: Int) {
        glft.note(
            "Receiving stopped: Android allows background receiving for 6 hours a day. Open glft to start it again.",
            isError = true,
        )
        stopSelf()
    }

    override fun onDestroy() {
        val app = glft
        Thread { app.node.stop() }.start()
        app.markReceiving(false)
        multicast?.release()
        scope.cancel()
        super.onDestroy()
    }

    companion object {
        const val ACTION_STOP = "stop"
    }
}
