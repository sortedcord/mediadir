package dev.sortedcord.mediadir.data

import android.content.Context

/** Minimal manual DI. */
object Graph {
    lateinit var settings: Settings; private set
    lateinit var repo: MediaRepository; private set
    fun init(ctx: Context) {
        if (::settings.isInitialized) return
        settings = Settings(ctx)
        repo = HttpMediaRepository(settings)
    }
}
