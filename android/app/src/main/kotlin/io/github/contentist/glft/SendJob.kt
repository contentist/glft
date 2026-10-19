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
 * going with glft in the background and shows its notification. If Android stops it (a
 * network change does), it runs again by itself and continues where it stopped; only the
 * person's Cancel ends it.
 */
class SendJob : JobService() {
    /** One run of the job. Android may start a new run (after a network change) before the
     *  old run's send has noticed it was stopped, so each run keeps its own state and an
     *  old run never touches a newer one. */
    private class Run(val params: JobParameters, val scope: CoroutineScope) {
        /** Set when Android, not the person, stopped this run; another run continues. */
        @Volatile
        var paused = false
    }

    @Volatile
    private var current: Run? = null

    override fun onStartJob(params: JobParameters): Boolean {
        val app = glft
        val queued = app.queuedSend()
        if (queued == null) {
            // The process was restarted and the choice is gone; nothing to send.
            app.fail("send-not-started", "the job ran after glft was restarted, so the files chosen were gone")
            return false
        }
        val device = queued.device
        val outgoing = queued.outgoing
        val running = SendState.Running(device.name, outgoing.summary, 0, outgoing.bytes)
        app.setSendState(running)
        setNotification(params, Notifications.SEND_ID, Notifications.sending(this, running), JOB_END_NOTIFICATION_POLICY_DETACH)
        val run = Run(params, CoroutineScope(SupervisorJob() + Dispatchers.Default))
        current = run
        // Progress, at most once a second: Android rate-limits notification updates.
        run.scope.launch {
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
                if (run.paused) {
                    // The next run continues with the same files; this one just ends.
                    run.scope.cancel()
                    return@thread
                }
                app.fail(e)
                SendState.Failed(device.name, outgoing.summary, e.headline, e.nextStep, e.detail)
            }
            run.scope.cancel()
            if (run.paused) return@thread
            app.clearQueuedSend()
            app.setSendState(end)
            Notifications.update(this, end)
            app.refreshDevices()
            jobFinished(params, false)
        }
        return true
    }

    override fun onStopJob(params: JobParameters): Boolean {
        val run = current ?: return false
        // Stopped from Task Manager: the person's choice, as Cancel is. Anything else is
        // Android's (a network change, the device getting busy): pause and run again.
        val byPerson = params.stopReason == JobParameters.STOP_REASON_USER
        val app = glft
        run.paused = !byPerson
        if (run.paused) {
            val to = app.queuedSend()?.device?.name ?: "the computer"
            val why = if (params.stopReason == JobParameters.STOP_REASON_CONSTRAINT_CONNECTIVITY) "The network changed" else "Android needed to pause it"
            app.note("$why, so the send to $to paused; it continues by itself (Android's reason: ${params.stopReason})")
        }
        app.node.cancelSend()
        run.scope.cancel()
        return run.paused
    }
}
