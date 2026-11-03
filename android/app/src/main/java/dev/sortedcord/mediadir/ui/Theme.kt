package dev.sortedcord.mediadir.ui

import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.darkColorScheme
import androidx.compose.runtime.Composable
import androidx.compose.ui.graphics.Color

val Accent = Color(0xFFFF4D6D)
val Bg = Color(0xFF0E0E12)
val CardBg = Color(0xFF1A1A22)
val Muted = Color(0xFF9A9AA8)

/** Preset swatches offered in the field display editor. */
val SwatchHex = listOf("#FF4D6D", "#FF9F1C", "#FFD166", "#06D6A0", "#2EC4B6", "#4CC9F0", "#4361EE", "#9B5DE5", "#F15BB5", "#8D99AE")

fun parseColor(hex: String?, fallback: Color): Color = runCatching {
    val h = hex!!.removePrefix("#")
    Color(("FF" + h.takeLast(6)).toLong(16))
}.getOrDefault(fallback)

@Composable
fun MediaDirTheme(content: @Composable () -> Unit) {
    MaterialTheme(
        colorScheme = darkColorScheme(primary = Accent, background = Bg, surface = Bg, surfaceVariant = CardBg,
            surfaceContainer = CardBg, surfaceContainerHigh = CardBg, surfaceContainerLow = Bg, onPrimary = Color.White),
        content = content,
    )
}
