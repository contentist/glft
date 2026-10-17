package io.github.contentist.glft

import android.app.job.JobParameters
import android.app.job.JobService
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.SupervisorJob
import kotlinx.coroutines.cancel
import kotlinx.coroutines.delay
import kotlinx.coroutines.launch
import uniffi.glft_ffi.GlftException
import kotlin.concurrent.thread

/**
 * A send the person started, run as a user-initiated data transfer job: Android keeps it
 * going with glft in the background and shows its notification. If Android or the person
 * stops it, sending the same files again continues where it stopped.
 */
class SendJob : JobService() {
    private var scope: CoroutineScope? = null

    override fun onStartJob(params: JobParameters): Boolean {
        val app = glft
        val queued = app.takeQueuedSend()
        if (queued == null) {
            // The process was restarted and the choice is gone; nothing to send.
            app.note("A send was interrupted before it started. Share the files again to send them.", isError = true)
            return false
        }
        val device = queued.device
        val outgoing = queued.outgoing
        val running = SendState.Running(device.name, outgoing.summary, 0, outgoing.bytes)
        app.setSendState(running)
        setNotification(params, Notifications.SEND_ID, Notifications.sending(this, running), JOB_END_NOTIFICATION_POLICY_DETACH)
        val scope = CoroutineScope(SupervisorJob() + Dispatchers.Default).also { scope = it }
        // Progress, at most once a second: Android rate-limits notification updates.
        scope.launch {
            var shown = running
            while (true) {
                val now = app.sendState.value
                if (now is SendState.Running && now != shown) {
                    Notifications.update(this@SendJob, now)
                    updateTransferredNetworkBytes(params, 0, now.done)
                    shown = now
                }
                delay(1000)
            }
        }
        thread(name = "glft-send") {
            val end = try {
                val r = app.node.send(device.id, outgoing.items, UriSource(contentResolver, outgoing.uris), queued.route, app.listener)
                SendState.Done(r.peer, r.summary, r.files.toInt(), r.secs, r.via)
            } catch (e: GlftException.Failed) {
                app.fail(e)
                SendState.Failed(device.name, outgoing.summary, e.headline, e.nextStep, e.detail)
            }
            scope.cancel()
            app.setSendState(end)
            Notifications.update(this, end)
            app.refreshDevices()
            jobFinished(params, false)
        }
        return true
    }

    override fun onStopJob(params: JobParameters): Boolean {
        // Stopped by Android or from Task Manager: end the send; what arrived is kept.
        glft.node.cancelSend()
        scope?.cancel()
        return false
    }
}
