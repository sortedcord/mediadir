package dev.sortedcord.mediadir.player

import android.content.Context
import android.graphics.Canvas
import android.graphics.Color
import android.graphics.Paint
import android.graphics.Path
import android.graphics.RectF
import android.graphics.Typeface
import android.view.View
import kotlin.math.cos
import kotlin.math.sin

/** Pill with an icon (volume speaker / brightness sun), a vertical level bar and a percentage. */
class LevelOverlay(ctx: Context) : View(ctx) {
    enum class Kind { VOLUME, BRIGHTNESS }
    private val d = resources.displayMetrics.density
    var kind = Kind.VOLUME
    private var level = 0f
    private val bg = Paint(Paint.ANTI_ALIAS_FLAG).apply { color = 0xB3000000.toInt() }
    private val track = Paint(Paint.ANTI_ALIAS_FLAG).apply { color = 0x40FFFFFF }
    private val fill = Paint(Paint.ANTI_ALIAS_FLAG).apply { color = Color.WHITE }
    private val icon = Paint(Paint.ANTI_ALIAS_FLAG).apply { color = Color.WHITE; strokeWidth = 2 * d; strokeCap = Paint.Cap.ROUND }
    private val text = Paint(Paint.ANTI_ALIAS_FLAG).apply {
        color = Color.WHITE; textSize = 13 * d; typeface = Typeface.DEFAULT_BOLD; textAlign = Paint.Align.CENTER
    }
    private val r = RectF()
    private val p = Path()
    private val hide = Runnable { animate().alpha(0f).setDuration(250).start() }

    init { alpha = 0f; isClickable = false }

    fun show(k: Kind, value: Float) {
        kind = k; level = value.coerceIn(0f, 1f)
        removeCallbacks(hide); animate().cancel(); alpha = 1f
        invalidate()
    }
    fun release() { removeCallbacks(hide); postDelayed(hide, 600) }

    override fun onMeasure(w: Int, h: Int) = setMeasuredDimension((56 * d).toInt(), (220 * d).toInt())

    override fun onDraw(c: Canvas) {
        val w = width.toFloat(); val h = height.toFloat()
        r.set(0f, 0f, w, h); c.drawRoundRect(r, w / 2, w / 2, bg)
        val cx = w / 2
        c.drawText("${(level * 100).toInt()}%", cx, 26 * d, text)
        val top = 40 * d; val bottom = h - 52 * d; val bw = 6 * d
        r.set(cx - bw / 2, top, cx + bw / 2, bottom); c.drawRoundRect(r, bw / 2, bw / 2, track)
        r.set(cx - bw / 2, bottom - (bottom - top) * level, cx + bw / 2, bottom); c.drawRoundRect(r, bw / 2, bw / 2, fill)
        val iy = h - 26 * d
        if (kind == Kind.BRIGHTNESS) {
            icon.style = Paint.Style.FILL
            c.drawCircle(cx, iy, 5 * d, icon)
            for (i in 0 until 8) {
                val a = i * Math.PI / 4
                c.drawLine(cx + (cos(a) * 8 * d).toFloat(), iy + (sin(a) * 8 * d).toFloat(),
                    cx + (cos(a) * 11 * d).toFloat(), iy + (sin(a) * 11 * d).toFloat(), icon)
            }
        } else {
            icon.style = Paint.Style.FILL
            p.reset()
            p.moveTo(cx - 10 * d, iy - 4 * d); p.lineTo(cx - 5 * d, iy - 4 * d); p.lineTo(cx + 1 * d, iy - 9 * d)
            p.lineTo(cx + 1 * d, iy + 9 * d); p.lineTo(cx - 5 * d, iy + 4 * d); p.lineTo(cx - 10 * d, iy + 4 * d); p.close()
            c.drawPath(p, icon)
            icon.style = Paint.Style.STROKE
            if (level > 0f) { r.set(cx - 2 * d, iy - 5 * d, cx + 6 * d, iy + 5 * d); c.drawArc(r, -60f, 120f, false, icon) }
            if (level > 0.5f) { r.set(cx - 2 * d, iy - 9 * d, cx + 10 * d, iy + 9 * d); c.drawArc(r, -60f, 120f, false, icon) }
            if (level == 0f) c.drawLine(cx + 4 * d, iy - 4 * d, cx + 10 * d, iy + 4 * d, icon)
        }
    }
}
