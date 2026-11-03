package dev.sortedcord.mediadir.player

import android.animation.ValueAnimator
import android.content.Context
import android.graphics.Canvas
import android.graphics.Color
import android.graphics.Paint
import android.graphics.Path
import android.graphics.RectF
import android.graphics.Typeface
import android.view.View
import android.view.animation.DecelerateInterpolator
import android.view.animation.LinearInterpolator
import kotlin.math.max
import kotlin.math.min

/**
 * Subtle circular status for the HQ pause frame: indeterminate spinning arc while the server extracts the frame,
 * determinate ring while bytes download, then ring completes and morphs into a green circle with a white tick.
 */
class HqIndicator(ctx: Context) : View(ctx) {
    private val d = resources.displayMetrics.density
    private enum class Mode { HIDDEN, SPIN, PROGRESS, DONE }
    private var mode = Mode.HIDDEN
    private var spin = 0f          // rotation for the indeterminate arc
    private var shownProgress = 0f // animated, never goes backwards
    private var targetProgress = 0f
    private var morph = 0f         // 0 = ring, 1 = filled green + tick
    private val ring = Paint(Paint.ANTI_ALIAS_FLAG).apply { style = Paint.Style.STROKE; strokeWidth = 2.5f * d; color = Color.WHITE; strokeCap = Paint.Cap.ROUND }
    private val track = Paint(Paint.ANTI_ALIAS_FLAG).apply { style = Paint.Style.STROKE; strokeWidth = 2.5f * d; color = 0x66FFFFFF }
    private val bg = Paint(Paint.ANTI_ALIAS_FLAG).apply { color = Color.BLACK }
    private val green = Paint(Paint.ANTI_ALIAS_FLAG).apply { color = 0xFF2EBD59.toInt() }
    private val tick = Paint(Paint.ANTI_ALIAS_FLAG).apply { style = Paint.Style.STROKE; strokeWidth = 3f * d; color = Color.WHITE; strokeCap = Paint.Cap.ROUND; strokeJoin = Paint.Join.ROUND }
    private val label = Paint(Paint.ANTI_ALIAS_FLAG).apply { color = Color.WHITE; textSize = 10.5f * d; typeface = Typeface.DEFAULT_BOLD; textAlign = Paint.Align.CENTER }
    private val oval = RectF()
    private val path = Path()
    private val spinAnim = ValueAnimator.ofFloat(0f, 360f).apply {
        duration = 900; repeatCount = ValueAnimator.INFINITE; interpolator = LinearInterpolator()
        addUpdateListener { spin = it.animatedValue as Float; invalidate() }
    }
    private var progAnim: ValueAnimator? = null
    private var doneAnim: ValueAnimator? = null
    private val restAlpha = 0.2f

    /** Press-and-hold on the green circle: true while held (show the original frame). */
    var onHold: ((Boolean) -> Unit)? = null
    private var held = false
    private val drawR get() = 17 * d  // drawn circle ~34dp; the view itself is a 52dp touch target

    init { alpha = 0f; visibility = GONE; isClickable = true }

    override fun onMeasure(w: Int, h: Int) { val s = (52 * d).toInt(); setMeasuredDimension(s, s) }

    @android.annotation.SuppressLint("ClickableViewAccessibility")
    override fun onTouchEvent(e: android.view.MotionEvent): Boolean {
        if (mode != Mode.DONE || morph < 1f) return mode != Mode.HIDDEN // swallow, do nothing while fetching
        when (e.actionMasked) {
            android.view.MotionEvent.ACTION_DOWN -> {
                held = true; performHapticFeedback(android.view.HapticFeedbackConstants.VIRTUAL_KEY)
                onHold?.invoke(true); invalidate()
            }
            android.view.MotionEvent.ACTION_UP, android.view.MotionEvent.ACTION_CANCEL -> {
                if (held) { held = false; onHold?.invoke(false); invalidate() }
            }
        }
        return true
    }

    /** Server is extracting: indeterminate. */
    fun start() {
        cancelAnims(); mode = Mode.SPIN; shownProgress = 0f; targetProgress = 0f; morph = 0f
        visibility = VISIBLE; animate().cancel(); animate().alpha(restAlpha).setDuration(150).start()
        spinAnim.start()
    }

    /** Download progress 0..1 (switches to determinate on first call). */
    fun progress(p: Float) {
        if (mode == Mode.HIDDEN || mode == Mode.DONE) return
        val t = p.coerceIn(0f, 1f)
        if (t <= targetProgress) return
        if (mode == Mode.SPIN) { mode = Mode.PROGRESS; spinAnim.cancel() }
        targetProgress = t
        progAnim?.cancel()
        val from = shownProgress
        progAnim = ValueAnimator.ofFloat(from, t).apply {
            duration = 200; interpolator = DecelerateInterpolator()
            addUpdateListener { shownProgress = max(shownProgress, it.animatedValue as Float); invalidate() }
            start()
        }
    }

    /** Still is visible: complete the ring, morph to green tick, hold, fade. */
    fun success() {
        if (mode == Mode.HIDDEN) return
        spinAnim.cancel(); progAnim?.cancel()
        mode = Mode.DONE
        animate().cancel(); animate().alpha(0.5f).setDuration(250).start()
        val from = shownProgress
        doneAnim = ValueAnimator.ofFloat(0f, 1f).apply {
            duration = 400; interpolator = DecelerateInterpolator()
            addUpdateListener {
                val f = it.animatedValue as Float
                shownProgress = from + (1f - from) * min(1f, f / 0.35f)
                morph = ((f - 0.35f) / 0.65f).coerceIn(0f, 1f)
                invalidate()
            }
            start()

        }
    }

    /** Failure / cancel: fade out quietly. */
    fun hide() {
        if (mode == Mode.HIDDEN && visibility != VISIBLE) return
        mode = Mode.HIDDEN; cancelAnims()
        if (held) { held = false; onHold?.invoke(false) }
        animate().cancel()
        animate().alpha(0f).setDuration(180).withEndAction { if (mode == Mode.HIDDEN) visibility = GONE }.start()
    }

    private fun cancelAnims() { spinAnim.cancel(); progAnim?.cancel(); doneAnim?.cancel() }

    override fun onDraw(c: Canvas) {
        val cx = width / 2f; val cy = height / 2f
        val r = drawR - ring.strokeWidth
        oval.set(cx - r, cy - r, cx + r, cy + r)
        val textY = cy - (label.descent() + label.ascent()) / 2
        if (mode == Mode.DONE && morph > 0f) {
            if (held) {
                // Original frame showing: grey outline only.
                track.color = 0xAAFFFFFF.toInt(); c.drawCircle(cx, cy, drawR, bg); c.drawCircle(cx, cy, r, track); track.color = 0x66FFFFFF
                label.alpha = 160; c.drawText("HQ", cx, textY, label); label.alpha = 255
                return
            }
            c.drawCircle(cx, cy, drawR, bg)
            green.alpha = (255 * morph).toInt()
            c.drawCircle(cx, cy, drawR * (0.55f + 0.45f * morph), green)
            if (morph < 1f) c.drawArc(oval, -90f, 360f, false, ring)
            // "HQ" writes in: revealed left-to-right with the fill.
            val tw = label.measureText("HQ")
            c.save(); c.clipRect(cx - tw / 2 - 2, 0f, cx - tw / 2 + (tw + 4) * morph, height.toFloat())
            c.drawText("HQ", cx, textY, label); c.restore()
            return
        }
        c.drawCircle(cx, cy, drawR, bg)
        c.drawCircle(cx, cy, r, track)
        when (mode) {
            Mode.SPIN -> c.drawArc(oval, spin - 90f, 100f, false, ring)
            else -> if (shownProgress > 0f) c.drawArc(oval, -90f, 360f * shownProgress, false, ring)
        }
        c.drawText("HQ", cx, textY, label)
    }
}
