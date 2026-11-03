package dev.sortedcord.mediadir.ui.screens

import androidx.compose.foundation.layout.*
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.LazyRow
import androidx.compose.foundation.lazy.items
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.verticalScroll
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.automirrored.filled.ArrowBack
import androidx.compose.material.icons.filled.Edit
import androidx.compose.material3.*
import androidx.compose.runtime.*
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.text.font.FontWeight
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

class PersonViewModel(val id: String) : LoadViewModel<PersonDetail>() {
    val fieldDefs = MutableStateFlow<List<FieldDefinition>>(emptyList())
    val message = MutableStateFlow<String?>(null)
    init { load() }
    override suspend fun fetch() = Graph.repo.person(id)
    fun loadFieldDefs() = viewModelScope.launch { runCatching { Graph.repo.fields() }.onSuccess { fieldDefs.value = it } }
    fun edit(block: suspend () -> Unit) = viewModelScope.launch {
        runCatching { block(); fetch() }.onSuccess { _state.value = UiState.Ready(it) }.onFailure { message.value = it.message }
    }
}

@OptIn(ExperimentalMaterial3Api::class)
@Composable
fun PersonScreen(nav: Nav, id: String, vm: PersonViewModel = viewModel(key = "person$id") { PersonViewModel(id) }) {
    val state by vm.state.collectAsStateWithLifecycle()
    val msg by vm.message.collectAsStateWithLifecycle()
    var editing by remember { mutableStateOf(false) }
    val snack = remember { SnackbarHostState() }
    LaunchedEffect(msg) { msg?.let { snack.showSnackbar(it); vm.message.value = null } }
    Scaffold(
        containerColor = Bg, snackbarHost = { SnackbarHost(snack) },
        topBar = {
            TopAppBar(title = {}, navigationIcon = { IconButton(onClick = nav::back) { Icon(Icons.AutoMirrored.Filled.ArrowBack, "Back") } },
                actions = { if (state is UiState.Ready) IconButton(onClick = { vm.loadFieldDefs(); editing = true }) { Icon(Icons.Default.Edit, "Edit") } },
                colors = TopAppBarDefaults.topAppBarColors(containerColor = Bg))
        },
    ) { pad ->
        Box(Modifier.padding(pad)) {
            StateContent(state, onRetry = { vm.load() }) { p ->
                LazyColumn(contentPadding = PaddingValues(bottom = 24.dp)) {
                    item {
                        Column(Modifier.fillMaxWidth().padding(16.dp), horizontalAlignment = Alignment.CenterHorizontally) {
                            Avatar(p.person.image, p.person.name, 140.dp)
                            Spacer(Modifier.height(12.dp))
                            Text(p.person.name, style = MaterialTheme.typography.headlineSmall, fontWeight = FontWeight.Bold, color = Color.White)
                            p.birthDate?.let { Text("Born $it", color = Muted, fontSize = 13.sp) }
                            Spacer(Modifier.height(8.dp))
                            HeaderBlocks(p.display.filter { it.placement == "header" }, nav::person)
                        }
                    }
                    p.bio?.takeIf { it.isNotBlank() }?.let { bio ->
                        item { Text(bio, color = Color(0xFFD6D6DE), fontSize = 14.sp, lineHeight = 20.sp, modifier = Modifier.padding(horizontal = 16.dp)) }
                    }
                    item { DetailBlocks(p.display, nav::person) }
                    val groups = p.credits.groupBy { it.role }
                    groups.forEach { (role, credits) ->
                        item(key = "role_$role") {
                            SectionTitle(role.titleCase() + " (${credits.size})")
                            LazyRow(contentPadding = PaddingValues(horizontal = 16.dp), horizontalArrangement = Arrangement.spacedBy(12.dp)) {
                                items(credits, key = { it.id }) { c ->
                                    Column(Modifier.width(cardWidth(c.item.aspect))) {
                                        MediaCard(c.item, onClick = nav::open)
                                        c.character?.let { Text("as $it", color = Muted, fontSize = 11.sp, maxLines = 1) }
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
    }
    val p = (state as? UiState.Ready)?.data
    if (editing && p != null) PersonEditSheet(vm, p) { editing = false }
}

@OptIn(ExperimentalMaterial3Api::class)
@Composable
private fun PersonEditSheet(vm: PersonViewModel, p: PersonDetail, onDismiss: () -> Unit) {
    val defs by vm.fieldDefs.collectAsStateWithLifecycle()
    val repo = Graph.repo
    ModalBottomSheet(onDismissRequest = onDismiss, sheetState = rememberModalBottomSheetState(skipPartiallyExpanded = true), containerColor = CardBg) {
        Column(Modifier.fillMaxWidth().verticalScroll(rememberScrollState()).padding(horizontal = 16.dp).padding(bottom = 32.dp).imePadding()) {
            Text("Edit person", style = MaterialTheme.typography.titleLarge)
            var name by remember(p) { mutableStateOf(p.person.name) }
            var sortName by remember(p) { mutableStateOf(p.person.sortName ?: "") }
            var birth by remember(p) { mutableStateOf(p.birthDate ?: "") }
            var bio by remember(p) { mutableStateOf(p.bio ?: "") }
            OutlinedTextField(name, { name = it }, label = { Text("Name") }, singleLine = true, modifier = Modifier.fillMaxWidth())
            OutlinedTextField(sortName, { sortName = it }, label = { Text("Sort name") }, singleLine = true, modifier = Modifier.fillMaxWidth())
            val badDate = birth.isNotBlank() && !Regex("""\d{4}(-\d{2}(-\d{2})?)?""").matches(birth.trim())
            OutlinedTextField(birth, { birth = it }, label = { Text("Birth date (YYYY-MM-DD)") }, isError = badDate, singleLine = true, modifier = Modifier.fillMaxWidth())
            OutlinedTextField(bio, { bio = it }, label = { Text("Bio") }, minLines = 3, modifier = Modifier.fillMaxWidth())
            val changes = buildMap<String, Any?> {
                if (name != p.person.name && name.isNotBlank()) put("name", name.trim())
                if (sortName != (p.person.sortName ?: "")) put("sort_name", sortName.trim().ifEmpty { null })
                if (birth != (p.birthDate ?: "")) put("birth_date", birth.trim().ifEmpty { null })
                if (bio != (p.bio ?: "")) put("bio", bio.trim().ifEmpty { null })
            }
            Button(enabled = changes.isNotEmpty() && !badDate, onClick = { vm.edit { repo.patchPerson(p.person.id, changes) } }, modifier = Modifier.padding(top = 8.dp)) { Text("Save") }
            HorizontalDivider(Modifier.padding(vertical = 12.dp))
            Text("Fields", style = MaterialTheme.typography.titleMedium)
            // Person fields: definitions that explicitly apply to "person", plus any already set on this person.
            val editable = defs.filter { !it.derived && ("person" in it.appliesTo || it.key in p.fields) }
            if (defs.isNotEmpty() && editable.isEmpty()) Text("No fields apply to people. Create one in Field settings with applies-to “person”.", color = Muted, fontSize = 13.sp)
            editable.forEach { def ->
                FieldEditor(def, p.fields[def.key], p.display.flatMap { it.people },
                    onSet = { v -> vm.edit { repo.setPersonField(p.person.id, def.key, v) } },
                    onClear = { vm.edit { repo.deletePersonField(p.person.id, def.key) } })
            }
        }
    }
}
