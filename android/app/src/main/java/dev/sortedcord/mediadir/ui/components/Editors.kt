package dev.sortedcord.mediadir.ui.components

import androidx.activity.compose.rememberLauncherForActivityResult
import androidx.activity.result.contract.ActivityResultContracts
import androidx.compose.foundation.background
import androidx.compose.foundation.lazy.LazyRow
import androidx.compose.foundation.lazy.itemsIndexed
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material.icons.automirrored.filled.ArrowBack
import androidx.compose.material.icons.automirrored.filled.ArrowForward
import androidx.compose.material.icons.filled.AddPhotoAlternate
import androidx.compose.ui.draw.clip
import androidx.compose.ui.platform.LocalContext
import dev.sortedcord.mediadir.domain.ImagePolicy
import kotlinx.coroutines.launch
import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.*
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.filled.Check
import androidx.compose.material.icons.filled.Close
import androidx.compose.material.icons.filled.Delete
import androidx.compose.material.icons.filled.PersonAdd
import androidx.compose.material3.*
import androidx.compose.runtime.*
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.text.input.KeyboardType
import androidx.compose.foundation.text.KeyboardOptions
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import dev.sortedcord.mediadir.data.Graph
import dev.sortedcord.mediadir.domain.FieldDefinition
import dev.sortedcord.mediadir.domain.Person
import dev.sortedcord.mediadir.domain.PersonRef
import dev.sortedcord.mediadir.ui.*
import kotlinx.coroutines.delay

/** Debounced person search with "create new" option. onPick(person, null) or onPick(null, newName). */
@Composable
fun PersonPicker(onPick: (Person?, String?) -> Unit, allowCreate: Boolean = true, label: String = "Search people") {
    var q by remember { mutableStateOf("") }
    var results by remember { mutableStateOf<List<Person>>(emptyList()) }
    LaunchedEffect(q) {
        if (q.isBlank()) { results = emptyList(); return@LaunchedEffect }
        delay(250)
        results = runCatching { Graph.repo.people(q, 8) }.getOrDefault(emptyList())
    }
    Column {
        OutlinedTextField(q, { q = it }, label = { Text(label) }, singleLine = true, modifier = Modifier.fillMaxWidth())
        results.forEach { p ->
            Row(Modifier.fillMaxWidth().clickable { onPick(p, null); q = "" }.padding(vertical = 6.dp), verticalAlignment = Alignment.CenterVertically) {
                Avatar(p.id, p.name, 32.dp); Spacer(Modifier.width(10.dp))
                Text(p.name, Modifier.weight(1f)); Text("${p.creditCount}", color = Muted, fontSize = 12.sp)
            }
        }
        if (allowCreate && q.isNotBlank() && results.none { it.name.equals(q.trim(), true) }) {
            Row(Modifier.fillMaxWidth().clickable { onPick(null, q.trim()); q = "" }.padding(vertical = 8.dp), verticalAlignment = Alignment.CenterVertically) {
                Icon(Icons.Default.PersonAdd, null, tint = Accent); Spacer(Modifier.width(10.dp)); Text("Create \"${q.trim()}\"", color = Accent)
            }
        }
    }
}

private fun asText(v: Any?): String = when (v) {
    null -> ""
    is List<*> -> v.joinToString(", ")
    is Double -> if (v % 1.0 == 0.0) v.toLong().toString() else v.toString()
    else -> v.toString()
}

/**
 * Editor for one FieldDefinition value. [value] is the raw JSON value; [people] resolves names for person ids.
 * onSet(value) -> PUT, onClear -> DELETE.
 */
@OptIn(ExperimentalLayoutApi::class)
@Composable
fun FieldEditor(def: FieldDefinition, value: Any?, people: List<PersonRef>, onSet: (Any?) -> Unit, onClear: () -> Unit) {
    Column(Modifier.fillMaxWidth().padding(vertical = 6.dp)) {
        Row(verticalAlignment = Alignment.CenterVertically) {
            Text(def.label, fontSize = 13.sp, color = Color.White, modifier = Modifier.weight(1f))
            Text(def.type, fontSize = 11.sp, color = Muted)
            if (value != null) IconButton(onClick = onClear, modifier = Modifier.size(32.dp)) { Icon(Icons.Default.Delete, "Clear", tint = Muted, modifier = Modifier.size(18.dp)) }
        }
        when (def.type) {
            "bool" -> Switch(checked = value == true, onCheckedChange = { onSet(it) })
            "rating" -> {
                var r by remember(value) { mutableFloatStateOf((value as? Number)?.toFloat() ?: 0f) }
                Row(verticalAlignment = Alignment.CenterVertically) {
                    Slider(r, { r = (it * 2).toInt() / 2f }, valueRange = 0f..10f, steps = 19, modifier = Modifier.weight(1f))
                    Text("%.1f".format(r), modifier = Modifier.width(40.dp))
                    IconButton(onClick = { onSet(r.toDouble()) }) { Icon(Icons.Default.Check, "Save", tint = Accent) }
                }
            }
            "image" -> ImageFieldEditor(value, onSet, onClear)
            "person" -> {
                val ids = when (value) { is List<*> -> value.mapNotNull { it?.toString() }; null -> emptyList(); else -> listOf(value.toString()) }
                val multi = value is List<*> || value == null
                FlowRow(horizontalArrangement = Arrangement.spacedBy(6.dp)) {
                    ids.forEach { id ->
                        val name = people.firstOrNull { it.id == id }?.name ?: id
                        InputChip(selected = false, onClick = {
                            val rest = ids - id
                            if (rest.isEmpty()) onClear() else onSet(if (multi) rest else rest.first())
                        }, label = { Text(name) }, trailingIcon = { Icon(Icons.Default.Close, null, Modifier.size(14.dp)) })
                    }
                }
                PersonPicker(allowCreate = false, label = "Add person", onPick = { p, _ ->
                    if (p != null && p.id !in ids) onSet(if (multi) ids + p.id else listOf(ids.first(), p.id))
                })
            }
            else -> {
                var t by remember(value) { mutableStateOf(asText(value)) }
                val numeric = def.type == "number" || def.type == "duration"
                val err = when (def.type) {
                    "date" -> t.isNotBlank() && !Regex("""\d{4}(-\d{2}(-\d{2})?)?""").matches(t.trim())
                    "number", "duration" -> t.isNotBlank() && t.trim().toDoubleOrNull() == null
                    else -> false
                }
                OutlinedTextField(
                    t, { t = it }, singleLine = def.type != "text", isError = err, modifier = Modifier.fillMaxWidth(),
                    placeholder = { Text(when (def.type) { "date" -> "YYYY-MM-DD"; "tags" -> "comma, separated"; "url" -> "https://"; else -> "" }) },
                    keyboardOptions = KeyboardOptions(keyboardType = when { numeric -> KeyboardType.Decimal; def.type == "url" -> KeyboardType.Uri; else -> KeyboardType.Text }),
                    trailingIcon = {
                        IconButton(enabled = !err && t != asText(value), onClick = {
                            val s = t.trim()
                            if (s.isEmpty()) onClear() else onSet(when (def.type) {
                                "number", "duration" -> s.toDouble().let { d -> if (d % 1.0 == 0.0) d.toLong() else d }
                                "tags" -> s.split(',').map { it.trim() }.filter { it.isNotEmpty() }
                                else -> s
                            })
                        }) { Icon(Icons.Default.Check, "Save") }
                    },
                )
            }
        }
    }
}

/** Lists current images, adds from the document picker (upload to /api/assets, then [onSet]), removes and reorders. */
@Composable
fun ImageFieldEditor(value: Any?, onSet: (Any?) -> Unit, onClear: () -> Unit) {
    val ctx = LocalContext.current
    val scope = rememberCoroutineScope()
    val hashes = ImagePolicy.hashes(value)
    var busy by remember { mutableStateOf(false) }
    var error by remember { mutableStateOf<String?>(null) }
    fun commit(list: List<String>) {
        when (list.size) { 0 -> onClear(); 1 -> onSet(mapOf("hash" to list[0])); else -> onSet(list.map { mapOf("hash" to it) }) }
    }
    val picker = rememberLauncherForActivityResult(ActivityResultContracts.OpenMultipleDocuments()) { uris ->
        if (uris.isEmpty()) return@rememberLauncherForActivityResult
        scope.launch {
            busy = true; error = null
            try {
                val added = uris.map { uri ->
                    Graph.repo.uploadAsset { ctx.contentResolver.openInputStream(uri) ?: throw java.io.IOException("Cannot open $uri") }.hash
                }
                commit((hashes + added).distinct())
            } catch (e: Exception) { error = e.message ?: "Upload failed" }
            finally { busy = false }
        }
    }
    Column {
        if (hashes.isNotEmpty()) LazyRow(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
            itemsIndexed(hashes, key = { _, h -> h }) { i, h ->
                Column(horizontalAlignment = Alignment.CenterHorizontally) {
                    Box(Modifier.size(96.dp).clip(RoundedCornerShape(8.dp)).background(CardBg)) { AssetImage(h, 96.dp, 96.dp) }
                    Row {
                        IconButton(enabled = i > 0, onClick = { commit(hashes.toMutableList().also { it.add(i - 1, it.removeAt(i)) }) }, modifier = Modifier.size(32.dp)) {
                            Icon(Icons.AutoMirrored.Filled.ArrowBack, "Move earlier", modifier = Modifier.size(18.dp))
                        }
                        IconButton(onClick = { commit(hashes - h) }, modifier = Modifier.size(32.dp)) { Icon(Icons.Default.Close, "Remove", modifier = Modifier.size(18.dp)) }
                        IconButton(enabled = i < hashes.lastIndex, onClick = { commit(hashes.toMutableList().also { it.add(i + 1, it.removeAt(i)) }) }, modifier = Modifier.size(32.dp)) {
                            Icon(Icons.AutoMirrored.Filled.ArrowForward, "Move later", modifier = Modifier.size(18.dp))
                        }
                    }
                }
            }
        }
        OutlinedButton(enabled = !busy, onClick = { picker.launch(arrayOf("image/*")) }) {
            Icon(Icons.Default.AddPhotoAlternate, null); Spacer(Modifier.width(6.dp)); Text(if (busy) "Uploading…" else "Add image")
        }
        error?.let { Text(it, color = Accent, fontSize = 12.sp) }
    }
}
