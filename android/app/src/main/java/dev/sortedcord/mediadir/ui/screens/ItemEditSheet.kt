package dev.sortedcord.mediadir.ui.screens

import androidx.compose.foundation.layout.*
import androidx.compose.foundation.selection.selectable
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.text.KeyboardOptions
import androidx.compose.foundation.verticalScroll
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.filled.Delete
import androidx.compose.material3.*
import androidx.compose.runtime.*
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.semantics.Role
import androidx.compose.ui.text.input.KeyboardType
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import dev.sortedcord.mediadir.data.Graph
import dev.sortedcord.mediadir.domain.*
import dev.sortedcord.mediadir.ui.*
import dev.sortedcord.mediadir.ui.components.*

/** Core keys edited through PATCH rather than the field editors. */
val CORE_ITEM_KEYS = setOf("title", "subtitle", "year", "overview", "genres", "sort_title")

@OptIn(ExperimentalMaterial3Api::class)
@Composable
fun ItemEditSheet(vm: ItemViewModel, d: ItemDetail, onDismiss: () -> Unit) {
    val defs by vm.fieldDefs.collectAsStateWithLifecycle()
    val repo = Graph.repo
    val id = d.summary.id
    ModalBottomSheet(onDismissRequest = onDismiss, sheetState = rememberModalBottomSheetState(skipPartiallyExpanded = true), containerColor = CardBg) {
        Column(Modifier.fillMaxWidth().verticalScroll(rememberScrollState()).padding(horizontal = 16.dp).padding(bottom = 32.dp).imePadding()) {
            Text("Edit ${d.summary.kind.titleCase()}", style = MaterialTheme.typography.titleLarge)
            if (d.locked.isNotEmpty()) Text("Locked: ${d.locked.joinToString()}", color = Muted, fontSize = 12.sp)
            Spacer(Modifier.height(8.dp))
            // --- Core fields (PATCH)
            var title by remember(d) { mutableStateOf(d.summary.title) }
            var subtitle by remember(d) { mutableStateOf(d.summary.subtitle ?: "") }
            var year by remember(d) { mutableStateOf(d.summary.year?.toString() ?: "") }
            var overview by remember(d) { mutableStateOf(d.overview ?: "") }
            var genres by remember(d) { mutableStateOf(d.genres.joinToString(", ")) }
            OutlinedTextField(title, { title = it }, label = { Text("Title") }, singleLine = true, modifier = Modifier.fillMaxWidth())
            OutlinedTextField(subtitle, { subtitle = it }, label = { Text("Subtitle") }, singleLine = true, modifier = Modifier.fillMaxWidth())
            OutlinedTextField(year, { year = it.filter(Char::isDigit).take(4) }, label = { Text("Year") }, singleLine = true,
                keyboardOptions = KeyboardOptions(keyboardType = KeyboardType.Number), modifier = Modifier.fillMaxWidth())
            OutlinedTextField(genres, { genres = it }, label = { Text("Genres (comma separated)") }, singleLine = true, modifier = Modifier.fillMaxWidth())
            OutlinedTextField(overview, { overview = it }, label = { Text("Overview") }, minLines = 3, modifier = Modifier.fillMaxWidth())
            var kind by remember(d) { mutableStateOf(d.summary.kind) }
            if (d.summary.parentId == null && d.summary.kind in listOf("movie", "music_video", "video", "track")) {
                Text("Media type", modifier = Modifier.padding(top = 8.dp))
                listOf("movie", "music_video", "video", "track").forEach { value ->
                    Row(Modifier.fillMaxWidth().selectable(selected = kind == value, role = Role.RadioButton, onClick = { kind = value }),
                        verticalAlignment = Alignment.CenterVertically) {
                        RadioButton(selected = kind == value, onClick = null)
                        Text(value.titleCase())
                    }
                }
            }
            val changes = buildMap<String, Any?> {
                if (kind != d.summary.kind) put("kind", kind)
                if (title != d.summary.title && title.isNotBlank()) put("title", title.trim())
                if (subtitle != (d.summary.subtitle ?: "")) put("subtitle", subtitle.trim().ifEmpty { null })
                if (year != (d.summary.year?.toString() ?: "")) put("year", year.toIntOrNull())
                if (overview != (d.overview ?: "")) put("overview", overview.trim().ifEmpty { null })
                val g = genres.split(',').map { it.trim() }.filter { it.isNotEmpty() }
                if (g != d.genres) put("genres", g)
            }
            Button(enabled = changes.isNotEmpty(), onClick = { vm.edit { repo.patchItem(id, changes) } }, modifier = Modifier.padding(top = 8.dp)) { Text("Save details") }

            // --- Fields (PUT/DELETE)
            HorizontalDivider(Modifier.padding(vertical = 12.dp))
            Text("Fields", style = MaterialTheme.typography.titleMedium)
            val editable = defs.filter { it.appliesTo(d.summary.kind) && !it.derived && it.key !in CORE_ITEM_KEYS }
            if (defs.isEmpty()) LinearProgressIndicator(Modifier.fillMaxWidth())
            else if (editable.isEmpty()) Text("No editable fields for this kind. Add custom fields in Field settings.", color = Muted, fontSize = 13.sp)
            val people = d.display.flatMap { it.people } + d.credits.map { it.person }
            editable.forEach { def ->
                FieldEditor(def, d.fields[def.key], people,
                    onSet = { v -> vm.edit { repo.setItemField(id, def.key, v) } },
                    onClear = { vm.edit { repo.deleteItemField(id, def.key) } })
            }

            // --- Credits
            HorizontalDivider(Modifier.padding(vertical = 12.dp))
            Text("Credits", style = MaterialTheme.typography.titleMedium)
            d.credits.forEach { c ->
                Row(verticalAlignment = Alignment.CenterVertically, modifier = Modifier.padding(vertical = 4.dp)) {
                    Avatar(c.person.id, c.person.name, 32.dp, images = c.person.images); Spacer(Modifier.width(10.dp))
                    Column(Modifier.weight(1f)) {
                        Text(c.person.name, color = Color.White)
                        Text(c.role.titleCase() + (c.character?.let { " · $it" } ?: ""), color = Muted, fontSize = 12.sp)
                    }
                    IconButton(onClick = { vm.edit { repo.deleteCredit(id, c.id) } }) { Icon(Icons.Default.Delete, "Remove", tint = Muted) }
                }
            }
            AddCredit { pid, name, role, character -> vm.edit { repo.addCredit(id, pid, name, role, character) } }
        }
    }
}

@OptIn(ExperimentalMaterial3Api::class)
@Composable
private fun AddCredit(onAdd: (String?, String?, String, String?) -> Unit) {
    var role by remember { mutableStateOf("actor") }
    var character by remember { mutableStateOf("") }
    var picked by remember { mutableStateOf<Pair<String?, String>?>(null) } // (personId or null, name)
    var roleMenu by remember { mutableStateOf(false) }
    Column(Modifier.padding(top = 8.dp)) {
        Text("Add credit", color = Muted, fontSize = 13.sp)
        if (picked == null) PersonPicker(onPick = { p, n -> picked = if (p != null) p.id to p.name else null to n!! })
        else Row(verticalAlignment = Alignment.CenterVertically) {
            Text(picked!!.second + if (picked!!.first == null) " (new)" else "", color = Color.White, modifier = Modifier.weight(1f))
            TextButton(onClick = { picked = null }) { Text("Change") }
        }
        Row(verticalAlignment = Alignment.CenterVertically) {
            ExposedDropdownMenuBox(roleMenu, { roleMenu = it }, Modifier.weight(1f)) {
                OutlinedTextField(role, { role = it.lowercase() }, label = { Text("Role") }, singleLine = true,
                    modifier = Modifier.menuAnchor(MenuAnchorType.PrimaryEditable))
                ExposedDropdownMenu(roleMenu, { roleMenu = false }) {
                    Kinds.ROLES.forEach { r -> DropdownMenuItem(text = { Text(r) }, onClick = { role = r; roleMenu = false }) }
                }
            }
            Spacer(Modifier.width(8.dp))
            OutlinedTextField(character, { character = it }, label = { Text("Character") }, singleLine = true, modifier = Modifier.weight(1f))
        }
        Button(enabled = picked != null && role.isNotBlank(), modifier = Modifier.padding(top = 8.dp), onClick = {
            val p = picked!!
            onAdd(p.first, if (p.first == null) p.second else null, role.trim(), character.trim().ifEmpty { null })
            picked = null; character = ""
        }) { Text("Add credit") }
    }
}
