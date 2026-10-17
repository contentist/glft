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
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.items
import androidx.compose.foundation.text.selection.SelectionContainer
import androidx.compose.material3.Button
import androidx.compose.material3.Card
import androidx.compose.material3.HorizontalDivider
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedButton
import androidx.compose.material3.Surface
import androidx.compose.material3.Switch
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.material3.dynamicDarkColorScheme
import androidx.compose.material3.dynamicLightColorScheme
import androidx.compose.runtime.Composable
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
import uniffi.glft_ffi.LogLine
import uniffi.glft_ffi.StorageChoice
import uniffi.glft_ffi.coreVersion
import java.text.SimpleDateFormat
import java.util.Date
import java.util.Locale

class MainActivity : ComponentActivity() {
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
    val scope = rememberCoroutineScope()
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
        if (!refused) startReceiving()
    }
    val picker = rememberLauncherForActivityResult(ActivityResultContracts.OpenDocumentTree()) { tree: Uri? ->
        if (tree != null) {
            val flags = Intent.FLAG_GRANT_READ_URI_PERMISSION or Intent.FLAG_GRANT_WRITE_URI_PERMISSION
            context.contentResolver.takePersistableUriPermission(tree, flags)
            app.setSaveTo(SaveTo.Picked(tree, SafStorage(context.contentResolver, tree).label))
        }
    }

    LazyColumn(
        Modifier.fillMaxSize().safeDrawingPadding().padding(horizontal = 20.dp),
        verticalArrangement = Arrangement.spacedBy(12.dp),
    ) {
        item {
            Text("glft", style = MaterialTheme.typography.headlineMedium, modifier = Modifier.padding(top = 12.dp))
            Text("GrapheneOS local file transfer · core ${coreVersion()}", style = MaterialTheme.typography.bodySmall)
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
                        when {
                            !on -> context.startService(Intent(context, ReceiveService::class.java).setAction(ReceiveService.ACTION_STOP))
                            now.ready -> startReceiving()
                            // The explanation comes first; Android's own dialog only after "Continue".
                            else -> explaining = true
                        }
                    })
                }
                if (explaining || refused || !access.network) {
                    PermissionHelp(
                        access = access,
                        refused = refused,
                        onContinue = { permissions.launch(access.missingRuntime.toTypedArray()) },
                    )
                }
                Text(
                    "Paired devices: ${app.node.allowedDevices().size}. Pairing by QR code comes next; until then, debug builds add a device over USB.",
                    style = MaterialTheme.typography.bodySmall,
                )
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

        item { Text("What's happening", style = MaterialTheme.typography.titleMedium) }
        items(log.asReversed()) { line -> LogRow(line) }
        item { Spacer(Modifier.padding(12.dp)) }
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

@Composable
private fun LogRow(line: LogLine) {
    val time = remember(line.atUnixMs) { SimpleDateFormat("HH:mm:ss", Locale.getDefault()).format(Date(line.atUnixMs)) }
    val color = if (line.isError) MaterialTheme.colorScheme.error else MaterialTheme.colorScheme.onSurface
    Row {
        Text(time, style = MaterialTheme.typography.bodySmall, color = MaterialTheme.colorScheme.onSurfaceVariant)
        Spacer(Modifier.width(10.dp))
        Column {
            Text(line.sentence, style = MaterialTheme.typography.bodyMedium, color = color)
            line.nextStep?.let { Text(it, style = MaterialTheme.typography.bodySmall) }
            line.detail?.let { Text("Details: $it", style = MaterialTheme.typography.bodySmall, color = MaterialTheme.colorScheme.onSurfaceVariant) }
        }
    }
}

/**
 * GrapheneOS's Network toggle can't be read with checkSelfPermission: apps are told
 * INTERNET is granted either way, so they don't crash. Opening a socket tells the truth.
 */
private fun networkAllowed(): Boolean = try {
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

/** Opens the folder picker at Download, where Android lets the user make a subfolder. */
private fun downloadsUri(): Uri =
    DocumentsContract.buildDocumentUri("com.android.externalstorage.documents", "primary:Download")
