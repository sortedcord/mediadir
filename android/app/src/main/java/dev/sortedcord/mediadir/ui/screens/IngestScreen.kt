package dev.sortedcord.mediadir.ui.screens

import android.provider.OpenableColumns
import androidx.activity.compose.rememberLauncherForActivityResult
import androidx.activity.result.contract.ActivityResultContracts
import androidx.compose.foundation.layout.*
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.verticalScroll
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.automirrored.filled.ArrowBack
import androidx.compose.material3.*
import androidx.compose.runtime.*
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.unit.dp
import dev.sortedcord.mediadir.data.Graph
import dev.sortedcord.mediadir.domain.*
import dev.sortedcord.mediadir.ui.*
import kotlinx.coroutines.delay
import kotlinx.coroutines.launch
import java.text.DateFormat
import java.util.Date

@OptIn(ExperimentalMaterial3Api::class)
@Composable
fun IngestScreen(nav: Nav) {
    val scope = rememberCoroutineScope()
    val resolver = LocalContext.current.contentResolver
    var status by remember { mutableStateOf<IngestStatus?>(null) }
    var records by remember { mutableStateOf<List<IngestRecord>>(emptyList()) }
    var error by remember { mutableStateOf<String?>(null) }
    var message by remember { mutableStateOf<String?>(null) }
    var busy by remember { mutableStateOf(false) }
    var uploading by remember { mutableStateOf(false) }
    suspend fun refresh() {
        runCatching { Graph.repo.ingestStatus() to Graph.repo.ingests() }
            .onSuccess { status = it.first; records = it.second; error = null }
            .onFailure { error = it.message ?: "Could not load ingest status" }
    }
    LaunchedEffect(Unit) { while (true) { refresh(); delay(5000) } }
    val picker = rememberLauncherForActivityResult(ActivityResultContracts.OpenDocument()) { uri ->
        if (uri != null) scope.launch {
            uploading = true
            message = "Uploading… Keep this screen open until the upload completes."
            try {
                val name = resolver.query(uri, arrayOf(OpenableColumns.DISPLAY_NAME), null, null, null)?.use { cursor ->
                    if (cursor.moveToFirst()) cursor.getString(0) else null
                } ?: error("Cannot read the file name")
                val record = Graph.repo.upload(name) { resolver.openInputStream(uri) ?: error("Cannot open file") }
                message = "${record.name}: ${record.state}" + (record.error?.let { " — $it" } ?: "")
                refresh()
            } catch (e: Exception) { message = "Upload failed: ${e.message}" }
            finally { uploading = false }
        }
    }
    Scaffold(containerColor = Bg, topBar = {
        TopAppBar(title = { Text("Ingest & storage") }, navigationIcon = {
            IconButton(onClick = nav::back) { Icon(Icons.AutoMirrored.Filled.ArrowBack, "Back") }
        })
    }) { pad ->
        Column(Modifier.padding(pad).padding(16.dp).verticalScroll(rememberScrollState()), verticalArrangement = Arrangement.spacedBy(8.dp)) {
            Text("Drop files in the server's ingest directory or upload a file here. Metadata is inferred from its file name.")
            Button(enabled = !uploading, onClick = { picker.launch(arrayOf("video/*", "audio/*", "application/octet-stream")) }) { Text("Upload media") }
            if (uploading) LinearProgressIndicator(Modifier.fillMaxWidth())
            message?.let { Text(it) }
            OutlinedButton(enabled = !busy, onClick = {
                scope.launch {
                    busy = true
                    runCatching { Graph.repo.pollIngest() }.onSuccess { message = "Ingest poll requested; failed files will be retried" }.onFailure { message = it.message }
                    refresh()
                    busy = false
                }
            }) { Text("Poll ingest directory & retry failures") }
            TextButton(onClick = { scope.launch { refresh() } }) { Text("Refresh") }
            error?.let { Text(it, color = MaterialTheme.colorScheme.error) }
            status?.let { s ->
                Text("Watcher: ${if (s.watching) "active" else "inactive"} · Polling: ${if (s.pollSecs > 0) "every ${s.pollSecs}s" else "disabled"}")
                Text("Queued: ${s.queue} · Settling: ${s.settling} · Active: ${s.active}")
                fun timestamp(t: Long?) = t?.let { DateFormat.getDateTimeInstance().format(Date(it * 1000)) } ?: "Not run yet"
                Text("Last backup: ${timestamp(s.lastBackup)}")
                Text("Last integrity check: ${timestamp(s.lastVerify)}")
                Text("Last sidecar sync: ${timestamp(s.lastSidecarSync)}")
                if (s.problems.isNotEmpty()) {
                    Text("Storage problems", style = MaterialTheme.typography.titleMedium)
                    s.problems.forEach { p ->
                        Text("${p.problem}: ${p.detail}", color = MaterialTheme.colorScheme.error)
                        TextButton(onClick = { nav.item(p.itemId) }) { Text("Open affected item") }
                    }
                }
            }
            HorizontalDivider()
            Text("Recent ingest", style = MaterialTheme.typography.titleMedium)
            if (records.isEmpty() && status != null) Text("No files ingested yet")
            records.forEach { r ->
                Text(r.name, style = MaterialTheme.typography.titleSmall)
                Text(r.state + (r.error?.let { " — $it" } ?: ""))
                r.itemId?.let { id -> TextButton(onClick = { nav.item(id) }) { Text("Open item") } }
                HorizontalDivider()
            }
        }
    }
}
