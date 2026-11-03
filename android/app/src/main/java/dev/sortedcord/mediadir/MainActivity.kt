package dev.sortedcord.mediadir

import android.os.Bundle
import androidx.activity.ComponentActivity
import androidx.activity.compose.setContent
import androidx.activity.enableEdgeToEdge
import dev.sortedcord.mediadir.data.Graph
import dev.sortedcord.mediadir.ui.AppNav
import dev.sortedcord.mediadir.ui.MediaDirTheme

class MainActivity : ComponentActivity() {
    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        Graph.init(this)
        enableEdgeToEdge()
        setContent { MediaDirTheme { AppNav(this) } }
    }
}
