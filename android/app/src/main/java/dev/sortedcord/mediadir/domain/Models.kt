package dev.sortedcord.mediadir.domain

data class Library(val id: String, val name: String, val kind: String, val path: String?, val itemCount: Int, val scanning: Boolean)

data class Images(
    val thumb: String?, val poster: String?, val backdrop: String?,
    val banner: String? = null, val clearlogo: String? = null, val landscape: String? = null, val discart: String? = null,
) {
    /** Best image for a card of the given aspect. */
    fun forAspect(aspect: String): String? = when (aspect) {
        "poster" -> poster ?: thumb ?: backdrop
        "landscape" -> thumb ?: landscape ?: backdrop ?: banner ?: poster
        else -> thumb ?: poster ?: backdrop
    }
    val hero: String? get() = backdrop ?: landscape ?: thumb ?: poster
}

data class PersonRef(val id: String, val name: String, val image: String?)

data class DisplayBlock(
    val key: String, val label: String, val type: String, val widget: String, val placement: String,
    val order: Int, val color: String?, val icon: String?, val text: String, val value: Any?,
    val people: List<PersonRef>,
)

data class ItemSummary(
    val id: String, val kind: String, val libraryId: String?, val parentId: String?, val title: String,
    val subtitle: String?, val year: Int?, val index: Int?, val parentIndex: Int?, val duration: Double?,
    val playable: Boolean, val childCount: Int?, val aspect: String, val images: Images, val addedAt: Long?,
    val card: List<DisplayBlock>, val indexEnd: Int? = null,
) {
    /** "E01" or "E01–E02" for multi-episode files. */
    val episodeLabel: String? get() = index?.let { i -> "E%02d".format(i) + (indexEnd?.takeIf { it > i }?.let { "–E%02d".format(it) } ?: "") }
}

data class VideoInfo(val codec: String, val profile: String?, val width: Int?, val height: Int?, val fps: Double?, val hdr: String?, val bitDepth: Int?)
data class AudioTrack(val index: Int, val codec: String?, val channels: Int?, val language: String?, val title: String?)
data class SubtitleTrack(val index: Int, val codec: String?, val language: String?, val title: String?, val forced: Boolean, val external: String?)

data class MediaFile(
    val id: String, val path: String?, val label: String, val original: Boolean, val container: String?,
    val mime: String?, val size: Long, val duration: Double?, val bitrate: Long?, val video: VideoInfo?,
    val audio: List<AudioTrack>, val subtitles: List<SubtitleTrack>, val streamUrl: String,
    val part: Int? = null, val edition: String? = null,
    val hash: String? = null, val originalName: String? = null, val addedAt: Long? = null,
) {
    /** Shorter side, used for ordering and the quality preference. 0 for audio. */
    val h: Int get() {
        val v = video ?: return 0
        return minOf(v.height ?: 0, v.width ?: 0).takeIf { it > 0 } ?: v.height ?: 0
    }
    fun describe(): String = listOfNotNull(
        label.ifBlank { null } ?: if (h > 0) "${h}p" else null,
        bitrate?.let { "%.1f Mbps".format(it / 1e6) },
        if (original) "Original" else null,
    ).joinToString(" · ")
}

data class Breadcrumb(val id: String, val kind: String, val title: String)
data class Credit(val id: String, val role: String, val character: String?, val order: Int, val person: PersonRef)

data class ItemDetail(
    val summary: ItemSummary, val overview: String?, val sortTitle: String?, val genres: List<String>,
    val locked: List<String>, val breadcrumbs: List<Breadcrumb>, val files: List<MediaFile>,
    val credits: List<Credit>, val children: List<ItemSummary>, val fields: Map<String, Any?>,
    val display: List<DisplayBlock>, val extras: List<ItemSummary> = emptyList(),
)

data class Person(val id: String, val name: String, val sortName: String?, val image: String?, val creditCount: Int)
data class PersonCredit(val id: String, val role: String, val character: String?, val item: ItemSummary)
data class PersonDetail(
    val person: Person, val bio: String?, val birthDate: String?, val fields: Map<String, Any?>,
    val display: List<DisplayBlock>, val credits: List<PersonCredit>,
)

data class FieldDisplay(val placement: String, val widget: String, val order: Int, val format: String?, val color: String?, val icon: String?)
data class FieldDefinition(
    val key: String, val label: String, val type: String, val builtin: Boolean, val derived: Boolean,
    val appliesTo: List<String>, val display: FieldDisplay,
) {
    fun appliesTo(kind: String) = appliesTo.isEmpty() || kind in appliesTo
}

data class Page<T>(val total: Int, val items: List<T>)
data class QueueEntry(val item: ItemSummary, val files: List<MediaFile>)
data class Playback(val startIndex: Int, val queue: List<QueueEntry>)
data class SearchResult(val items: List<ItemSummary>, val people: List<Person>)

object Kinds {
    val ITEM_KINDS = listOf("movie", "show", "season", "episode", "artist", "album", "track", "music_video", "video")
    val LIBRARY_KINDS = listOf("movies", "shows", "music", "music_videos", "videos")
    val FIELD_TYPES = listOf("text", "number", "date", "url", "tags", "rating", "bool", "duration", "person")
    val PLACEMENTS = listOf("card_badge", "card_meta", "header", "info", "section", "hidden")
    val WIDGETS = listOf("badge", "chip", "text", "rating", "link", "tags", "people", "markdown", "stat")
    val ROLES = listOf("actor", "director", "writer", "producer", "artist", "album_artist", "featured", "singer", "composer", "lyricist", "dancer", "guest", "creator")
}

// Managed storage and ingest (API v3).
data class IngestRecord(val id: String, val name: String, val state: String, val error: String?, val itemId: String?)
data class StorageProblem(val itemId: String, val problem: String, val detail: String)
data class IngestStatus(
    val watching: Boolean, val pollSecs: Int, val queue: Int, val settling: Int, val active: Int,
    val lastSidecarSync: Long?, val lastVerify: Long?, val lastBackup: Long?, val problems: List<StorageProblem>,
)
