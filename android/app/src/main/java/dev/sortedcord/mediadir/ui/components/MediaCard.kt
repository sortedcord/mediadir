package dev.sortedcord.mediadir.ui.components

import androidx.compose.foundation.background
import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.*
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material3.LinearProgressIndicator
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.layout.ContentScale
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.Dp
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import coil.compose.AsyncImage
import dev.sortedcord.mediadir.data.Graph
import dev.sortedcord.mediadir.domain.ItemSummary
import dev.sortedcord.mediadir.ui.*

fun aspectRatio(aspect: String) = when (aspect) { "poster" -> 2f / 3f; "square" -> 1f; else -> 16f / 9f }

/** Card width for carousels, by aspect, so rows look balanced. */
fun cardWidth(aspect: String): Dp = when (aspect) { "poster" -> 120.dp; "square" -> 140.dp; else -> 220.dp }

@Composable
fun MediaCard(item: ItemSummary, modifier: Modifier = Modifier, onClick: (ItemSummary) -> Unit) {
    val badges = item.card.filter { it.placement == "card_badge" }
    val meta = item.card.filter { it.placement == "card_meta" }
    Column(modifier.clickable { onClick(item) }) {
        Box(Modifier.fillMaxWidth().aspectRatio(aspectRatio(item.aspect)).clip(RoundedCornerShape(10.dp)).background(CardBg)) {
            Text(item.title.take(1).uppercase(), color = Muted, fontSize = 28.sp, modifier = Modifier.align(Alignment.Center))
            Graph.settings.url(item.images.forAspect(item.aspect))?.let {
                AsyncImage(it, item.title, Modifier.fillMaxSize(), contentScale = ContentScale.Crop)
            }
            if (badges.isNotEmpty()) Row(Modifier.align(Alignment.TopStart).padding(6.dp), horizontalArrangement = Arrangement.spacedBy(4.dp)) {
                badges.take(4).forEach { Pill(it.text, parseColor(it.color, Accent)) }
            }
            val pos = remember(item.id) { Graph.settings.position(item.id) }
            val dur = item.duration
            if (item.playable && pos > 0 && dur != null && dur > 0) {
                LinearProgressIndicator(
                    progress = { (pos / 1000.0 / dur).toFloat().coerceIn(0f, 1f) },
                    modifier = Modifier.align(Alignment.BottomCenter).fillMaxWidth().height(3.dp),
                    color = Accent, trackColor = Color.Black.copy(alpha = 0.5f),
                )
            }
        }
        Spacer(Modifier.height(6.dp))
        Text(item.title, fontSize = 13.sp, fontWeight = FontWeight.Medium, color = Color.White, maxLines = 2, overflow = TextOverflow.Ellipsis)
        item.subtitle?.takeIf { it.isNotBlank() }?.let {
            Text(it, fontSize = 11.sp, color = Muted, maxLines = 1, overflow = TextOverflow.Ellipsis)
        }
        if (meta.isNotEmpty()) Text(meta.joinToString(" · ") { it.text }, fontSize = 11.sp, color = Muted, maxLines = 1, overflow = TextOverflow.Ellipsis)
    }
}

@Composable
private fun <T> remember(key: Any, calc: () -> T): T = androidx.compose.runtime.remember(key) { calc() }
