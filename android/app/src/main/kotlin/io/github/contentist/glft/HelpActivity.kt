package io.github.contentist.glft

import android.content.Context
import android.content.Intent
import android.os.Bundle
import androidx.activity.ComponentActivity
import androidx.activity.compose.setContent
import androidx.compose.foundation.isSystemInDarkTheme
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.safeDrawingPadding
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.verticalScroll
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Surface
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.material3.dynamicDarkColorScheme
import androidx.compose.material3.dynamicLightColorScheme
import androidx.compose.runtime.Composable
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.text.AnnotatedString
import androidx.compose.ui.text.SpanStyle
import androidx.compose.ui.text.buildAnnotatedString
import androidx.compose.ui.text.font.FontFamily
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.withStyle
import androidx.compose.ui.unit.dp
import uniffi.glft_ffi.helpPage

/**
 * One help page, built into the app from `docs/help/<id>.md`: what happened, why, and what
 * to do. Opened from an error's "Help" button anywhere in glft; Back closes it.
 */
class HelpActivity : ComponentActivity() {
    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        val page = intent.getStringExtra(EXTRA_ID)?.let { helpPage(it) }
        if (page == null) {
            finish()
            return
        }
        setContent {
            val context = LocalContext.current
            val colors = if (isSystemInDarkTheme()) dynamicDarkColorScheme(context) else dynamicLightColorScheme(context)
            MaterialTheme(colorScheme = colors) {
                Surface(Modifier.fillMaxSize()) { HelpPage(page, onClose = { finish() }) }
            }
        }
    }

    companion object {
        private const val EXTRA_ID = "id"

        /** Whether there is a page for [id], so a Help button is worth showing. */
        fun has(id: String?): Boolean = id != null && helpPage(id) != null

        fun open(context: Context, id: String) {
            context.startActivity(Intent(context, HelpActivity::class.java).putExtra(EXTRA_ID, id))
        }
    }
}

@Composable
private fun HelpPage(markdown: String, onClose: () -> Unit) {
    val lines = markdown.lines()
    val title = lines.firstOrNull()?.removePrefix("# ") ?: "Help"
    Column(
        Modifier.fillMaxSize().safeDrawingPadding().padding(horizontal = 20.dp).verticalScroll(rememberScrollState()),
        verticalArrangement = Arrangement.spacedBy(6.dp),
    ) {
        Row(Modifier.padding(vertical = 12.dp)) {
            Text(title, style = MaterialTheme.typography.headlineSmall, modifier = Modifier.weight(1f))
            TextButton(onClick = onClose) { Text("Close") }
        }
        // Paragraphs are joined; headings, list items and code lines stand alone.
        var paragraph = StringBuilder()
        val blocks = mutableListOf<Pair<String, String>>()
        fun flush() {
            if (paragraph.isNotBlank()) blocks += "p" to paragraph.toString().trim()
            paragraph = StringBuilder()
        }
        var inCode = false
        for (line in lines.drop(1)) {
            when {
                line.startsWith("```") -> { flush(); inCode = !inCode }
                inCode -> blocks += "code" to line
                line.startsWith("## ") -> { flush(); blocks += "h" to line.removePrefix("## ") }
                line.isBlank() -> flush()
                line.trimStart().startsWith("- ") || line.trimStart().firstOrNull()?.isDigit() == true && line.trimStart().contains(". ") -> {
                    flush(); paragraph.append(line.trim())
                }
                else -> paragraph.append(' ').append(line.trim())
            }
        }
        flush()
        for ((kind, text) in blocks) {
            when (kind) {
                "h" -> Text(text, style = MaterialTheme.typography.titleMedium, modifier = Modifier.padding(top = 10.dp))
                "code" -> Text(text, fontFamily = FontFamily.Monospace, style = MaterialTheme.typography.bodySmall)
                else -> Text(inline(if (text.startsWith("- ")) "• " + text.drop(2) else text), style = MaterialTheme.typography.bodyMedium)
            }
        }
        Text("", Modifier.padding(12.dp))
    }
}

/** `**bold**` and `code` within a paragraph. */
private fun inline(text: String): AnnotatedString = buildAnnotatedString {
    text.split("**").forEachIndexed { i, part ->
        part.split('`').forEachIndexed { j, piece ->
            val style = SpanStyle(
                fontWeight = if (i % 2 == 1) FontWeight.Bold else null,
                fontFamily = if (j % 2 == 1) FontFamily.Monospace else null,
            )
            withStyle(style) { append(piece) }
        }
    }
}
