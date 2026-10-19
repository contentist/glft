package io.github.contentist.glft

import android.app.Notification
import android.app.NotificationChannel
import android.app.NotificationManager
import android.app.PendingIntent
import android.content.BroadcastReceiver
import android.content.Context
import android.content.Intent
import uniffi.glft_ffi.OfferInfo
import uniffi.glft_ffi.ReceivedInfo

/**
 * Three channels: the always-visible "receiving" notice (design rule 6: nothing
 * listens unseen), offers that need an answer, and offers that were missed.
 */
object Notifications {
    const val RECEIVING_CHANNEL = "receiving"
    private const val OFFER_CHANNEL = "offers"
    private const val MISSED_CHANNEL = "missed"
    private const val SEND_CHANNEL = "sending"
    /** "arrivals", not "received": that channel, without pop-ups, may exist from before,
     *  and a channel's importance can't change once created. */
    private const val RECEIVED_CHANNEL = "arrivals"
    const val RECEIVING_ID = 1
    const val EXTRA_JOB = "job"
    const val EXTRA_OFFER = "offer"

    fun createChannels(context: Context) {
        val nm = context.getSystemService(NotificationManager::class.java)
        // The first build of "Received files" didn't pop up; its replacement is "arrivals".
        nm.deleteNotificationChannel("received")
        nm.createNotificationChannels(
            listOf(
                NotificationChannel(RECEIVING_CHANNEL, "Receiving", NotificationManager.IMPORTANCE_LOW).apply {
                    description = "Shown whenever glft can receive files"
                },
                NotificationChannel(OFFER_CHANNEL, "Incoming files", NotificationManager.IMPORTANCE_HIGH).apply {
                    description = "A paired device wants to send you files"
                },
                NotificationChannel(MISSED_CHANNEL, "Missed offers", NotificationManager.IMPORTANCE_DEFAULT),
                // Pops up: files accepted without asking would otherwise arrive unnoticed.
                NotificationChannel(RECEIVED_CHANNEL, "Received files", NotificationManager.IMPORTANCE_HIGH).apply {
                    description = "Files that arrived, and where they are"
                },
                NotificationChannel(SEND_CHANNEL, "Sending", NotificationManager.IMPORTANCE_LOW).apply {
                    description = "Progress of files this phone is sending, and how it ended"
                },
            ),
        )
    }

    fun receiving(context: Context, saveTo: String): Notification {
        val stop = PendingIntent.getService(
            context, 0,
            Intent(context, ReceiveService::class.java).setAction(ReceiveService.ACTION_STOP),
            PendingIntent.FLAG_IMMUTABLE,
        )
        return Notification.Builder(context, RECEIVING_CHANNEL)
            .setSmallIcon(android.R.drawable.stat_sys_download)
            .setContentTitle("glft is receiving")
            .setContentText("Paired devices can send you files. Saving to $saveTo.")
            .setContentIntent(openApp(context))
            .setOngoing(true)
            .addAction(Notification.Action.Builder(null, "Stop", stop).build())
            .build()
    }

    /** The send job's notification while it runs. */
    fun sending(context: Context, s: SendState.Running, jobId: Int): Notification {
        val cancel = cancelIntent(context, jobId)
        val pct = if (s.total > 0) (s.done * 100 / s.total).toInt() else 0
        return Notification.Builder(context, SEND_CHANNEL)
            .setSmallIcon(android.R.drawable.stat_sys_upload)
            .setContentTitle("Sending to ${s.peer}")
            .setContentText(if (s.done > 0) "${s.summary} · $pct%" else "${s.summary} · reading the files")
            .setProgress(100, pct, s.done == 0L)
            .setContentIntent(openApp(context))
            .setOngoing(true)
            .addAction(Notification.Action.Builder(null, "Cancel", cancel).build())
            .build()
    }

    /** A send chosen while another runs: it starts when that one ends. */
    fun waiting(context: Context, send: GlftApp.QueuedSend): Notification =
        Notification.Builder(context, SEND_CHANNEL)
            .setSmallIcon(android.R.drawable.stat_sys_upload)
            .setContentTitle("Waiting to send to ${send.device.name}")
            .setContentText("${send.outgoing.summary} · after the current send")
            .setContentIntent(openApp(context))
            .setOngoing(true)
            .addAction(Notification.Action.Builder(null, "Cancel", cancelIntent(context, send.jobId)).build())
            .build()

    private fun cancelIntent(context: Context, jobId: Int): PendingIntent = PendingIntent.getBroadcast(
        context, jobId, Intent(context, SendActionReceiver::class.java).setAction("cancel").putExtra(EXTRA_JOB, jobId), PendingIntent.FLAG_IMMUTABLE,
    )

    /** Shows a send's progress or how it ended, in its job's notification. */
    fun update(context: Context, s: SendState, jobId: Int) {
        val n = when (s) {
            is SendState.Running -> sending(context, s, jobId)
            is SendState.Done -> Notification.Builder(context, SEND_CHANNEL)
                .setSmallIcon(android.R.drawable.stat_sys_upload_done)
                .setContentTitle("Sent to ${s.peer}" + (s.via?.let { " over $it" } ?: ""))
                .setContentText(doneText(s))
                .setStyle(Notification.BigTextStyle().bigText(doneText(s)))
                .setContentIntent(openApp(context))
                .setAutoCancel(true)
                .build()
            is SendState.Failed -> Notification.Builder(context, SEND_CHANNEL)
                .setSmallIcon(android.R.drawable.stat_notify_error)
                .setContentTitle(s.headline)
                .setContentText(s.nextStep)
                .setStyle(Notification.BigTextStyle().bigText(s.nextStep))
                .setContentIntent(openApp(context))
                .setAutoCancel(true)
                .build()
            SendState.Idle -> return
        }
        nm(context).notify(jobId, n)
    }

    fun showOffer(context: Context, offer: OfferInfo) {
        val n = Notification.Builder(context, OFFER_CHANNEL)
            .setSmallIcon(android.R.drawable.stat_sys_download)
            .setContentTitle("${offer.peer} wants to send you files")
            .setContentText("${offer.summary}, to ${offer.dest}")
            .setContentIntent(openApp(context))
            .setOngoing(true)
            .setCategory(Notification.CATEGORY_MESSAGE)
            .addAction(Notification.Action.Builder(null, "Accept", answer(context, offer.id, true)).build())
            .addAction(Notification.Action.Builder(null, "Decline", answer(context, offer.id, false)).build())
            .build()
        nm(context).notify(offerTag(offer.id), 0, n)
    }

    fun cancelOffer(context: Context, id: ULong) = nm(context).cancel(offerTag(id), 0)

    /** "Received from Pixel 8 Pro: screenshot.png is in Download/glft", with Open and Show folder. */
    fun showReceived(context: Context, r: ReceivedInfo) {
        val landed = r.landed.ifEmpty { "${r.summary} is in ${r.dest}" }
        val one = r.items.singleOrNull()
        val open = one?.let { Places.openIntent(context, it) }
        val folder = r.items.firstOrNull()?.let { Places.folderIntent(if (it.isDir) it.place else Places.folderOf(it)) }
        fun pending(intent: Intent, code: Int) =
            PendingIntent.getActivity(context, code, intent, PendingIntent.FLAG_IMMUTABLE or PendingIntent.FLAG_UPDATE_CURRENT)
        val code = (r.atUnixMs and 0xffffff).toInt()
        val builder = Notification.Builder(context, RECEIVED_CHANNEL)
            .setSmallIcon(android.R.drawable.stat_sys_download_done)
            .setContentTitle("Received from ${r.peer}")
            .setContentText("$landed. Every file verified.")
            .setStyle(Notification.BigTextStyle().bigText("$landed. Every file verified."))
            .setContentIntent(open?.let { pending(it, code) } ?: folder?.let { pending(it, code) } ?: openApp(context))
            .setAutoCancel(true)
        open?.let { builder.addAction(Notification.Action.Builder(null, "Open", pending(it, code + 1)).build()) }
        folder?.let { builder.addAction(Notification.Action.Builder(null, "Show folder", pending(it, code + 2)).build()) }
        nm(context).notify("received-${r.atUnixMs}", 0, builder.build())
    }

    fun showMissed(context: Context, offer: OfferInfo) {
        val n = Notification.Builder(context, MISSED_CHANNEL)
            .setSmallIcon(android.R.drawable.stat_notify_error)
            .setContentTitle("Missed: ${offer.peer} stopped waiting")
            .setContentText("${offer.summary} was not received. Ask them to send again.")
            .setContentIntent(openApp(context))
            .setAutoCancel(true)
            .build()
        nm(context).notify("missed-${offer.id}", 0, n)
    }

    private fun answer(context: Context, id: ULong, accept: Boolean): PendingIntent {
        val intent = Intent(context, OfferActionReceiver::class.java)
            .setAction(if (accept) "accept" else "decline")
            .putExtra(EXTRA_OFFER, id.toLong())
        return PendingIntent.getBroadcast(context, (id.toInt() shl 1) or (if (accept) 1 else 0), intent, PendingIntent.FLAG_IMMUTABLE)
    }

    private fun openApp(context: Context): PendingIntent = PendingIntent.getActivity(
        context, 0, Intent(context, MainActivity::class.java).addFlags(Intent.FLAG_ACTIVITY_SINGLE_TOP), PendingIntent.FLAG_IMMUTABLE,
    )

    private fun offerTag(id: ULong) = "offer-$id"
    private fun nm(context: Context) = context.getSystemService(NotificationManager::class.java)
}

/** Accept / Decline buttons on an offer notification. */
class OfferActionReceiver : BroadcastReceiver() {
    override fun onReceive(context: Context, intent: Intent) {
        val id = intent.getLongExtra(Notifications.EXTRA_OFFER, -1)
        if (id < 0) return
        context.glft.node.answerOffer(id.toULong(), intent.action == "accept")
    }
}

/** Cancel on the send notification. */
class SendActionReceiver : BroadcastReceiver() {
    override fun onReceive(context: Context, intent: Intent) {
        val jobId = intent.getIntExtra(Notifications.EXTRA_JOB, -1)
        if (context.glft.isNext(jobId)) context.glft.node.cancelSend() else Sends.cancelWaiting(context, jobId)
    }
}
