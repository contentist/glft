package io.github.contentist.glft

import android.content.BroadcastReceiver
import android.content.Context
import android.content.Intent
import uniffi.glft_ffi.GlftException

/**
 * Debug builds only. Adds a device to the allowed list from adb:
 *
 *     adb shell am broadcast -a io.github.contentist.glft.DEBUG_ALLOW \
 *         -n io.github.contentist.glft/.DebugAllowReceiver --es id <device id>
 *
 * QR pairing (Day 3) replaces this.
 */
class DebugAllowReceiver : BroadcastReceiver() {
    override fun onReceive(context: Context, intent: Intent) {
        val id = intent.getStringExtra("id") ?: return
        val app = context.glft
        try {
            app.node.allowDevice(id)
            app.note("Allowed device ${id.take(10)} (added over USB, debug build)")
        } catch (e: GlftException.Failed) {
            app.fail(e)
        }
    }
}
