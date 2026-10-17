package io.github.contentist.glft

import android.app.Application
import android.content.Context
import android.net.Uri
import android.os.Environment
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.update
import uniffi.glft_ffi.LogLine
import uniffi.glft_ffi.Node
import uniffi.glft_ffi.NodeListener
import uniffi.glft_ffi.OfferInfo
import uniffi.glft_ffi.StorageChoice
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

    override fun onCreate() {
        super.onCreate()
        Notifications.createChannels(this)
        val prefs = getSharedPreferences("glft", Context.MODE_PRIVATE)
        val tree = prefs.getString("tree", null)
        val label = prefs.getString("treeLabel", null)
        if (tree != null && label != null) _saveTo.value = SaveTo.Picked(Uri.parse(tree), label)
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

    private fun append(line: LogLine) = _log.update { (it + line).takeLast(500) }

    val listener = object : NodeListener {
        override fun onLog(line: LogLine) = append(line)

        override fun onOffer(offer: OfferInfo) {
            _offers.update { it + offer }
            Notifications.showOffer(this@GlftApp, offer)
        }

        override fun onOfferClosed(id: ULong, missed: Boolean) {
            val offer = _offers.value.find { it.id == id }
            _offers.update { list -> list.filterNot { it.id == id } }
            Notifications.cancelOffer(this@GlftApp, id)
            if (missed && offer != null) Notifications.showMissed(this@GlftApp, offer)
        }

        override fun onStopped(error: String?) {
            _receiving.value = false
            if (error != null) note(error, isError = true)
        }
    }

    fun markReceiving(on: Boolean) {
        _receiving.value = on
    }
}

val Context.glft: GlftApp get() = applicationContext as GlftApp
