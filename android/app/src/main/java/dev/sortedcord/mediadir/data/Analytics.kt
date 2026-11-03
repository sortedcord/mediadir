package dev.sortedcord.mediadir.data

import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.SupervisorJob
import kotlinx.coroutines.asCoroutineDispatcher
import kotlinx.coroutines.launch
import org.json.JSONObject
import java.net.HttpURLConnection
import java.net.URL
import java.util.concurrent.Executors

/** Fire-and-forget POST /api/analytics/playback on a single background thread (keeps event order). Failures are ignored. */
object Analytics {
    private val scope = CoroutineScope(SupervisorJob() + Executors.newSingleThreadExecutor { r -> Thread(r, "analytics").apply { isDaemon = true } }.asCoroutineDispatcher())

    fun playback(fileId: String, event: String, positionS: Double, durationS: Double?) {
        val settings = runCatching { Graph.settings }.getOrNull() ?: return
        val server = settings.server
        scope.launch {
            runCatching {
                val body = JSONObject().put("file_id", fileId).put("event", event).put("position_s", positionS.coerceAtLeast(0.0))
                if (durationS != null && durationS > 0) body.put("duration_s", durationS)
                val c = URL("$server/api/analytics/playback").openConnection() as HttpURLConnection
                try {
                    c.requestMethod = "POST"; c.connectTimeout = 5000; c.readTimeout = 5000; c.doOutput = true
                    c.setRequestProperty("Content-Type", "application/json")
                    settings.applyIdentity(c)
                    c.outputStream.use { it.write(body.toString().toByteArray()) }
                    c.responseCode
                    (if (c.responseCode >= 400) c.errorStream else c.inputStream)?.close()
                } finally { c.disconnect() }
            }
        }
    }
}

/** Turns player state samples into start/progress/pause/resume/stop events for the file actually playing. */
class PlaybackReporter {
    private enum class S { IDLE, PLAYING, PAUSED }
    private var fileId: String? = null
    private var state = S.IDLE
    private var lastProgress = 0L
    private var lastPos = 0.0
    private var lastDur: Double? = null

    /** Call on any player change and periodically. [playing] = actively playing (not paused/ended). */
    fun update(file: String?, playing: Boolean, posS: Double, durS: Double?) {
        if (file != fileId) { stop(); fileId = file }
        val f = fileId ?: return
        lastPos = posS; lastDur = durS
        val now = System.currentTimeMillis()
        when {
            playing && state == S.IDLE -> { Analytics.playback(f, "start", posS, durS); state = S.PLAYING; lastProgress = now }
            playing && state == S.PAUSED -> { Analytics.playback(f, "resume", posS, durS); state = S.PLAYING; lastProgress = now }
            playing && now - lastProgress >= 30_000 -> { Analytics.playback(f, "progress", posS, durS); lastProgress = now }
            !playing && state == S.PLAYING -> { Analytics.playback(f, "pause", posS, durS); state = S.PAUSED }
        }
    }

    /** End the current session (release/finish/switch). Optional exact position. */
    fun stop(posS: Double? = null, durS: Double? = null) {
        val f = fileId
        if (f != null && state != S.IDLE) Analytics.playback(f, "stop", posS ?: lastPos, durS ?: lastDur)
        state = S.IDLE
    }
}
