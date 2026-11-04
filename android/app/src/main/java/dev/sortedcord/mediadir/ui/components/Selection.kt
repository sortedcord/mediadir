package dev.sortedcord.mediadir.ui.components

import androidx.activity.compose.BackHandler
import androidx.compose.foundation.background
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.shape.CircleShape
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.filled.Close
import androidx.compose.material3.*
import androidx.compose.runtime.*
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import dev.sortedcord.mediadir.domain.ItemSummary
import dev.sortedcord.mediadir.ui.*

/** Ordered multi-selection of playable items. The first one selected is the merge target. */
class SelectionState {
    val items = mutableStateListOf<ItemSummary>()
    val active get() = items.isNotEmpty()
    /** 1-based selection order, or 0 when not selected. */
    fun order(id: String) = items.indexOfFirst { it.id == id } + 1
    fun toggle(item: ItemSummary) {
        val i = items.indexOfFirst { it.id == item.id }
        if (i >= 0) items.removeAt(i) else items.add(item)
    }
    fun clear() = items.clear()
}

val TargetColor = Color(0xFFE8A93A)

@Composable
fun SelectionBadge(order: Int, modifier: Modifier = Modifier) {
    Box(modifier.size(24.dp).background(if (order == 1) TargetColor else Accent, CircleShape), contentAlignment = Alignment.Center) {
        Text(order.toString(), color = if (order == 1) Color.Black else Color.White, fontSize = 12.sp, fontWeight = FontWeight.Bold)
    }
}

@OptIn(ExperimentalMaterial3Api::class)
@Composable
fun SelectionTopBar(vm: ScrollHolder) {
    val sel = vm.selection
    TopAppBar(
        title = { Text("${sel.items.size} selected") },
        navigationIcon = { IconButton(onClick = sel::clear) { Icon(Icons.Default.Close, "Cancel selection") } },
        actions = { TextButton(enabled = sel.items.size >= 2 && !vm.merging, onClick = { vm.confirmMerge = true }) { Text("Merge") } },
        colors = TopAppBarDefaults.topAppBarColors(containerColor = CardBg),
    )
}

/** Back handling, the confirmation dialog, progress and result snackbars for the selection/merge flow. */
@Composable
fun MergeHost(vm: ScrollHolder, snack: SnackbarHostState, onMerged: () -> Unit) {
    val sel = vm.selection
    BackHandler(enabled = sel.active) { sel.clear() }
    val msg = vm.mergeMessage
    LaunchedEffect(msg) { if (msg != null) { snack.showSnackbar(msg); vm.mergeMessage = null } }
    if (vm.confirmMerge && sel.items.size >= 2) {
        val target = sel.items.first()
        AlertDialog(
            onDismissRequest = { vm.confirmMerge = false },
            title = { Text("Merge items") },
            text = { Text("Merge ${sel.items.size} items into '${target.title}'? The other items' files become alternate versions; their metadata will be discarded.") },
            confirmButton = { TextButton(onClick = { vm.confirmMerge = false; vm.merge(onMerged) }) { Text("Merge") } },
            dismissButton = { TextButton(onClick = { vm.confirmMerge = false }) { Text("Cancel") } },
        )
    }
    if (vm.merging) LinearProgressIndicator(Modifier.fillMaxWidth(), color = Accent)
}
