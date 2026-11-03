package dev.sortedcord.mediadir.ui.components

import android.content.Intent
import android.net.Uri
import androidx.compose.foundation.BorderStroke
import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.*
import androidx.compose.foundation.lazy.LazyRow
import androidx.compose.foundation.lazy.items
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.automirrored.filled.OpenInNew
import androidx.compose.material.icons.filled.Star
import androidx.compose.material.icons.filled.StarBorder
import androidx.compose.material.icons.filled.StarHalf
import androidx.compose.material3.*
import androidx.compose.runtime.Composable
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import dev.sortedcord.mediadir.domain.DisplayBlock
import dev.sortedcord.mediadir.domain.ImagePolicy
import androidx.compose.ui.draw.clip
import dev.sortedcord.mediadir.ui.*

/** Values of a block as a list of strings (tags/genres arrays or a single text). */
fun DisplayBlock.values(): List<String> = when (val v = value) {
    is List<*> -> v.mapNotNull { it?.toString() }.filter { it.isNotBlank() }
    else -> listOf(text).filter { it.isNotBlank() }
}

/** Rating as 0..5 stars. Values > 5 are treated as a 0..10 scale. */
fun DisplayBlock.stars(): Float? {
    val n = (value as? Number)?.toFloat() ?: text.takeWhile { it.isDigit() || it == '.' }.toFloatOrNull() ?: return null
    return if (n > 5f) (n / 2f).coerceAtMost(5f) else n
}

@Composable
fun Stars(stars: Float, color: Color, size: Int = 16) {
    Row {
        for (i in 1..5) {
            val icon = when { stars >= i -> Icons.Default.Star; stars >= i - 0.5f -> Icons.Default.StarHalf; else -> Icons.Default.StarBorder }
            Icon(icon, null, tint = color, modifier = Modifier.size(size.dp))
        }
    }
}

private fun openUrl(ctx: android.content.Context, url: String) {
    runCatching { ctx.startActivity(Intent(Intent.ACTION_VIEW, Uri.parse(if ("://" in url) url else "https://$url"))) }
}

@Composable
fun Chip(text: String, color: Color, onClick: (() -> Unit)? = null) {
    Surface(
        shape = RoundedCornerShape(50), color = color.copy(alpha = 0.12f), border = BorderStroke(1.dp, color.copy(alpha = 0.5f)),
        modifier = if (onClick != null) Modifier.clickable(onClick = onClick) else Modifier,
    ) { Text(text, fontSize = 12.sp, color = Color.White, modifier = Modifier.padding(horizontal = 10.dp, vertical = 4.dp)) }
}

/** Compact inline rendering (used by header chips and info values). */
@OptIn(ExperimentalLayoutApi::class)
@Composable
fun BlockInline(b: DisplayBlock, onPerson: (String) -> Unit) {
    val ctx = LocalContext.current
    val color = parseColor(b.color, if (b.placement == "header") Accent else Muted)
    when (b.widget) {
        "badge" -> Pill(b.text, parseColor(b.color, Accent))
        "rating" -> b.stars()?.let { s ->
            Row(verticalAlignment = Alignment.CenterVertically) {
                Stars(s, parseColor(b.color, Color(0xFFFFD166))); Spacer(Modifier.width(4.dp)); Text(b.text, fontSize = 12.sp, color = Muted)
            }
        } ?: Text(b.text, fontSize = 13.sp)
        "link" -> Row(Modifier.clickable { openUrl(ctx, (b.value as? String) ?: b.text) }, verticalAlignment = Alignment.CenterVertically) {
            Text(b.text, color = parseColor(b.color, Accent), fontSize = 13.sp)
            Icon(Icons.AutoMirrored.Filled.OpenInNew, null, tint = parseColor(b.color, Accent), modifier = Modifier.size(14.dp).padding(start = 2.dp))
        }
        "tags", "chip" -> FlowRow(horizontalArrangement = Arrangement.spacedBy(6.dp), verticalArrangement = Arrangement.spacedBy(6.dp)) {
            b.values().forEach { Chip(it, color) }
        }
        "people" -> FlowRow(horizontalArrangement = Arrangement.spacedBy(6.dp), verticalArrangement = Arrangement.spacedBy(6.dp)) {
            if (b.people.isEmpty()) Chip(b.text, color)
            b.people.forEach { p -> Chip(p.name, color) { onPerson(p.id) } }
        }
        "image" -> Row(horizontalArrangement = Arrangement.spacedBy(6.dp)) {
            ImagePolicy.hashes(b.value).forEach { h -> Box(Modifier.size(96.dp).clip(RoundedCornerShape(8.dp))) { AssetImage(h, 96.dp, 96.dp) } }
        }
        "stat" -> Text(b.text, fontSize = 15.sp, fontWeight = FontWeight.Bold, color = parseColor(b.color, Color.White))
        else -> Text(b.text, fontSize = 13.sp, color = if (b.color != null) color else Color.White)
    }
}

@OptIn(ExperimentalLayoutApi::class)
@Composable
fun HeaderBlocks(blocks: List<DisplayBlock>, onPerson: (String) -> Unit) {
    if (blocks.isEmpty()) return
    FlowRow(horizontalArrangement = Arrangement.spacedBy(6.dp), verticalArrangement = Arrangement.spacedBy(6.dp)) {
        blocks.forEach { b ->
            when (b.widget) {
                "rating", "badge", "tags", "people", "link" -> Box(Modifier.align(Alignment.CenterVertically)) { BlockInline(b, onPerson) }
                else -> Chip(b.text, parseColor(b.color, Accent))
            }
        }
    }
}

@Composable
fun InfoRows(blocks: List<DisplayBlock>, onPerson: (String) -> Unit) {
    if (blocks.isEmpty()) return
    Column(Modifier.padding(horizontal = 16.dp).fillMaxWidth()) {
        blocks.forEach { b ->
            Row(Modifier.fillMaxWidth().padding(vertical = 5.dp)) {
                Text(b.label, color = Muted, fontSize = 13.sp, modifier = Modifier.width(120.dp))
                Box(Modifier.weight(1f)) { BlockInline(b, onPerson) }
            }
            HorizontalDivider(color = CardBg)
        }
    }
}

@Composable
fun SectionBlock(b: DisplayBlock, onPerson: (String) -> Unit) {
    Column(Modifier.fillMaxWidth().padding(vertical = 6.dp)) {
        SectionTitle(b.label)
        when (b.widget) {
            "people" -> LazyRow(contentPadding = PaddingValues(horizontal = 16.dp), horizontalArrangement = Arrangement.spacedBy(12.dp)) {
                items(b.people, key = { it.id }) { p -> PersonTile(p.id, p.name, null, onPerson, images = p.images) }
            }
            "markdown", "text" -> Column(Modifier.padding(horizontal = 16.dp), verticalArrangement = Arrangement.spacedBy(8.dp)) {
                b.text.split(Regex("\n\\s*\n")).map { it.trim() }.filter { it.isNotEmpty() }.forEach {
                    Text(it.replace(Regex("[*_`#>]+"), ""), fontSize = 14.sp, color = Color(0xFFD6D6DE), lineHeight = 20.sp)
                }
            }
            "image" -> LazyRow(contentPadding = PaddingValues(horizontal = 16.dp), horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                items(ImagePolicy.hashes(b.value)) { h -> Box(Modifier.height(160.dp).widthIn(max = 280.dp).aspectRatio(4f / 3f).clip(RoundedCornerShape(8.dp))) { AssetImage(h, 280.dp, 160.dp, fit = "contain") } }
            }
            "stat" -> Text(b.text, fontSize = 28.sp, fontWeight = FontWeight.Bold, color = parseColor(b.color, Accent), modifier = Modifier.padding(horizontal = 16.dp))
            else -> Box(Modifier.padding(horizontal = 16.dp)) { BlockInline(b, onPerson) }
        }
    }
}

/** Renders a detail page's header/info/section blocks generically. */
@Composable
fun DetailBlocks(blocks: List<DisplayBlock>, onPerson: (String) -> Unit) {
    InfoRows(blocks.filter { it.placement == "info" }, onPerson)
    blocks.filter { it.placement == "section" }.forEach { SectionBlock(it, onPerson) }
}
