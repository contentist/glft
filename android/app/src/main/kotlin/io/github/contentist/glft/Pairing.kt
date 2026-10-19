package io.github.contentist.glft

import android.Manifest
import android.content.pm.PackageManager
import androidx.activity.compose.BackHandler
import androidx.activity.compose.rememberLauncherForActivityResult
import androidx.activity.result.contract.ActivityResultContracts
import androidx.camera.core.CameraSelector
import androidx.camera.core.ImageAnalysis
import androidx.camera.core.Preview
import androidx.camera.lifecycle.ProcessCameraProvider
import androidx.camera.view.PreviewView
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.aspectRatio
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.safeDrawingPadding
import androidx.compose.material3.Button
import androidx.compose.material3.Checkbox
import androidx.compose.material3.CircularProgressIndicator
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedButton
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.runtime.DisposableEffect
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberCoroutineScope
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.unit.dp
import androidx.compose.ui.viewinterop.AndroidView
import androidx.core.content.ContextCompat
import androidx.lifecycle.compose.LocalLifecycleOwner
import java.util.concurrent.Executors
import java.util.concurrent.atomic.AtomicBoolean
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.launch
import kotlinx.coroutines.withContext
import uniffi.glft_ffi.GlftException
import uniffi.glft_ffi.PairingPeer
import uniffi.glft_ffi.RememberChoice
import zxingcpp.BarcodeReader

/** Where the pairing screen is. */
private sealed interface Step {
    data object ExplainCamera : Step
    data object CameraRefused : Step
    data object Scan : Step
    data object Paste : Step
    data object Connecting : Step
    data class Ask(val peer: PairingPeer) : Step
    data class Waiting(val peer: PairingPeer) : Step
    data class Done(val lines: List<String>) : Step
    data class Failed(val headline: String, val nextStep: String, val detail: String) : Step
}

/**
 * Pairing with a computer: scan the code `glft pair` shows (or paste it), then
 * "Remember this device?", then both sides' answers.
 */
@Composable
fun PairScreen(onClose: () -> Unit) {
    val context = LocalContext.current
    val app = context.glft
    val scope = rememberCoroutineScope()
    val hasCamera = { context.checkSelfPermission(Manifest.permission.CAMERA) == PackageManager.PERMISSION_GRANTED }
    var step by remember { mutableStateOf<Step>(if (hasCamera()) Step.Scan else Step.ExplainCamera) }
    val cameraPermission = rememberLauncherForActivityResult(ActivityResultContracts.RequestPermission()) { granted ->
        step = if (granted) Step.Scan else Step.CameraRefused
    }

    fun fail(e: GlftException.Failed) {
        step = Step.Failed(e.headline, e.nextStep, e.detail)
    }

    fun connect(code: String) {
        step = Step.Connecting
        scope.launch {
            try {
                val peer = withContext(Dispatchers.IO) { app.node.pair(code, app.listener) }
                step = Step.Ask(peer)
            } catch (e: GlftException.Failed) {
                fail(e)
            }
        }
    }

    fun answer(peer: PairingPeer, choice: RememberChoice, autoAccept: Boolean) {
        step = Step.Waiting(peer)
        scope.launch {
            try {
                val theirs = withContext(Dispatchers.IO) { app.node.answerPairing(choice, autoAccept, app.listener) }
                app.refreshDevices()
                step = Step.Done(outcome(peer.name, choice, theirs))
            } catch (e: GlftException.Failed) {
                fail(e)
            }
        }
    }

    val close = {
        app.node.cancelPairing()
        onClose()
    }
    BackHandler(onBack = close)

    Column(
        Modifier.fillMaxSize().safeDrawingPadding().padding(20.dp),
        verticalArrangement = Arrangement.spacedBy(12.dp),
    ) {
        Text("Pair with a computer", style = MaterialTheme.typography.headlineSmall)
        when (val s = step) {
            Step.ExplainCamera -> {
                Text("On the computer, open glft and click \"Pair a phone…\" (or run `glft pair` in a terminal). It shows a QR code.")
                Text(
                    "glft needs the camera to read it. The camera is used only while this screen is open, " +
                        "and only to read the code.",
                )
                Row(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                    Button(onClick = { cameraPermission.launch(Manifest.permission.CAMERA) }) { Text("Continue") }
                    OutlinedButton(onClick = { step = Step.Paste }) { Text("Paste the code instead") }
                }
            }
            Step.CameraRefused -> {
                Text("Camera access is off for glft, so it can't read the QR code.")
                Text("You can paste the code instead: it is the line of text under the QR code on the computer.")
                Button(onClick = { step = Step.Paste }) { Text("Paste the code") }
            }
            Step.Scan -> {
                Text("Point the camera at the QR code on the computer.")
                QrScanner(Modifier.fillMaxWidth().aspectRatio(1f)) { code -> connect(code) }
                TextButton(onClick = { step = Step.Paste }) { Text("Paste the code instead") }
            }
            Step.Paste -> {
                var text by remember { mutableStateOf("") }
                Text("Paste the line under the QR code. It starts with GLFT1/.")
                OutlinedTextField(value = text, onValueChange = { text = it }, label = { Text("Pairing code") }, modifier = Modifier.fillMaxWidth())
                Button(enabled = text.isNotBlank(), onClick = { connect(text) }) { Text("Pair") }
            }
            Step.Connecting -> Busy("Connecting to the computer that shows the code…")
            is Step.Ask -> {
                var autoAccept by remember { mutableStateOf(false) }
                Text("\"${s.peer.name}\" read your code and its key matches.", style = MaterialTheme.typography.titleMedium)
                Text("Remember this device?")
                Text(
                    "A remembered device can offer you files whenever receiving is on. \"Just this time\" lasts until you switch receiving off.",
                    style = MaterialTheme.typography.bodySmall,
                )
                Row(verticalAlignment = Alignment.CenterVertically) {
                    Checkbox(checked = autoAccept, onCheckedChange = { autoAccept = it })
                    Text("Accept its files without asking (only if remembered)")
                }
                Row(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                    Button(onClick = { answer(s.peer, RememberChoice.YES, autoAccept) }) { Text("Remember") }
                    OutlinedButton(onClick = { answer(s.peer, RememberChoice.JUST_THIS_TIME, false) }) { Text("Just this time") }
                }
                TextButton(onClick = { answer(s.peer, RememberChoice.NO, false) }) { Text("Don't pair") }
            }
            is Step.Waiting -> Busy("Waiting for the person at \"${s.peer.name}\" to answer…")
            is Step.Done -> {
                s.lines.forEach { Text(it) }
                Button(onClick = onClose) { Text("Done") }
            }
            is Step.Failed -> {
                Text(s.headline, style = MaterialTheme.typography.titleMedium, color = MaterialTheme.colorScheme.error)
                Text(s.nextStep)
                Text("Details: ${s.detail}", style = MaterialTheme.typography.bodySmall, color = MaterialTheme.colorScheme.onSurfaceVariant)
                Row(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                    Button(onClick = { step = if (hasCamera()) Step.Scan else Step.Paste }) { Text("Try again") }
                    OutlinedButton(onClick = onClose) { Text("Close") }
                }
            }
        }
        if (step !is Step.Done && step !is Step.Failed) TextButton(onClick = close) { Text("Cancel") }
    }
}

private fun outcome(name: String, ours: RememberChoice, theirs: RememberChoice): List<String> = buildList {
    add(
        when (ours) {
            RememberChoice.YES -> "Paired with \"$name\". This phone remembers it."
            RememberChoice.JUST_THIS_TIME -> "Paired with \"$name\" until you switch receiving off."
            RememberChoice.NO -> "Not paired with \"$name\"."
        },
    )
    if (ours != RememberChoice.NO) {
        add(
            when (theirs) {
                RememberChoice.YES -> "\"$name\" remembers this phone too."
                RememberChoice.JUST_THIS_TIME -> "\"$name\" paired for its current session only."
                RememberChoice.NO -> "\"$name\" chose not to pair, so it won't know this phone."
            },
        )
    }
}

@Composable
private fun Busy(sentence: String) {
    Row(verticalAlignment = Alignment.CenterVertically, horizontalArrangement = Arrangement.spacedBy(12.dp)) {
        CircularProgressIndicator()
        Text(sentence)
    }
}

/** Camera preview that calls [onCode] once with the first glft pairing code it reads. */
@Composable
private fun QrScanner(modifier: Modifier, onCode: (String) -> Unit) {
    val context = LocalContext.current
    val lifecycleOwner = LocalLifecycleOwner.current
    val providerFuture = remember { ProcessCameraProvider.getInstance(context) }
    val executor = remember { Executors.newSingleThreadExecutor() }
    val found = remember { AtomicBoolean(false) }
    var notGlft by remember { mutableStateOf(false) }
    DisposableEffect(Unit) {
        onDispose {
            if (providerFuture.isDone) providerFuture.get().unbindAll()
            executor.shutdown()
        }
    }
    AndroidView(
        modifier = modifier,
        factory = { ctx ->
            PreviewView(ctx).also { view ->
                providerFuture.addListener({
                    val provider = providerFuture.get()
                    val preview = Preview.Builder().build().also { it.setSurfaceProvider(view.surfaceProvider) }
                    val reader = BarcodeReader(
                        BarcodeReader.Options(formats = setOf(BarcodeReader.Format.QR_CODE), tryInvert = true, tryHarder = true),
                    )
                    val analysis = ImageAnalysis.Builder().setBackpressureStrategy(ImageAnalysis.STRATEGY_KEEP_ONLY_LATEST).build()
                    analysis.setAnalyzer(executor) { image ->
                        image.use {
                            if (found.get()) return@use
                            val texts = reader.read(it).mapNotNull { r -> r.text }
                            val code = texts.firstOrNull { t -> t.trim().uppercase().startsWith("GLFT1/") }
                            if (code != null && found.compareAndSet(false, true)) {
                                view.post { onCode(code) }
                            } else if (texts.isNotEmpty()) {
                                view.post { notGlft = true }
                            }
                        }
                    }
                    provider.unbindAll()
                    provider.bindToLifecycle(lifecycleOwner, CameraSelector.DEFAULT_BACK_CAMERA, preview, analysis)
                }, ContextCompat.getMainExecutor(ctx))
            }
        },
    )
    if (notGlft) Text("That QR code is not a glft pairing code.", style = MaterialTheme.typography.bodySmall)
    LaunchedEffect(notGlft) {
        if (notGlft) {
            kotlinx.coroutines.delay(3000)
            notGlft = false
        }
    }
}
