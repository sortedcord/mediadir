package dev.sortedcord.mediadir.ui

import android.content.Context
import android.content.Intent
import androidx.compose.foundation.layout.*
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.filled.Home
import androidx.compose.material.icons.filled.Search
import androidx.compose.material.icons.filled.Settings
import androidx.compose.material3.*
import androidx.compose.runtime.*
import androidx.compose.runtime.saveable.rememberSaveable
import androidx.compose.ui.Modifier
import androidx.compose.ui.graphics.vector.ImageVector
import androidx.navigation.compose.currentBackStackEntryAsState
import androidx.navigation.NavHostController
import androidx.navigation.NavType
import androidx.navigation.NavGraph.Companion.findStartDestination
import androidx.navigation.compose.NavHost
import androidx.navigation.compose.composable
import androidx.navigation.compose.rememberNavController
import androidx.navigation.navArgument
import dev.sortedcord.mediadir.domain.ItemSummary
import dev.sortedcord.mediadir.ui.screens.*

object Routes {
    const val HOME = "home"; const val LIBRARY = "library/{id}"; const val ITEM = "item/{id}"; const val PERSON = "person/{id}"
    const val SEARCH = "search"; const val SETTINGS = "settings"; const val FIELDS = "fields"
}

/** Fired with the tab route when the already-selected tab is tapped while at its root. */
object TabReselect { val events = kotlinx.coroutines.flow.MutableSharedFlow<String>(extraBufferCapacity = 1) }

@Composable
fun OnTabReselect(route: String, action: suspend () -> Unit) {
    LaunchedEffect(route) { TabReselect.events.collect { if (it == route) action() } }
}

private data class Tab(val route: String, val label: String, val icon: ImageVector)
private val tabs = listOf(
    Tab(Routes.HOME, "Home", Icons.Default.Home),
    Tab(Routes.SEARCH, "Search", Icons.Default.Search),
    Tab(Routes.SETTINGS, "Settings", Icons.Default.Settings),
)

class Nav(val controller: NavHostController, private val ctx: Context) {
    fun back() { if (!controller.popBackStack()) (ctx as? android.app.Activity)?.finish() }
    fun library(id: String) = controller.navigate("library/$id")
    fun item(id: String) = controller.navigate("item/$id")
    fun open(item: ItemSummary) = item(item.id)
    fun person(id: String) = controller.navigate("person/$id")
    fun search() = controller.navigate(Routes.SEARCH)
    fun settings() = controller.navigate(Routes.SETTINGS)
    fun settingsPage(name: String) = controller.navigate("settings/$name")
    fun ingest() = controller.navigate("ingest")
    fun fieldSettings() = controller.navigate(Routes.FIELDS)
    fun home() = controller.navigate(Routes.HOME) { popUpTo(0) }
    /** Contract with the player: it fetches /api/items/{id}/playback itself. */
    fun play(itemId: String) = ctx.startActivity(
        Intent(ctx, dev.sortedcord.mediadir.player.PlayerActivity::class.java).putExtra("item_id", itemId))
}

@Composable
fun AppNav(ctx: Context) {
    val c = rememberNavController()
    val nav = remember(c) { Nav(c, ctx) }
    val idArg = listOf(navArgument("id") { type = NavType.StringType })
    val entry by c.currentBackStackEntryAsState()
    val route = entry?.destination?.route
    var selected by rememberSaveable { mutableStateOf(Routes.HOME) }
    LaunchedEffect(route) { if (tabs.any { it.route == route }) selected = route!! }
    Column(Modifier.fillMaxSize()) {
        NavHost(c, startDestination = Routes.HOME, modifier = Modifier.weight(1f).consumeWindowInsets(WindowInsets.navigationBars)) {
            composable(Routes.HOME) { HomeScreen(nav) }
            composable(Routes.LIBRARY, idArg) { LibraryScreen(nav, it.arguments!!.getString("id")!!) }
            composable(Routes.ITEM, idArg) { ItemScreen(nav, it.arguments!!.getString("id")!!) }
            composable(Routes.PERSON, idArg) { PersonScreen(nav, it.arguments!!.getString("id")!!) }
            composable(Routes.SEARCH) { SearchScreen(nav) }
            composable(Routes.SETTINGS) { SettingsScreen(nav) }
            composable("settings/server") { ServerSettingsScreen(nav) }
            composable("settings/account") { AccountSettingsScreen(nav) }
            composable("settings/playback") { PlaybackSettingsScreen(nav) }
            composable("settings/about") { AboutSettingsScreen(nav) }
            composable("ingest") { IngestScreen(nav) }
            composable(Routes.FIELDS) { FieldSettingsScreen(nav) }
        }
        NavigationBar {
            tabs.forEach { t ->
                NavigationBarItem(
                    selected = selected == t.route,
                    onClick = {
                        if (selected == t.route) {
                            if (route == t.route) TabReselect.events.tryEmit(t.route)
                            else if (!c.popBackStack(t.route, inclusive = false)) c.navigate(t.route) { launchSingleTop = true }
                        } else {
                            c.navigate(t.route) {
                                popUpTo(c.graph.findStartDestination().id) { saveState = true }
                                launchSingleTop = true; restoreState = true
                            }
                        }
                    },
                    icon = { Icon(t.icon, null) }, label = { Text(t.label) },
                )
            }
        }
    }
}
