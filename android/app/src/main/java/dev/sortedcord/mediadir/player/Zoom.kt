package dev.sortedcord.mediadir.player

import android.animation.ValueAnimator
import android.view.View
import android.view.animation.DecelerateInterpolator
import kotlin.math.max

/**
 * Pinch zoom for the video content frame (which PlayerView lays out at "fit" size, centred in [container]).
 * Modes: fit (scale 1) → pinch out snaps to fill (crop to cover the screen) → further pinch zooms up to ~4x with
 * panning, edges clamped. Pinching back in below fill snaps to fit.
 */
class ZoomController(
    private val target: View,
    private val container: View,
    private val onSnap: (filled: Boolean) -> Unit,
) {
    var scale = 1f; private set
    private var tx = 0f
    private var ty = 0f
    var filled = false; private set
    var enabled = true
        set(v) { field = v; apply() }
    private var anim: ValueAnimator? = null
    private var accum = 1f
    private var locked = false // ignore rest of a gesture after snapping back to fit

    init {
        target.addOnLayoutChangeListener { _, l, t, r, b, ol, ot, or_, ob ->
            if (r - l != or_ - ol || b - t != ob - ot) {
                // Video size / orientation changed: keep the mode, recompute the scale.
                anim?.cancel()
                if (filled) { scale = max(scale, fillScale()).coerceAtMost(maxScale()); clamp() }
                apply()
            }
        }
    }

    val zoomed get() = enabled && scale > 1.01f
    /** Zoomed past the fill level: one-finger drags pan. */
    val beyondFill get() = enabled && scale > (if (hasFill()) fillScale() * 1.02f else 1.01f)

    fun fillScale(): Float {
        val w = target.width; val h = target.height
        if (w <= 0 || h <= 0 || container.width <= 0 || container.height <= 0) return 1f
        return max(container.width / w.toFloat(), container.height / h.toFloat()).coerceAtLeast(1f)
    }
    /** Raised while a high-res still frame is shown so its detail is reachable. */
    var maxOverride = 0f
        set(v) {
            field = v
            if (scale > maxScale()) { anim?.cancel(); animateTo(maxScale(), tx, ty) }
        }
    var onTransform: (() -> Unit)? = null
    // Effectively unlimited zoom; 64x is only a float-safety cap.
    private fun maxScale() = max(64f, maxOverride)
    private fun hasFill() = fillScale() > 1.02f

    fun begin() { anim?.cancel(); accum = 1f; locked = false }

    fun scaleBy(f: Float, fx: Float, fy: Float, dx: Float, dy: Float) {
        if (locked || !enabled) return
        val fill = fillScale()
        if (!filled) {
            accum *= f
            if (accum > 1.12f || !hasFill()) {
                filled = true
                if (hasFill()) { animateTo(fill, 0f, 0f); onSnap(true); return }
            } else {
                // Small live feedback before the snap.
                setTransform(accum.coerceIn(1f, 1.12f), 0f, 0f); return
            }
        }
        val min = if (hasFill()) fill * 0.8f else 0.85f
        val ns = (scale * f).coerceIn(min, maxScale())
        if (ns < (if (hasFill()) fill * 0.9f else 0.95f)) {
            filled = false; locked = true
            animateTo(1f, 0f, 0f); onSnap(false); return
        }
        // Keep the focal point under the fingers, and follow the fingers' movement.
        val k = ns / scale
        val cx = fx - container.width / 2f; val cy = fy - container.height / 2f
        tx = cx - (cx - tx) * k + dx
        ty = cy - (cy - ty) * k + dy
        scale = ns
        clamp(); apply()
    }

    fun end() {
        if (!enabled) return
        if (!filled) { if (scale != 1f) animateTo(1f, 0f, 0f) }
        else {
            val fill = if (hasFill()) fillScale() else 1f
            if (scale < fill) animateTo(fill, tx, ty)
        }
    }

    fun pan(dx: Float, dy: Float): Boolean {
        if (!zoomed || anim?.isRunning == true) return false
        tx -= dx; ty -= dy; clamp(); apply(); return true
    }

    /** Programmatic toggle (fit ↔ fill). */
    fun toggle() {
        anim?.cancel()
        filled = !filled
        if (filled) animateTo(if (hasFill()) fillScale() else 1.5f, 0f, 0f) else animateTo(1f, 0f, 0f)
        onSnap(filled)
    }

    fun reset() { anim?.cancel(); filled = false; setTransform(1f, 0f, 0f) }

    private fun clamp(s: Float = scale) {
        val mx = max(0f, (target.width * s - container.width) / 2f)
        val my = max(0f, (target.height * s - container.height) / 2f)
        tx = tx.coerceIn(-mx, mx); ty = ty.coerceIn(-my, my)
    }

    private fun setTransform(s: Float, x: Float, y: Float) { scale = s; tx = x; ty = y; apply() }

    private fun animateTo(s: Float, x: Float, y: Float) {
        anim?.cancel()
        val s0 = scale; val x0 = tx; val y0 = ty
        val sx = x; val sy = y
        anim = ValueAnimator.ofFloat(0f, 1f).apply {
            duration = 180; interpolator = DecelerateInterpolator()
            addUpdateListener {
                val t = it.animatedValue as Float
                scale = s0 + (s - s0) * t
                tx = x0 + (sx - x0) * t; ty = y0 + (sy - y0) * t
                clamp(); apply()
            }
            start()
        }
    }

    private fun apply() {
        val s = if (enabled) scale else 1f
        target.scaleX = s; target.scaleY = s
        target.translationX = if (enabled) tx else 0f
        target.translationY = if (enabled) ty else 0f
        onTransform?.invoke()
    }
}
