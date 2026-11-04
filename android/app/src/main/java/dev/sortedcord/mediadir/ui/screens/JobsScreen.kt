package dev.sortedcord.mediadir.ui.screens

import androidx.compose.foundation.background
import androidx.compose.foundation.clickable
import androidx.compose.foundation.horizontalScroll
import androidx.compose.foundation.layout.*
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.items
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.verticalScroll
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.automirrored.filled.ArrowBack
import androidx.compose.material.icons.filled.Add
import androidx.compose.material.icons.filled.Refresh
import androidx.compose.material3.*
import androidx.compose.runtime.*
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.text.font.FontFamily
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import androidx.lifecycle.Lifecycle
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import androidx.lifecycle.compose.LocalLifecycleOwner
import androidx.lifecycle.repeatOnLifecycle
import androidx.lifecycle.viewModelScope
import androidx.compose.runtime.saveable.rememberSaveable
import androidx.lifecycle.viewmodel.compose.viewModel
import dev.sortedcord.mediadir.data.*
import dev.sortedcord.mediadir.ui.*
import dev.sortedcord.mediadir.ui.components.*
import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.channels.Channel
import kotlinx.coroutines.coroutineScope
import kotlinx.coroutines.delay
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.launch

internal fun stateColor(s: String) = when (s) {
    "running" -> Color(0xFF4CC9F0); "queued" -> Color(0xFFFFD166); "paused" -> Color(0xFFFF9F1C)
    "failed" -> Color(0xFFFF4D6D); "done" -> Color(0xFF06D6A0); else -> Muted
}

internal fun relTime(ts: Long?): String {
    if (ts == null) return "-"
    val d = System.currentTimeMillis() / 1000 - ts
    val a = kotlin.math.abs(d)
    val t = when { a < 5 -> "now"; a < 60 -> "${a}s"; a < 3600 -> "${a / 60}m"; a < 86400 -> "${a / 3600}h"; else -> "${a / 86400}d" }
    return if (t == "now") "just now" else if (d >= 0) "$t ago" else "in $t"
}

private fun errText(e: Throwable) = e.message ?: e.javaClass.simpleName

class JobsViewModel : ScrollHolder() {
    val stateFilter = MutableStateFlow<String?>(null)
    val typeFilter = MutableStateFlow<String?>(null)
    val jobs = MutableStateFlow<UiState<List<JobInfo>>>(UiState.Loading)
    val schedules = MutableStateFlow<List<ScheduleInfo>>(emptyList())
    val live = MutableStateFlow(false)
    val message = MutableStateFlow<String?>(null)
    private val trigger = Channel<Unit>(Channel.CONFLATED)

    fun setFilters(state: String?, type: String?) { stateFilter.value = state; typeFilter.value = type; trigger.trySend(Unit) }

    suspend fun refresh() {
        runCatching { Graph.repo.jobs(stateFilter.value, typeFilter.value) }
            .onSuccess { jobs.value = UiState.Ready(it.second) }
            .onFailure { if (jobs.value !is UiState.Ready) jobs.value = UiState.Error(errText(it)) }
        runCatching { Graph.repo.schedules() }.onSuccess { schedules.value = it }
    }

    fun act(block: suspend () -> Unit) = viewModelScope.launch {
        try { block(); message.value = null } catch (e: CancellationException) { throw e } catch (e: Exception) { message.value = errText(e) }
        refresh()
    }
    fun runType(type: String) = act { Graph.repo.createJob(type) }
    fun runSchedule(id: String) = act { Graph.repo.runSchedule(id) }
    fun updateSchedule(id: String, changes: Map<String, Any?>) = act { Graph.repo.updateSchedule(id, changes) }

    /** Runs while the screen is visible: live updates (SSE, polling fallback). */
    suspend fun run() = coroutineScope {
        refresh()
        launch { for (t in trigger) { delay(250); refresh() } }
        liveJobs(null, null, onLive = { live.value = it }, onEvent = { ev ->
            val cur = (jobs.value as? UiState.Ready)?.data
            if (ev.event == "progress" && cur != null && cur.any { it.id == ev.id }) {
                jobs.value = UiState.Ready(cur.map { if (it.id == ev.id) it.copy(progress = ev.progress ?: it.progress) else it })
            } else trigger.trySend(Unit)
        }, onPoll = { refresh() })
    }
}

@OptIn(ExperimentalMaterial3Api::class)
@Composable
fun JobsScreen(nav: Nav, vm: JobsViewModel = viewModel()) {
    val lifecycle = LocalLifecycleOwner.current.lifecycle
    LaunchedEffect(vm) { lifecycle.repeatOnLifecycle(Lifecycle.State.STARTED) { vm.run() } }
    val state by vm.jobs.collectAsStateWithLifecycle()
    val schedules by vm.schedules.collectAsStateWithLifecycle()
    val sf by vm.stateFilter.collectAsStateWithLifecycle()
    val tf by vm.typeFilter.collectAsStateWithLifecycle()
    val live by vm.live.collectAsStateWithLifecycle()
    val msg by vm.message.collectAsStateWithLifecycle()
    var tab by rememberSaveable { mutableIntStateOf(0) }
    var showRun by remember { mutableStateOf(false) }
    var editing by remember { mutableStateOf<ScheduleInfo?>(null) }
    val list = vm.listState("jobs")
    val schedList = vm.listState("schedules")
    Scaffold(containerColor = Bg, topBar = {
        TopAppBar(title = { Column { Text("Jobs & schedules"); Text(if (live) "Live" else "Polling every 3s", style = MaterialTheme.typography.labelSmall, color = if (live) stateColor("done") else Muted) } },
            navigationIcon = { IconButton(onClick = nav::back) { Icon(Icons.AutoMirrored.Filled.ArrowBack, "Back") } },
            actions = {
                IconButton(onClick = { vm.act { } }) { Icon(Icons.Default.Refresh, "Refresh") }
                IconButton(onClick = { showRun = true }) { Icon(Icons.Default.Add, "Run job") }
            },
            colors = TopAppBarDefaults.topAppBarColors(containerColor = Bg))
    }) { pad ->
        Column(Modifier.padding(pad)) {
            TabRow(tab, containerColor = Bg) {
                Tab(tab == 0, { tab = 0 }, text = { Text("Jobs") })
                Tab(tab == 1, { tab = 1 }, text = { Text("Schedules") })
            }
            msg?.let { Text(it, color = MaterialTheme.colorScheme.error, modifier = Modifier.padding(16.dp, 8.dp)) }
            if (tab == 0) {
                Column(Modifier.horizontalScroll(rememberScrollState()).padding(horizontal = 12.dp, vertical = 4.dp)) {
                    Row(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                        FilterChip(sf == null, { vm.setFilters(null, tf) }, label = { Text("All") })
                        JobStates.forEach { s -> FilterChip(sf == s, { vm.setFilters(if (sf == s) null else s, tf) }, label = { Text(s) }) }
                    }
                    Row(horizontalArrangement = Arrangement.spacedBy(8.dp), modifier = Modifier.padding(top = 4.dp)) {
                        FilterChip(tf == null, { vm.setFilters(sf, null) }, label = { Text("Any type") })
                        JobTypes.forEach { t -> FilterChip(tf == t, { vm.setFilters(sf, if (tf == t) null else t) }, label = { Text(t) }) }
                    }
                }
                when (val s = state) {
                    is UiState.Loading -> Box(Modifier.fillMaxSize(), Alignment.Center) { CircularProgressIndicator() }
                    is UiState.Error -> Box(Modifier.fillMaxSize(), Alignment.Center) { Text(s.message, color = MaterialTheme.colorScheme.error) }
                    is UiState.Ready ->
                        if (s.data.isEmpty()) Box(Modifier.fillMaxSize(), Alignment.Center) { Text("No jobs", color = Muted) }
                        else LazyColumn(state = list, contentPadding = PaddingValues(16.dp), verticalArrangement = Arrangement.spacedBy(8.dp)) {
                            items(s.data, key = { it.id }) { JobRow(it) { nav.job(it.id) } }
                        }
                }
            } else {
                LazyColumn(state = schedList, contentPadding = PaddingValues(16.dp), verticalArrangement = Arrangement.spacedBy(8.dp)) {
                    items(schedules, key = { it.id }) { sc ->
                        ScheduleRow(sc, onToggle = { vm.updateSchedule(sc.id, mapOf("enabled" to it)) }, onEdit = { editing = sc }, onRun = { vm.runSchedule(sc.id) })
                    }
                }
            }
        }
    }
    if (showRun) RunJobDialog({ showRun = false }) { vm.runType(it); showRun = false; tab = 0 }
    editing?.let { sc -> ScheduleDialog(sc, { editing = null }) { changes -> vm.updateSchedule(sc.id, changes); editing = null } }
}

@Composable
private fun StateBadge(state: String) {
    val c = stateColor(state)
    Text(state, color = c, style = MaterialTheme.typography.labelMedium,
        modifier = Modifier.background(c.copy(alpha = 0.15f), RoundedCornerShape(50)).padding(horizontal = 10.dp, vertical = 2.dp))
}

@Composable
private fun JobProgressBar(p: JobProgress?, state: String) {
    if (p == null) { if (state == "running") LinearProgressIndicator(Modifier.fillMaxWidth()); return }
    val unit = p.unit?.let { " $it" } ?: ""
    if (p.total != null && p.total > 0) {
        LinearProgressIndicator(progress = { (p.done.toFloat() / p.total).coerceIn(0f, 1f) }, modifier = Modifier.fillMaxWidth())
        Text("${p.done} / ${p.total}$unit", style = MaterialTheme.typography.bodySmall, color = Muted)
    } else {
        if (state == "running") LinearProgressIndicator(Modifier.fillMaxWidth())
        Text("${p.done}$unit", style = MaterialTheme.typography.bodySmall, color = Muted)
    }
}

@Composable
private fun JobRow(j: JobInfo, onClick: () -> Unit) {
    Column(Modifier.fillMaxWidth().background(CardBg, RoundedCornerShape(16.dp)).clickable(onClick = onClick).padding(14.dp), verticalArrangement = Arrangement.spacedBy(6.dp)) {
        Row(verticalAlignment = Alignment.CenterVertically) {
            Text(j.type, style = MaterialTheme.typography.titleMedium, modifier = Modifier.weight(1f))
            StateBadge(j.state)
        }
        JobProgressBar(j.progress, j.state)
        val ts = when { j.finishedAt != null -> "finished ${relTime(j.finishedAt)}"; j.startedAt != null -> "started ${relTime(j.startedAt)}"; else -> "created ${relTime(j.createdAt)}" }
        Text(ts + (j.scheduleId?.let { " · schedule $it" } ?: ""), style = MaterialTheme.typography.bodySmall, color = Muted)
        j.error?.let { Text(it, style = MaterialTheme.typography.bodySmall, color = stateColor("failed"), maxLines = 2, overflow = TextOverflow.Ellipsis) }
    }
}

@Composable
private fun ScheduleRow(sc: ScheduleInfo, onToggle: (Boolean) -> Unit, onEdit: () -> Unit, onRun: () -> Unit) {
    Column(Modifier.fillMaxWidth().background(CardBg, RoundedCornerShape(16.dp)).padding(14.dp), verticalArrangement = Arrangement.spacedBy(4.dp)) {
        Row(verticalAlignment = Alignment.CenterVertically) {
            Text(sc.type, style = MaterialTheme.typography.titleMedium, modifier = Modifier.weight(1f))
            Switch(sc.enabled, onToggle)
        }
        Text(sc.cron?.let { "cron $it (UTC)" } ?: sc.intervalSecs?.let { "every ${fmtInterval(it)}" } ?: "no schedule", style = MaterialTheme.typography.bodyMedium)
        Text("Next ${relTime(sc.nextRun)} · last ${relTime(sc.lastRun)}", style = MaterialTheme.typography.bodySmall, color = Muted)
        Row { TextButton(onEdit) { Text("Edit") }; TextButton(onRun) { Text("Run now") } }
    }
}

private fun fmtInterval(s: Long) = when {
    s % 86400 == 0L -> "${s / 86400}d"; s % 3600 == 0L -> "${s / 3600}h"; s % 60 == 0L -> "${s / 60}m"; else -> "${s}s"
}

@Composable
private fun RunJobDialog(onDismiss: () -> Unit, onRun: (String) -> Unit) {
    AlertDialog(onDismissRequest = onDismiss, title = { Text("Run job now") },
        text = { Column(Modifier.verticalScroll(rememberScrollState())) { RunnableJobTypes.forEach { t -> Text(t, Modifier.fillMaxWidth().clickable { onRun(t) }.padding(vertical = 12.dp)) } } },
        confirmButton = {}, dismissButton = { TextButton(onDismiss) { Text("Cancel") } })
}


@Composable
private fun ScheduleDialog(sc: ScheduleInfo, onDismiss: () -> Unit, onSave: (Map<String, Any?>) -> Unit) {
    var interval by remember { mutableStateOf(sc.intervalSecs?.toString() ?: "") }
    var cron by remember { mutableStateOf(sc.cron ?: "") }
    AlertDialog(onDismissRequest = onDismiss, title = { Text("Edit ${sc.type} schedule") },
        text = {
            Column(verticalArrangement = Arrangement.spacedBy(8.dp)) {
                OutlinedTextField(interval, { interval = it.filter(Char::isDigit) }, label = { Text("Interval (seconds)") }, singleLine = true)
                OutlinedTextField(cron, { cron = it }, label = { Text("Cron (m h dom mon dow, UTC)") }, singleLine = true)
                Text("Cron wins over the interval. Leave it empty to use the interval.", style = MaterialTheme.typography.bodySmall, color = Muted)
            }
        },
        confirmButton = {
            TextButton(onClick = {
                val m = mutableMapOf<String, Any?>("cron" to cron.trim())
                interval.toLongOrNull()?.takeIf { it >= 1 }?.let { m["interval_secs"] = it }
                onSave(m)
            }) { Text("Save") }
        },
        dismissButton = { TextButton(onDismiss) { Text("Cancel") } })
}

// ---------------------------------------------------------------- detail

@OptIn(ExperimentalMaterial3Api::class)
@Composable
fun JobDetailScreen(nav: Nav, id: String) {
    var job by remember { mutableStateOf<JobInfo?>(null) }
    var error by remember { mutableStateOf<String?>(null) }
    var actionError by remember { mutableStateOf<String?>(null) }
    var confirmDelete by remember { mutableStateOf(false) }
    val scope = rememberCoroutineScope()
    val lifecycle = LocalLifecycleOwner.current.lifecycle
    suspend fun load() {
        runCatching { Graph.repo.job(id) }.onSuccess { job = it; error = null }.onFailure { if (job == null) error = errText(it) }
    }
    LaunchedEffect(id) {
        lifecycle.repeatOnLifecycle(Lifecycle.State.STARTED) {
            load()
            coroutineScope {
                val t = Channel<Unit>(Channel.CONFLATED)
                launch { for (x in t) { delay(500); load() } }
                liveJobs(null, id, onLive = {}, onEvent = { t.trySend(Unit) }, onPoll = { load() })
            }
        }
    }
    fun act(a: String) = scope.launch {
        runCatching { Graph.repo.jobAction(id, a) }.onSuccess { actionError = null }.onFailure { actionError = errText(it) }
        load()
    }
    val listState = remember { androidx.compose.foundation.lazy.LazyListState() }
    val logCount = job?.logs?.size ?: 0
    LaunchedEffect(logCount) {
        if (logCount == 0) return@LaunchedEffect
        val last = listState.layoutInfo.visibleItemsInfo.lastOrNull()?.index ?: -1
        val total = listState.layoutInfo.totalItemsCount
        if (last >= total - 3 || total == 0 || last < 0) listState.scrollToItem(listState.layoutInfo.totalItemsCount.coerceAtLeast(1) + logCount)
    }
    Scaffold(containerColor = Bg, topBar = {
        TopAppBar(title = { Text(job?.type ?: "Job") },
            navigationIcon = { IconButton(onClick = nav::back) { Icon(Icons.AutoMirrored.Filled.ArrowBack, "Back") } },
            colors = TopAppBarDefaults.topAppBarColors(containerColor = Bg))
    }) { pad ->
        val j = job
        if (j == null) { Box(Modifier.padding(pad).fillMaxSize(), Alignment.Center) { if (error != null) Text(error!!, color = MaterialTheme.colorScheme.error) else CircularProgressIndicator() }; return@Scaffold }
        LazyColumn(Modifier.padding(pad), state = listState, contentPadding = PaddingValues(16.dp), verticalArrangement = Arrangement.spacedBy(10.dp)) {
            item {
                Column(verticalArrangement = Arrangement.spacedBy(6.dp)) {
                    Row(verticalAlignment = Alignment.CenterVertically) {
                        StateBadge(j.state); Spacer(Modifier.width(10.dp))
                        Text("attempt ${j.attempts}/${j.maxAttempts}", color = Muted, style = MaterialTheme.typography.bodySmall)
                    }
                    JobProgressBar(j.progress, j.state)
                    Text("id ${j.id}", color = Muted, style = MaterialTheme.typography.bodySmall)
                    Text("created ${relTime(j.createdAt)} · started ${relTime(j.startedAt)} · finished ${relTime(j.finishedAt)}", color = Muted, style = MaterialTheme.typography.bodySmall)
                    j.scheduleId?.let { Text("schedule $it", color = Muted, style = MaterialTheme.typography.bodySmall) }
                }
            }
            item {
                Row(Modifier.horizontalScroll(rememberScrollState()), horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                    if (j.state == "running" || j.state == "queued" || j.state == "paused") OutlinedButton({ act("cancel") }) { Text("Cancel") }
                    if (j.state == "running" || j.state == "queued") OutlinedButton({ act("pause") }) { Text("Pause") }
                    if (j.state == "paused" || j.state == "running") OutlinedButton({ act("resume") }) { Text(if (j.state == "running") "Withdraw pause" else "Resume") }
                    if (j.state == "failed" || j.state == "cancelled") Button({ act("retry") }) { Text("Retry") }
                    if (j.finished) OutlinedButton({ confirmDelete = true }, colors = ButtonDefaults.outlinedButtonColors(contentColor = MaterialTheme.colorScheme.error)) { Text("Delete") }
                }
            }
            actionError?.let { item { Text(it, color = MaterialTheme.colorScheme.error) } }
            j.error?.let { item { Section("Error") { Mono(it, stateColor("failed")) } } }
            item { Section("Params") { Mono(j.params) } }
            j.result?.let { item { Section("Result") { Mono(it) } } }
            item { Text("Logs (${j.logs.size})", style = MaterialTheme.typography.labelLarge, color = Accent) }
            if (j.logs.isEmpty()) item { Text("No log lines", color = Muted) }
            items(j.logs.size) { i ->
                val l = j.logs[i]
                val c = when (l.level) { "error" -> stateColor("failed"); "warn" -> stateColor("paused"); else -> MaterialTheme.colorScheme.onSurface }
                Text("${fmtClock(l.ts)} ${l.level.uppercase().padEnd(5)} ${l.msg}", color = c, fontFamily = FontFamily.Monospace, fontSize = 11.sp, lineHeight = 14.sp)
            }
        }
        if (confirmDelete) AlertDialog(onDismissRequest = { confirmDelete = false }, title = { Text("Delete job?") },
            text = { Text("This removes the ${j.type} job and its logs permanently.") },
            confirmButton = {
                TextButton({
                    confirmDelete = false
                    scope.launch { runCatching { Graph.repo.deleteJob(id) }.onSuccess { nav.back() }.onFailure { actionError = errText(it) } }
                }) { Text("Delete", color = MaterialTheme.colorScheme.error) }
            },
            dismissButton = { TextButton({ confirmDelete = false }) { Text("Cancel") } })
    }
}

private fun fmtClock(ts: Long) = java.text.SimpleDateFormat("HH:mm:ss", java.util.Locale.US).format(java.util.Date(ts * 1000))

@Composable
private fun Section(title: String, content: @Composable () -> Unit) {
    Column(verticalArrangement = Arrangement.spacedBy(4.dp)) {
        Text(title, style = MaterialTheme.typography.labelLarge, color = Accent)
        Box(Modifier.fillMaxWidth().background(CardBg, RoundedCornerShape(12.dp)).padding(10.dp)) { content() }
    }
}

@Composable
private fun Mono(text: String, color: Color = Color.Unspecified) =
    Text(text, color = color, fontFamily = FontFamily.Monospace, fontSize = 12.sp, modifier = Modifier.horizontalScroll(rememberScrollState()))
