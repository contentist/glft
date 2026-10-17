package io.github.contentist.glft

import android.content.Intent
import androidx.activity.compose.BackHandler
import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.safeDrawingPadding
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.items
import androidx.compose.material3.Button
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.unit.dp
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import uniffi.glft_ffi.LogLine
import uniffi.glft_ffi.coreVersion
import java.text.SimpleDateFormat
import java.util.Date
import java.util.Locale

/** "What's happening": every step glft took, newest first, and a way to share it for help. */
@Composable
fun LogScreen(onClose: () -> Unit) {
    val context = LocalContext.current
    val log by context.glft.log.collectAsStateWithLifecycle()
    BackHandler(onBack = onClose)
    Column(Modifier.fillMaxSize().safeDrawingPadding().padding(horizontal = 20.dp)) {
        Row(Modifier.padding(vertical = 12.dp), horizontalArrangement = Arrangement.spacedBy(8.dp)) {
            Text("What's happening", style = MaterialTheme.typography.headlineSmall, modifier = Modifier.weight(1f))
            TextButton(onClick = onClose) { Text("Close") }
        }
        Text("Tap an error for its details.", style = MaterialTheme.typography.bodySmall)
        Button(
            enabled = log.isNotEmpty(),
            onClick = {
                val send = Intent(Intent.ACTION_SEND)
                    .setType("text/plain")
                    .putExtra(Intent.EXTRA_SUBJECT, "glft log")
                    .putExtra(Intent.EXTRA_TEXT, logText(log))
                context.startActivity(Intent.createChooser(send, "Share glft's log"))
            },
            modifier = Modifier.padding(vertical = 8.dp),
        ) { Text("Share as text") }
        LazyColumn(verticalArrangement = Arrangement.spacedBy(8.dp)) {
            items(log.asReversed()) { line -> LogRow(line, expandable = true) }
            item { Spacer(Modifier.padding(12.dp)) }
        }
    }
}

/** One log line: time and sentence; an error adds its next step, and its details on tap. */
@Composable
fun LogRow(line: LogLine, expandable: Boolean = false) {
    var open by remember { mutableStateOf(!expandable) }
    val time = remember(line.atUnixMs) { SimpleDateFormat("HH:mm:ss", Locale.getDefault()).format(Date(line.atUnixMs)) }
    val color = if (line.isError) MaterialTheme.colorScheme.error else MaterialTheme.colorScheme.onSurface
    Row(if (line.detail != null && expandable) Modifier.clickable { open = !open } else Modifier) {
        Text(time, style = MaterialTheme.typography.bodySmall, color = MaterialTheme.colorScheme.onSurfaceVariant)
        Spacer(Modifier.width(10.dp))
        Column {
            Text(line.sentence, style = MaterialTheme.typography.bodyMedium, color = color)
            line.nextStep?.let { Text(it, style = MaterialTheme.typography.bodySmall) }
            if (open) {
                line.detail?.let { Text("Details: $it", style = MaterialTheme.typography.bodySmall, color = MaterialTheme.colorScheme.onSurfaceVariant) }
            } else if (line.detail != null) {
                Text("Details…", style = MaterialTheme.typography.bodySmall, color = MaterialTheme.colorScheme.onSurfaceVariant)
            }
        }
    }
}

/** The log as plain text, oldest first, for a help request. */
private fun logText(log: List<LogLine>): String {
    val time = SimpleDateFormat("yyyy-MM-dd HH:mm:ss", Locale.getDefault())
    return buildString {
        appendLine("glft ${coreVersion()} on ${android.os.Build.MODEL}, Android ${android.os.Build.VERSION.RELEASE}")
        log.forEach { line ->
            appendLine("${time.format(Date(line.atUnixMs))}  ${line.sentence}")
            line.nextStep?.let { appendLine("    What to do: $it") }
            line.detail?.let { appendLine("    Details: $it") }
        }
    }
}
