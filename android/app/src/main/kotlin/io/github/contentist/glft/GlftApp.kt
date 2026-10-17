package io.github.contentist.glft

import android.app.Application
import android.content.Context
import android.net.ConnectivityManager
import android.net.Network
import android.net.NetworkCapabilities
import android.net.NetworkRequest
import android.net.Uri
import android.os.Environment
import android.util.Log
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.update
import uniffi.glft_ffi.LogLine
import uniffi.glft_ffi.Node
import uniffi.glft_ffi.NodeListener
import uniffi.glft_ffi.OfferInfo
import uniffi.glft_ffi.PairedDevice
import uniffi.glft_ffi.ReceivedInfo
import uniffi.glft_ffi.Route
import uniffi.glft_ffi.StorageChoice
import org.json.JSONArray
import org.json.JSONObject
import java.io.File

/** Where received files go, as the user chose it. */
sealed interface SaveTo {
    val label: String

    /** Download/glft through the media store: the default, no setup. */
    data object Downloads : SaveTo {
        override val label = MediaStoreStorage.BASE
    }

    /** A folder picked with Android's folder picker. */
    data class Picked(val tree: Uri, override val label: String) : SaveTo
}

/** What the phone's network allows glft, as far as Android tells it. */
data class NetState(
    /** A VPN is the default network (Yggdrasil, WireGuard, …). */
    val vpn: Boolean = false,
    /** Android blocks glft from Wi-Fi: a VPN with "Block connections without VPN". */
    val wifiBlocked: Boolean = false,
)

/**
 * Holds the one [Node] for the process, the event log and pending offers, so the
 * screen, the receive service and notification actions all see the same state.
 */
class GlftApp : Application() {
    val node: Node by lazy { Node(filesDir.path, deviceName()) }

    /** The name the user gave the phone in Settings, else its model ("Pixel 8 Pro"). */
    private fun deviceName(): String =
        android.provider.Settings.Global.getString(contentResolver, android.provider.Settings.Global.DEVICE_NAME)
            ?.takeIf { it.isNotBlank() } ?: android.os.Build.MODEL

    private val _log = MutableStateFlow<List<LogLine>>(emptyList())
    val log: StateFlow<List<LogLine>> = _log

    private val _offers = MutableStateFlow<List<OfferInfo>>(emptyList())
    val offers: StateFlow<List<OfferInfo>> = _offers

    private val _receiving = MutableStateFlow(false)
    val receiving: StateFlow<Boolean> = _receiving

    private val _saveTo = MutableStateFlow<SaveTo>(SaveTo.Downloads)
    val saveTo: StateFlow<SaveTo> = _saveTo

    private val _devices = MutableStateFlow<List<PairedDevice>>(emptyList())
    val devices: StateFlow<List<PairedDevice>> = _devices

    /** Bytes verified of the transfer arriving now, or null when none is. */
    private val _progress = MutableStateFlow<Pair<ULong, ULong>?>(null)
    val progress: StateFlow<Pair<ULong, ULong>?> = _progress

    private val _received = MutableStateFlow<List<ReceivedInfo>>(emptyList())
    val received: StateFlow<List<ReceivedInfo>> = _received

    fun refreshDevices() {
        _devices.value = node.devices()
    }

    private val _sendState = MutableStateFlow<SendState>(SendState.Idle)
    val sendState: StateFlow<SendState> = _sendState

    fun setSendState(state: SendState) {
        _sendState.value = state
    }

    /** The send chosen on screen, handed to [SendJob] in this process. */
    class QueuedSend(val device: PairedDevice, val outgoing: Outgoing, val route: Route)

    private var queuedSend: QueuedSend? = null

    fun queueSend(device: PairedDevice, outgoing: Outgoing, route: Route) {
        queuedSend = QueuedSend(device, outgoing, route)
        _sendState.value = SendState.Running(device.name, outgoing.summary, 0, outgoing.bytes)
    }

    fun takeQueuedSend(): QueuedSend? = queuedSend.also { queuedSend = null }

    private val _net = MutableStateFlow(NetState())
    val net: StateFlow<NetState> = _net

    /**
     * Follows whether a VPN is active and whether it blocks glft from the local network, as
     * "Block connections without VPN" does. Android doesn't report that block for Wi-Fi
     * itself, so glft asks directly: a UDP socket "connected" to the router sends nothing,
     * and fails with "Permission denied" when the local network is off limits. Android also
     * blocks every network for an app sitting in the background; then the VPN is blocked
     * too, which lockdown alone doesn't do. The core uses this to explain failures.
     */
    private fun watchNetwork() {
        val cm = getSystemService(ConnectivityManager::class.java)
        var vpn = false
        var vpnBlocked = false
        // The Wi-Fi router, whose reachability tells whether the local network is blocked.
        var router: java.net.InetAddress? = null
        fun localBlocked(): Boolean {
            val to = router ?: return false
            return try {
                java.net.DatagramSocket().use { it.connect(java.net.InetSocketAddress(to, 9)) }
                false
            } catch (e: Exception) {
                (e.message ?: "").contains("Permission denied") || (e.cause?.message ?: "").contains("EPERM")
            }
        }
        fun publish() {
            // In the background Android blocks glft everywhere, which hides whether the VPN
            // blocks the local network; keep the last answer until glft can look again.
            val blocked = when {
                !vpn -> false
                vpnBlocked -> _net.value.wifiBlocked
                else -> localBlocked()
            }
            if (BuildConfig.DEBUG) Log.d("glft", "network: vpn=$vpn vpnBlocked=$vpnBlocked router=$router localBlocked=$blocked")
            val now = NetState(vpn = vpn, wifiBlocked = blocked)
            if (now != _net.value) {
                val was = _net.value
                _net.value = now
                node.setNetworkState(now.vpn, now.wifiBlocked)
                if (now.wifiBlocked && !was.wifiBlocked) {
                    note("A VPN is blocking the local network (Block connections without VPN); glft can only use a mesh such as Yggdrasil")
                } else if (!now.wifiBlocked && was.wifiBlocked) {
                    note("The local network is reachable again")
                }
            }
        }
        recheckNetwork = { publish() }
        cm.registerDefaultNetworkCallback(object : ConnectivityManager.NetworkCallback() {
            override fun onCapabilitiesChanged(network: Network, caps: NetworkCapabilities) {
                vpn = caps.hasTransport(NetworkCapabilities.TRANSPORT_VPN)
                publish()
            }

            override fun onBlockedStatusChanged(network: Network, isBlocked: Boolean) {
                vpnBlocked = isBlocked
                publish()
            }

            override fun onLost(network: Network) {
                vpn = false
                vpnBlocked = false
                publish()
            }
        })
        val wifi = NetworkRequest.Builder().addTransportType(NetworkCapabilities.TRANSPORT_WIFI).build()
        cm.registerNetworkCallback(wifi, object : ConnectivityManager.NetworkCallback() {
            override fun onLinkPropertiesChanged(network: Network, lp: android.net.LinkProperties) {
                router = lp.routes.firstOrNull { it.hasGateway() && it.gateway is java.net.Inet4Address }?.gateway
                publish()
            }

            override fun onLost(network: Network) {
                router = null
                publish()
            }
        })
    }

    /** Checks the network again, e.g. when glft comes back on screen. */
    var recheckNetwork: () -> Unit = {}
        private set

    override fun onCreate() {
        super.onCreate()
        Notifications.createChannels(this)
        val prefs = getSharedPreferences("glft", Context.MODE_PRIVATE)
        val tree = prefs.getString("tree", null)
        val label = prefs.getString("treeLabel", null)
        if (tree != null && label != null) _saveTo.value = SaveTo.Picked(Uri.parse(tree), label)
        _received.value = loadReceived()
        watchNetwork()
    }

    private val receivedFile get() = File(filesDir, "received.json")

    private fun loadReceived(): List<ReceivedInfo> = try {
        val array = JSONArray(receivedFile.readText())
        (0 until array.length()).map { i ->
            val o = array.getJSONObject(i)
            ReceivedInfo(o.getLong("at"), o.getString("peer"), o.getString("summary"), o.getInt("files").toUInt(), o.getString("dest"))
        }
    } catch (e: Exception) {
        emptyList()
    }

    private fun saveReceived(list: List<ReceivedInfo>) {
        val array = JSONArray()
        list.forEach {
            array.put(JSONObject().put("at", it.atUnixMs).put("peer", it.peer).put("summary", it.summary).put("files", it.files.toInt()).put("dest", it.dest))
        }
        receivedFile.writeText(array.toString())
    }

    fun setSaveTo(choice: SaveTo) {
        val prefs = getSharedPreferences("glft", Context.MODE_PRIVATE).edit()
        when (choice) {
            SaveTo.Downloads -> prefs.remove("tree").remove("treeLabel")
            is SaveTo.Picked -> prefs.putString("tree", choice.tree.toString()).putString("treeLabel", choice.label)
        }
        prefs.apply()
        _saveTo.value = choice
    }

    fun storageChoice(): StorageChoice = when (val s = _saveTo.value) {
        SaveTo.Downloads -> StorageChoice.Foreign(MediaStoreStorage(contentResolver))
        is SaveTo.Picked -> StorageChoice.Foreign(SafStorage(contentResolver, s.tree))
    }

    /** The plain-path route to Download/glft, kept for the storage benchmark. */
    fun directPathChoice(): StorageChoice {
        val dir = File(Environment.getExternalStoragePublicDirectory(Environment.DIRECTORY_DOWNLOADS), MediaStoreStorage.FOLDER)
        return StorageChoice.Path(dir.path, MediaStoreStorage.BASE)
    }

    /** Logs an error from the core with its next step and raw detail. */
    fun fail(e: uniffi.glft_ffi.GlftException.Failed) =
        append(LogLine(System.currentTimeMillis(), e.headline, true, e.nextStep, "[${e.id}] ${e.detail}"))

    fun note(sentence: String, isError: Boolean = false) =
        append(LogLine(System.currentTimeMillis(), sentence, isError, null, null))

    private fun append(line: LogLine) {
        // Debug builds also copy the log to logcat, so tests over adb can time events.
        if (BuildConfig.DEBUG) Log.i("glft", line.sentence + (line.detail?.let { " [$it]" } ?: ""))
        _log.update { (it + line).takeLast(500) }
    }

    /** Whether the main screen is visible; set by [MainActivity]. */
    @Volatile
    var inFront = false
        set(value) {
            field = value
            // Offers still waiting move between the app's cards and notifications.
            for (offer in _offers.value) {
                if (value) Notifications.cancelOffer(this, offer.id) else Notifications.showOffer(this, offer)
            }
        }

    val listener = object : NodeListener {
        override fun onLog(line: LogLine) = append(line)

        override fun onOffer(offer: OfferInfo) {
            _offers.update { it + offer }
            // On screen, the card in the app is the offer; a notification would show it twice.
            if (!inFront) Notifications.showOffer(this@GlftApp, offer)
        }

        override fun onOfferClosed(id: ULong, missed: Boolean) {
            val offer = _offers.value.find { it.id == id }
            _offers.update { list -> list.filterNot { it.id == id } }
            Notifications.cancelOffer(this@GlftApp, id)
            if (missed && offer != null) Notifications.showMissed(this@GlftApp, offer)
        }

        override fun onStopped(error: String?) {
            _receiving.value = false
            _progress.value = null
            if (error != null) note(error, isError = true)
        }

        override fun onProgress(done: ULong, total: ULong) {
            _progress.value = if (done >= total) null else done to total
        }

        override fun onSendProgress(done: ULong, total: ULong) {
            _sendState.update { s -> if (s is SendState.Running) s.copy(done = done.toLong(), total = total.toLong()) else s }
        }

        override fun onReceived(received: ReceivedInfo) {
            _progress.value = null
            val list = (listOf(received) + _received.value).take(100)
            _received.value = list
            saveReceived(list)
            refreshDevices()
        }
    }

    fun markReceiving(on: Boolean) {
        _receiving.value = on
    }
}

val Context.glft: GlftApp get() = applicationContext as GlftApp
