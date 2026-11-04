package dev.sortedcord.mediadir.data

import dev.sortedcord.mediadir.data.Json.arr
import dev.sortedcord.mediadir.data.Json.int
import dev.sortedcord.mediadir.data.Json.long
import dev.sortedcord.mediadir.data.Json.str
import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.delay
import kotlinx.coroutines.withContext
import okhttp3.OkHttpClient
import okhttp3.Request
import org.json.JSONObject
import java.util.concurrent.TimeUnit

data class JobProgress(val done: Long, val total: Long?, val unit: String?)
data class JobLog(val ts: Long, val level: String, val msg: String)
data class JobInfo(
    val id: String, val type: String, val params: String, val state: String, val progress: JobProgress?,
    val attempts: Int, val maxAttempts: Int, val error: String?, val result: String?,
    val createdAt: Long?, val startedAt: Long?, val finishedAt: Long?, val scheduleId: String?, val logs: List<JobLog>,
) {
    val finished get() = state == "done" || state == "failed" || state == "cancelled"
}
data class JobStats(val byState: Map<String, Int>, val byType: Map<String, Map<String, Int>>)
data class ScheduleInfo(val id: String, val type: String, val intervalSecs: Long?, val cron: String?, val enabled: Boolean, val nextRun: Long?, val lastRun: Long?)
data class JobEvent(val event: String, val id: String, val type: String, val state: String?, val progress: JobProgress?, val error: String?)

val JobStates = listOf("running", "queued", "paused", "failed", "done", "cancelled")
val JobTypes = listOf("ingest", "hash", "verify", "sidecars", "backup", "asset_gc", "analytics_snapshot", "analytics_rollup")
val RunnableJobTypes = JobTypes.filter { it != "ingest" }

internal fun parseProgress(o: JSONObject?): JobProgress? =
    o?.let { JobProgress(it.long("done") ?: 0, it.long("total"), it.str("unit")) }

internal fun parseJob(o: JSONObject): JobInfo {
    fun pretty(k: String) = o.opt(k).takeIf { it != null && it != JSONObject.NULL }?.let { if (it is JSONObject) it.toString(2) else it.toString() }
    return JobInfo(
        o.optString("id"), o.optString("type"), pretty("params") ?: "{}", o.optString("state"), parseProgress(o.optJSONObject("progress")),
        o.int("attempts") ?: 0, o.int("max_attempts") ?: 1, o.str("error"), pretty("result"),
        o.long("created_at"), o.long("started_at"), o.long("finished_at"), o.str("schedule_id"),
        o.arr("logs").let { a -> (0 until a.length()).mapNotNull { a.optJSONObject(it) }.map { JobLog(it.long("ts") ?: 0, it.optString("level"), it.optString("msg")) } },
    )
}

internal fun parseSchedule(o: JSONObject) = ScheduleInfo(
    o.optString("id"), o.optString("type"), o.long("interval_secs"), o.str("cron")?.ifBlank { null },
    o.optBoolean("enabled"), o.long("next_run"), o.long("last_run"),
)

private val sseClient by lazy { OkHttpClient.Builder().readTimeout(45, TimeUnit.SECONDS).connectTimeout(5, TimeUnit.SECONDS).build() }

/** Blocks reading SSE until the stream ends or fails (throws); cancelling the coroutine closes the connection. */
private suspend fun streamEvents(type: String?, id: String?, onHello: () -> Unit, onEvent: (JobEvent) -> Unit) = withContext(Dispatchers.IO) {
    val q = listOfNotNull(type?.let { "type=$it" }, id?.let { "id=$it" }).joinToString("&").let { if (it.isEmpty()) "" else "?$it" }
    val req = Request.Builder().url(Graph.settings.server + "/api/jobs/events$q").header("Accept", "text/event-stream")
    Graph.settings.identityHeaders().forEach { (k, v) -> req.header(k, v) }
    val call = sseClient.newCall(req.build())
    val handle = coroutineContext[kotlinx.coroutines.Job]!!.invokeOnCompletion { if (it != null) call.cancel() }
    try {
        call.execute().use { resp ->
            if (!resp.isSuccessful) throw ApiException(resp.code, "HTTP ${resp.code}")
            val src = resp.body!!.source()
            var ev = ""; val data = StringBuilder()
            while (true) {
                val line = src.readUtf8Line() ?: throw java.io.IOException("stream closed")
                when {
                    line.isEmpty() -> {
                        if (ev == "hello") onHello()
                        else if (ev == "lagged") onEvent(JobEvent("lagged", "", "", null, null, null))
                        else if (ev == "job" && data.isNotEmpty()) runCatching {
                            val o = JSONObject(data.toString())
                            onEvent(JobEvent(o.optString("event"), o.optString("id"), o.optString("type"), o.str("state"), parseProgress(o.optJSONObject("progress")), o.str("error")))
                        }
                        ev = ""; data.setLength(0)
                    }
                    line.startsWith(":") -> {}
                    line.startsWith("event:") -> ev = line.substring(6).trim()
                    line.startsWith("data:") -> data.append(line.substring(5).trimStart())
                }
            }
        }
    } finally { handle.dispose() }
}

/**
 * Live job updates: SSE with exponential-backoff reconnect; while SSE is down, [onPoll] runs every 3s.
 * [onLive] reports whether SSE is currently connected. Runs until cancelled.
 */
suspend fun liveJobs(type: String?, id: String?, onLive: (Boolean) -> Unit, onEvent: (JobEvent) -> Unit, onPoll: suspend () -> Unit) {
    var failures = 0
    while (true) {
        try {
            streamEvents(type, id, { failures = 0; onLive(true) }, onEvent)
        } catch (e: CancellationException) { throw e } catch (_: Exception) { }
        onLive(false)
        failures++
        val wait = minOf(30_000L, 1000L shl minOf(failures, 5))
        var waited = 0L
        while (waited < wait) { try { onPoll() } catch (e: CancellationException) { throw e } catch (_: Exception) { }; delay(3000); waited += 3000 }
    }
}
