package dev.sortedcord.mediadir.ui.components

import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.runtime.*
import androidx.compose.ui.Modifier
import androidx.compose.ui.layout.ContentScale
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.platform.LocalDensity
import androidx.compose.ui.unit.Dp
import coil.compose.AsyncImage
import coil.request.CachePolicy
import coil.request.ImageRequest
import dev.sortedcord.mediadir.data.Graph
import dev.sortedcord.mediadir.domain.ImagePolicy
import dev.sortedcord.mediadir.domain.ItemSummary
import java.util.concurrent.ConcurrentHashMap

/**
 * Loads `/asset/{hash}` sized for the view (device pixels, webp). Coil keys memory and disk caches by the full URL;
 * widths are rounded up to 64 px buckets so the same image is reused across similar views.
 */
@Composable
fun AssetImage(hash: String?, width: Dp, height: Dp? = null, modifier: Modifier = Modifier.fillMaxSize(),
               contentScale: ContentScale = ContentScale.Crop, description: String? = null, fit: String = "cover") {
    if (hash == null) return
    val d = LocalDensity.current
    val wpx = ((with(d) { width.roundToPx() } + 63) / 64) * 64
    val hpx = height?.let { with(d) { it.roundToPx() } }?.let { ((it + 63) / 64) * 64 }
    val url = Graph.settings.asset(hash, wpx, hpx, fit)
    val req = ImageRequest.Builder(LocalContext.current).data(url).memoryCacheKey(url).diskCacheKey(url)
        .diskCachePolicy(CachePolicy.ENABLED).memoryCachePolicy(CachePolicy.ENABLED).crossfade(true).build()
    AsyncImage(req, description, modifier, contentScale = contentScale)
}

/** Resolves card images missing from a summary by walking the parent chain (parents' summaries carry images). In-memory cache. */
object ImageResolver {
    private val NONE = ""
    private val cards = ConcurrentHashMap<String, String>()
    private val persons = ConcurrentHashMap<String, String>()

    fun clear() { cards.clear(); persons.clear() }

    /** Card image for [item]: its summary images (no fetch), else the parent chain (fetched only then, cached). */
    suspend fun card(item: ItemSummary): String? {
        ImagePolicy.card(item.imageValues)?.let { return it }
        return item.parentId?.let { byId(it) }
    }

    /** Card image of the item with this id, falling back along the parent chain. */
    suspend fun byId(id: String): String? {
        val visited = mutableListOf<String>()
        var cur: String? = id
        var found: String? = null
        while (cur != null && visited.size < 6) {
            val cached = cards[cur]
            if (cached != null) { found = cached.takeIf { it != NONE }; break }
            visited += cur
            val d = runCatching { Graph.repo.item(cur!!) }.getOrNull() ?: return null
            found = ImagePolicy.card(d.summary.imageValues).takeIf { it != null } ?: ImagePolicy.card(d.fields)
            if (found != null) break
            cur = d.summary.parentId
        }
        visited.forEach { cards[it] = found ?: NONE }
        return found
    }

    suspend fun person(id: String): String? {
        persons[id]?.let { return it.takeIf { h -> h != NONE } }
        val h = runCatching { Graph.repo.person(id) }.getOrNull()?.let { ImagePolicy.person(it.fields) }
        persons[id] = h ?: NONE
        return h
    }
}

@Composable
fun rememberCardHash(item: ItemSummary): String? {
    val direct = ImagePolicy.card(item.imageValues)
    if (direct != null) return direct
    return produceState<String?>(null, item.id) { value = ImageResolver.card(item) }.value
}
