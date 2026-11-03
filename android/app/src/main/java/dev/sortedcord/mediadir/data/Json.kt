package dev.sortedcord.mediadir.data

import dev.sortedcord.mediadir.domain.*
import org.json.JSONArray
import org.json.JSONObject

/** Lenient org.json parsers: unknown fields ignored, missing/null optional fields tolerated. */
internal object Json {
    fun JSONObject.str(k: String): String? = if (has(k) && !isNull(k)) optString(k) else null
    fun JSONObject.int(k: String): Int? = if (has(k) && !isNull(k)) optDouble(k).takeIf { !it.isNaN() }?.toInt() else null
    fun JSONObject.long(k: String): Long? = if (has(k) && !isNull(k)) optDouble(k).takeIf { !it.isNaN() }?.toLong() else null
    fun JSONObject.dbl(k: String): Double? = if (has(k) && !isNull(k)) optDouble(k).takeIf { !it.isNaN() } else null
    fun JSONObject.arr(k: String): JSONArray = optJSONArray(k) ?: JSONArray()
    inline fun <T> JSONArray.map(f: (JSONObject) -> T): List<T> = (0 until length()).mapNotNull { optJSONObject(it)?.let(f) }
    fun JSONArray.strings(): List<String> = (0 until length()).mapNotNull { if (isNull(it)) null else optString(it) }

    /** Converts a JSON value into plain Kotlin (Map/List/String/Number/Boolean/null). */
    fun plain(v: Any?): Any? = when (v) {
        null, JSONObject.NULL -> null
        is JSONObject -> v.keys().asSequence().associateWith { plain(v.opt(it)) }
        is JSONArray -> (0 until v.length()).map { plain(v.opt(it)) }
        else -> v
    }
    /** Converts plain Kotlin back into a JSON value. */
    fun wrap(v: Any?): Any = when (v) {
        null -> JSONObject.NULL
        is Map<*, *> -> JSONObject().apply { v.forEach { (k, x) -> put(k.toString(), wrap(x)) } }
        is List<*> -> JSONArray().apply { v.forEach { put(wrap(it)) } }
        else -> v
    }

    fun ingest(o: JSONObject) = IngestRecord(o.optString("id"), o.optString("name"), o.optString("state"), o.str("error"), o.str("item_id"))
    fun ingestStatus(o: JSONObject) = IngestStatus(
        o.optBoolean("watching"), o.optInt("poll_secs"), o.optInt("queue"), o.optInt("settling"), o.optInt("active"),
        o.long("last_sidecar_sync"), o.long("last_verify"), o.long("last_backup"),
        o.arr("problems").map { StorageProblem(it.optString("item_id"), it.optString("problem"), it.optString("detail")) },
    )

    fun library(o: JSONObject) = Library(o.optString("id"), o.optString("name"), o.optString("kind"), o.str("path"), o.int("item_count") ?: 0, o.optBoolean("scanning"))

    fun personRef(o: JSONObject) = PersonRef(o.optString("id"), o.optString("name"), fieldsMap(o.optJSONObject("images")))

    fun block(o: JSONObject) = DisplayBlock(
        key = o.optString("key"), label = o.optString("label"), type = o.optString("type", "text"),
        widget = o.optString("widget", "text"), placement = o.optString("placement", "info"),
        order = o.int("order") ?: 0, color = o.str("color"), icon = o.str("icon"),
        text = o.str("text") ?: plain(o.opt("value"))?.toString() ?: "", value = plain(o.opt("value")),
        people = o.arr("people").map(::personRef),
    )

    fun summary(o: JSONObject) = ItemSummary(
        id = o.optString("id"), kind = o.optString("kind"), libraryId = o.str("library_id"), parentId = o.str("parent_id"),
        title = o.optString("title"), subtitle = o.str("subtitle"), year = o.int("year"), index = o.int("index"),
        parentIndex = o.int("parent_index"), duration = o.dbl("duration"), playable = o.optBoolean("playable"),
        childCount = o.int("child_count"), aspect = o.str("aspect") ?: "landscape",
        addedAt = o.long("added_at"), card = o.arr("card").map(::block).sortedBy { it.order }, indexEnd = o.int("index_end"),
        images = fieldsMap(o.optJSONObject("images")),
    )

    fun relationSummary(o: JSONObject) = RelationSummary(
        o.optString("id"), o.optString("type"), (o.str("name") ?: o.str("label"))?.ifBlank { null }, o.int("member_count") ?: 0, o.int("ord"), o.str("role")?.ifBlank { null })

    fun relationDetail(o: JSONObject) = RelationDetail(relationSummary(o),
        o.arr("members").map { RelationMember(it.optString("item_id"), it.int("ord") ?: 0, it.str("role")?.ifBlank { null }, summary(it.optJSONObject("item") ?: JSONObject())) })

    fun file(o: JSONObject) = MediaFile(
        id = o.optString("id"), path = o.str("path"), label = o.optString("label"), original = o.optBoolean("original", true),
        container = o.str("container"), mime = o.str("mime"), size = o.long("size") ?: 0, duration = o.dbl("duration"),
        bitrate = o.long("bitrate"),
        video = o.optJSONObject("video")?.let { v ->
            VideoInfo(v.optString("codec"), v.str("profile"), v.int("width"), v.int("height"), v.dbl("fps"), v.str("hdr"), v.int("bit_depth"))
        },
        audio = o.arr("audio").map { AudioTrack(it.int("index") ?: 0, it.str("codec"), it.int("channels"), it.str("language"), it.str("title")) },
        subtitles = o.arr("subtitles").map { SubtitleTrack(it.int("index") ?: 0, it.str("codec"), it.str("language"), it.str("title"), it.optBoolean("forced"), it.str("external")) },
        streamUrl = o.str("stream_url") ?: "/stream/${o.optString("id")}",
        part = o.int("part"), edition = o.str("edition"),
        hash = o.str("hash"), originalName = o.str("original_name"), addedAt = o.long("added_at"),
    )

    fun fieldsMap(o: JSONObject?): Map<String, Any?> = (plain(o) as? Map<String, Any?>) ?: emptyMap()

    fun detail(o: JSONObject) = ItemDetail(
        summary = summary(o), overview = o.str("overview"), sortTitle = o.str("sort_title"),
        genres = o.arr("genres").strings(), locked = o.arr("locked").strings(),
        breadcrumbs = o.arr("breadcrumbs").map { Breadcrumb(it.optString("id"), it.optString("kind"), it.optString("title")) },
        files = o.arr("files").map(::file),
        credits = o.arr("credits").map { Credit(it.optString("id"), it.optString("role"), it.str("character"), it.int("order") ?: 0, personRef(it.optJSONObject("person") ?: JSONObject())) },
        children = o.arr("children").map(::summary), fields = fieldsMap(o.optJSONObject("fields")),
        display = o.arr("display").map(::block).sortedBy { it.order },
        extras = o.arr("extras").map(::summary),
        relations = o.arr("relations").map(::relationSummary),
    )

    fun person(o: JSONObject) = Person(o.optString("id"), o.optString("name"), o.str("sort_name"), o.int("credit_count") ?: 0, fieldsMap(o.optJSONObject("images")))

    fun personDetail(o: JSONObject) = PersonDetail(
        person = person(o), bio = o.str("bio"), birthDate = o.str("birth_date"), fields = fieldsMap(o.optJSONObject("fields")),
        display = o.arr("display").map(::block).sortedBy { it.order },
        credits = o.arr("credits").map { PersonCredit(it.optString("id"), it.optString("role"), it.str("character"), summary(it.optJSONObject("item") ?: JSONObject())) },
    )

    fun field(o: JSONObject): FieldDefinition {
        val d = o.optJSONObject("display") ?: JSONObject()
        return FieldDefinition(
            key = o.optString("key"), label = o.optString("label"), type = o.optString("type", "text"),
            builtin = o.optBoolean("builtin"), derived = o.optBoolean("derived"), appliesTo = o.arr("applies_to").strings(),
            display = FieldDisplay(d.optString("placement", "info"), d.optString("widget", "text"), d.int("order") ?: 0, d.str("format"), d.str("color"), d.str("icon")),
        )
    }

    fun fieldJson(f: FieldDefinition) = JSONObject()
        .put("key", f.key).put("label", f.label).put("type", f.type)
        .put("applies_to", JSONArray(f.appliesTo))
        .put("display", JSONObject().put("placement", f.display.placement).put("widget", f.display.widget)
            .put("order", f.display.order).put("format", f.display.format ?: JSONObject.NULL)
            .put("color", f.display.color ?: JSONObject.NULL).put("icon", f.display.icon ?: JSONObject.NULL))
}
