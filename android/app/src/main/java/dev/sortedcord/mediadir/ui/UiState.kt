package dev.sortedcord.mediadir.ui

import androidx.compose.runtime.getValue
import androidx.compose.runtime.setValue
import androidx.lifecycle.ViewModel
import androidx.lifecycle.viewModelScope
import kotlinx.coroutines.Job
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.launch

sealed interface UiState<out T> {
    data object Loading : UiState<Nothing>
    data class Error(val message: String) : UiState<Nothing>
    data class Ready<T>(val data: T) : UiState<T>
}

/** Scroll positions hoisted into the ViewModel so they survive navigation and state swaps (Loading/Ready). */
open class ScrollHolder : ViewModel() {
    private val lists = HashMap<String, androidx.compose.foundation.lazy.LazyListState>()
    private val grids = HashMap<String, androidx.compose.foundation.lazy.grid.LazyGridState>()
    fun listState(key: String) = lists.getOrPut(key) { androidx.compose.foundation.lazy.LazyListState() }
    fun gridState(key: String) = grids.getOrPut(key) { androidx.compose.foundation.lazy.grid.LazyGridState() }

    // multi-select + merge (selection survives scroll and recomposition; first selected = merge target)
    val selection = dev.sortedcord.mediadir.ui.components.SelectionState()
    var confirmMerge by androidx.compose.runtime.mutableStateOf(false)
    var merging by androidx.compose.runtime.mutableStateOf(false)
    var mergeMessage by androidx.compose.runtime.mutableStateOf<String?>(null)

    /** Merge the selection into its first item, then run [onDone] (refresh) and leave selection mode. */
    fun merge(onDone: () -> Unit) {
        val items = selection.items.toList()
        if (items.size < 2 || merging) return
        merging = true
        viewModelScope.launch {
            runCatching { dev.sortedcord.mediadir.data.Graph.repo.mergeItems(items.first().id, items.drop(1).map { it.id }) }
                .onSuccess { mergeMessage = "Merged ${items.size} items into '${items.first().title}'"; selection.clear(); onDone() }
                .onFailure { mergeMessage = "Merge failed: ${it.message ?: it.javaClass.simpleName}" }
            merging = false
        }
    }
}

/** ViewModel holding a single loadable UiState. */
abstract class LoadViewModel<T> : ScrollHolder() {
    protected val _state = MutableStateFlow<UiState<T>>(UiState.Loading)
    val state: StateFlow<UiState<T>> = _state
    private var job: Job? = null

    protected abstract suspend fun fetch(): T

    fun load(showSpinner: Boolean = _state.value !is UiState.Ready) {
        job?.cancel()
        if (showSpinner) _state.value = UiState.Loading
        job = viewModelScope.launch {
            _state.value = runCatching { fetch() }.fold({ UiState.Ready(it) }, { UiState.Error(it.message ?: it.javaClass.simpleName) })
        }
    }
}
