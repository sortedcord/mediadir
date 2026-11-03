package dev.sortedcord.mediadir

import android.app.Application
import coil.ImageLoader
import coil.ImageLoaderFactory
import dev.sortedcord.mediadir.data.Graph
import okhttp3.OkHttpClient

class App : Application(), ImageLoaderFactory {
    override fun onCreate() { super.onCreate(); Graph.init(this) }

    /** Coil's OkHttp client adds the analytics identity headers to every image request. */
    override fun newImageLoader(): ImageLoader = ImageLoader.Builder(this)
        .okHttpClient {
            OkHttpClient.Builder().addInterceptor { chain ->
                val b = chain.request().newBuilder()
                Graph.settings.identityHeaders().forEach { (k, v) -> b.header(k, v) }
                chain.proceed(b.build())
            }.build()
        }.build()
}
