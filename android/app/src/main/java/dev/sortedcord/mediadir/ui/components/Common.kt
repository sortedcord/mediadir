package dev.sortedcord.mediadir.ui.components

import dev.sortedcord.mediadir.domain.ImagePolicy
import androidx.compose.foundation.background
import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.*
import androidx.compose.foundation.shape.CircleShape
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.filled.Person
import androidx.compose.material3.*
import androidx.compose.runtime.Composable
import androidx.compose.runtime.produceState
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.layout.ContentScale
import androidx.compose.ui.text.style.TextAlign
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.Dp
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import dev.sortedcord.mediadir.data.Graph
import dev.sortedcord.mediadir.ui.*

@Composable
fun <T> StateContent(state: UiState<T>, onRetry: () -> Unit, content: @Composable (T) -> Unit) {
    when (state) {
        is UiState.Loading -> Box(Modifier.fillMaxSize(), contentAlignment = Alignment.Center) { CircularProgressIndicator(color = Accent) }
        is UiState.Error -> Column(Modifier.fillMaxSize().padding(24.dp), verticalArrangement = Arrangement.Center, horizontalAlignment = Alignment.CenterHorizontally) {
            Text(state.message, color = Muted, textAlign = TextAlign.Center)
            Spacer(Modifier.height(12.dp))
            Button(onClick = onRetry) { Text("Retry") }
        }
        is UiState.Ready -> content(state.data)
    }
}

@Composable
fun Avatar(personId: String?, name: String, size: Dp, modifier: Modifier = Modifier, hash: String? = null, images: Map<String, Any?>? = null) {
    // `images` (from PersonRef/Person summaries) is authoritative: no fetch. Only with neither hash nor images is the person fetched.
    val photo = hash ?: images?.let { ImagePolicy.person(it) }
        ?: if (images == null) personId?.let { id -> produceState<String?>(null, id) { value = ImageResolver.person(id) }.value } else null
    Box(modifier.size(size).clip(CircleShape).background(CardBg), contentAlignment = Alignment.Center) {
        val initials = name.split(' ').filter { it.isNotBlank() }.take(2).joinToString("") { it.take(1).uppercase() }
        if (initials.isNotEmpty()) Text(initials, color = Muted, fontSize = (size.value / 3).sp)
        else Icon(Icons.Default.Person, null, tint = Muted)
        AssetImage(photo, size, size, description = name)
    }
}

@Composable
fun PersonTile(id: String, name: String, caption: String?, onClick: (String) -> Unit, hash: String? = null, images: Map<String, Any?>? = null) {
    Column(Modifier.width(84.dp).clickable { onClick(id) }, horizontalAlignment = Alignment.CenterHorizontally) {
        Avatar(id, name, 72.dp, hash = hash, images = images)
        Spacer(Modifier.height(6.dp))
        Text(name, fontSize = 12.sp, maxLines = 2, overflow = TextOverflow.Ellipsis, textAlign = TextAlign.Center, color = Color.White)
        caption?.let { Text(it, fontSize = 11.sp, color = Muted, maxLines = 1, overflow = TextOverflow.Ellipsis, textAlign = TextAlign.Center) }
    }
}

@Composable
fun Pill(text: String, color: Color = Accent, filled: Boolean = true, modifier: Modifier = Modifier) {
    Text(
        text, fontSize = 10.sp, color = if (filled) Color.White else color, maxLines = 1,
        modifier = modifier.clip(RoundedCornerShape(4.dp))
            .background(if (filled) color.copy(alpha = 0.9f) else color.copy(alpha = 0.15f))
            .padding(horizontal = 5.dp, vertical = 1.dp),
    )
}

@Composable
fun SectionTitle(text: String, modifier: Modifier = Modifier, trailing: @Composable (() -> Unit)? = null) {
    Row(modifier.fillMaxWidth().padding(horizontal = 16.dp, vertical = 8.dp), verticalAlignment = Alignment.CenterVertically) {
        Text(text, style = MaterialTheme.typography.titleMedium, color = Color.White, modifier = Modifier.weight(1f))
        trailing?.invoke()
    }
}
