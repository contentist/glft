package io.github.contentist.glft

import android.content.ContentResolver
import android.net.Uri
import android.provider.DocumentsContract
import android.provider.DocumentsContract.Document
import java.util.concurrent.ConcurrentHashMap
import uniffi.glft_ffi.OpenedPartial
import uniffi.glft_ffi.StorageBackend
import uniffi.glft_ffi.StorageException

/**
 * "Save somewhere else": a folder the user picked with Android's folder picker.
 *
 * Unlike the media store, this sees everything in the folder, so name clashes become
 * "Album (2)" reliably. Partials are created as ".name.glft-part" next to their final
 * place and renamed when verified.
 */
class SafStorage(private val resolver: ContentResolver, private val tree: Uri) : StorageBackend {
    private val rootDoc: Uri = DocumentsContract.buildDocumentUriUsingTree(tree, DocumentsContract.getTreeDocumentId(tree))
    // Called from several transfer threads at once.
    private val known = ConcurrentHashMap<String, Uri>().apply { put("", rootDoc) }
    val label: String = displayName(rootDoc) ?: "the chosen folder"

    override fun exists(rel: String): Boolean = guard { resolve(rel) != null }

    // Partials are named ".name.glft-part", so a document under the final name is finished.
    override fun published(rel: String): Boolean = exists(rel)

    @Synchronized
    override fun makeDir(rel: String) = guard {
        var path = ""
        var parent = rootDoc
        for (segment in rel.split('/')) {
            path = if (path.isEmpty()) segment else "$path/$segment"
            parent = resolve(path)
                ?: DocumentsContract.createDocument(resolver, parent, Document.MIME_TYPE_DIR, segment)
                    ?.also { known[path] = it }
                ?: throw IllegalStateException("could not create folder $path")
        }
    }

    override fun createPartial(rel: String): OpenedPartial = guard {
        val parentRel = rel.substringBeforeLast('/', "")
        val name = rel.substringAfterLast('/')
        val parent = resolve(parentRel) ?: throw IllegalStateException("folder $parentRel is missing")
        val doc = DocumentsContract.createDocument(resolver, parent, mimeType(name), ".$name.glft-part")
            ?: throw IllegalStateException("could not create $rel")
        val pfd = resolver.openFileDescriptor(doc, "rw") ?: throw IllegalStateException("could not open $rel")
        OpenedPartial(pfd.detachFd(), doc.toString())
    }

    override fun reopenPartial(rel: String, token: String): OpenedPartial? = guard {
        reopen(resolver, Uri.parse(token))?.let { OpenedPartial(it, token) }
    }

    override fun finish(rel: String, token: String) = guard {
        if (resolve(rel) != null) throw StorageException.Failed("$rel already exists", diskFull = false, alreadyExists = true)
        val renamed = DocumentsContract.renameDocument(resolver, Uri.parse(token), rel.substringAfterLast('/'))
            ?: throw IllegalStateException("could not rename $rel")
        known[rel] = renamed
        Unit
    }

    override fun discard(token: String) = guard {
        DocumentsContract.deleteDocument(resolver, Uri.parse(token))
        Unit
    }

    override fun describe(rel: String): String {
        val parent = rel.substringBeforeLast('/', "")
        return if (parent.isEmpty()) label else "$label/$parent"
    }

    /** Finds the document for a relative path, walking down from the last known folder. */
    private fun resolve(rel: String): Uri? {
        known[rel]?.let { return it }
        val parentRel = rel.substringBeforeLast('/', "")
        val parent = resolve(parentRel) ?: return null
        val name = rel.substringAfterLast('/')
        val children = DocumentsContract.buildChildDocumentsUriUsingTree(tree, DocumentsContract.getDocumentId(parent))
        resolver.query(children, arrayOf(Document.COLUMN_DOCUMENT_ID, Document.COLUMN_DISPLAY_NAME), null, null, null)?.use { c ->
            while (c.moveToNext()) {
                if (c.getString(1) == name) {
                    return DocumentsContract.buildDocumentUriUsingTree(tree, c.getString(0)).also { known[rel] = it }
                }
            }
        }
        return null
    }

    private fun displayName(doc: Uri): String? =
        resolver.query(doc, arrayOf(Document.COLUMN_DISPLAY_NAME), null, null, null)?.use { c ->
            if (c.moveToFirst()) c.getString(0) else null
        }
}
