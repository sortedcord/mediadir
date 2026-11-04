package dev.sortedcord.mediadir.ui.screens

import androidx.compose.foundation.background
import androidx.compose.foundation.border
import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.*
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.items
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.shape.CircleShape
import androidx.compose.foundation.text.KeyboardOptions
import androidx.compose.foundation.verticalScroll
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.automirrored.filled.ArrowBack
import androidx.compose.material.icons.filled.Add
import androidx.compose.material.icons.filled.Block
import androidx.compose.material3.*
import androidx.compose.runtime.*
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.text.input.KeyboardType
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import androidx.lifecycle.viewModelScope
import androidx.lifecycle.viewmodel.compose.viewModel
import dev.sortedcord.mediadir.data.Graph
import dev.sortedcord.mediadir.domain.*
import dev.sortedcord.mediadir.ui.*
import dev.sortedcord.mediadir.ui.components.*
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.launch

class FieldSettingsViewModel : LoadViewModel<List<FieldDefinition>>() {
    val message = MutableStateFlow<String?>(null)
    init { load() }
    override suspend fun fetch() = Graph.repo.fields().sortedBy { it.display.order }
    fun run(block: suspend () -> Unit) = viewModelScope.launch {
        runCatching { block() }.onSuccess { load(false) }.onFailure { message.value = it.message }
    }
}

@OptIn(ExperimentalMaterial3Api::class)
@Composable
fun FieldSettingsScreen(nav: Nav, vm: FieldSettingsViewModel = viewModel()) {
    val state by vm.state.collectAsStateWithLifecycle()
    val msg by vm.message.collectAsStateWithLifecycle()
    var editing by remember { mutableStateOf<FieldDefinition?>(null) }
    var creating by remember { mutableStateOf(false) }
    val snack = remember { SnackbarHostState() }
    LaunchedEffect(msg) { msg?.let { snack.showSnackbar(it); vm.message.value = null } }
    Scaffold(containerColor = Bg, snackbarHost = { SnackbarHost(snack) },
        topBar = {
            TopAppBar(title = { Text("Library & metadata") }, navigationIcon = { IconButton(onClick = nav::back) { Icon(Icons.AutoMirrored.Filled.ArrowBack, "Back") } },
                colors = TopAppBarDefaults.topAppBarColors(containerColor = Bg))
        },
        floatingActionButton = { ExtendedFloatingActionButton(onClick = { creating = true }, containerColor = Accent, icon = { Icon(Icons.Default.Add, null) }, text = { Text("Custom field") }) },
    ) { pad ->
        Box(Modifier.padding(pad)) {
            StateContent(state, onRetry = { vm.load() }) { defs ->
                LazyColumn(contentPadding = PaddingValues(bottom = 96.dp)) {
                    items(defs, key = { it.key }) { f ->
                        Row(Modifier.fillMaxWidth().clickable { editing = f }.padding(horizontal = 16.dp, vertical = 10.dp), verticalAlignment = Alignment.CenterVertically) {
                            Box(Modifier.size(14.dp).clip(CircleShape).background(parseColor(f.display.color, Muted)))
                            Spacer(Modifier.width(12.dp))
                            Column(Modifier.weight(1f)) {
                                Row(verticalAlignment = Alignment.CenterVertically) {
                                    Text(f.label, color = if (f.display.placement == "hidden") Muted else Color.White)
                                    Spacer(Modifier.width(6.dp))
                                    Pill(if (f.builtin) "builtin" else "custom", if (f.builtin) Muted else Accent, filled = false)
                                    if (f.derived) { Spacer(Modifier.width(4.dp)); Pill("derived", Muted, filled = false) }
                                }
                                Text("${f.key} · ${f.type} · ${f.display.placement} / ${f.display.widget} · #${f.display.order}", color = Muted, fontSize = 12.sp)
                                Text(if (f.appliesTo.isEmpty()) "all kinds" else f.appliesTo.joinToString(), color = Muted, fontSize = 11.sp)
                            }
                            if (f.display.placement == "hidden") Icon(Icons.Default.Block, "Hidden", tint = Muted)
                        }
                        HorizontalDivider(color = CardBg)
                    }
                }
            }
        }
    }
    editing?.let { f ->
        FieldSheet(f, isNew = false, onDismiss = { editing = null },
            onSave = { vm.run { Graph.repo.updateField(it) }; editing = null },
            onDelete = { vm.run { Graph.repo.deleteField(f.key) }; editing = null })
    }
    if (creating) {
        val blank = FieldDefinition("", "", "text", builtin = false, derived = false, appliesTo = emptyList(),
            display = FieldDisplay("info", "text", 100, null, null, null))
        FieldSheet(blank, isNew = true, onDismiss = { creating = false },
            onSave = { vm.run { Graph.repo.createField(it) }; creating = false }, onDelete = {})
    }
}

private fun defaultWidget(type: String) = when (type) {
    "tags" -> "tags"; "rating" -> "rating"; "url" -> "link"; "person" -> "people"; "bool" -> "badge"; "image" -> "image"; else -> "text"
}

@OptIn(ExperimentalMaterial3Api::class, ExperimentalLayoutApi::class)
@Composable
private fun FieldSheet(f: FieldDefinition, isNew: Boolean, onDismiss: () -> Unit, onSave: (FieldDefinition) -> Unit, onDelete: () -> Unit) {
    var key by remember { mutableStateOf(f.key) }
    var label by remember { mutableStateOf(f.label) }
    var type by remember { mutableStateOf(f.type) }
    var applies by remember { mutableStateOf(f.appliesTo.toSet()) }
    var placement by remember { mutableStateOf(f.display.placement) }
    var widget by remember { mutableStateOf(f.display.widget) }
    var order by remember { mutableStateOf(f.display.order.toString()) }
    var format by remember { mutableStateOf(f.display.format ?: "") }
    var color by remember { mutableStateOf(f.display.color) }
    var confirmDelete by remember { mutableStateOf(false) }
    val keyOk = Regex("[a-z][a-z0-9_]*").matches(key)
    ModalBottomSheet(onDismissRequest = onDismiss, sheetState = rememberModalBottomSheetState(skipPartiallyExpanded = true), containerColor = CardBg) {
        Column(Modifier.fillMaxWidth().verticalScroll(rememberScrollState()).padding(horizontal = 16.dp).padding(bottom = 32.dp).imePadding()) {
            Text(if (isNew) "New custom field" else f.label, style = MaterialTheme.typography.titleLarge)
            if (isNew) OutlinedTextField(key, { key = it.lowercase().replace(' ', '_') }, label = { Text("Key") }, isError = key.isNotEmpty() && !keyOk,
                singleLine = true, modifier = Modifier.fillMaxWidth())
            else Text("${f.key} · ${f.type}" + if (f.derived) " · derived" else "", color = Muted, fontSize = 12.sp)
            OutlinedTextField(label, { label = it }, label = { Text("Label") }, singleLine = true, modifier = Modifier.fillMaxWidth())
            if (isNew) {
                Label("Type")
                Choices(Kinds.FIELD_TYPES, type) { type = it; widget = defaultWidget(it); if (it == "image") placement = "hidden" }
            }
            run {
                Label("Applies to (none = all kinds)")
                FlowRow(horizontalArrangement = Arrangement.spacedBy(6.dp)) {
                    (Kinds.ITEM_KINDS + "person").forEach { k ->
                        FilterChip(k in applies, { applies = if (k in applies) applies - k else applies + k }, label = { Text(k) })
                    }
                }
            }
            Label("Placement")
            Choices(Kinds.PLACEMENTS, placement) { placement = it }
            Label("Widget")
            Choices(Kinds.WIDGETS, widget) { widget = it }
            Row {
                OutlinedTextField(order, { order = it.filter { c -> c.isDigit() || c == '-' } }, label = { Text("Order") }, singleLine = true,
                    keyboardOptions = KeyboardOptions(keyboardType = KeyboardType.Number), modifier = Modifier.weight(1f))
                Spacer(Modifier.width(8.dp))
                OutlinedTextField(format, { format = it }, label = { Text("Format e.g. {value}fps") }, singleLine = true, modifier = Modifier.weight(2f))
            }
            Label("Color")
            FlowRow(horizontalArrangement = Arrangement.spacedBy(8.dp), verticalArrangement = Arrangement.spacedBy(8.dp)) {
                Box(Modifier.size(32.dp).clip(CircleShape).border(2.dp, if (color == null) Color.White else Muted, CircleShape).clickable { color = null },
                    contentAlignment = Alignment.Center) { Text("—", color = Muted) }
                SwatchHex.forEach { hex ->
                    Box(Modifier.size(32.dp).clip(CircleShape).background(parseColor(hex, Muted))
                        .border(if (color.equals(hex, true)) 3.dp else 0.dp, Color.White, CircleShape).clickable { color = hex })
                }
            }
            Spacer(Modifier.height(16.dp))
            Row(verticalAlignment = Alignment.CenterVertically) {
                Button(enabled = label.isNotBlank() && (!isNew || keyOk), onClick = {
                    onSave(f.copy(key = key, label = label.trim(), type = type, appliesTo = applies.toList(),
                        display = f.display.copy(placement = placement, widget = widget, order = order.toIntOrNull() ?: f.display.order,
                            format = format.ifBlank { null }, color = color)))
                }, colors = ButtonDefaults.buttonColors(containerColor = Accent)) { Text(if (isNew) "Create" else "Save") }
                Spacer(Modifier.width(8.dp))
                if (!isNew && placement != "hidden") TextButton(onClick = { placement = "hidden" }) { Text("Hide") }
                Spacer(Modifier.weight(1f))
                if (!isNew && !f.builtin) TextButton(onClick = { confirmDelete = true }) { Text("Delete", color = Accent) }
                if (!isNew && f.builtin) TextButton(onClick = onDelete) { Text("Reset to default", color = Accent) }
            }
        }
    }
    if (confirmDelete) AlertDialog(onDismissRequest = { confirmDelete = false },
        title = { Text("Delete ${f.label}?") }, text = { Text("This also deletes all values of this field.") },
        confirmButton = { TextButton(onClick = { confirmDelete = false; onDelete() }) { Text("Delete", color = Accent) } },
        dismissButton = { TextButton(onClick = { confirmDelete = false }) { Text("Cancel") } })
}

@Composable private fun Label(t: String) = Text(t, color = Muted, fontSize = 13.sp, modifier = Modifier.padding(top = 12.dp, bottom = 4.dp))

@OptIn(ExperimentalLayoutApi::class)
@Composable
private fun Choices(options: List<String>, selected: String, onSelect: (String) -> Unit) {
    FlowRow(horizontalArrangement = Arrangement.spacedBy(6.dp)) {
        options.forEach { o -> FilterChip(o == selected, { onSelect(o) }, label = { Text(o) }) }
    }
}
