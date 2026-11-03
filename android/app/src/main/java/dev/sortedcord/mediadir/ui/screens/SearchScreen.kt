package dev.sortedcord.mediadir.ui.screens

import androidx.compose.foundation.layout.*
import androidx.compose.foundation.lazy.grid.*
import androidx.compose.foundation.lazy.LazyRow
import androidx.compose.foundation.lazy.items
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.automirrored.filled.ArrowBack
import androidx.compose.material3.*
import androidx.compose.runtime.*
import androidx.compose.ui.Modifier
import androidx.compose.ui.focus.FocusRequester
import androidx.compose.ui.focus.focusRequester
import androidx.compose.ui.unit.dp
import androidx.lifecycle.ViewModel
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import androidx.lifecycle.viewModelScope
import androidx.lifecycle.viewmodel.compose.viewModel
import dev.sortedcord.mediadir.data.Graph
import dev.sortedcord.mediadir.domain.SearchResult
import dev.sortedcord.mediadir.ui.*
import dev.sortedcord.mediadir.ui.components.*
import kotlinx.coroutines.Job
import kotlinx.coroutines.delay
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.launch

class SearchViewModel : ViewModel() {
    val query = MutableStateFlow("")
    val state = MutableStateFlow<UiState<SearchResult>?>(null)
    private var job: Job? = null
    fun setQuery(q: String) {
        query.value = q; job?.cancel()
        if (q.isBlank()) { state.value = null; return }
        job = viewModelScope.launch {
            delay(300); state.value = UiState.Loading
            state.value = runCatching { Graph.repo.search(q.trim()) }.fold({ UiState.Ready(it) }, { UiState.Error(it.message ?: "Error") })
        }
    }
}

@OptIn(ExperimentalMaterial3Api::class)
@Composable
fun SearchScreen(nav: Nav, vm: SearchViewModel = viewModel()) {
    val q by vm.query.collectAsStateWithLifecycle()
    val state by vm.state.collectAsStateWithLifecycle()
    val focus = remember { FocusRequester() }
    LaunchedEffect(Unit) { runCatching { focus.requestFocus() } }
    Scaffold(containerColor = Bg, topBar = {
        TopAppBar(
            navigationIcon = { IconButton(onClick = nav::back) { Icon(Icons.AutoMirrored.Filled.ArrowBack, "Back") } },
            title = {
                TextField(q, vm::setQuery, placeholder = { Text("Search titles and people") }, singleLine = true,
                    colors = TextFieldDefaults.colors(focusedContainerColor = Bg, unfocusedContainerColor = Bg),
                    modifier = Modifier.fillMaxWidth().focusRequester(focus))
            },
            colors = TopAppBarDefaults.topAppBarColors(containerColor = Bg),
        )
    }) { pad ->
        Box(Modifier.padding(pad)) {
            val s = state ?: return@Box
            StateContent(s, onRetry = { vm.setQuery(q) }) { r ->
                LazyVerticalGrid(GridCells.Adaptive(160.dp), contentPadding = PaddingValues(16.dp),
                    horizontalArrangement = Arrangement.spacedBy(12.dp), verticalArrangement = Arrangement.spacedBy(16.dp)) {
                    if (r.people.isNotEmpty()) item(span = { GridItemSpan(maxLineSpan) }) {
                        Column {
                            Text("People", style = MaterialTheme.typography.titleMedium)
                            Spacer(Modifier.height(8.dp))
                            LazyRow(horizontalArrangement = Arrangement.spacedBy(12.dp)) {
                                items(r.people, key = { it.id }) { p -> PersonTile(p.id, p.name, "${p.creditCount} credits", nav::person, images = p.images) }
                            }
                        }
                    }
                    if (r.items.isNotEmpty()) item(span = { GridItemSpan(maxLineSpan) }) { Text("Items", style = MaterialTheme.typography.titleMedium) }
                    items(r.items, key = { it.id }) { MediaCard(it, onClick = nav::open) }
                    if (r.items.isEmpty() && r.people.isEmpty()) item(span = { GridItemSpan(maxLineSpan) }) { Text("No results", color = Muted) }
                }
            }
        }
    }
}
