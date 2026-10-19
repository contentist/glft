package io.github.contentist.glft

import android.content.Intent
import android.net.Uri
import android.os.Bundle
import androidx.activity.ComponentActivity
import androidx.activity.compose.setContent
import androidx.compose.foundation.background
import androidx.compose.foundation.clickable
import androidx.compose.foundation.isSystemInDarkTheme
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.safeDrawingPadding
import androidx.compose.material3.Button
import androidx.compose.material3.Card
import androidx.compose.material3.CircularProgressIndicator
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.material3.dynamicDarkColorScheme
import androidx.compose.material3.dynamicLightColorScheme
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.unit.dp
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.withContext
import uniffi.glft_ffi.LogLine
import uniffi.glft_ffi.errorLine

/**
 * glft in Android's Share menu: one file or many. A sheet shows what is being sent and the
 * paired computers; nothing is sent until one is tapped.
 */
class ShareActivity : ComponentActivity() {
    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        val uris = sharedUris(intent)
        setContent {
            val context = LocalContext.current
            val colors = if (isSystemInDarkTheme()) dynamicDarkColorScheme(context) else dynamicLightColorScheme(context)
            MaterialTheme(colorScheme = colors) {
                Box(
                    Modifier.fillMaxSize().background(Color.Black.copy(alpha = 0.4f)).clickable { finish() },
                    contentAlignment = Alignment.BottomCenter,
                ) {
                    // Taps on the sheet itself don't close it.
                    Box(Modifier.safeDrawingPadding().padding(12.dp).clickable(enabled = false) {}) {
                        ShareSheet(uris, onDone = { finish() })
                    }
                }
            }
        }
    }

    private fun sharedUris(intent: Intent): List<Uri> = when (intent.action) {
        Intent.ACTION_SEND -> listOfNotNull(intent.getParcelableExtra(Intent.EXTRA_STREAM, Uri::class.java))
        Intent.ACTION_SEND_MULTIPLE -> intent.getParcelableArrayListExtra(Intent.EXTRA_STREAM, Uri::class.java).orEmpty()
        else -> emptyList()
    }
}

@Composable
private fun ShareSheet(uris: List<Uri>, onDone: () -> Unit) {
    val context = LocalContext.current
    val app = context.glft
    val devices by app.devices.collectAsStateWithLifecycle()
    var outgoing by remember { mutableStateOf<Outgoing?>(null) }
    var problem by remember { mutableStateOf<LogLine?>(null) }
    LaunchedEffect(uris) {
        app.refreshDevices()
        if (uris.isEmpty()) {
            problem = errorLine("text-not-supported", "the share had no files (EXTRA_STREAM empty)")
            return@LaunchedEffect
        }
        try {
            outgoing = withContext(Dispatchers.IO) { context.contentResolver.outgoingFiles(uris) }
        } catch (e: Exception) {
            problem = errorLine("source-unreadable", e.message ?: e.toString())
        }
    }
    when {
        problem != null -> Message(problem!!, onDone)
        app.sendState.value is SendState.Running -> Message(errorLine("send-already-running", ""), onDone)
        outgoing == null -> Card(Modifier.fillMaxWidth()) {
            Box(Modifier.fillMaxWidth().padding(24.dp), contentAlignment = Alignment.Center) { CircularProgressIndicator() }
        }
        else -> SendSheet(
            outgoing = outgoing!!,
            devices = devices,
            onSend = { device, route ->
                if (!networkAllowed()) {
                    problem = errorLine("network-denied", "the Network permission is off for glft")
                } else {
                    Sends.start(context, device, outgoing!!, route)
                    onDone()
                }
            },
            onCancel = onDone,
        )
    }
}

/** An error from the catalogue: what happened, and what to do. */
@Composable
private fun Message(line: LogLine, onDone: () -> Unit) {
    Card(Modifier.fillMaxWidth()) {
        Column(Modifier.padding(16.dp), verticalArrangement = Arrangement.spacedBy(8.dp)) {
            Text(line.sentence, style = MaterialTheme.typography.titleSmall)
            line.nextStep?.let { Text("What to do: $it") }
            Row(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                Button(onClick = onDone) { Text("OK") }
                if (HelpActivity.has(line.id)) {
                    val context = LocalContext.current
                    TextButton(onClick = { HelpActivity.open(context, line.id!!) }) { Text("Help") }
                }
            }
        }
    }
}
