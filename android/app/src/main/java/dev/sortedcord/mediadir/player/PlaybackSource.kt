package dev.sortedcord.mediadir.player

import dev.sortedcord.mediadir.data.Json
import dev.sortedcord.mediadir.data.Settings
import dev.sortedcord.mediadir.domain.QueueEntry
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.withContext
import org.json.JSONObject
import java.net.HttpURLConnection
import java.net.URL

/** A /playback queue entry plus the v2.1 Kodi fields the player needs (stack `part`, per-file `edition`). */
data class PEntry(val entry: QueueEntry, val part: Int?, val editions: Map<String, String?>) {
    val item get() = entry.item
    val files get() = entry.files
    fun edition(fileId: String?) = fileId?.let { editions[it] }
}

data class PQueue(val startIndex: Int, val queue: List<PEntry>)

/** Fetches /api/items/{id}/playback directly so the player can read fields the shared domain models may not expose yet. */
object PlaybackSource {
    suspend fun load(settings: Settings, id: String): PQueue = withContext(Dispatchers.IO) {
        val text = fetch(settings.server + "/api/items/$id/playback")
        val o = JSONObject(text)
        val arr = o.optJSONArray("queue")
        val q = (0 until (arr?.length() ?: 0)).mapNotNull { i ->
            val e = arr!!.optJSONObject(i) ?: return@mapNotNull null
            val filesJ = e.optJSONArray("files")
            val fileObjs = (0 until (filesJ?.length() ?: 0)).mapNotNull { filesJ!!.optJSONObject(it) }
            val files = fileObjs.map { Json.file(it) }
            val editions = fileObjs.associate { f ->
                f.optString("id") to (if (f.has("edition") && !f.isNull("edition")) f.optString("edition").ifBlank { null } else null)
            }
            var part: Int? = if (e.has("part") && !e.isNull("part")) e.optInt("part") else null
            if (part == null) part = fileObjs.firstOrNull()?.let { if (it.has("part") && !it.isNull("part")) it.optInt("part") else null }
            PEntry(QueueEntry(Json.summary(e.optJSONObject("item") ?: JSONObject()), files), part, editions)
        }
        PQueue(o.optInt("start_index"), q)
    }

    private fun fetch(url: String, attempt: Int = 0): String {
        val c = URL(url).openConnection() as HttpURLConnection
        return try {
            c.connectTimeout = 5000; c.readTimeout = 20000
            c.setRequestProperty("Accept", "application/json")
            val code = c.responseCode
            val body = (if (code >= 400) c.errorStream else c.inputStream)?.bufferedReader()?.use { it.readText() } ?: ""
            if (code >= 400) throw java.io.IOException("HTTP $code")
            body
        } catch (e: java.io.IOException) {
            if (attempt == 0 && e.message?.startsWith("HTTP") != true) fetch(url, 1) else throw e
        } finally { c.disconnect() }
    }
}
