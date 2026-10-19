package io.github.contentist.glft

import android.content.ActivityNotFoundException
import android.content.ContentUris
import android.content.Context
import android.content.Intent
import android.provider.DocumentsContract
import android.provider.MediaStore
import android.widget.Toast
import uniffi.glft_ffi.ReceivedItem

/**
 * Getting to a received file in one tap: opening it in its usual app, or showing its folder
 * in Files. Works for files saved to Download/glft (the media store); for a folder the
 * person picked, the place is shown as text.
 */
object Places {
    private const val EXTERNAL_DOCS = "com.android.externalstorage.documents"

    private fun inDownloads(place: String) = place.startsWith("Download/") || place == "Download"

    /** Opens a file in its app, or a folder in Files. Null if that can't be done from here. */
    fun openIntent(context: Context, item: ReceivedItem): Intent? {
        if (!inDownloads(item.place)) return null
        if (item.isDir) return folderIntent(item.place)
        val collection = MediaStore.Downloads.getContentUri(MediaStore.VOLUME_EXTERNAL_PRIMARY)
        val dir = item.place.substringBeforeLast('/') + "/"
        val uri = context.contentResolver.query(
            collection,
            arrayOf(MediaStore.MediaColumns._ID),
            "${MediaStore.MediaColumns.RELATIVE_PATH}=? AND ${MediaStore.MediaColumns.DISPLAY_NAME}=?",
            arrayOf(dir, item.name),
            null,
        )?.use { c -> if (c.moveToFirst()) ContentUris.withAppendedId(collection, c.getLong(0)) else null } ?: return null
        val type = context.contentResolver.getType(uri) ?: "*/*"
        return Intent(Intent.ACTION_VIEW).setDataAndType(uri, type).addFlags(Intent.FLAG_GRANT_READ_URI_PERMISSION)
    }

    /** Files at `folder` ("Download/glft"), or at the folder holding a file's place. */
    fun folderIntent(folder: String): Intent? {
        if (!inDownloads(folder)) return null
        val uri = DocumentsContract.buildDocumentUri(EXTERNAL_DOCS, "primary:$folder")
        return Intent(Intent.ACTION_VIEW).setDataAndType(uri, DocumentsContract.Document.MIME_TYPE_DIR)
    }

    fun folderOf(item: ReceivedItem): String = item.place.substringBeforeLast('/', item.place)

    /** Starts [intent], or says where the file is if nothing can open it. */
    fun start(context: Context, intent: Intent?, place: String) {
        val shown = intent != null && try {
            context.startActivity(intent.addFlags(Intent.FLAG_ACTIVITY_NEW_TASK))
            true
        } catch (_: ActivityNotFoundException) {
            false
        }
        if (!shown) Toast.makeText(context, "It's in $place", Toast.LENGTH_LONG).show()
    }
}
