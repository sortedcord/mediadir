package dev.sortedcord.mediadir.ui

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
