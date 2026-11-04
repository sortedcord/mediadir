package dev.sortedcord.mediadir.data

import dev.sortedcord.mediadir.domain.*
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.withContext
import org.json.JSONArray
import org.json.JSONObject
import java.net.HttpURLConnection
import java.net.URL
import java.net.URLEncoder

class ApiException(val code: Int, message: String) : Exception(message)

interface MediaRepository {
    suspend fun health(): Int
    suspend fun libraries(): List<Library>
    suspend fun items(libraryId: String? = null, parentId: String? = null, personId: String? = null, kind: String? = null,
                      sort: String? = null, order: String? = null, limit: Int = 50, offset: Int = 0): Page<ItemSummary>
    suspend fun item(id: String): ItemDetail
    suspend fun patchItem(id: String, changes: Map<String, Any?>): ItemDetail
    suspend fun setItemField(id: String, key: String, value: Any?): ItemDetail
    suspend fun deleteItemField(id: String, key: String): ItemDetail
    suspend fun addCredit(id: String, personId: String?, personName: String?, role: String, character: String?): ItemDetail
    suspend fun deleteCredit(id: String, creditId: String): ItemDetail
    suspend fun playback(id: String): Playback
    suspend fun people(q: String, limit: Int = 20): List<Person>
    suspend fun person(id: String): PersonDetail
    suspend fun patchPerson(id: String, changes: Map<String, Any?>)
    suspend fun setPersonField(id: String, key: String, value: Any?)
    suspend fun deletePersonField(id: String, key: String)
    suspend fun fields(): List<FieldDefinition>
    suspend fun createField(f: FieldDefinition)
    suspend fun updateField(f: FieldDefinition)
    suspend fun deleteField(key: String)
    suspend fun search(q: String): SearchResult
    suspend fun ingestStatus(): IngestStatus
    suspend fun ingests(): List<IngestRecord>
    suspend fun upload(name: String, open: () -> java.io.InputStream): IngestRecord
    suspend fun deleteItem(id: String)
    suspend fun mergeItems(targetId: String, sourceIds: List<String>): ItemDetail
    suspend fun deleteFile(id: String)
    suspend fun pollIngest()
    suspend fun relation(id: String): RelationDetail
    suspend fun uploadAsset(open: () -> java.io.InputStream): UploadedAsset
    suspend fun jobs(state: String? = null, type: String? = null, limit: Int = 200): Pair<Int, List<JobInfo>>
    suspend fun job(id: String): JobInfo
    suspend fun createJob(type: String): JobInfo
    suspend fun jobAction(id: String, action: String): JobInfo
    suspend fun deleteJob(id: String)
    suspend fun jobStats(): JobStats
    suspend fun schedules(): List<ScheduleInfo>
    suspend fun updateSchedule(id: String, changes: Map<String, Any?>): ScheduleInfo
    suspend fun runSchedule(id: String): JobInfo
}

class HttpMediaRepository(private val settings: Settings) : MediaRepository {
    private val base get() = settings.server

    /** Retry reads on stale keep-alive connections; mutations must not be replayed. */
    private suspend fun call(method: String, path: String, body: Any? = null): Any? = withContext(Dispatchers.IO) {
        try { call1(method, path, body) } catch (e: java.io.IOException) {
            if (method != "GET") throw e
            call1(method, path, body)
        }
    }

    private fun call1(method: String, path: String, body: Any?): Any? = run {
        val c = URL(base + path).openConnection() as HttpURLConnection
        try {
            c.requestMethod = method // Android's HttpURLConnection (okhttp) accepts PATCH
            c.connectTimeout = 5000; c.readTimeout = 20000
            c.setRequestProperty("Accept", "application/json")
            Graph.settings.applyIdentity(c)
            if (body != null) {
                c.doOutput = true
                c.setRequestProperty("Content-Type", "application/json")
                c.outputStream.use { it.write(body.toString().toByteArray()) }
            }
            val code = c.responseCode
            val text = (if (code >= 400) c.errorStream else c.inputStream)?.bufferedReader()?.use { it.readText() } ?: ""
            if (code >= 400) {
                val msg = runCatching { JSONObject(text).optString("error") }.getOrNull()?.ifBlank { null } ?: "HTTP $code"
                throw ApiException(code, msg)
            }
            val t = text.trim()
            when {
                t.startsWith("{") -> JSONObject(t)
                t.startsWith("[") -> JSONArray(t)
                else -> null
            }
        } finally { c.disconnect() }
    }

    private fun obj(a: Any?) = a as? JSONObject ?: JSONObject()
    private fun enc(s: String) = URLEncoder.encode(s, "UTF-8")
    private fun query(vararg p: Pair<String, Any?>) = p.filter { it.second != null }.joinToString("&", prefix = "?") { "${it.first}=${enc(it.second.toString())}" }
    private fun mapJson(m: Map<String, Any?>) = Json.wrap(m) as JSONObject

    override suspend fun health() = obj(call("GET", "/api/health")).optInt("version", 1)
    override suspend fun libraries() = (call("GET", "/api/libraries") as? JSONArray ?: JSONArray()).let { a -> with(Json) { a.map(::library) } }

    override suspend fun items(libraryId: String?, parentId: String?, personId: String?, kind: String?, sort: String?, order: String?, limit: Int, offset: Int): Page<ItemSummary> {
        val o = obj(call("GET", "/api/items" + query("library_id" to libraryId, "parent_id" to parentId, "person_id" to personId,
            "kind" to kind, "sort" to sort, "order" to order, "limit" to limit, "offset" to offset)))
        return with(Json) { Page(o.optInt("total"), o.arr("items").map(::summary)) }
    }

    override suspend fun item(id: String) = Json.detail(obj(call("GET", "/api/items/$id")))
    override suspend fun patchItem(id: String, changes: Map<String, Any?>) = Json.detail(obj(call("PATCH", "/api/items/$id", mapJson(changes))))
    override suspend fun setItemField(id: String, key: String, value: Any?) =
        Json.detail(obj(call("PUT", "/api/items/$id/fields/${enc(key)}", JSONObject().put("value", Json.wrap(value)))))
    override suspend fun deleteItemField(id: String, key: String) = Json.detail(obj(call("DELETE", "/api/items/$id/fields/${enc(key)}")))
    override suspend fun addCredit(id: String, personId: String?, personName: String?, role: String, character: String?) =
        Json.detail(obj(call("POST", "/api/items/$id/credits", mapJson(mapOf("person_id" to personId, "person_name" to personName, "role" to role, "character" to character).filterValues { it != null }))))
    override suspend fun deleteCredit(id: String, creditId: String) = Json.detail(obj(call("DELETE", "/api/items/$id/credits/$creditId")))

    override suspend fun playback(id: String): Playback {
        val o = obj(call("GET", "/api/items/$id/playback"))
        return with(Json) {
            Playback(o.optInt("start_index"), o.arr("queue").map { QueueEntry(summary(it.optJSONObject("item") ?: JSONObject()), it.arr("files").map(::file)) })
        }
    }

    override suspend fun people(q: String, limit: Int) =
        obj(call("GET", "/api/people" + query("q" to q, "limit" to limit))).let { o -> with(Json) { o.arr("people").map(::person) } }
    override suspend fun person(id: String) = Json.personDetail(obj(call("GET", "/api/people/$id")))
    override suspend fun patchPerson(id: String, changes: Map<String, Any?>) { call("PATCH", "/api/people/$id", mapJson(changes)) }
    override suspend fun setPersonField(id: String, key: String, value: Any?) { call("PUT", "/api/people/$id/fields/${enc(key)}", JSONObject().put("value", Json.wrap(value))) }
    override suspend fun deletePersonField(id: String, key: String) { call("DELETE", "/api/people/$id/fields/${enc(key)}") }

    override suspend fun fields() = (call("GET", "/api/fields") as? JSONArray ?: JSONArray()).let { a -> with(Json) { a.map(::field) } }
    override suspend fun createField(f: FieldDefinition) { call("POST", "/api/fields", Json.fieldJson(f)) }
    override suspend fun updateField(f: FieldDefinition) {
        val j = Json.fieldJson(f).apply { remove("key"); remove("type") }
        call("PUT", "/api/fields/${enc(f.key)}", j)
    }
    override suspend fun deleteField(key: String) { call("DELETE", "/api/fields/${enc(key)}") }

    override suspend fun search(q: String): SearchResult {
        val o = obj(call("GET", "/api/search" + query("q" to q)))
        return with(Json) { SearchResult(o.arr("items").map(::summary), o.arr("people").map(::person)) }
    }
    override suspend fun pollIngest() { call("POST", "/api/ingest/scan") }
    override suspend fun ingestStatus() = Json.ingestStatus(obj(call("GET", "/api/ingest/status")))
    override suspend fun ingests() = (call("GET", "/api/ingest?limit=100") as? JSONArray ?: JSONArray()).let { a -> with(Json) { a.map(::ingest) } }
    override suspend fun relation(id: String) = Json.relationDetail(obj(call("GET", "/api/relations/$id")))
    override suspend fun uploadAsset(open: () -> java.io.InputStream): UploadedAsset = withContext(Dispatchers.IO) {
        val c = URL(base + "/api/assets").openConnection() as HttpURLConnection
        try {
            c.requestMethod = "POST"; c.connectTimeout = 5000; c.readTimeout = 120000
            c.doOutput = true; c.setChunkedStreamingMode(64 * 1024)
            c.setRequestProperty("Content-Type", "application/octet-stream")
            Graph.settings.applyIdentity(c)
            open().use { input -> c.outputStream.use { output -> input.copyTo(output) } }
            val code = c.responseCode
            val text = (if (code >= 400) c.errorStream else c.inputStream)?.bufferedReader()?.use { it.readText() } ?: ""
            val j = runCatching { JSONObject(text) }.getOrDefault(JSONObject())
            if (code >= 400) throw ApiException(code, j.optString("error").ifBlank { "HTTP $code" })
            with(Json) { UploadedAsset(j.optString("hash"), j.str("mime"), j.int("width"), j.int("height"), j.long("size")) }
        } finally { c.disconnect() }
    }
    override suspend fun jobs(state: String?, type: String?, limit: Int): Pair<Int, List<JobInfo>> {
        val o = obj(call("GET", "/api/jobs" + query("state" to state, "type" to type, "limit" to limit)))
        return o.optInt("total") to o.optJSONArray("jobs").let { a -> (0 until (a?.length() ?: 0)).mapNotNull { a!!.optJSONObject(it) }.map(::parseJob) }
    }
    override suspend fun job(id: String) = parseJob(obj(call("GET", "/api/jobs/$id")))
    override suspend fun createJob(type: String) = parseJob(obj(call("POST", "/api/jobs", JSONObject().put("type", type))))
    override suspend fun jobAction(id: String, action: String) = parseJob(obj(call("POST", "/api/jobs/$id/$action", JSONObject())))
    override suspend fun deleteJob(id: String) { call("DELETE", "/api/jobs/$id") }
    override suspend fun jobStats(): JobStats {
        val o = obj(call("GET", "/api/jobs/stats"))
        fun counts(j: JSONObject?): Map<String, Int> = j?.keys()?.asSequence()?.associateWith { j.optInt(it) } ?: emptyMap()
        val bt = o.optJSONObject("by_type")
        return JobStats(counts(o.optJSONObject("by_state")), bt?.keys()?.asSequence()?.associateWith { counts(bt.optJSONObject(it)) } ?: emptyMap())
    }
    override suspend fun schedules() = (call("GET", "/api/schedules") as? JSONArray ?: JSONArray()).let { a -> (0 until a.length()).mapNotNull { a.optJSONObject(it) }.map(::parseSchedule) }
    override suspend fun updateSchedule(id: String, changes: Map<String, Any?>) = parseSchedule(obj(call("PUT", "/api/schedules/$id", mapJson(changes))))
    override suspend fun runSchedule(id: String) = parseJob(obj(call("POST", "/api/schedules/$id/run", JSONObject())))

    override suspend fun deleteItem(id: String) { call("DELETE", "/api/items/$id") }
    override suspend fun mergeItems(targetId: String, sourceIds: List<String>) =
        Json.detail(obj(call("POST", "/api/items/$targetId/merge", JSONObject().put("sources", JSONArray(sourceIds)))))
    override suspend fun deleteFile(id: String) { call("DELETE", "/api/files/$id") }

    // Stream once: retrying a partially completed upload could hide its real outcome.
    override suspend fun upload(name: String, open: () -> java.io.InputStream): IngestRecord = withContext(Dispatchers.IO) {
        val c = URL(base + "/api/ingest/upload").openConnection() as HttpURLConnection
        try {
            c.requestMethod = "POST"
            c.connectTimeout = 5000
            c.readTimeout = 600000
            c.doOutput = true
            c.setChunkedStreamingMode(64 * 1024)
            c.setRequestProperty("Content-Type", "application/octet-stream")
            Graph.settings.applyIdentity(c)
            c.setRequestProperty("X-Filename", enc(name).replace("+", "%20"))
            open().use { input -> c.outputStream.use { output -> input.copyTo(output) } }
            val code = c.responseCode
            val text = (if (code >= 400) c.errorStream else c.inputStream)?.bufferedReader()?.use { it.readText() } ?: ""
            val json = JSONObject(text)
            if (code >= 400) throw ApiException(code, json.optString("error", "HTTP $code"))
            Json.ingest(json)
        } finally { c.disconnect() }
    }
}
