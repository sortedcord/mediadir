package dev.sortedcord.mediadir.ui

import android.content.Context
import android.content.Intent
import androidx.compose.runtime.Composable
import androidx.navigation.NavHostController
import androidx.navigation.NavType
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

class Nav(val controller: NavHostController, private val ctx: Context) {
    fun back() { if (!controller.popBackStack()) (ctx as? android.app.Activity)?.finish() }
    fun library(id: String) = controller.navigate("library/$id")
    fun item(id: String) = controller.navigate("item/$id")
    fun open(item: ItemSummary) = item(item.id)
    fun person(id: String) = controller.navigate("person/$id")
    fun search() = controller.navigate(Routes.SEARCH)
    fun settings() = controller.navigate(Routes.SETTINGS)
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
    val nav = Nav(c, ctx)
    val idArg = listOf(navArgument("id") { type = NavType.StringType })
    NavHost(c, startDestination = Routes.HOME) {
        composable(Routes.HOME) { HomeScreen(nav) }
        composable(Routes.LIBRARY, idArg) { LibraryScreen(nav, it.arguments!!.getString("id")!!) }
        composable(Routes.ITEM, idArg) { ItemScreen(nav, it.arguments!!.getString("id")!!) }
        composable(Routes.PERSON, idArg) { PersonScreen(nav, it.arguments!!.getString("id")!!) }
        composable(Routes.SEARCH) { SearchScreen(nav) }
        composable(Routes.SETTINGS) { SettingsScreen(nav) }
        composable("ingest") { IngestScreen(nav) }
        composable(Routes.FIELDS) { FieldSettingsScreen(nav) }
    }
}
