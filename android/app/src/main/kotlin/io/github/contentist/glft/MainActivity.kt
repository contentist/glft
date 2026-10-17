package io.github.contentist.glft

import android.Manifest
import android.content.ClipData
import android.content.ClipboardManager
import android.content.Intent
import android.content.pm.ApplicationInfo
import android.content.pm.PackageManager
import android.net.Uri
import android.os.Bundle
import android.provider.DocumentsContract
import android.provider.Settings
import androidx.activity.ComponentActivity
import androidx.activity.compose.rememberLauncherForActivityResult
import androidx.activity.compose.setContent
import androidx.activity.enableEdgeToEdge
import androidx.activity.result.contract.ActivityResultContracts
import androidx.compose.foundation.isSystemInDarkTheme
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.safeDrawingPadding
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.items
import androidx.compose.foundation.text.selection.SelectionContainer
import androidx.compose.material3.Button
import androidx.compose.material3.Card
import androidx.compose.material3.HorizontalDivider
import androidx.compose.material3.LinearProgressIndicator
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedButton
import androidx.compose.material3.Surface
import androidx.compose.material3.Switch
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.material3.dynamicDarkColorScheme
import androidx.compose.material3.dynamicLightColorScheme
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableIntStateOf
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberCoroutineScope
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.unit.dp
import androidx.lifecycle.Lifecycle
import androidx.lifecycle.compose.LifecycleEventEffect
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.launch
import kotlinx.coroutines.withContext
import uniffi.glft_ffi.GlftException
import uniffi.glft_ffi.StorageChoice
import uniffi.glft_ffi.coreVersion
import java.text.SimpleDateFormat
import java.util.Date
import java.util.Locale

class MainActivity : ComponentActivity() {
    override fun onStart() {
        super.onStart()
        glft.inFront = true
        // The VPN's lockdown can be switched while glft is away; look again.
        Thread { glft.recheckNetwork() }.start()
    }

    override fun onStop() {
        glft.inFront = false
        super.onStop()
    }

    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        enableEdgeToEdge()
        setContent {
            val context = LocalContext.current
            val colors = if (isSystemInDarkTheme()) dynamicDarkColorScheme(context) else dynamicLightColorScheme(context)
            MaterialTheme(colorScheme = colors) {
                Surface(Modifier.fillMaxSize()) { Screen() }
            }
        }
    }
}

@Composable
private fun Screen() {
    val context = LocalContext.current
    val app = context.glft
    val log by app.log.collectAsStateWithLifecycle()
    val offers by app.offers.collectAsStateWithLifecycle()
    val receiving by app.receiving.collectAsStateWithLifecycle()
    val saveTo by app.saveTo.collectAsStateWithLifecycle()
    val devices by app.devices.collectAsStateWithLifecycle()
    val progress by app.progress.collectAsStateWithLifecycle()
    val received by app.received.collectAsStateWithLifecycle()
    val sendState by app.sendState.collectAsStateWithLifecycle()
    val net by app.net.collectAsStateWithLifecycle()
    val scope = rememberCoroutineScope()
    // Chosen in a picker, waiting for "to which computer?".
    var choosing by remember { mutableStateOf<Outgoing?>(null) }
    // Set when the network explanation was shown for sending.
    var forSending by remember { mutableStateOf(false) }
    var pairing by remember { mutableStateOf(false) }
    var showLog by remember { mutableStateOf(false) }
    // Set when the network explanation was shown for pairing rather than receiving.
    var forPairing by remember { mutableStateOf(false) }
    LaunchedEffect(Unit) { app.refreshDevices() }
    // Re-read permissions whenever the screen comes back, e.g. from glft's settings.
    var resumed by remember { mutableIntStateOf(0) }
    LifecycleEventEffect(Lifecycle.Event.ON_RESUME) { resumed++ }
    val access = remember(resumed) { Access.of(context) }
    var explaining by remember { mutableStateOf(false) }
    var refused by remember { mutableStateOf(false) }

    val startReceiving = {
        context.startForegroundService(Intent(context, ReceiveService::class.java))
    }
    val permissions = rememberLauncherForActivityResult(ActivityResultContracts.RequestMultiplePermissions()) {
        resumed++
        explaining = false
        refused = Access.of(context).missingRuntime.isNotEmpty()
        if (!refused) {
            when {
                forSending -> {}
                forPairing -> pairing = true
                else -> startReceiving()
            }
        }
    }
    val picker = rememberLauncherForActivityResult(ActivityResultContracts.OpenDocumentTree()) { tree: Uri? ->
        if (tree != null) {
            val flags = Intent.FLAG_GRANT_READ_URI_PERMISSION or Intent.FLAG_GRANT_WRITE_URI_PERMISSION
            context.contentResolver.takePersistableUriPermission(tree, flags)
            app.setSaveTo(SaveTo.Picked(tree, SafStorage(context.contentResolver, tree).label))
        }
    }

    /** Reads what was picked off the main thread, then asks where to send it. */
    fun prepare(read: () -> Outgoing) {
        scope.launch {
            try {
                choosing = withContext(Dispatchers.IO) { read() }
            } catch (e: Exception) {
                app.note("Couldn't read what you picked: ${e.message}", isError = true)
            }
        }
    }
    val pickFiles = rememberLauncherForActivityResult(ActivityResultContracts.OpenMultipleDocuments()) { uris: List<Uri> ->
        if (uris.isNotEmpty()) prepare { context.contentResolver.outgoingFiles(uris) }
    }
    val pickFolder = rememberLauncherForActivityResult(ActivityResultContracts.OpenDocumentTree()) { tree: Uri? ->
        if (tree != null) {
            // Kept so a send that is stopped and started again can read the folder.
            context.contentResolver.takePersistableUriPermission(tree, Intent.FLAG_GRANT_READ_URI_PERMISSION)
            prepare { context.contentResolver.outgoingFolder(tree) }
        }
    }
    /** Pickers need nothing, but sending needs the network; explain first if it's off. */
    val whenNetworkAllowed = { pick: () -> Unit ->
        val now = Access.of(context)
        if (now.network && now.localNetwork) {
            pick()
        } else {
            forSending = true
            forPairing = false
            explaining = true
        }
    }

    if (pairing) {
        PairScreen(onClose = { pairing = false; app.refreshDevices() })
        return
    }
    if (showLog) {
        LogScreen(onClose = { showLog = false })
        return
    }

    LazyColumn(
        Modifier.fillMaxSize().safeDrawingPadding().padding(horizontal = 20.dp),
        verticalArrangement = Arrangement.spacedBy(12.dp),
    ) {
        item {
            Text("glft", style = MaterialTheme.typography.headlineMedium, modifier = Modifier.padding(top = 12.dp))
            Text("GrapheneOS local file transfer · core ${coreVersion()}", style = MaterialTheme.typography.bodySmall)
        }

        if (net.wifiBlocked) {
            item { VpnLockdownCard(devices) }
        }

        item {
            Section("This device") {
                SelectionContainer { Text(app.node.id(), style = MaterialTheme.typography.bodyMedium) }
                TextButton(onClick = {
                    context.getSystemService(ClipboardManager::class.java)
                        .setPrimaryClip(ClipData.newPlainText("glft device id", app.node.id()))
                }) { Text("Copy id") }
            }
        }

        item {
            Section("Send") {
                val running = sendState is SendState.Running
                Row(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                    Button(enabled = !running, onClick = { whenNetworkAllowed { pickFiles.launch(arrayOf("*/*")) } }) { Text("Send files") }
                    OutlinedButton(enabled = !running, onClick = { whenNetworkAllowed { pickFolder.launch(null) } }) { Text("Send a folder") }
                }
                Text("Or share from any app and choose glft.", style = MaterialTheme.typography.bodySmall)
                if (forSending && (explaining || refused || !access.network)) {
                    PermissionHelp(
                        access = access,
                        refused = refused,
                        onContinue = { permissions.launch(access.missingRuntime.toTypedArray()) },
                    )
                }
                choosing?.let { outgoing ->
                    SendSheet(
                        outgoing = outgoing,
                        devices = devices,
                        onSend = { device, route ->
                            choosing = null
                            Sends.start(context, device, outgoing, route)
                        },
                        onCancel = { choosing = null },
                    )
                }
                SendStatus(sendState)
            }
        }

        item {
            Section("Save received files to") {
                Text(saveTo.label, style = MaterialTheme.typography.bodyLarge)
                Row(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                    OutlinedButton(onClick = { picker.launch(downloadsUri()) }) { Text("Save somewhere else") }
                    if (saveTo is SaveTo.Picked) {
                        TextButton(onClick = { app.setSaveTo(SaveTo.Downloads) }) { Text("Use ${MediaStoreStorage.BASE}") }
                    }
                }
            }
        }

        item {
            Section("Receiving") {
                Row(verticalAlignment = Alignment.CenterVertically) {
                    Text(
                        if (receiving) "On: paired devices can send to this phone" else "Off: nothing can be sent to this phone",
                        Modifier.weight(1f),
                    )
                    Switch(checked = receiving, onCheckedChange = { on ->
                        val now = Access.of(context)
                        forPairing = false
                        forSending = false
                        when {
                            !on -> context.startService(Intent(context, ReceiveService::class.java).setAction(ReceiveService.ACTION_STOP))
                            now.ready -> startReceiving()
                            // The explanation comes first; Android's own dialog only after "Continue".
                            else -> explaining = true
                        }
                    })
                }
                if (!forPairing && !forSending && (explaining || refused || !access.network)) {
                    PermissionHelp(
                        access = access,
                        refused = refused,
                        onContinue = { permissions.launch(access.missingRuntime.toTypedArray()) },
                    )
                }
                progress?.let { (done, total) ->
                    LinearProgressIndicator(
                        progress = { if (total > 0u) done.toFloat() / total.toFloat() else 0f },
                        modifier = Modifier.fillMaxWidth().padding(top = 4.dp),
                    )
                    Text("${size(done)} of ${size(total)} verified", style = MaterialTheme.typography.bodySmall)
                }
            }
        }

        item {
            Section("Paired devices") {
                if (devices.isEmpty()) {
                    Text("None yet. Only paired devices can send files to this phone.", style = MaterialTheme.typography.bodySmall)
                }
                devices.forEach { d ->
                    Column(Modifier.padding(vertical = 4.dp)) {
                        Text(d.name, style = MaterialTheme.typography.bodyLarge)
                        Text(
                            buildString {
                                append(if (d.remembered) "Remembered" else "Until receiving is switched off")
                                append(" · ${d.id.take(10)}")
                                d.lastSeenUnix?.let { append(" · last transfer ${SimpleDateFormat("d MMM HH:mm", Locale.getDefault()).format(Date(it * 1000))}") }
                            },
                            style = MaterialTheme.typography.bodySmall,
                        )
                        Row(verticalAlignment = Alignment.CenterVertically) {
                            if (d.remembered) {
                                Text("Accept its files without asking", Modifier.weight(1f), style = MaterialTheme.typography.bodyMedium)
                                Switch(checked = d.autoAccept, onCheckedChange = { on ->
                                    app.node.setAutoAccept(d.id, on)
                                    app.refreshDevices()
                                })
                            } else {
                                Spacer(Modifier.weight(1f))
                            }
                            TextButton(onClick = {
                                app.node.forget(d.id)
                                app.note("Forgot \"${d.name}\"; it can no longer send files to this phone")
                                app.refreshDevices()
                            }) { Text("Forget") }
                        }
                    }
                }
                OutlinedButton(onClick = {
                    val now = Access.of(context)
                    if (now.network && now.localNetwork) {
                        pairing = true
                    } else {
                        forPairing = true
                        forSending = false
                        explaining = true
                    }
                }) { Text("Pair with a computer") }
                if (forPairing && (explaining || refused || !access.network)) {
                    PermissionHelp(
                        access = access,
                        refused = refused,
                        onContinue = { permissions.launch(access.missingRuntime.toTypedArray()) },
                    )
                }
            }
        }

        items(offers, key = { "offer-${it.id}" }) { offer ->
            Card(Modifier.fillMaxWidth()) {
                Column(Modifier.padding(16.dp)) {
                    Text("${offer.peer} wants to send you files", style = MaterialTheme.typography.titleMedium)
                    Text("${offer.summary}, to ${offer.dest}")
                    Row(horizontalArrangement = Arrangement.spacedBy(8.dp), modifier = Modifier.padding(top = 8.dp)) {
                        Button(onClick = { app.node.answerOffer(offer.id, true) }) { Text("Accept") }
                        OutlinedButton(onClick = { app.node.answerOffer(offer.id, false) }) { Text("Decline") }
                    }
                }
            }
        }

        if (context.applicationInfo.flags and ApplicationInfo.FLAG_DEBUGGABLE != 0) {
            item {
                Section("Storage test (debug builds)") {
                    Text("Writes 1,000 files of 64 KB the way a transfer does, then reports the time.", style = MaterialTheme.typography.bodySmall)
                    Row(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                        val run = { name: String, choice: () -> StorageChoice ->
                            scope.launch {
                                app.note("Storage test ($name) started")
                                val result = withContext(Dispatchers.IO) {
                                    try {
                                        app.node.benchmarkStorage(choice(), 1000u, 65536u)
                                    } catch (e: GlftException.Failed) {
                                        "failed: ${e.headline} [${e.detail}]"
                                    }
                                }
                                app.note("Storage test ($name): $result")
                            }
                            Unit
                        }
                        OutlinedButton(onClick = { run("media store") { StorageChoice.Foreign(MediaStoreStorage(context.contentResolver)) } }) { Text("Media store") }
                        OutlinedButton(onClick = { run("direct path") { app.directPathChoice() } }) { Text("Direct") }
                        OutlinedButton(
                            enabled = saveTo is SaveTo.Picked,
                            onClick = { run("picked folder") { app.storageChoice() } },
                        ) { Text("Picked") }
                    }
                }
            }
        }

        if (received.isNotEmpty()) {
            item { Text("Received", style = MaterialTheme.typography.titleMedium) }
            items(received.take(10), key = { "received-${it.atUnixMs}" }) { r ->
                Column {
                    Text(if (r.files == 1u) "✓ Verified · ${r.summary}" else "✓ All ${r.files} files verified · ${r.summary}", style = MaterialTheme.typography.bodyMedium)
                    Text(
                        "From ${r.peer} · ${r.dest} · ${SimpleDateFormat("d MMM HH:mm", Locale.getDefault()).format(Date(r.atUnixMs))}",
                        style = MaterialTheme.typography.bodySmall,
                    )
                }
            }
        }

        item {
            Row(verticalAlignment = Alignment.CenterVertically) {
                Text("What's happening", style = MaterialTheme.typography.titleMedium, modifier = Modifier.weight(1f))
                TextButton(onClick = { showLog = true }) { Text("Show all") }
            }
        }
        items(log.takeLast(3).asReversed()) { line -> LogRow(line) }
        item { Spacer(Modifier.padding(12.dp)) }
    }
}

/** Shown while a VPN in lockdown cuts glft off from the local network. */
@Composable
private fun VpnLockdownCard(devices: List<uniffi.glft_ffi.PairedDevice>) {
    val context = LocalContext.current
    val meshDevices = devices.filter { it.mesh != null }
    Card(Modifier.fillMaxWidth()) {
        Column(Modifier.padding(16.dp), verticalArrangement = Arrangement.spacedBy(6.dp)) {
            Text("Your VPN is blocking Wi-Fi transfers", style = MaterialTheme.typography.titleSmall, color = MaterialTheme.colorScheme.error)
            Text(
                "Your VPN is set to \"Block connections without VPN\". While that's on, this phone can't send to your " +
                    "computer over Wi-Fi, and your computer can't send to this phone over Wi-Fi.",
            )
            Text(
                if (meshDevices.isNotEmpty()) {
                    "You can still send over ${meshDevices.first().mesh}. To use Wi-Fi again, turn that setting off."
                } else {
                    "To use Wi-Fi again, turn that setting off."
                },
            )
            OutlinedButton(onClick = { context.startActivity(Intent(Settings.ACTION_VPN_SETTINGS)) }) { Text("Open VPN settings") }
        }
    }
}

/** How the current or last send is going. */
@Composable
private fun SendStatus(state: SendState) {
    val context = LocalContext.current
    when (state) {
        SendState.Idle -> {}
        is SendState.Running -> {
            Text("Sending ${state.summary} to ${state.peer}", style = MaterialTheme.typography.bodyMedium)
            LinearProgressIndicator(
                progress = { if (state.total > 0) state.done.toFloat() / state.total.toFloat() else 0f },
                modifier = Modifier.fillMaxWidth().padding(top = 4.dp),
            )
            Row(verticalAlignment = Alignment.CenterVertically) {
                Text(
                    if (state.done > 0) "${size(state.done.toULong())} of ${size(state.total.toULong())} sent" else "Reading the files and waiting for an answer",
                    Modifier.weight(1f),
                    style = MaterialTheme.typography.bodySmall,
                )
                TextButton(onClick = { context.glft.node.cancelSend() }) { Text("Cancel") }
            }
        }
        is SendState.Done -> Text(
            "✓ Sent to ${state.peer}${state.via?.let { " over $it" } ?: ""}: ${state.summary}, ${if (state.files == 1) "verified" else "all ${state.files} files verified"}",
            style = MaterialTheme.typography.bodyMedium,
        )
        is SendState.Failed -> {
            Text(state.headline, style = MaterialTheme.typography.bodyMedium, color = MaterialTheme.colorScheme.error)
            Text(state.nextStep, style = MaterialTheme.typography.bodySmall)
            Text("Details: ${state.detail}", style = MaterialTheme.typography.bodySmall, color = MaterialTheme.colorScheme.onSurfaceVariant)
        }
    }
}

@Composable
private fun Section(title: String, content: @Composable () -> Unit) {
    Column(verticalArrangement = Arrangement.spacedBy(4.dp)) {
        Text(title, style = MaterialTheme.typography.titleMedium)
        content()
        HorizontalDivider(Modifier.padding(top = 8.dp))
    }
}

@Composable
private fun PermissionHelp(access: Access, refused: Boolean, onContinue: () -> Unit) {
    val context = LocalContext.current
    val openSettings = {
        context.startActivity(
            Intent(Settings.ACTION_APPLICATION_DETAILS_SETTINGS, Uri.fromParts("package", context.packageName, null)),
        )
    }
    Card(Modifier.fillMaxWidth()) {
        Column(Modifier.padding(16.dp), verticalArrangement = Arrangement.spacedBy(6.dp)) {
            if (!access.network) {
                Text("Network access is off for glft.", style = MaterialTheme.typography.titleSmall)
                Text(
                    "GrapheneOS lets you cut an app off from the network, and glft is cut off. " +
                        "It needs the network only to reach your own devices directly; nothing goes to the internet.",
                )
                Text("Turn it on under Permissions, Network.", style = MaterialTheme.typography.bodySmall)
            }
            if (!access.localNetwork) {
                Text("glft needs local network access.", style = MaterialTheme.typography.titleSmall)
                Text(
                    "It talks to your computer directly over your Wi-Fi. Android lists this under \"Nearby devices\", " +
                        "with wording about finding and locating devices; glft only connects to devices you paired.",
                )
            }
            if (!access.notifications) {
                Text("glft needs to show notifications.", style = MaterialTheme.typography.titleSmall)
                Text("While glft can receive, a notification always says so, and incoming files ask for your OK there.")
            }
            Row(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                if (access.missingRuntime.isNotEmpty() && !refused) Button(onClick = onContinue) { Text("Continue") }
                if (!access.network || refused) Button(onClick = openSettings) { Text("Open glft's settings") }
            }
        }
    }
}

/**
 * GrapheneOS's Network toggle can't be read with checkSelfPermission: apps are told
 * INTERNET is granted either way, so they don't crash. Opening a socket tells the truth.
 */
fun networkAllowed(): Boolean = try {
    java.net.DatagramSocket().close()
    true
} catch (e: java.net.SocketException) {
    false
}

/** What glft may do right now. */
private data class Access(val network: Boolean, val localNetwork: Boolean, val notifications: Boolean) {
    /** Permissions Android can ask for in a dialog. GrapheneOS's Network toggle is not one. */
    val missingRuntime: List<String>
        get() = buildList {
            if (!localNetwork) add(Manifest.permission.ACCESS_LOCAL_NETWORK)
            if (!notifications) add(Manifest.permission.POST_NOTIFICATIONS)
        }
    val ready get() = network && localNetwork && notifications

    companion object {
        fun of(context: android.content.Context): Access {
            fun has(p: String) = context.checkSelfPermission(p) == PackageManager.PERMISSION_GRANTED
            return Access(
                network = networkAllowed(),
                localNetwork = has(Manifest.permission.ACCESS_LOCAL_NETWORK),
                notifications = has(Manifest.permission.POST_NOTIFICATIONS),
            )
        }
    }
}

/** Sizes in decimal units, as the core and file managers show them. */
fun size(bytes: ULong): String {
    val units = listOf("KB", "MB", "GB", "TB")
    if (bytes < 1000u) return "$bytes bytes"
    var v = bytes.toDouble()
    var unit = ""
    for (u in units) {
        v /= 1000
        unit = u
        if (v < 1000) break
    }
    return if (v < 10) "%.1f %s".format(v, unit) else "%.0f %s".format(v, unit)
}

/** Opens the folder picker at Download, where Android lets the user make a subfolder. */
private fun downloadsUri(): Uri =
    DocumentsContract.buildDocumentUri("com.android.externalstorage.documents", "primary:Download")
