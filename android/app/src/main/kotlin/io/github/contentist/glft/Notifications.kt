package io.github.contentist.glft

import android.app.Notification
import android.app.NotificationChannel
import android.app.NotificationManager
import android.app.PendingIntent
import android.content.BroadcastReceiver
import android.content.Context
import android.content.Intent
import uniffi.glft_ffi.OfferInfo

/**
 * Three channels: the always-visible "receiving" notice (design rule 6: nothing
 * listens unseen), offers that need an answer, and offers that were missed.
 */
object Notifications {
    const val RECEIVING_CHANNEL = "receiving"
    private const val OFFER_CHANNEL = "offers"
    private const val MISSED_CHANNEL = "missed"
    const val RECEIVING_ID = 1
    const val EXTRA_OFFER = "offer"

    fun createChannels(context: Context) {
        val nm = context.getSystemService(NotificationManager::class.java)
        nm.createNotificationChannels(
            listOf(
                NotificationChannel(RECEIVING_CHANNEL, "Receiving", NotificationManager.IMPORTANCE_LOW).apply {
                    description = "Shown whenever glft can receive files"
                },
                NotificationChannel(OFFER_CHANNEL, "Incoming files", NotificationManager.IMPORTANCE_HIGH).apply {
                    description = "A paired device wants to send you files"
                },
                NotificationChannel(MISSED_CHANNEL, "Missed offers", NotificationManager.IMPORTANCE_DEFAULT),
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
