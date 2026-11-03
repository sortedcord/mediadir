package dev.sortedcord.mediadir.player

import android.content.Context
import android.graphics.Bitmap
import android.graphics.BitmapFactory
import android.graphics.BitmapRegionDecoder
import android.graphics.Canvas
import android.graphics.Paint
import android.graphics.Rect
import android.graphics.RectF
import android.os.Handler
import android.os.Looper
import android.util.Log
import android.view.View
import java.io.File
import java.util.concurrent.Executors
import kotlin.math.max
import kotlin.math.min

/**
 * Still-frame overlay placed INSIDE the video content frame, so it inherits exactly the same fit/fill/zoom transform
 * as the video surface. Draws a downsampled base bitmap plus on-demand high-res tiles (BitmapRegionDecoder) for the
 * visible region at the current on-screen scale, so an 8K frame never gets decoded whole.
 */
class HqFrameView(ctx: Context, private val screenScale: () -> Float) : View(ctx) {
    private val main = Handler(Looper.getMainLooper())
    private val exec = Executors.newSingleThreadExecutor()
    private var decoder: BitmapRegionDecoder? = null
    private var base: Bitmap? = null
    var srcW = 0; private set
    var srcH = 0; private set
    private var gen = 0
    private data class Key(val n: Int, val x: Int, val y: Int)
    private val tiles = HashMap<Key, Bitmap>()
    private val pending = HashSet<Key>()
    private val paint = Paint(Paint.FILTER_BITMAP_FLAG or Paint.ANTI_ALIAS_FLAG)
    private val dst = RectF()
    private val src = Rect()
    private var baseSample = 1

    init { alpha = 0f; visibility = GONE; isClickable = false }

    private var shown = false
    val active get() = shown

    /** Decode [file] off the main thread; calls [onReady] on the main thread (or nothing on failure). */
    fun load(file: File, onReady: () -> Unit) {
        val g = ++gen
        val vw = max(width, resources.displayMetrics.widthPixels)
        exec.execute {
            try {
                val o = BitmapFactory.Options().apply { inJustDecodeBounds = true }
                BitmapFactory.decodeFile(file.path, o)
                if (o.outWidth <= 0) return@execute
                var n = 1
                while (o.outWidth / (n * 2) >= vw) n *= 2
                val b = BitmapFactory.decodeFile(file.path, BitmapFactory.Options().apply {
                    inSampleSize = n; inPreferredConfig = Bitmap.Config.RGB_565
                }) ?: return@execute
                @Suppress("DEPRECATION")
                val dec = BitmapRegionDecoder.newInstance(file.path, false)
                main.post {
                    if (g != gen) { b.recycle(); dec?.recycle(); return@post }
                    clearInternal()
                    shown = true
                    base = b; decoder = dec; srcW = o.outWidth; srcH = o.outHeight; baseSample = n
                    visibility = VISIBLE
                    animate().cancel(); animate().alpha(1f).setDuration(200).start()
                    invalidate(); onReady()
                }
            } catch (t: Throwable) { Log.w("HqFrame", "decode failed", t) }
        }
    }

    /** Fade out and release everything; cancels pending decodes. */
    fun drop() {
        gen++
        shown = false
        if (base == null) { visibility = GONE; return }
        animate().cancel()
        animate().alpha(0f).setDuration(150).withEndAction { if (alpha == 0f) { clearInternal(); visibility = GONE } }.start()
    }

    private fun clearInternal() {
        base?.recycle(); base = null
        decoder?.recycle(); decoder = null
        tiles.values.forEach { it.recycle() }; tiles.clear(); pending.clear()
    }

    override fun onDraw(c: Canvas) {
        val b = base ?: return
        val w = width.toFloat(); val h = height.toFloat()
        if (w <= 0 || h <= 0) return
        dst.set(0f, 0f, w, h); c.drawBitmap(b, null, dst, paint)
        val dec = decoder ?: return
        // Source pixels per on-screen pixel.
        val ratio = srcW / (w * screenScale().coerceAtLeast(0.01f))
        // Past ~4x native the source pixels are magnified: keep them crisp (nearest neighbour).
        paint.isFilterBitmap = ratio > 0.25f
        var n = 1
        while (n * 2 <= ratio) n *= 2
        if (n >= baseSample) { trim(emptySet()); return }
        // Visible part of this view in its own coordinates.
        val vis = visibleRect() ?: return
        val sx = srcW / w; val sy = srcH / h
        val tileSrc = 512 * n
        val x0 = (vis.left * sx / tileSrc).toInt(); val x1 = (vis.right * sx / tileSrc).toInt()
        val y0 = (vis.top * sy / tileSrc).toInt(); val y1 = (vis.bottom * sy / tileSrc).toInt()
        val need = HashSet<Key>()
        for (ty in y0..y1) for (tx in x0..x1) {
            val k = Key(n, tx, ty); need += k
            val t = tiles[k]
            val l = tx * tileSrc; val tp = ty * tileSrc
            val r = min(srcW, l + tileSrc); val bt = min(srcH, tp + tileSrc)
            if (l >= srcW || tp >= srcH) continue
            if (t != null) {
                dst.set(l / sx, tp / sy, r / sx, bt / sy); c.drawBitmap(t, null, dst, paint)
            } else if (k !in pending) {
                pending += k
                val g = gen
                exec.execute {
                    val bmp = try {
                        dec.decodeRegion(Rect(l, tp, r, bt), BitmapFactory.Options().apply {
                            inSampleSize = n; inPreferredConfig = Bitmap.Config.ARGB_8888
                        })
                    } catch (e: Throwable) { null }
                    main.post {
                        pending -= k
                        if (bmp == null) return@post
                        if (g != gen || decoder !== dec) { bmp.recycle(); return@post }
                        tiles[k] = bmp; invalidate()
                    }
                }
            }
        }
        trim(need)
    }

    /** Recycle tiles that are off-screen or at another detail level. */
    private fun trim(keep: Set<Key>) {
        val it = tiles.entries.iterator()
        while (it.hasNext()) { val e = it.next(); if (e.key !in keep) { e.value.recycle(); it.remove() } }
    }

    /** The part of this view visible on screen, in this view's local coordinates. */
    private fun visibleRect(): RectF? {
        val p = parent as? View ?: return null
        val root = p.parent as? View ?: return null
        val s = p.scaleX.takeIf { it > 0 } ?: return null
        val w = width.toFloat(); val h = height.toFloat()
        // Parent is scaled around its centre and translated; its layout box sits at (p.left, p.top) inside root.
        val cx = p.left + p.width / 2f + p.translationX; val cy = p.top + p.height / 2f + p.translationY
        val l = (0f - cx) / s + p.width / 2f - left; val r = (root.width - cx) / s + p.width / 2f - left
        val t = (0f - cy) / s + p.height / 2f - top; val b = (root.height - cy) / s + p.height / 2f - top
        return RectF(max(0f, l), max(0f, t), min(w, r), min(h, b)).takeIf { it.width() > 0 && it.height() > 0 }
    }

    fun release() { gen++; exec.shutdownNow(); clearInternal() }
}
