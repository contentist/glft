package io.github.contentist.glft

import android.app.job.JobInfo
import android.app.job.JobScheduler
import android.content.ClipData
import android.content.ComponentName
import android.content.Intent
import android.content.ContentResolver
import android.content.Context
import android.net.NetworkRequest
import android.net.Uri
import android.provider.DocumentsContract
import android.provider.OpenableColumns
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.material3.Button
import androidx.compose.material3.Card
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.ui.Modifier
import androidx.compose.ui.unit.dp
import uniffi.glft_ffi.FolderEntry
import android.widget.Toast
import androidx.compose.runtime.getValue
import androidx.compose.ui.platform.LocalContext
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import uniffi.glft_ffi.PairedDevice
import uniffi.glft_ffi.Route
import uniffi.glft_ffi.SendItem
import uniffi.glft_ffi.SendSource
import uniffi.glft_ffi.StorageException

/** What the person chose to send, ready for [uniffi.glft_ffi.Node.send]. */
class Outgoing(
    val items: List<SendItem>,
    /** The files, by the index [SendItem]s and [FolderEntry]s use. */
    val uris: List<Uri>,
    /** "3 files, 12 MB" or "Album (folder), 23 files, 230 MB". */
    val summary: String,
    val bytes: Long,
    /** Files read through one-off grants (shared or picked files), which the job must hold
     *  while it runs; a folder's grant is kept permanently instead. */
    val grants: List<Uri> = emptyList(),
)

/** Where a send is, for the screen and the job's notification. */
sealed interface SendState {
    data object Idle : SendState
    data class Running(val peer: String, val summary: String, val done: Long = 0, val total: Long = 0) : SendState
    /** [via] names the mesh (Yggdrasil) if that carried the data; null for Wi-Fi. */
    data class Done(val peer: String, val summary: String, val files: Int, val secs: Double, val via: String? = null) : SendState
    data class Failed(val peer: String, val summary: String, val headline: String, val nextStep: String, val detail: String) : SendState
}

/** Opens the chosen files for the core, once per request it makes. */
class UriSource(private val resolver: ContentResolver, private val uris: List<Uri>) : SendSource {
    override fun open(index: UInt): Int {
        val uri = uris.getOrNull(index.toInt()) ?: throw StorageException.Failed("no file $index", false, false)
        return try {
            val fd = resolver.openFileDescriptor(uri, "r") ?: throw StorageException.Failed("$uri could not be opened", false, false)
            fd.detachFd()
        } catch (e: StorageException) {
            throw e
        } catch (e: Exception) {
            throw StorageException.Failed("$uri: ${e.message}", false, false)
        }
    }
}

/** Files shared from another app or picked in glft. Blocking: queries their names and sizes. */
fun ContentResolver.outgoingFiles(uris: List<Uri>): Outgoing {
    var bytes = 0L
    val items = uris.mapIndexed { i, uri ->
        var name = uri.lastPathSegment?.substringAfterLast('/') ?: "file"
        query(uri, arrayOf(OpenableColumns.DISPLAY_NAME, OpenableColumns.SIZE), null, null, null)?.use { c ->
            if (c.moveToFirst()) {
                c.getString(0)?.takeIf { it.isNotBlank() }?.let { name = it }
                if (!c.isNull(1)) bytes += c.getLong(1)
            }
        }
        SendItem(name, i.toUInt(), emptyList())
    }
    val what = if (items.size == 1) items[0].name else "${items.size} files"
    return Outgoing(items, uris, "$what, ${size(bytes.toULong())}", bytes, grants = uris)
}

/** A folder picked with the folder picker, with everything below it. Blocking: walks the tree. */
fun ContentResolver.outgoingFolder(tree: Uri): Outgoing {
    val uris = mutableListOf<Uri>()
    val entries = mutableListOf<FolderEntry>()
    var bytes = 0L
    val columns = arrayOf(
        DocumentsContract.Document.COLUMN_DOCUMENT_ID,
        DocumentsContract.Document.COLUMN_DISPLAY_NAME,
        DocumentsContract.Document.COLUMN_MIME_TYPE,
        DocumentsContract.Document.COLUMN_SIZE,
    )
    fun walk(docId: String, prefix: String) {
        val children = DocumentsContract.buildChildDocumentsUriUsingTree(tree, docId)
        query(children, columns, null, null, null)?.use { c ->
            while (c.moveToNext()) {
                val id = c.getString(0)
                val name = c.getString(1) ?: continue
                val rel = if (prefix.isEmpty()) name else "$prefix/$name"
                if (c.getString(2) == DocumentsContract.Document.MIME_TYPE_DIR) {
                    entries += FolderEntry(rel, null)
                    walk(id, rel)
                } else {
                    entries += FolderEntry(rel, uris.size.toUInt())
                    uris += DocumentsContract.buildDocumentUriUsingTree(tree, id)
                    if (!c.isNull(3)) bytes += c.getLong(3)
                }
            }
        }
    }
    val rootId = DocumentsContract.getTreeDocumentId(tree)
    var name = rootId.substringAfterLast(':').substringAfterLast('/').ifEmpty { "Folder" }
    query(DocumentsContract.buildDocumentUriUsingTree(tree, rootId), arrayOf(DocumentsContract.Document.COLUMN_DISPLAY_NAME), null, null, null)?.use { c ->
        if (c.moveToFirst()) c.getString(0)?.takeIf { it.isNotBlank() }?.let { name = it }
    }
    walk(rootId, "")
    val files = uris.size
    return Outgoing(
        listOf(SendItem(name, null, entries)),
        uris,
        "$name (folder), ${if (files == 1) "1 file" else "$files files"}, ${size(bytes.toULong())}",
        bytes,
    )
}

object Sends {
    private const val JOB_ID = 1

    /**
     * Starts sending [outgoing] to [device] as a user-initiated transfer job. It must be
     * called while glft is on screen, which is when the person chose to send.
     */
    fun start(context: Context, device: PairedDevice, outgoing: Outgoing, route: Route): Boolean {
        val app = context.glft
        if (app.sendState.value is SendState.Running) {
            app.fail("send-already-running")
            return false
        }
        app.queueSend(device, outgoing, route)
        val builder = JobInfo.Builder(JOB_ID, ComponentName(context, SendJob::class.java))
        if (outgoing.grants.isNotEmpty()) {
            // The job holds the read grants for as long as it runs, after the share sheet
            // or picker that received them has closed.
            val clip = ClipData("glft send", arrayOf("*/*"), ClipData.Item(outgoing.grants[0]))
            outgoing.grants.drop(1).forEach { clip.addItem(ClipData.Item(it)) }
            builder.setClipData(clip, Intent.FLAG_GRANT_READ_URI_PERMISSION)
        }
        val job = builder
            .setUserInitiated(true)
            // Any network: the computer may be on Wi-Fi without internet, or reachable only
            // through a VPN or mesh such as Yggdrasil.
            .setRequiredNetwork(NetworkRequest.Builder().clearCapabilities().build())
            .setEstimatedNetworkBytes(0, outgoing.bytes)
            .build()
        val scheduled = context.getSystemService(JobScheduler::class.java).schedule(job) == JobScheduler.RESULT_SUCCESS
        if (scheduled) {
            val how = if (route == Route.MESH) " over ${device.mesh ?: "the mesh"}" else ""
            Toast.makeText(context, "Sending to ${device.name}$how…", Toast.LENGTH_SHORT).show()
        } else {
            app.fail("send-not-started", "JobScheduler refused the job")
        }
        return scheduled
    }
}

/**
 * "Send … to which computer?", for the Share menu and the in-app pickers. A computer glft
 * also knows on a mesh gets a choice of route: Wi-Fi, or the mesh (Yggdrasil).
 */
@Composable
fun SendSheet(outgoing: Outgoing, devices: List<PairedDevice>, onSend: (PairedDevice, Route) -> Unit, onCancel: () -> Unit) {
    val net by LocalContext.current.glft.net.collectAsStateWithLifecycle()
    Card(Modifier.fillMaxWidth()) {
        Column(Modifier.padding(16.dp), verticalArrangement = Arrangement.spacedBy(8.dp)) {
            Text("Send ${outgoing.summary}", style = MaterialTheme.typography.titleMedium)
            if (devices.isEmpty()) {
                Text("No paired computer yet. Pair one first: in glft, under Paired devices, \"Pair with a computer\".")
            } else {
                Text("To:", style = MaterialTheme.typography.bodySmall)
                devices.forEach { d ->
                    // Wi-Fi first; the mesh if Wi-Fi can't reach it (another network, or a
                    // VPN in lockdown). On the same Wi-Fi a mesh route would move to Wi-Fi
                    // anyway, so there is nothing to choose.
                    Button(onClick = { onSend(d, Route.AUTO) }, modifier = Modifier.fillMaxWidth()) { Text(d.name) }
                    d.mesh?.let { mesh ->
                        Text(
                            when {
                                net.wifiBlocked -> "Your VPN blocks Wi-Fi, so this goes over $mesh."
                                net.vpn -> "Also reachable over $mesh when you're not on the same Wi-Fi."
                                else -> "Reachable over $mesh away from home, if $mesh is running on this phone."
                            },
                            style = MaterialTheme.typography.bodySmall,
                        )
                    }
                }
            }
            TextButton(onClick = onCancel) { Text("Cancel") }
        }
    }
}
