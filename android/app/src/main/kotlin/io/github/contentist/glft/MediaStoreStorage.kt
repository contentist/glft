package io.github.contentist.glft

import android.content.ContentResolver
import android.content.ContentValues
import android.net.Uri
import android.os.Bundle
import android.os.Environment
import android.provider.MediaStore
import android.webkit.MimeTypeMap
import java.io.File
import uniffi.glft_ffi.OpenedPartial
import uniffi.glft_ffi.StorageBackend
import uniffi.glft_ffi.StorageException

/**
 * The default receive location: Download/glft, written through the media store.
 *
 * Needs no permission and no setup. Files are inserted with IS_PENDING set, so other
 * apps don't see them until they are verified and published; music players then pick
 * them up straight away. The media store only shows this app its own files, so a name
 * clash with something another app (or an earlier install) put there can't be seen.
 */
class MediaStoreStorage(private val resolver: ContentResolver) : StorageBackend {
    private val collection: Uri = MediaStore.Downloads.getContentUri(MediaStore.VOLUME_EXTERNAL_PRIMARY)
    private val downloads: File = Environment.getExternalStoragePublicDirectory(Environment.DIRECTORY_DOWNLOADS)

    override fun exists(rel: String): Boolean = guard {
        val (dir, name) = split(rel)
        File(downloads, "$FOLDER/$rel").exists() ||
            count("${MediaStore.MediaColumns.RELATIVE_PATH}=? AND ${MediaStore.MediaColumns.DISPLAY_NAME}=?", arrayOf(dir, name)) > 0 ||
            count("${MediaStore.MediaColumns.RELATIVE_PATH} LIKE ?", arrayOf("$BASE/$rel/%")) > 0
    }

    // The media store creates folders as files are inserted; making them here keeps
    // empty folders, where the platform allows it.
    override fun makeDir(rel: String) = guard {
        File(downloads, "$FOLDER/$rel").mkdirs()
        Unit
    }

    override fun createPartial(rel: String): OpenedPartial = guard {
        val (dir, name) = split(rel)
        val values = ContentValues().apply {
            put(MediaStore.MediaColumns.DISPLAY_NAME, name)
            put(MediaStore.MediaColumns.RELATIVE_PATH, dir)
            put(MediaStore.MediaColumns.MIME_TYPE, mimeType(name))
            put(MediaStore.MediaColumns.IS_PENDING, 1)
        }
        val uri = resolver.insert(collection, values) ?: throw IllegalStateException("media store refused $rel")
        val pfd = resolver.openFileDescriptor(uri, "rw") ?: throw IllegalStateException("could not open $uri")
        OpenedPartial(pfd.detachFd(), uri.toString())
    }

    override fun finish(rel: String, token: String) = guard {
        val values = ContentValues().apply { put(MediaStore.MediaColumns.IS_PENDING, 0) }
        resolver.update(Uri.parse(token), values, null, null)
        Unit
    }

    override fun discard(token: String) = guard {
        resolver.delete(Uri.parse(token), null, null)
        Unit
    }

    override fun describe(rel: String): String {
        val parent = rel.substringBeforeLast('/', "")
        return if (parent.isEmpty()) BASE else "$BASE/$parent"
    }

    /** "Album/CD1/a.flac" → ("Download/glft/Album/CD1/", "a.flac") */
    private fun split(rel: String): Pair<String, String> {
        val parent = rel.substringBeforeLast('/', "")
        val dir = if (parent.isEmpty()) "$BASE/" else "$BASE/$parent/"
        return dir to rel.substringAfterLast('/')
    }

    private fun count(selection: String, args: Array<String>): Int {
        val query = Bundle().apply {
            putString(ContentResolver.QUERY_ARG_SQL_SELECTION, selection)
            putStringArray(ContentResolver.QUERY_ARG_SQL_SELECTION_ARGS, args)
            putInt(MediaStore.QUERY_ARG_MATCH_PENDING, MediaStore.MATCH_INCLUDE)
            putInt(ContentResolver.QUERY_ARG_LIMIT, 1)
        }
        return resolver.query(collection, arrayOf(MediaStore.MediaColumns._ID), query, null)?.use { it.count } ?: 0
    }

    companion object {
        const val FOLDER = "glft"
        const val BASE = "Download/$FOLDER"
    }
}

fun mimeType(name: String): String =
    MimeTypeMap.getSingleton().getMimeTypeFromExtension(name.substringAfterLast('.', "").lowercase())
        ?: "application/octet-stream"

/** Runs a storage call, turning any failure into the error Rust understands. */
inline fun <T> guard(block: () -> T): T = try {
    block()
} catch (e: StorageException) {
    throw e
} catch (e: Exception) {
    val text = e.toString()
    throw StorageException.Failed(
        reason = text,
        diskFull = text.contains("ENOSPC") || text.contains("No space left"),
        alreadyExists = text.contains("EEXIST") || text.contains("already exists"),
    )
}
