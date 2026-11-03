package dev.sortedcord.mediadir.ui.screens

import androidx.compose.foundation.layout.*
import androidx.compose.foundation.lazy.grid.*
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.automirrored.filled.ArrowBack
import androidx.compose.material.icons.automirrored.filled.Sort
import androidx.compose.material3.*
import androidx.compose.runtime.*
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.unit.dp
import androidx.lifecycle.ViewModel
import androidx.lifecycle.Lifecycle
import androidx.lifecycle.compose.LifecycleEventEffect
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import androidx.lifecycle.viewModelScope
import androidx.lifecycle.viewmodel.compose.viewModel
import dev.sortedcord.mediadir.data.Graph
import dev.sortedcord.mediadir.domain.ItemSummary
import dev.sortedcord.mediadir.domain.Library
import dev.sortedcord.mediadir.ui.*
import dev.sortedcord.mediadir.ui.components.*
import kotlinx.coroutines.Job
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.launch

data class LibraryData(val library: Library?, val items: List<ItemSummary>, val total: Int, val loadingMore: Boolean)

class LibraryViewModel(private val libraryId: String) : ViewModel() {
    val state = MutableStateFlow<UiState<LibraryData>>(UiState.Loading)
    val sort = MutableStateFlow("title")
    private var job: Job? = null
    private val pageSize = 60

    init { reload() }

    fun setSort(s: String) { if (s != sort.value) { sort.value = s; reload() } }

    fun reload() {
        job?.cancel(); state.value = UiState.Loading
        job = viewModelScope.launch {
            state.value = runCatching {
                val lib = runCatching { Graph.repo.libraries().firstOrNull { it.id == libraryId } }.getOrNull()
                val p = Graph.repo.items(libraryId = libraryId, sort = sort.value, order = order(), limit = pageSize, offset = 0)
                LibraryData(lib, p.items, p.total, false)
            }.fold({ UiState.Ready(it) }, { UiState.Error(it.message ?: "Error") })
        }
    }

    private fun order() = if (sort.value == "title") "asc" else "desc"

    fun loadMore() {
        val cur = (state.value as? UiState.Ready)?.data ?: return
        if (cur.loadingMore || cur.items.size >= cur.total) return
        state.value = UiState.Ready(cur.copy(loadingMore = true))
        job = viewModelScope.launch {
            val p = runCatching { Graph.repo.items(libraryId = libraryId, sort = sort.value, order = order(), limit = pageSize, offset = cur.items.size) }.getOrNull()
            state.value = UiState.Ready(cur.copy(items = cur.items + (p?.items ?: emptyList()).filter { n -> cur.items.none { it.id == n.id } },
                total = p?.total ?: cur.items.size, loadingMore = false))
        }
    }
}

@OptIn(ExperimentalMaterial3Api::class)
@Composable
fun LibraryScreen(nav: Nav, libraryId: String, vm: LibraryViewModel = viewModel(key = "lib$libraryId") { LibraryViewModel(libraryId) }) {
    val state by vm.state.collectAsStateWithLifecycle()
    LifecycleEventEffect(Lifecycle.Event.ON_RESUME) { vm.reload() }
    val sort by vm.sort.collectAsStateWithLifecycle()
    var menu by remember { mutableStateOf(false) }
    val title = ((state as? UiState.Ready)?.data?.library?.name) ?: "Library"
    Scaffold(
        containerColor = Bg,
        topBar = {
            TopAppBar(
                title = { Text(title) },
                navigationIcon = { IconButton(onClick = nav::back) { Icon(Icons.AutoMirrored.Filled.ArrowBack, "Back") } },
                actions = {
                    Box {
                        IconButton(onClick = { menu = true }) { Icon(Icons.AutoMirrored.Filled.Sort, "Sort") }
                        DropdownMenu(menu, onDismissRequest = { menu = false }) {
                            listOf("title" to "Title", "added" to "Recently added", "year" to "Year").forEach { (k, l) ->
                                DropdownMenuItem(text = { Text(l + if (k == sort) "  ✓" else "") }, onClick = { menu = false; vm.setSort(k) })
                            }
                        }
                    }
                },
                colors = TopAppBarDefaults.topAppBarColors(containerColor = Bg),
            )
        },
    ) { pad ->
        Box(Modifier.padding(pad)) {
            StateContent(state, onRetry = vm::reload) { data ->
                val aspect = data.items.firstOrNull()?.aspect ?: "landscape"
                val min = when (aspect) { "poster" -> 110.dp; "square" -> 140.dp; else -> 170.dp }
                val grid = rememberLazyGridState()
                val nearEnd by remember { derivedStateOf { grid.layoutInfo.visibleItemsInfo.lastOrNull()?.index?.let { it >= data.items.size - 12 } ?: false } }
                LaunchedEffect(nearEnd, data.items.size) { if (nearEnd) vm.loadMore() }
                if (data.items.isEmpty()) Text("Nothing here yet.", color = Muted, modifier = Modifier.padding(16.dp))
                LazyVerticalGrid(
                    GridCells.Adaptive(min), state = grid, contentPadding = PaddingValues(16.dp),
                    horizontalArrangement = Arrangement.spacedBy(12.dp), verticalArrangement = Arrangement.spacedBy(16.dp),
                ) {
                    items(data.items, key = { it.id }) { MediaCard(it, onClick = nav::open) }
                    if (data.loadingMore) item(span = { GridItemSpan(maxLineSpan) }) {
                        Box(Modifier.fillMaxWidth(), contentAlignment = Alignment.Center) { CircularProgressIndicator(color = Accent) }
                    }
                }
            }
        }
    }
}
