package dev.sortedcord.mediadir.ui.screens

import androidx.compose.foundation.layout.*
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.LazyRow
import androidx.compose.foundation.lazy.items
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.filled.Search
import androidx.compose.material.icons.filled.Settings
import androidx.compose.material3.*
import androidx.compose.runtime.*
import androidx.compose.ui.Modifier
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.unit.dp
import androidx.lifecycle.Lifecycle
import androidx.lifecycle.compose.LifecycleEventEffect
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import androidx.lifecycle.viewmodel.compose.viewModel
import dev.sortedcord.mediadir.data.Graph
import dev.sortedcord.mediadir.domain.ItemSummary
import dev.sortedcord.mediadir.domain.Library
import dev.sortedcord.mediadir.ui.*
import dev.sortedcord.mediadir.ui.components.*
import kotlinx.coroutines.async
import kotlinx.coroutines.awaitAll
import kotlinx.coroutines.coroutineScope

data class HomeRow(val library: Library, val items: List<ItemSummary>, val total: Int)

class HomeViewModel : LoadViewModel<List<HomeRow>>() {
    init { load() }
    override suspend fun fetch(): List<HomeRow> = coroutineScope {
        val repo = Graph.repo
        repo.libraries().map { lib ->
            async {
                val p = runCatching { repo.items(libraryId = lib.id, sort = "added", order = "desc", limit = 20) }.getOrNull()
                HomeRow(lib, p?.items ?: emptyList(), p?.total ?: lib.itemCount)
            }
        }.awaitAll()
    }
}

@OptIn(ExperimentalMaterial3Api::class)
@Composable
fun HomeScreen(nav: Nav, vm: HomeViewModel = viewModel()) {
    val state by vm.state.collectAsStateWithLifecycle()
    LifecycleEventEffect(Lifecycle.Event.ON_RESUME) { vm.load(showSpinner = false) }
    Scaffold(
        containerColor = Bg,
        topBar = {
            TopAppBar(
                title = { Text("MediaDir", fontWeight = FontWeight.Bold) },
                actions = {
                    IconButton(onClick = nav::search) { Icon(Icons.Default.Search, "Search") }
                    IconButton(onClick = nav::settings) { Icon(Icons.Default.Settings, "Settings") }
                },
                colors = TopAppBarDefaults.topAppBarColors(containerColor = Bg),
            )
        },
    ) { pad ->
        Box(Modifier.padding(pad)) {
            StateContent(state, onRetry = { vm.load() }) { rows ->
                if (rows.isEmpty()) Text("No libraries on this server.", color = Muted, modifier = Modifier.padding(16.dp))
                LazyColumn(state = vm.listState("main"), contentPadding = PaddingValues(bottom = 24.dp)) {
                    items(rows, key = { it.library.id }) { row ->
                        SectionTitle(row.library.name + if (row.library.scanning) "  · scanning…" else "") {
                            TextButton(onClick = { nav.library(row.library.id) }) { Text("See all (${row.total})") }
                        }
                        val aspect = row.items.firstOrNull()?.aspect ?: "landscape"
                        LazyRow(state = vm.listState("row_" + row.library.id), contentPadding = PaddingValues(horizontal = 16.dp), horizontalArrangement = Arrangement.spacedBy(12.dp)) {
                            items(row.items, key = { it.id }) { item ->
                                MediaCard(item, Modifier.width(cardWidth(item.aspect.ifBlank { aspect })), onClick = nav::open)
                            }
                        }
                        Spacer(Modifier.height(8.dp))
                    }
                }
            }
        }
    }
}
