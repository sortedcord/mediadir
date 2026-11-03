package dev.sortedcord.mediadir.player

import android.annotation.SuppressLint
import android.content.Context
import android.view.GestureDetector
import android.view.MotionEvent
import android.view.ScaleGestureDetector
import android.widget.FrameLayout
import androidx.annotation.OptIn
import androidx.media3.common.util.UnstableApi
import androidx.media3.ui.PlayerView

/** PlayerView that never toggles its controller by itself; it only reports that a touch reached the background. */
@OptIn(UnstableApi::class)
@SuppressLint("ViewConstructor", "ClickableViewAccessibility")
class TouchPlayerView(ctx: Context, private val onBackgroundDown: () -> Unit) : PlayerView(ctx) {
    override fun onTouchEvent(event: MotionEvent): Boolean {
        if (event.actionMasked == MotionEvent.ACTION_DOWN) onBackgroundDown()
        return true
    }
    override fun performClick(): Boolean = true // tap handling lives in GestureLayer
}

/**
 * Root layer that sees every touch first. Children (the controller buttons, time bar) get the events as usual;
 * taps/double-taps/pans only count when the touch landed on the video background ([backgroundDown], set by
 * [TouchPlayerView]). A pinch cancels the children's gesture and goes to the zoom handler.
 */
@SuppressLint("ViewConstructor")
class GestureLayer(ctx: Context, private val l: Listener) : FrameLayout(ctx) {
    interface Listener {
        fun zoomEnabled(): Boolean
        fun onSingleTap()
        fun onDoubleTap(x: Float, y: Float)
        /** A plain tap while a double-tap seek is in progress; return true if consumed. */
        fun onSeekTap(x: Float, y: Float): Boolean
        fun inSeekMode(): Boolean
        fun onScaleBegin()
        fun onScale(factor: Float, fx: Float, fy: Float, dx: Float, dy: Float)
        fun onScaleEnd()
        /** One-finger drag on the background (pan when zoomed, else level swipes). */
        fun onDrag(down: MotionEvent, e: MotionEvent, dx: Float, dy: Float): Boolean
        fun onDragEnd()
    }

    var backgroundDown = false
    private var multi = false
    private var scaling = false
    private var childrenCancelled = false
    private var lastFx = 0f
    private var lastFy = 0f

    private val scaleDet = ScaleGestureDetector(ctx, object : ScaleGestureDetector.SimpleOnScaleGestureListener() {
        override fun onScaleBegin(d: ScaleGestureDetector): Boolean {
            if (!l.zoomEnabled()) return false
            scaling = true; lastFx = d.focusX; lastFy = d.focusY
            l.onScaleBegin(); return true
        }
        override fun onScale(d: ScaleGestureDetector): Boolean {
            l.onScale(d.scaleFactor, d.focusX, d.focusY, d.focusX - lastFx, d.focusY - lastFy)
            lastFx = d.focusX; lastFy = d.focusY
            return true
        }
        override fun onScaleEnd(d: ScaleGestureDetector) { l.onScaleEnd() }
    }).apply { isQuickScaleEnabled = false }

    private val tapDet = GestureDetector(ctx, object : GestureDetector.SimpleOnGestureListener() {
        override fun onDown(e: MotionEvent) = true
        override fun onSingleTapUp(e: MotionEvent): Boolean =
            backgroundDown && !multi && l.inSeekMode() && l.onSeekTap(e.x, e.y)
        override fun onSingleTapConfirmed(e: MotionEvent): Boolean {
            if (backgroundDown && !multi && !l.inSeekMode()) l.onSingleTap()
            return true
        }
        override fun onDoubleTap(e: MotionEvent): Boolean {
            if (backgroundDown && !multi) l.onDoubleTap(e.x, e.y)
            return true
        }
        override fun onScroll(e1: MotionEvent?, e2: MotionEvent, dx: Float, dy: Float): Boolean =
            e1 != null && backgroundDown && !multi && !scaling && l.onDrag(e1, e2, dx, dy)
    })

    override fun dispatchTouchEvent(ev: MotionEvent): Boolean {
        if (ev.actionMasked == MotionEvent.ACTION_DOWN) {
            backgroundDown = false; multi = false; childrenCancelled = false
        }
        if (ev.pointerCount > 1) multi = true
        if (!childrenCancelled) super.dispatchTouchEvent(ev)
        scaleDet.onTouchEvent(ev)
        if (scaling && !childrenCancelled) {
            val c = MotionEvent.obtain(ev).apply { action = MotionEvent.ACTION_CANCEL }
            super.dispatchTouchEvent(c); c.recycle()
            childrenCancelled = true
        }
        tapDet.onTouchEvent(ev)
        if (ev.actionMasked == MotionEvent.ACTION_UP || ev.actionMasked == MotionEvent.ACTION_CANCEL) { scaling = false; l.onDragEnd() }
        if (multi) l.onDragEnd()
        return true
    }
}
