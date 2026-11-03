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

/** ViewModel holding a single loadable UiState. */
abstract class LoadViewModel<T> : ViewModel() {
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
