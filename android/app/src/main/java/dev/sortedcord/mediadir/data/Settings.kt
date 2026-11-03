package dev.sortedcord.mediadir.data

import android.content.Context

/** Tiny SharedPreferences wrapper: server URL, quality preference, resume positions. */
class Settings(ctx: Context) {
    private val prefs = ctx.applicationContext.getSharedPreferences("mediadir", 0)
    private val positions = ctx.applicationContext.getSharedPreferences("positions", 0)

    var server: String
        get() = (prefs.getString("server", DEFAULT_SERVER) ?: DEFAULT_SERVER).trim().trimEnd('/')
        set(v) = prefs.edit().putString("server", v.trim().trimEnd('/')).apply()

    /** 0 = Auto (best playable), otherwise max height (e.g. 2160, 1080, 720). */
    var quality: Int
        get() = prefs.getInt("quality", 0)
        set(v) = prefs.edit().putInt("quality", v).apply()

    /** Replace the paused frame with a full-quality still from the server. */
    var hqPauseFrame: Boolean
        get() = prefs.getBoolean("hq_pause_frame", true)
        set(v) = prefs.edit().putBoolean("hq_pause_frame", v).apply()

    /** How long playback must stay paused before the HQ frame is fetched. */
    var hqPauseDelayMs: Int
        get() = prefs.getInt("hq_pause_delay_ms", 1200)
        set(v) = prefs.edit().putInt("hq_pause_delay_ms", v).apply()

    fun position(itemId: String) = positions.getLong(itemId, 0)
    fun savePosition(itemId: String, ms: Long) = positions.edit().putLong(itemId, ms).apply()

    /** Absolute URL from a server-relative path. */
    fun url(path: String?): String? = when {
        path.isNullOrBlank() -> null
        path.startsWith("http://") || path.startsWith("https://") -> path
        else -> server + (if (path.startsWith("/")) path else "/$path")
    }

    /** Asset URL; the server never upscales. Sizes are in pixels. */
    fun asset(hash: String, w: Int? = null, h: Int? = null, fit: String? = null): String = buildString {
        append(server).append("/asset/").append(hash)
        append("?fmt=webp")
        if (w != null) append("&w=").append(w)
        if (h != null) append("&h=").append(h)
        if (w != null && h != null && fit != null) append("&fit=").append(fit)
    }

    companion object { const val DEFAULT_SERVER = "http://192.168.0.113:8096" }
}
