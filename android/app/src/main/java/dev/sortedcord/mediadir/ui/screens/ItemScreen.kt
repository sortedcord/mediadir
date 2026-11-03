package dev.sortedcord.mediadir.ui.screens

import androidx.compose.foundation.background
import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.*
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.LazyListScope
import androidx.compose.foundation.lazy.LazyRow
import androidx.compose.foundation.lazy.items
import androidx.compose.foundation.shape.CircleShape
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.automirrored.filled.ArrowBack
import androidx.compose.material.icons.filled.Delete
import androidx.compose.material.icons.filled.Edit
import androidx.compose.material.icons.filled.PlayArrow
import androidx.compose.material3.*
import androidx.compose.runtime.*
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.graphics.Brush
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.layout.ContentScale
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import androidx.lifecycle.Lifecycle
import androidx.lifecycle.compose.LifecycleEventEffect
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import androidx.lifecycle.viewModelScope
import androidx.lifecycle.viewmodel.compose.viewModel
import coil.compose.AsyncImage
import dev.sortedcord.mediadir.data.Graph
import dev.sortedcord.mediadir.domain.*
import dev.sortedcord.mediadir.ui.*
import dev.sortedcord.mediadir.ui.components.*
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.launch

class ItemViewModel(val id: String) : LoadViewModel<ItemDetail>() {
    val fieldDefs = MutableStateFlow<List<FieldDefinition>>(emptyList())
    val message = MutableStateFlow<String?>(null)
    /** Episodes of the selected season when viewing a show. */
    val season = MutableStateFlow<Pair<String, UiState<List<ItemSummary>>>?>(null)

    init { load() }
    override suspend fun fetch() = Graph.repo.item(id)

    fun loadFieldDefs() = viewModelScope.launch { runCatching { Graph.repo.fields() }.onSuccess { fieldDefs.value = it } }

    fun selectSeason(seasonId: String) {
        season.value = seasonId to UiState.Loading
        viewModelScope.launch {
            val r = runCatching { Graph.repo.item(seasonId).children }
            season.value = seasonId to r.fold({ UiState.Ready(it) }, { UiState.Error(it.message ?: "Error") })
        }
    }

    /** Runs an edit returning the new detail. */
    fun edit(block: suspend () -> ItemDetail) = viewModelScope.launch {
        runCatching { block() }.onSuccess { ImageResolver.clear(); _state.value = UiState.Ready(it) }.onFailure { message.value = it.message }
    }
}

@OptIn(ExperimentalMaterial3Api::class)
@Composable
fun ItemScreen(nav: Nav, id: String, vm: ItemViewModel = viewModel(key = "item$id") { ItemViewModel(id) }) {
    val state by vm.state.collectAsStateWithLifecycle()
    val msg by vm.message.collectAsStateWithLifecycle()
    val scope = rememberCoroutineScope()
    var deleting by remember { mutableStateOf(false) }
    var deleteTarget by remember { mutableStateOf<Pair<String?, String>?>(null) }
    var editing by remember { mutableStateOf(false) }
    var resumeTick by remember { mutableIntStateOf(0) }
    LifecycleEventEffect(Lifecycle.Event.ON_RESUME) { resumeTick++ }
    val snack = remember { SnackbarHostState() }
    LaunchedEffect(msg) { msg?.let { snack.showSnackbar(it); vm.message.value = null } }

    Scaffold(containerColor = Bg, snackbarHost = { SnackbarHost(snack) }) { pad ->
        Box(Modifier.fillMaxSize()) {
            StateContent(state, onRetry = { vm.load() }) { d ->
                ItemContent(nav, vm, d, resumeTick, pad.calculateBottomPadding()) { file -> deleteTarget = file.id to (file.path ?: file.label) }
            }
            Row(Modifier.statusBarsPadding().padding(8.dp).fillMaxWidth()) {
                RoundIcon(onClick = nav::back) { Icon(Icons.AutoMirrored.Filled.ArrowBack, "Back") }
                Spacer(Modifier.weight(1f))
                if (state is UiState.Ready) RoundIcon(onClick = { deleteTarget = null to (state as UiState.Ready<ItemDetail>).data.summary.title }) { Icon(Icons.Default.Delete, "Delete item") }
                if (state is UiState.Ready) RoundIcon(onClick = { vm.loadFieldDefs(); editing = true }) { Icon(Icons.Default.Edit, "Edit") }
            }
        }
    }
    deleteTarget?.let { target ->
        AlertDialog(onDismissRequest = { if (!deleting) deleteTarget = null },
            title = { Text("Delete ${target.second}?") },
            text = { Text(if (target.first == null) "This removes this item, its children, and their stored media files. This cannot be undone."
                else "This permanently removes this stored version. Removing the last version also removes the item. This cannot be undone.") },
            confirmButton = { TextButton(enabled = !deleting, onClick = {
                scope.launch {
                    deleting = true
                    try {
                        if (target.first == null) Graph.repo.deleteItem(id) else Graph.repo.deleteFile(target.first!!)
                        deleteTarget = null
                        if (target.first == null) nav.home()
                        else try { Graph.repo.item(id); vm.load() } catch (e: dev.sortedcord.mediadir.data.ApiException) {
                            if (e.code == 404) nav.home() else throw e
                        }
                    } catch (e: Exception) { vm.message.value = e.message }
                    finally { deleting = false }
                }
            }) { Text(if (deleting) "Deleting…" else "Delete permanently") } },
            dismissButton = { TextButton(enabled = !deleting, onClick = { deleteTarget = null }) { Text("Cancel") } })
    }

    val d = (state as? UiState.Ready)?.data
    if (editing && d != null) ItemEditSheet(vm, d, onDismiss = { editing = false })
}

@Composable
fun RoundIcon(onClick: () -> Unit, content: @Composable () -> Unit) {
    Box(Modifier.size(40.dp).clip(CircleShape).background(Color.Black.copy(alpha = 0.45f)).clickable(onClick = onClick), contentAlignment = Alignment.Center) {
        CompositionLocalProvider(LocalContentColor provides Color.White) { content() }
    }
}

@OptIn(ExperimentalLayoutApi::class)
@Composable
private fun ItemContent(nav: Nav, vm: ItemViewModel, d: ItemDetail, resumeTick: Int, bottom: androidx.compose.ui.unit.Dp, onDeleteFile: (MediaFile) -> Unit) {
    val s = d.summary
    val settings = Graph.settings
    val header = d.display.filter { it.placement == "header" && it.type != "image" }
    val pos = remember(resumeTick, s.id) { settings.position(s.id) }
    LazyColumn(Modifier.fillMaxSize(), contentPadding = PaddingValues(bottom = bottom + 24.dp)) {
        item {
            val own = remember(d) { ImagePolicy.card(d.fields) ?: ImagePolicy.card(s.imageValues) }
            val explicitBackdrop = remember(d) { ImagePolicy.backdropOnly(d.fields) }
            // Parent-chain fallback only when this item has no image of its own.
            val inherited by produceState<String?>(null, d, own) {
                value = if (own != null || s.parentId == null) null else ImageResolver.byId(s.parentId)
            }
            val card = own ?: inherited
            val hero = ImagePolicy.backdrop(d.fields) ?: card
            Box(Modifier.fillMaxWidth().height(if (s.aspect == "landscape" || explicitBackdrop != null) 260.dp else 220.dp)) {
                AssetImage(hero, 480.dp, 260.dp, description = s.title)
                Box(Modifier.fillMaxSize().background(Brush.verticalGradient(listOf(Color.Transparent, Bg.copy(alpha = 0.6f), Bg))))
                if (own != null && explicitBackdrop != null && s.aspect != "landscape") {
                    Box(Modifier.align(Alignment.BottomStart).padding(16.dp).width(if (s.aspect == "poster") 90.dp else 110.dp)
                        .aspectRatio(aspectRatio(s.aspect)).clip(RoundedCornerShape(8.dp))) {
                        AssetImage(own, 110.dp, 110.dp / aspectRatio(s.aspect))
                    }
                } else if (hero == null) {
                    Text(s.title.take(1), fontSize = 64.sp, color = Muted, modifier = Modifier.align(Alignment.Center))
                }
            }
        }
        item {
            Column(Modifier.padding(horizontal = 16.dp)) {
                if (d.breadcrumbs.isNotEmpty()) Row {
                    d.breadcrumbs.forEachIndexed { i, b ->
                        Text(b.title + if (i < d.breadcrumbs.lastIndex) "  ›  " else "", color = Accent, fontSize = 13.sp, modifier = Modifier.clickable { nav.item(b.id) })
                    }
                }
                Text(s.title, style = MaterialTheme.typography.headlineSmall, fontWeight = FontWeight.Bold, color = Color.White)
                s.subtitle?.takeIf { it.isNotBlank() }?.let { Text(it, color = Muted, fontSize = 14.sp) }
                Spacer(Modifier.height(8.dp))
                HeaderBlocks(header, nav::person)
                Spacer(Modifier.height(12.dp))
                if (s.playable || d.children.isNotEmpty()) {
                    Button(onClick = { nav.play(s.id) }, colors = ButtonDefaults.buttonColors(containerColor = Accent)) {
                        Icon(Icons.Default.PlayArrow, null)
                        Spacer(Modifier.width(6.dp))
                        Text(if (s.playable && pos > 0) "Resume from ${(pos / 1000.0).fmtDuration()}" else if (s.playable) "Play" else "Play all")
                    }
                    if (s.playable && pos > 0) TextButton(onClick = { settings.savePosition(s.id, 0); nav.play(s.id) }) { Text("Play from start") }
                }
                Spacer(Modifier.height(8.dp))
            }
        }
        if (d.files.isNotEmpty()) item {
            Column(Modifier.padding(horizontal = 16.dp)) {
                Text("Files & versions", style = MaterialTheme.typography.titleMedium)
                d.files.forEach { file ->
                    Row(verticalAlignment = Alignment.CenterVertically) {
                        Column(Modifier.weight(1f)) {
                            Text(file.label, color = Color.White)
                            file.path?.let { Text(it, color = Muted, fontSize = 12.sp) }
                        }
                        IconButton(onClick = { onDeleteFile(file) }) { Icon(Icons.Default.Delete, "Delete version") }
                    }
                }
            }
        }
        item { DetailBlocks(d.display, nav::person) }
        if (d.extras.isNotEmpty()) item {
            SectionTitle("Extras")
            LazyRow(contentPadding = PaddingValues(horizontal = 16.dp), horizontalArrangement = Arrangement.spacedBy(12.dp)) {
                items(d.extras, key = { it.id }) { x -> MediaCard(x, Modifier.width(200.dp), onClick = { nav.play(it.id) }) }
            }
        }
        if (d.credits.isNotEmpty()) item {
            SectionTitle("Cast & crew")
            LazyRow(contentPadding = PaddingValues(horizontal = 16.dp), horizontalArrangement = Arrangement.spacedBy(12.dp)) {
                items(d.credits, key = { it.id }) { c ->
                    PersonTile(c.person.id, c.person.name, c.character ?: c.role.titleCase(), nav::person)
                }
            }
        }
        relationsSection(nav, d)
        children(nav, vm, d)
    }
}

private fun LazyListScope.relationsSection(nav: Nav, d: ItemDetail) {
    if (d.relations.isEmpty()) return
    item(key = "relations") {
        val details by produceState<List<RelationDetail>>(emptyList(), d.relations) {
            value = d.relations.mapNotNull { r -> runCatching { Graph.repo.relation(r.id) }.getOrNull() }
        }
        Column {
            details.groupBy { it.summary.type }.forEach { (type, rels) ->
                rels.forEach { r ->
                    SectionTitle((r.summary.name ?: type.replace('_', ' ').titleCase()).let { if (r.summary.name != null) "$it · ${type.replace('_', ' ')}" else it })
                    LazyRow(contentPadding = PaddingValues(horizontal = 16.dp), horizontalArrangement = Arrangement.spacedBy(12.dp)) {
                        items(r.members, key = { it.itemId }) { m ->
                            Column(Modifier.width(cardWidth(m.item.aspect))) {
                                MediaCard(m.item, onClick = { nav.item(it.id) })
                                m.role?.let { Text(it, color = Muted, fontSize = 11.sp, maxLines = 1) }
                                if (m.itemId == d.summary.id) Text("This item", color = Accent, fontSize = 11.sp)
                            }
                        }
                    }
                }
            }
        }
    }
}

private fun LazyListScope.children(nav: Nav, vm: ItemViewModel, d: ItemDetail) {
    val kids = d.children
    if (kids.isEmpty()) return
    val kind = kids.first().kind
    when {
        kind == "season" -> item {
            val sel by vm.season.collectAsStateWithLifecycle()
            LaunchedEffect(d.summary.id) { if (vm.season.value == null) vm.selectSeason(kids.first().id) }
            SectionTitle("Seasons")
            LazyRow(contentPadding = PaddingValues(horizontal = 16.dp), horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                items(kids, key = { it.id }) { k ->
                    FilterChip(selected = sel?.first == k.id, onClick = { vm.selectSeason(k.id) }, label = { Text(k.title) })
                }
            }
            when (val st = sel?.second) {
                is UiState.Ready -> Column { st.data.forEach { EpisodeRow(it, nav) } }
                is UiState.Error -> Text(st.message, color = Muted, modifier = Modifier.padding(16.dp))
                else -> Box(Modifier.fillMaxWidth().padding(16.dp), contentAlignment = Alignment.Center) { CircularProgressIndicator(color = Accent) }
            }
            TextButton(onClick = { sel?.first?.let(nav::item) }, modifier = Modifier.padding(horizontal = 8.dp)) { Text("Open season") }
        }
        kind == "episode" -> {
            item { SectionTitle("Episodes") }
            items(kids, key = { it.id }) { EpisodeRow(it, nav) }
        }
        kind == "track" -> {
            item { SectionTitle("Tracks") }
            items(kids, key = { it.id }) { TrackRow(it, nav) }
        }
        else -> {
            item { SectionTitle(when (kind) { "album" -> "Albums"; else -> "Contents" }) }
            val perRow = when (kids.first().aspect) { "landscape" -> 2; else -> 3 }
            items(kids.chunked(perRow)) { row ->
                Row(Modifier.padding(horizontal = 16.dp, vertical = 6.dp), horizontalArrangement = Arrangement.spacedBy(12.dp)) {
                    row.forEach { MediaCard(it, Modifier.weight(1f), onClick = nav::open) }
                    repeat(perRow - row.size) { Spacer(Modifier.weight(1f)) }
                }
            }
        }
    }
}

@Composable
private fun EpisodeRow(e: ItemSummary, nav: Nav) {
    Row(Modifier.fillMaxWidth().clickable { nav.item(e.id) }.padding(horizontal = 16.dp, vertical = 8.dp), verticalAlignment = Alignment.CenterVertically) {
        Box(Modifier.width(150.dp).aspectRatio(16f / 9f).clip(RoundedCornerShape(8.dp)).background(CardBg)) {
            AssetImage(rememberCardHash(e), 150.dp, 84.dp)
            Box(Modifier.align(Alignment.Center).size(32.dp).clip(CircleShape).background(Color.Black.copy(alpha = 0.5f)).clickable { nav.play(e.id) }, contentAlignment = Alignment.Center) {
                Icon(Icons.Default.PlayArrow, "Play", tint = Color.White)
            }
            Row(Modifier.align(Alignment.TopStart).padding(4.dp), horizontalArrangement = Arrangement.spacedBy(3.dp)) {
                e.card.filter { it.placement == "card_badge" }.take(3).forEach { Pill(it.text, parseColor(it.color, Accent)) }
            }
        }
        Spacer(Modifier.width(12.dp))
        Column(Modifier.weight(1f)) {
            Text((e.episodeLabel?.let { "$it · " } ?: "") + e.title, color = Color.White, fontSize = 14.sp, maxLines = 2, overflow = TextOverflow.Ellipsis)
            val meta = listOfNotNull(e.duration?.takeIf { it > 0 }?.fmtDuration()) + e.card.filter { it.placement == "card_meta" }.map { it.text }
            if (meta.isNotEmpty()) Text(meta.joinToString(" · "), color = Muted, fontSize = 12.sp, maxLines = 1)
        }
    }
}

@Composable
private fun TrackRow(t: ItemSummary, nav: Nav) {
    Row(Modifier.fillMaxWidth().clickable { nav.play(t.id) }.padding(horizontal = 16.dp, vertical = 10.dp), verticalAlignment = Alignment.CenterVertically) {
        Text((t.index ?: 0).takeIf { it > 0 }?.toString() ?: "•", color = Muted, modifier = Modifier.width(32.dp))
        Column(Modifier.weight(1f).clickable { nav.item(t.id) }) {
            Text(t.title, color = Color.White, fontSize = 14.sp, maxLines = 1, overflow = TextOverflow.Ellipsis)
            val meta = listOfNotNull(t.subtitle) + t.card.map { it.text }
            if (meta.isNotEmpty()) Text(meta.joinToString(" · "), color = Muted, fontSize = 12.sp, maxLines = 1, overflow = TextOverflow.Ellipsis)
        }
        t.duration?.takeIf { it > 0 }?.let { Text(it.fmtDuration(), color = Muted, fontSize = 12.sp) }
    }
}
