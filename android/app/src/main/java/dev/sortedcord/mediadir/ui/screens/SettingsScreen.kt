package dev.sortedcord.mediadir.ui.screens

import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.*
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.verticalScroll
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.automirrored.filled.ArrowBack
import androidx.compose.material.icons.automirrored.filled.KeyboardArrowRight
import androidx.compose.material3.*
import androidx.compose.runtime.*
import androidx.compose.runtime.saveable.rememberSaveable
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import dev.sortedcord.mediadir.data.Graph
import dev.sortedcord.mediadir.ui.*
import kotlinx.coroutines.launch

@OptIn(ExperimentalMaterial3Api::class)
@Composable
fun SettingsScreen(nav: Nav) {
    val s = Graph.settings
    val scope = rememberCoroutineScope()
    var url by rememberSaveable { mutableStateOf(s.server) }
    var quality by remember { mutableIntStateOf(s.quality) }
    var hq by remember { mutableStateOf(s.hqPauseFrame) }
    var hqDelay by remember { mutableIntStateOf(s.hqPauseDelayMs) }
    var userName by rememberSaveable { mutableStateOf(s.userName) }
    val scroll = rememberScrollState()
    OnTabReselect(Routes.SETTINGS) { scroll.animateScrollTo(0) }
    var status by remember { mutableStateOf<String?>(null) }
    Scaffold(containerColor = Bg, topBar = {
        TopAppBar(title = { Text("Settings") },
            colors = TopAppBarDefaults.topAppBarColors(containerColor = Bg))
    }) { pad ->
        Column(Modifier.padding(pad).padding(16.dp).verticalScroll(scroll)) {
            Text("Server", style = MaterialTheme.typography.titleMedium)
            OutlinedTextField(url, { url = it }, label = { Text("Server URL") }, singleLine = true, modifier = Modifier.fillMaxWidth())
            Row {
                Button(onClick = {
                    s.server = url
                    scope.launch {
                        status = runCatching { "Connected · API v${Graph.repo.health()}" }.getOrElse { "Can't reach server: ${it.message}" }
                    }
                }) { Text("Save & test") }
                Spacer(Modifier.width(8.dp))
                TextButton(onClick = { s.server = url; nav.home() }) { Text("Save & reload") }
            }
            status?.let { Text(it, color = Muted, fontSize = 13.sp) }

            HorizontalDivider(Modifier.padding(vertical = 16.dp))
            Text("Identity", style = MaterialTheme.typography.titleMedium)
            Text("Shown in the server's playback analytics. Optional.", color = Muted, fontSize = 12.sp)
            OutlinedTextField(userName, { userName = it; s.userName = it }, label = { Text("User name") }, singleLine = true, modifier = Modifier.fillMaxWidth())

            HorizontalDivider(Modifier.padding(vertical = 16.dp))
            Text("Default quality", style = MaterialTheme.typography.titleMedium)
            Text("Auto picks the highest file this device can decode (hardware preferred).", color = Muted, fontSize = 12.sp)
            listOf(0 to "Auto", 2160 to "Up to 4K", 1440 to "Up to 1440p", 1080 to "Up to 1080p", 720 to "Up to 720p", 480 to "Up to 480p").forEach { (h, l) ->
                Row(Modifier.fillMaxWidth().clickable { quality = h; s.quality = h }, verticalAlignment = Alignment.CenterVertically) {
                    RadioButton(quality == h, { quality = h; s.quality = h }); Text(l)
                }
            }

            HorizontalDivider(Modifier.padding(vertical = 16.dp))
            Text("Playback", style = MaterialTheme.typography.titleMedium)
            Row(Modifier.fillMaxWidth().clickable { hq = !hq; s.hqPauseFrame = hq }.padding(vertical = 8.dp), verticalAlignment = Alignment.CenterVertically) {
                Column(Modifier.weight(1f)) {
                    Text("High-quality pause frame")
                    Text("When paused, replace the frame with the highest-quality version from the server (great for zooming)", color = Muted, fontSize = 12.sp)
                }
                Switch(hq, { hq = it; s.hqPauseFrame = it })
            }
            if (hq) {
                Text("Fetch after pausing for", color = Muted, fontSize = 12.sp)
                Row(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                    listOf(500 to "0.5s", 1200 to "1.2s", 2000 to "2s", 3000 to "3s").forEach { (ms, l) ->
                        FilterChip(selected = hqDelay == ms, onClick = { hqDelay = ms; s.hqPauseDelayMs = ms }, label = { Text(l) })
                    }
                }
            }

            HorizontalDivider(Modifier.padding(vertical = 16.dp))
            Row(Modifier.fillMaxWidth().clickable { nav.fieldSettings() }.padding(vertical = 12.dp), verticalAlignment = Alignment.CenterVertically) {
                Column(Modifier.weight(1f)) {
                    Text("Fields & display", style = MaterialTheme.typography.titleMedium)
                    Text("Custom metadata fields and how they're shown", color = Muted, fontSize = 12.sp)
                }
                Icon(Icons.AutoMirrored.Filled.KeyboardArrowRight, null)
            }
            HorizontalDivider(Modifier.padding(vertical = 16.dp))
            OutlinedButton(onClick = nav::ingest) { Text("Media ingest & storage") }

        }
    }
}
