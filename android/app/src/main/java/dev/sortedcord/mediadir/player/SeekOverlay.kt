package dev.sortedcord.mediadir.player

import android.animation.ValueAnimator
import android.content.Context
import android.graphics.Canvas
import android.graphics.Color
import android.graphics.Paint
import android.graphics.Path
import android.graphics.Typeface
import android.view.View
import android.view.animation.DecelerateInterpolator

/** YouTube-style double-tap seek feedback: a curved side panel, a ripple from the tap, chevrons and "N seconds". */
class SeekOverlay(ctx: Context) : View(ctx) {
    private val d = resources.displayMetrics.density
    private var side = 0 // -1 left, +1 right, 0 hidden
    private var seconds = 0
    private var tapX = 0f
    private var tapY = 0f
    private var ripple = 0f   // 0..1
    private var chevron = 0f  // 0..1 running phase
    private val arc = Path()
    private val arcPaint = Paint(Paint.ANTI_ALIAS_FLAG).apply { color = 0x30FFFFFF }
    private val ripplePaint = Paint(Paint.ANTI_ALIAS_FLAG).apply { color = Color.WHITE }
    private val textPaint = Paint(Paint.ANTI_ALIAS_FLAG).apply {
        color = Color.WHITE; textSize = 14 * d; typeface = Typeface.DEFAULT_BOLD; textAlign = Paint.Align.CENTER
        setShadowLayer(4 * d, 0f, 0f, 0x80000000.toInt())
    }
    private val triPaint = Paint(Paint.ANTI_ALIAS_FLAG).apply { color = Color.WHITE }
    private val tri = Path()
    private var rippleAnim: ValueAnimator? = null
    private val chevronAnim = ValueAnimator.ofFloat(0f, 1f).apply {
        duration = 750; repeatCount = ValueAnimator.INFINITE
        addUpdateListener { chevron = it.animatedValue as Float; invalidate() }
    }
    private val hide = Runnable {
        animate().alpha(0f).setDuration(200).withEndAction { side = 0; seconds = 0; chevronAnim.cancel(); invalidate() }.start()
    }

    init { isClickable = false; isFocusable = false; alpha = 0f }

    val activeSide get() = side

    /** Register a seek tap on [s] side at (x,y); returns the accumulated seconds. */
    fun tap(s: Int, x: Float, y: Float, step: Int): Int {
        removeCallbacks(hide)
        animate().cancel()
        if (s != side) seconds = 0
        side = s; seconds += step; tapX = x; tapY = y
        alpha = 1f
        rippleAnim?.cancel()
        rippleAnim = ValueAnimator.ofFloat(0f, 1f).apply {
            duration = 450; interpolator = DecelerateInterpolator()
            addUpdateListener { ripple = it.animatedValue as Float; invalidate() }
            start()
        }
        if (!chevronAnim.isRunning) chevronAnim.start()
        postDelayed(hide, 800)
        return seconds
    }

    fun clear() { removeCallbacks(hide); hide.run() }

    override fun onDraw(c: Canvas) {
        if (side == 0) return
        val w = width.toFloat(); val h = height.toFloat()
        val r = h * 0.95f
        val cx = if (side < 0) -r * 0.45f else w + r * 0.45f
        val panelW = r * 0.55f
        arc.reset(); arc.addCircle(cx, h / 2, r, Path.Direction.CW)
        c.save(); c.clipPath(arc)
        c.drawRect(if (side < 0) 0f else w - panelW, 0f, if (side < 0) panelW else w, h, arcPaint)
        if (ripple < 1f) {
            ripplePaint.alpha = ((1f - ripple) * 70).toInt()
            c.drawCircle(tapX, tapY, ripple * r, ripplePaint)
        }
        c.restore()
        val mx = if (side < 0) panelW * 0.55f else w - panelW * 0.55f
        val my = h / 2
        // Three chevron triangles lighting up in sequence.
        val ts = 9 * d
        for (i in 0 until 3) {
            val phase = ((chevron * 3) - i).let { if (it < 0) it + 3 else it }
            triPaint.alpha = (if (phase < 1f) 255 * (1 - phase * 0.7f) else 70f).toInt().coerceIn(50, 255)
            val ox = mx + (i - 1) * ts * 1.4f * side
            tri.reset()
            val tip = ox + side * ts / 2; val back = ox - side * ts / 2
            tri.moveTo(back, my - ts * 0.6f - 6 * d); tri.lineTo(tip, my - 6 * d); tri.lineTo(back, my + ts * 0.6f - 6 * d); tri.close()
            c.drawPath(tri, triPaint)
        }
        c.drawText("$seconds seconds", mx, my + 22 * d, textPaint)
    }
}
