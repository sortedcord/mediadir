package dev.sortedcord.mediadir.ui.screens

import android.content.ClipData
import android.content.ClipboardManager
import android.content.Context
import android.widget.Toast
import androidx.compose.foundation.layout.*
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.verticalScroll
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.automirrored.filled.ArrowBack
import androidx.compose.material.icons.filled.*
import androidx.compose.material3.*
import androidx.compose.runtime.*
import androidx.compose.runtime.saveable.rememberSaveable
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.unit.dp
import dev.sortedcord.mediadir.data.Graph
import dev.sortedcord.mediadir.ui.*
import dev.sortedcord.mediadir.ui.components.*
import kotlinx.coroutines.launch

private val qualities = listOf(0 to "Auto", 2160 to "Up to 4K", 1440 to "Up to 1440p", 1080 to "Up to 1080p", 720 to "Up to 720p", 480 to "Up to 480p")

@OptIn(ExperimentalMaterial3Api::class)
@Composable
private fun SettingsPage(title: String, nav: Nav?, scroll: androidx.compose.foundation.ScrollState = rememberScrollState(), content: @Composable ColumnScope.() -> Unit) {
    Scaffold(containerColor = Bg, topBar = {
        TopAppBar(title = { Text(title) },
            navigationIcon = { if (nav != null) IconButton(onClick = nav::back) { Icon(Icons.AutoMirrored.Filled.ArrowBack, "Back") } },
            colors = TopAppBarDefaults.topAppBarColors(containerColor = Bg))
    }) { pad -> Column(Modifier.padding(pad).verticalScroll(scroll).padding(bottom = 24.dp), content = content) }
}

private fun appVersion(ctx: Context): String =
    runCatching { ctx.packageManager.getPackageInfo(ctx.packageName, 0).versionName }.getOrNull() ?: "unknown"

/** Top-level category list, Android-system-settings style. */
@Composable
fun SettingsScreen(nav: Nav) {
    val s = Graph.settings
    val ctx = LocalContext.current
    val scroll = rememberScrollState()
    OnTabReselect(Routes.SETTINGS) { scroll.animateScrollTo(0) }
    var jobsSummary by remember { mutableStateOf<String?>(null) }
    LaunchedEffect(Unit) {
        while (true) {
            runCatching { Graph.repo.jobStats() }.onSuccess { st ->
                val parts = listOf("running", "queued", "paused", "failed").mapNotNull { k -> st.byState[k]?.takeIf { it > 0 }?.let { "$it $k" } }
                jobsSummary = if (parts.isEmpty()) "Idle" else parts.joinToString(" · ")
            }
            kotlinx.coroutines.delay(5000)
        }
    }
    val quality = qualities.firstOrNull { it.first == s.quality }?.second ?: "Auto"
    SettingsPage("Settings", null, scroll) {
        Spacer(Modifier.height(8.dp))
        SettingsGroup {
            SettingsRow("Server", s.server, Icons.Default.Dns, { nav.settingsPage("server") }, chevron = true)
            SettingsDivider()
            SettingsRow("Account", s.userName.ifBlank { "No user name set" }, Icons.Default.Person, { nav.settingsPage("account") }, chevron = true)
            SettingsDivider()
            SettingsRow("Playback", "$quality · HQ pause frame ${if (s.hqPauseFrame) "on" else "off"} · Loop ${if (s.loopVideo) "on" else "off"}",
                Icons.Default.PlayCircle, { nav.settingsPage("playback") }, chevron = true)
            SettingsDivider()
            SettingsRow("Library & metadata", "Custom fields and how they're shown", Icons.Default.Tune, nav::fieldSettings, chevron = true)
            SettingsDivider()
            SettingsRow("Ingest & storage", "Upload media, ingest queue, backups, integrity", Icons.Default.Storage, nav::ingest, chevron = true)
            SettingsDivider()
            SettingsRow("Jobs & schedules", jobsSummary ?: "Background work, progress and schedules", Icons.Default.Schedule, nav::jobs, chevron = true)
            SettingsDivider()
            SettingsRow("About", "Version ${appVersion(ctx)}", Icons.Default.Info, { nav.settingsPage("about") }, chevron = true)
        }
    }
}

@Composable
fun ServerSettingsScreen(nav: Nav) {
    val s = Graph.settings
    val scope = rememberCoroutineScope()
    var url by rememberSaveable { mutableStateOf(s.server) }
    var status by remember { mutableStateOf<String?>(null) }
    SettingsPage("Server", nav) {
        SettingsSection("Connection")
        SettingsGroup {
            SettingsBlock {
                OutlinedTextField(url, { url = it }, label = { Text("Server URL") }, singleLine = true, modifier = Modifier.fillMaxWidth())
                Row(verticalAlignment = Alignment.CenterVertically) {
                    Button(onClick = {
                        s.server = url
                        scope.launch {
                            status = runCatching { "Connected · API v${Graph.repo.health()}" }.getOrElse { "Can't reach server: ${it.message}" }
                        }
                    }) { Text("Save & test") }
                    Spacer(Modifier.width(8.dp))
                    TextButton(onClick = { s.server = url; nav.home() }) { Text("Save & reload") }
                }
                status?.let { Text(it, color = Muted, style = MaterialTheme.typography.bodySmall) }
            }
        }
    }
}

@Composable
fun AccountSettingsScreen(nav: Nav) {
    val s = Graph.settings
    val ctx = LocalContext.current
    var userName by rememberSaveable { mutableStateOf(s.userName) }
    SettingsPage("Account", nav) {
        SettingsSection("Identity")
        SettingsGroup {
            SettingsBlock {
                Text("Shown in the server's playback analytics. Optional.", color = Muted, style = MaterialTheme.typography.bodySmall)
                OutlinedTextField(userName, { userName = it; s.userName = it }, label = { Text("User name") }, singleLine = true, modifier = Modifier.fillMaxWidth())
            }
            SettingsDivider()
            SettingsRow("Device ID", s.deviceId, Icons.Default.PhoneAndroid, onClick = {
                (ctx.getSystemService(Context.CLIPBOARD_SERVICE) as ClipboardManager).setPrimaryClip(ClipData.newPlainText("Device ID", s.deviceId))
                Toast.makeText(ctx, "Device ID copied", Toast.LENGTH_SHORT).show()
            }, trailing = { Icon(Icons.Default.ContentCopy, "Copy", tint = Muted) })
        }
    }
}

@Composable
fun PlaybackSettingsScreen(nav: Nav) {
    val s = Graph.settings
    var quality by remember { mutableIntStateOf(s.quality) }
    var hq by remember { mutableStateOf(s.hqPauseFrame) }
    var hqDelay by remember { mutableIntStateOf(s.hqPauseDelayMs) }
    var loop by remember { mutableStateOf(s.loopVideo) }
    SettingsPage("Playback", nav) {
        SettingsSection("Default quality")
        SettingsGroup {
            Text("Auto picks the highest file this device can decode (hardware preferred).", color = Muted,
                style = MaterialTheme.typography.bodySmall, modifier = Modifier.padding(start = 16.dp, end = 16.dp, top = 12.dp))
            qualities.forEach { (h, l) ->
                SettingsRow(l, onClick = { quality = h; s.quality = h }, trailing = { RadioButton(quality == h, { quality = h; s.quality = h }) })
            }
        }
        SettingsSection("Player")
        SettingsGroup {
            SettingsSwitchRow("Loop video", "Repeat the current item (an A-B loop takes precedence)", loop, { loop = it; s.loopVideo = it })
            SettingsDivider()
            SettingsSwitchRow("High-quality pause frame", "When paused, replace the frame with the highest-quality version from the server (great for zooming)", hq, { hq = it; s.hqPauseFrame = it })
            if (hq) {
                SettingsDivider()
                SettingsBlock {
                    Text("Fetch after pausing for", color = Muted, style = MaterialTheme.typography.bodySmall)
                    Row(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                        listOf(500 to "0.5s", 1200 to "1.2s", 2000 to "2s", 3000 to "3s").forEach { (ms, l) ->
                            FilterChip(selected = hqDelay == ms, onClick = { hqDelay = ms; s.hqPauseDelayMs = ms }, label = { Text(l) })
                        }
                    }
                }
            }
        }
    }
}

@Composable
fun AboutSettingsScreen(nav: Nav) {
    val ctx = LocalContext.current
    val dev = ctx.packageName.endsWith(".dev")
    SettingsPage("About", nav) {
        SettingsSection("MediaDir")
        SettingsGroup {
            SettingsRow("Version", appVersion(ctx), Icons.Default.Info)
            SettingsDivider()
            SettingsRow("Build type", if (dev) "Debug (MediaDir Dev)" else "Release", Icons.Default.Build)
            SettingsDivider()
            SettingsRow("Package", ctx.packageName, Icons.Default.Apps)
            SettingsDivider()
            SettingsRow("Server", Graph.settings.server, Icons.Default.Dns)
        }
    }
}
