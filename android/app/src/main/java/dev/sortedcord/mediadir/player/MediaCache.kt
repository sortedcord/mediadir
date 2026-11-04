package dev.sortedcord.mediadir.player

import android.content.Context
import androidx.annotation.OptIn
import androidx.media3.common.util.UnstableApi
import androidx.media3.database.StandaloneDatabaseProvider
import androidx.media3.datasource.DataSource
import androidx.media3.datasource.DataSpec
import androidx.media3.datasource.cache.CacheDataSource
import androidx.media3.datasource.cache.CacheKeyFactory
import androidx.media3.datasource.cache.LeastRecentlyUsedCacheEvictor
import androidx.media3.datasource.cache.SimpleCache
import java.io.File

/**
 * App-wide on-disk cache (LRU, 512 MB) in front of the HTTP data source, so re-reading the start of a
 * file (moov atom, first GOP) on loop / seek hits disk instead of the network. SimpleCache allows only
 * one instance per directory, hence the singleton.
 */
@OptIn(UnstableApi::class)
object MediaCache {
    private const val MAX_BYTES = 512L * 1024 * 1024
    @Volatile private var cache: SimpleCache? = null

    /** Stable key per file: scheme/query-free `/stream/<id>` path, so any changing query params don't split the cache. */
    private val keyFactory = CacheKeyFactory { spec: DataSpec ->
        spec.key ?: (spec.uri.encodedPath ?: spec.uri.toString())
    }

    private fun get(ctx: Context): SimpleCache = cache ?: synchronized(this) {
        cache ?: SimpleCache(
            File(ctx.applicationContext.cacheDir, "media3"),
            LeastRecentlyUsedCacheEvictor(MAX_BYTES),
            StandaloneDatabaseProvider(ctx.applicationContext),
        ).also { cache = it }
    }

    fun wrap(ctx: Context, upstream: DataSource.Factory): DataSource.Factory =
        CacheDataSource.Factory()
            .setCache(get(ctx))
            .setUpstreamDataSourceFactory(upstream)
            .setCacheKeyFactory(keyFactory)
            .setFlags(CacheDataSource.FLAG_IGNORE_CACHE_ON_ERROR)
}
