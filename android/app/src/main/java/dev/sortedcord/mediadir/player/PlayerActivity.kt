package dev.sortedcord.mediadir.player

import dev.sortedcord.mediadir.domain.ImagePolicy
import dev.sortedcord.mediadir.ui.components.ImageResolver
import android.app.AlertDialog
import android.app.PictureInPictureParams
import android.content.Context
import android.content.Intent
import android.content.pm.ActivityInfo
import android.graphics.Color
import android.graphics.Typeface
import android.graphics.drawable.GradientDrawable
import android.net.Uri
import android.os.Build
import android.os.Bundle
import android.os.Handler
import android.os.Looper
import android.text.TextUtils
import android.util.Rational
import android.view.Gravity
import android.view.HapticFeedbackConstants
import android.view.MotionEvent
import android.view.View
import android.view.ViewGroup
import android.view.WindowManager
import android.widget.ArrayAdapter
import android.widget.FrameLayout
import android.widget.ImageButton
import android.widget.ImageView
import android.widget.LinearLayout
import android.widget.TextView
import android.widget.Toast
import androidx.activity.ComponentActivity
import androidx.activity.OnBackPressedCallback
import androidx.annotation.OptIn
import androidx.core.view.ViewCompat
import androidx.core.view.WindowCompat
import androidx.core.view.WindowInsetsCompat
import androidx.core.view.WindowInsetsControllerCompat
import androidx.lifecycle.Lifecycle
import androidx.lifecycle.lifecycleScope
import androidx.media3.common.AudioAttributes
import androidx.media3.common.C
import androidx.media3.common.MediaItem
import androidx.media3.common.MediaMetadata
import androidx.media3.common.MimeTypes
import androidx.media3.common.PlaybackException
import androidx.media3.common.Player
import androidx.media3.common.util.UnstableApi
import androidx.media3.datasource.DefaultHttpDataSource
import androidx.media3.exoplayer.DefaultLoadControl
import androidx.media3.exoplayer.DefaultRenderersFactory
import androidx.media3.exoplayer.ExoPlayer
import androidx.media3.exoplayer.source.DefaultMediaSourceFactory
import androidx.media3.exoplayer.trackselection.DefaultTrackSelector
import androidx.media3.ui.AspectRatioFrameLayout
import androidx.media3.ui.PlayerView
import coil.Coil
import coil.request.ImageRequest
import dev.sortedcord.mediadir.data.Graph
import dev.sortedcord.mediadir.domain.MediaFile
import dev.sortedcord.mediadir.domain.Quality
import dev.sortedcord.mediadir.domain.QueueEntry
import kotlinx.coroutines.launch

/**
 * Direct-play queue player. Hardware MediaCodec decoders first, bundled FFmpeg decoders as fallback.
 * Each queue entry plays the best playable MediaFile; the user can switch file on the fly.
 * Gestures: tap toggles controls, double-tap left/right seeks ±10s (accumulating), pinch zooms (fit → fill → 4x) + pan.
 *
 * Launch: `Intent(ctx, PlayerActivity::class.java).putExtra("item_id", id)` (+ optional int `start_index`).
 */
@OptIn(UnstableApi::class)
class PlayerActivity : ComponentActivity(), GestureLayer.Listener {
    companion object {
        const val EXTRA_ITEM_ID = "item_id"
        const val EXTRA_START_INDEX = "start_index"
        private const val SEEK_STEP_S = 10

        /** Quality choice for this app session (null = not chosen → settings default). 0 = Auto, else max height. */
        private var sessionMaxH: Int? = null

        fun start(ctx: Context, itemId: String, startIndex: Int? = null) =
            ctx.startActivity(Intent(ctx, PlayerActivity::class.java).putExtra(EXTRA_ITEM_ID, itemId).apply {
                if (startIndex != null) putExtra(EXTRA_START_INDEX, startIndex)
            })
    }

    private val settings get() = Graph.settings
    private val d by lazy { resources.displayMetrics.density }
    private var player: ExoPlayer? = null
    private lateinit var root: GestureLayer
    private lateinit var view: TouchPlayerView
    private lateinit var zoom: ZoomController
    private lateinit var seekOverlay: SeekOverlay
    private lateinit var levelOverlay: LevelOverlay
    private lateinit var playBtn: ImageButton
    private lateinit var hq: HqFrameView
    private lateinit var hqChip: HqIndicator
    private var hqJob: kotlinx.coroutines.Job? = null
    private val hqStart = Runnable { requestHqFrame() }
    // Presentation time of the last frame actually released to the surface (renderer timeline) and the
    // renderer-to-media offset, calibrated on the first frame after each start/seek (that frame == target).
    @Volatile private var lastFramePtsUs = Long.MIN_VALUE
    @Volatile private var calibrateTargetUs = Long.MIN_VALUE
    @Volatile private var ptsOffsetUs = Long.MIN_VALUE
    private fun calibrate(targetMs: Long) { lastFramePtsUs = Long.MIN_VALUE; calibrateTargetUs = targetMs * 1000 }

    /** Media position of the frame on screen, or currentPosition when unknown. */
    private fun shownFrameMs(p: Player): Long {
        // Media3 reports the frame's media pts directly (verified on device: rawPts == media time, offset ~0);
        // the post-seek calibration is only a fallback if a renderer offset ever shows up.
        val pts = lastFramePtsUs
        if (pts == Long.MIN_VALUE) return p.currentPosition
        val off = ptsOffsetUs.takeIf { it != Long.MIN_VALUE && kotlin.math.abs(it) > 1_000_000 } ?: 0L
        val ms = (pts - off) / 1000
        return if (ms < 0 || kotlin.math.abs(ms - p.currentPosition) > 10_000) p.currentPosition else ms
    }
    private lateinit var loopBtn: TextView
    private var loopA = -1L
    private var loopB = -1L
    private lateinit var loop: AbLoop
    private lateinit var topBar: LinearLayout
    private lateinit var titleView: TextView
    private lateinit var subView: TextView
    private lateinit var qualityBtn: TextView
    private lateinit var hint: TextView
    private lateinit var audioPane: LinearLayout
    private lateinit var artView: ImageView
    private lateinit var audioTitle: TextView
    private lateinit var audioSub: TextView
    private lateinit var status: TextView

    private var queue: List<PEntry> = emptyList()
    private var chosen: MutableList<MediaFile?> = mutableListOf()
    private val manual = mutableSetOf<Int>()        // entries where the user explicitly picked a file
    private val fellBack = mutableSetOf<Int>()      // entries that already auto-fell back once
    private var startIndex = 0
    private var restoreIndex = -1
    private var restorePos = 0L
    private var lastSeekTapAt = 0L
    private var seekBase = -1L                      // position the current accumulated seek started from
    private var inPip = false
    private val ui = Handler(Looper.getMainLooper())
    private val saveTick = object : Runnable {
        override fun run() { savePos(); ui.postDelayed(this, 5000) }
    }

    private val maxH get() = sessionMaxH ?: settings.quality

    // ---------------------------------------------------------------- setup

    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        Graph.init(this)
        window.addFlags(WindowManager.LayoutParams.FLAG_KEEP_SCREEN_ON)
        if (Build.VERSION.SDK_INT >= 28) window.attributes = window.attributes.apply {
            layoutInDisplayCutoutMode = WindowManager.LayoutParams.LAYOUT_IN_DISPLAY_CUTOUT_MODE_SHORT_EDGES
        }
        WindowCompat.setDecorFitsSystemWindows(window, false)
        WindowInsetsControllerCompat(window, window.decorView).apply {
            hide(WindowInsetsCompat.Type.systemBars())
            systemBarsBehavior = WindowInsetsControllerCompat.BEHAVIOR_SHOW_TRANSIENT_BARS_BY_SWIPE
        }
        buildViews()
        if (savedInstanceState != null) {
            restoreIndex = savedInstanceState.getInt("index", -1); restorePos = savedInstanceState.getLong("pos", 0)
        }
        onBackPressedDispatcher.addCallback(this, object : OnBackPressedCallback(true) {
            override fun handleOnBackPressed() = close()
        })
        load()
    }

    private fun dp(v: Number) = (v.toFloat() * d).toInt()

    private fun buildViews() {
        root = GestureLayer(this, this).apply { setBackgroundColor(Color.BLACK) }
        view = TouchPlayerView(this) { root.backgroundDown = true }.apply {
            setShowSubtitleButton(true)
            setShowNextButton(true); setShowPreviousButton(true)
            setFullscreenButtonClickListener { if (zoomEnabled()) zoom.toggle() }
            setBackgroundColor(Color.BLACK)
            keepScreenOn = true
            controllerShowTimeoutMs = 4000
            controllerAutoShow = false // pausing (e.g. via the ghost button) must not pop the controller
            resizeMode = AspectRatioFrameLayout.RESIZE_MODE_FIT
            useController = false // enabled once the queue is loaded
        }
        val content = view.findViewById<View>(androidx.media3.ui.R.id.exo_content_frame)
        zoom = ZoomController(content, view) { filled ->
            view.performHapticFeedback(HapticFeedbackConstants.CLOCK_TICK)
            showHint(if (filled) "Zoomed to fill" else "Original")
        }
        hq = HqFrameView(this) { content.scaleX }
        (content as ViewGroup).addView(hq, FrameLayout.LayoutParams(ViewGroup.LayoutParams.MATCH_PARENT, ViewGroup.LayoutParams.MATCH_PARENT))
        zoom.onTransform = { hq.invalidate() }
        loop = AbLoop(content, ::newPlayer, { player }, { player?.let { p -> mediaItem(p.currentMediaItemIndex) } }) { updateGhostButtons(); hqOnStateChange() }

        // Audio-only pane (artwork + titles), below the controller.
        artView = ImageView(this).apply { scaleType = ImageView.ScaleType.CENTER_CROP; setBackgroundColor(0xFF1A1A22.toInt()) }
        audioTitle = TextView(this).apply { setTextColor(Color.WHITE); textSize = 20f; typeface = Typeface.DEFAULT_BOLD; gravity = Gravity.CENTER }
        audioSub = TextView(this).apply { setTextColor(0xFFB0B0BC.toInt()); textSize = 15f; gravity = Gravity.CENTER }
        audioPane = LinearLayout(this).apply {
            orientation = LinearLayout.VERTICAL; gravity = Gravity.CENTER
            setPadding(dp(32), dp(72), dp(32), dp(160))
            val size = (minOf(resources.displayMetrics.widthPixels, resources.displayMetrics.heightPixels) * 0.7f).toInt()
            addView(artView, LinearLayout.LayoutParams(size, size).apply { bottomMargin = dp(24) })
            addView(audioTitle); addView(audioSub)
            visibility = View.GONE
        }
        view.overlayFrameLayout?.addView(audioPane, FrameLayout.LayoutParams(ViewGroup.LayoutParams.MATCH_PARENT, ViewGroup.LayoutParams.MATCH_PARENT))

        seekOverlay = SeekOverlay(this)
        view.overlayFrameLayout?.addView(seekOverlay, FrameLayout.LayoutParams(ViewGroup.LayoutParams.MATCH_PARENT, ViewGroup.LayoutParams.MATCH_PARENT))

        // Top bar: back, title + queue position, quality.
        val back = ImageButton(this).apply {
            setImageResource(androidx.media3.ui.R.drawable.exo_ic_chevron_left)
            setColorFilter(Color.WHITE)
            background = GradientDrawable().apply { shape = GradientDrawable.OVAL; setColor(0x33000000) }
            contentDescription = "Back"
            scaleType = ImageView.ScaleType.FIT_CENTER
            setPadding(dp(8), dp(8), dp(8), dp(8))
            setOnClickListener { close() }
        }
        titleView = TextView(this).apply {
            setTextColor(Color.WHITE); textSize = 16f; typeface = Typeface.DEFAULT_BOLD
            maxLines = 1; ellipsize = TextUtils.TruncateAt.END
        }
        subView = TextView(this).apply {
            setTextColor(0xFFCCCCD6.toInt()); textSize = 12f; maxLines = 1; ellipsize = TextUtils.TruncateAt.END
        }
        val titles = LinearLayout(this).apply {
            orientation = LinearLayout.VERTICAL
            setPadding(dp(12), 0, dp(12), 0)
            addView(titleView); addView(subView)
        }
        qualityBtn = TextView(this).apply {
            text = "Auto"; setTextColor(Color.WHITE); textSize = 13f; typeface = Typeface.DEFAULT_BOLD
            gravity = Gravity.CENTER
            setCompoundDrawablesRelativeWithIntrinsicBounds(androidx.media3.ui.R.drawable.exo_ic_settings, 0, 0, 0)
            compoundDrawablePadding = dp(6)
            compoundDrawablesRelative[0]?.let { it.setTint(Color.WHITE); it.setBounds(0, 0, dp(18), dp(18)) }
            setCompoundDrawablesRelative(compoundDrawablesRelative[0], null, null, null)
            background = GradientDrawable().apply { cornerRadius = 18 * d; setColor(0x55000000) }
            setPadding(dp(12), dp(7), dp(14), dp(7))
            setOnClickListener { showQualityDialog() }
        }
        topBar = LinearLayout(this).apply {
            orientation = LinearLayout.HORIZONTAL; gravity = Gravity.CENTER_VERTICAL
            background = GradientDrawable(GradientDrawable.Orientation.TOP_BOTTOM, intArrayOf(0x99000000.toInt(), 0x00000000))
            setPadding(dp(12), dp(12), dp(12), dp(28))
            addView(back, LinearLayout.LayoutParams(dp(42), dp(42)))
            addView(titles, LinearLayout.LayoutParams(0, ViewGroup.LayoutParams.WRAP_CONTENT, 1f))
            addView(qualityBtn)
            visibility = View.GONE
        }
        hint = TextView(this).apply {
            setTextColor(Color.WHITE); textSize = 14f; typeface = Typeface.DEFAULT_BOLD
            background = GradientDrawable().apply { cornerRadius = 16 * d; setColor(0x99000000.toInt()) }
            setPadding(dp(14), dp(7), dp(14), dp(7)); alpha = 0f
        }
        status = TextView(this).apply { setTextColor(Color.WHITE); gravity = Gravity.CENTER; text = "Loading…" }

        root.addView(view)
        root.addView(status, FrameLayout.LayoutParams(ViewGroup.LayoutParams.WRAP_CONTENT, ViewGroup.LayoutParams.WRAP_CONTENT, Gravity.CENTER))
        root.addView(hint, FrameLayout.LayoutParams(ViewGroup.LayoutParams.WRAP_CONTENT, ViewGroup.LayoutParams.WRAP_CONTENT,
            Gravity.TOP or Gravity.CENTER_HORIZONTAL).apply { topMargin = dp(84) })
        val ghost = { v: View ->
            v.background = GradientDrawable().apply { shape = GradientDrawable.OVAL; setColor(0xFF000000.toInt()); setStroke(dp(1.5), Color.WHITE) }
            v.alpha = 0.2f; v.visibility = View.GONE
        }
        playBtn = ImageButton(this).apply {
            setImageResource(androidx.media3.ui.R.drawable.exo_icon_pause); setColorFilter(Color.WHITE)
            scaleType = ImageView.ScaleType.CENTER_INSIDE; setPadding(dp(10), dp(10), dp(10), dp(10))
            contentDescription = "Play/pause"; ghost(this)
            setOnClickListener {
                val p = player ?: return@setOnClickListener
                if (!loop.togglePlay()) { if (p.playWhenReady) p.pause() else p.play() } // never brings up the controller
                updateGhostButtons()
                hqOnStateChange()
            }
        }
        loopBtn = TextView(this).apply {
            text = "A·B"; setTextColor(Color.WHITE); textSize = 13f; typeface = Typeface.DEFAULT_BOLD; gravity = Gravity.CENTER
            contentDescription = "A-B loop"; ghost(this)
            setOnClickListener { loopTap() }
        }
        root.addView(playBtn, FrameLayout.LayoutParams(dp(48), dp(48), Gravity.BOTTOM or Gravity.END))
        root.addView(loopBtn, FrameLayout.LayoutParams(dp(48), dp(48), Gravity.BOTTOM or Gravity.START))
        hqChip = HqIndicator(this)
        hqChip.onHold = hold@{ down ->
            if (!hq.active) return@hold
            hq.animate().cancel()
            hq.animate().alpha(if (down) 0f else 1f).setDuration(80).start()
        }
        root.addView(hqChip, FrameLayout.LayoutParams(ViewGroup.LayoutParams.WRAP_CONTENT, ViewGroup.LayoutParams.WRAP_CONTENT,
            Gravity.TOP or Gravity.END).apply { topMargin = dp(84); rightMargin = dp(24) })
        levelOverlay = LevelOverlay(this)
        root.addView(levelOverlay, FrameLayout.LayoutParams(ViewGroup.LayoutParams.WRAP_CONTENT, ViewGroup.LayoutParams.WRAP_CONTENT,
            Gravity.START or Gravity.CENTER_VERTICAL).apply { marginStart = dp(48); marginEnd = dp(48) })
        root.addView(topBar, FrameLayout.LayoutParams(ViewGroup.LayoutParams.MATCH_PARENT, ViewGroup.LayoutParams.WRAP_CONTENT, Gravity.TOP))
        ViewCompat.setOnApplyWindowInsetsListener(root) { _, ins ->
            val c = ins.getInsets(WindowInsetsCompat.Type.displayCutout())
            topBar.setPadding(dp(12) + c.left, dp(12) + c.top, dp(12) + c.right, dp(28))
            val g = ins.getInsets(WindowInsetsCompat.Type.systemGestures())
            val bottom = maxOf(dp(24), g.bottom + dp(16))
            (playBtn.layoutParams as FrameLayout.LayoutParams).apply { bottomMargin = bottom; rightMargin = maxOf(dp(24), g.right + c.right + dp(16)) }
            (loopBtn.layoutParams as FrameLayout.LayoutParams).apply { bottomMargin = bottom; leftMargin = maxOf(dp(24), g.left + c.left + dp(16)) }
            playBtn.requestLayout(); loopBtn.requestLayout()
            ins
        }
        view.setControllerVisibilityListener(PlayerView.ControllerVisibilityListener { vis -> fadeTopBar(vis == View.VISIBLE); updateGhostButtons() })
        setContentView(root)
    }

    private fun fadeTopBar(show: Boolean) {
        topBar.animate().cancel()
        if (show && !inPip && queue.isNotEmpty()) {
            if (topBar.visibility != View.VISIBLE) { topBar.alpha = 0f; topBar.visibility = View.VISIBLE }
            topBar.animate().alpha(1f).setDuration(200).start()
        } else if (topBar.visibility == View.VISIBLE) {
            topBar.animate().alpha(0f).setDuration(200).withEndAction { topBar.visibility = View.GONE }.start()
        }
    }

    /** Ghost play/pause + A-B buttons: only while playing with the controller hidden (video, not PiP). */
    private fun updateGhostButtons() {
        val p = player
        val show = p != null && p.playbackState != Player.STATE_ENDED &&
            !view.isControllerFullyVisible && !inPip && !isAudio()
        val v = if (show) View.VISIBLE else View.GONE
        playBtn.visibility = v; loopBtn.visibility = v
        playBtn.setImageResource(if (p != null && loop.effectivePlayWhenReady(p)) androidx.media3.ui.R.drawable.exo_icon_pause else androidx.media3.ui.R.drawable.exo_icon_play)
    }

    private fun loopTap() {
        val p = player ?: return
        val pos = p.currentPosition
        when {
            loopA < 0 -> { loopA = pos; showHint("A set") }
            loopB < 0 -> {
                if (kotlin.math.abs(pos - loopA) < 300) return
                if (pos < loopA) { loopB = loopA; loopA = pos } else loopB = pos
                showHint("Loop A–B"); loop.arm(loopA, loopB)
            }
            else -> { clearLoop(); showHint("Loop cleared") }
        }
        renderLoop()
    }

    private fun clearLoop() {
        loopA = -1; loopB = -1; loop.disarm(); renderLoop()
    }

    private fun renderLoop() {
        loopBtn.text = when { loopA < 0 -> "A·B"; loopB < 0 -> "A"; else -> "A–B" }
        val marks = listOf(loopA, loopB).filter { it >= 0 }
        view.setExtraAdGroupMarkers(if (marks.isEmpty()) null else marks.toLongArray(), if (marks.isEmpty()) null else BooleanArray(marks.size))
    }

    // ---------------------------------------------------------------- HQ pause frame

    private fun pixels(f: MediaFile) = f.video?.let { (it.width ?: 0).toLong() * (it.height ?: 0) } ?: 0L

    /** Called on every play/pause/seek/item change: (re)arm or cancel the HQ still. */
    private fun hqOnStateChange() {
        val p = player
        ui.removeCallbacks(hqStart)
        val paused = p != null && !loop.shadowActive && !loop.frozen && !p.playWhenReady && p.playbackState == Player.STATE_READY
        if (!paused) { dropHq(); return }
        if (hq.active || hqJob?.isActive == true) return
        if (!settings.hqPauseFrame || inPip || isAudio()) return
        ui.postDelayed(hqStart, settings.hqPauseDelayMs.toLong().coerceAtLeast(0))
    }

    private fun dropHq() {
        ui.removeCallbacks(hqStart)
        hqJob?.cancel(); hqJob = null
        hq.drop(); zoom.maxOverride = 0f
        hqChip.hide()
    }

    private fun requestHqFrame() {
        val p = player ?: return
        val i = p.currentMediaItemIndex
        val e = queue.getOrNull(i) ?: return
        val cur = chosen.getOrNull(i) ?: return
        val best = e.files.filter { it.video != null }
            .sortedWith(compareByDescending<MediaFile> { pixels(it) }.thenByDescending { it.original }).firstOrNull() ?: return
        if (pixels(best) <= pixels(cur)) return
        val posMs = shownFrameMs(p)
        android.util.Log.i("HqFrame", "request t=${posMs}ms currentPosition=${p.currentPosition}ms diff=${posMs - p.currentPosition}ms rawPts=$lastFramePtsUs offset=$ptsOffsetUs")
        val url = settings.url("/frame/${best.id}") + "?fmt=jpg&t=" + "%.3f".format(java.util.Locale.US, posMs / 1000.0)
        hqChip.start()
        hqJob = lifecycleScope.launch {
            val file = runCatching {
                kotlinx.coroutines.withTimeout(40_000) {
                    kotlinx.coroutines.withContext(kotlinx.coroutines.Dispatchers.IO) {
                        val c = java.net.URL(url).openConnection() as java.net.HttpURLConnection
                        try {
                            c.connectTimeout = 5000; c.readTimeout = 40_000
                            if (c.responseCode != 200) throw java.io.IOException("HTTP ${c.responseCode}")
                            c.getHeaderField("X-Frame-Pts")?.toDoubleOrNull()?.let { pts ->
                                val fps = best.video?.fps ?: 30.0
                                if (kotlin.math.abs(pts - posMs / 1000.0) > 1.0 / fps + 0.001)
                                    android.util.Log.i("HqFrame", "pts $pts differs from requested ${posMs / 1000.0}")
                            }
                            val out = java.io.File(cacheDir, "hqframe.img")
                            val total = c.contentLengthLong
                            c.inputStream.use { inp -> out.outputStream().use { o ->
                                val buf = ByteArray(64 * 1024); var done = 0L; var lastPost = 0L
                                while (true) {
                                    val n = inp.read(buf); if (n < 0) break
                                    o.write(buf, 0, n); done += n
                                    val now = System.nanoTime()
                                    if (total > 0 && now - lastPost > 50_000_000) {
                                        lastPost = now; val f = done.toFloat() / total
                                        ui.post { hqChip.progress(f) }
                                    }
                                }
                            } }
                            if (total > 0) ui.post { hqChip.progress(1f) }
                            out
                        } finally { c.disconnect() }
                    }
                }
            }.getOrNull()
            val still = player
            if (file == null || still == null || still.playWhenReady || still.currentMediaItemIndex != i) { hqChip.hide(); return@launch }
            hq.load(file) {
                // Allow zooming to native 1:1 of the still (capped at 8x).
                val w = (hq.parent as View).width.coerceAtLeast(1)
                zoom.maxOverride = (hq.srcW / w.toFloat()).coerceIn(4f, 8f)
                hqChip.postDelayed({ if (hq.active) hqChip.success() else hqChip.hide() }, 200) // stays until playback resumes
            }
        }
    }

    private val hideHint = Runnable { hint.animate().alpha(0f).setDuration(250).start() }
    private fun showHint(text: String) {
        ui.removeCallbacks(hideHint)
        hint.text = text; hint.animate().cancel(); hint.animate().alpha(1f).setDuration(120).start()
        ui.postDelayed(hideHint, 1100)
    }

    private fun close() {
        savePos()
        finish()
    }

    // ---------------------------------------------------------------- queue

    private fun load() {
        val id = (intent.getStringExtra(EXTRA_ITEM_ID) ?: intent.getStringExtra("id")) ?: return finish()
        val forcedStart = if (intent.hasExtra(EXTRA_START_INDEX)) intent.getIntExtra(EXTRA_START_INDEX, 0) else null
        lifecycleScope.launch {
            runCatching { PlaybackSource.load(settings, id) }.onSuccess { pb ->
                queue = pb.queue.filter { it.files.isNotEmpty() }
                if (queue.isEmpty()) { status.text = "Nothing playable"; return@onSuccess }
                val si = forcedStart ?: pb.startIndex
                startIndex = pb.queue.getOrNull(si)?.let { s -> queue.indexOf(s).takeIf { it >= 0 } } ?: 0
                chosen = queue.map { pickFor(it) }.toMutableList()
                if (forcedStart == null) resumePart()
                status.visibility = View.GONE
                if (lifecycle.currentState.isAtLeast(Lifecycle.State.STARTED) && player == null) build()
            }.onFailure { status.text = "Can't load queue: ${it.message}" }
        }
    }

    private fun currentIndex() = player?.currentMediaItemIndex ?: startIndex
    private fun currentFiles() = queue.getOrNull(currentIndex())?.files ?: emptyList()

    /** Best playable file, preferring the main version (no edition) so Auto never jumps to another cut. */
    private fun pickFor(e: PEntry, h: Int = maxH): MediaFile? {
        val main = e.files.filter { e.edition(it.id) == null && Quality.playable(it) }
        return Quality.pick(main.ifEmpty { e.files }, h)
    }
    private fun isAudio() = chosen.getOrNull(currentIndex())?.let { it.video == null } ?: false

    private fun subtitleMime(codec: String?, url: String): String = when {
        codec?.contains("vtt") == true || url.endsWith(".vtt") -> MimeTypes.TEXT_VTT
        codec in listOf("ass", "ssa") || url.endsWith(".ass") || url.endsWith(".ssa") -> MimeTypes.TEXT_SSA
        else -> MimeTypes.APPLICATION_SUBRIP
    }

    private fun mediaItem(i: Int): MediaItem {
        val e = queue[i]; val f = chosen[i] ?: e.files.first()
        val art = ImagePolicy.card(e.item.imageValues)?.let { settings.asset(it, 512) }
        val subs = f.subtitles.mapNotNull { s ->
            val u = settings.url(s.external) ?: return@mapNotNull null
            MediaItem.SubtitleConfiguration.Builder(Uri.parse(u))
                .setMimeType(subtitleMime(s.codec, u)).setLanguage(s.language).setLabel(s.title ?: s.language)
                .setSelectionFlags(if (s.forced) C.SELECTION_FLAG_FORCED else 0).build()
        }
        return MediaItem.Builder().setMediaId("${e.item.id}/${e.part ?: 0}/${f.id}").setUri(settings.url(f.streamUrl))
            .setSubtitleConfigurations(subs)
            .setMediaMetadata(MediaMetadata.Builder().setTitle(e.item.title).setArtist(e.item.subtitle)
                .setArtworkUri(art?.let(Uri::parse)).build())
            .build()
    }

    private fun fileTitle(f: MediaFile): String {
        val v = f.video ?: return f.label.ifBlank { f.audio.firstOrNull()?.codec?.uppercase() ?: "Audio" }
        return if (f.h > 0) "${f.h}p · ${v.codec.uppercase()}" else f.label
    }

    private fun onEntryChanged() {
        val i = currentIndex(); val e = queue.getOrNull(i) ?: return
        val f = chosen[i]
        val audio = f?.video == null
        val portrait = f?.video?.let { (it.height ?: 0) > (it.width ?: 0) } ?: false
        requestedOrientation = when {
            audio || portrait -> ActivityInfo.SCREEN_ORIENTATION_UNSPECIFIED
            else -> ActivityInfo.SCREEN_ORIENTATION_SENSOR_LANDSCAPE
        }
        audioPane.visibility = if (audio) View.VISIBLE else View.GONE
        view.controllerShowTimeoutMs = if (audio) 0 else 4000
        view.controllerHideOnTouch = !audio
        if (audio) zoom.reset()
        audioTitle.text = e.item.title; audioSub.text = e.item.subtitle ?: ""
        artView.setImageDrawable(null)
        if (audio) lifecycleScope.launch {
            val h = ImageResolver.card(e.item) ?: return@launch
            if (queue.getOrNull(currentIndex())?.item?.id != e.item.id) return@launch
            val u = settings.asset(h, 768)
            Coil.imageLoader(this@PlayerActivity).enqueue(ImageRequest.Builder(this@PlayerActivity).data(u).memoryCacheKey(u).target(artView).build())
        }
        titleView.text = e.item.title
        subView.text = listOfNotNull(
            partLabel(i) ?: itemPosition(i),
            e.item.subtitle?.takeIf { it.isNotBlank() },
        ).joinToString("  ·  ")
        subView.visibility = if (subView.text.isNullOrEmpty()) View.GONE else View.VISIBLE
        qualityBtn.text = f?.let { (if (i in manual) "" else "Auto · ") + (if (it.h > 0) "${it.h}p" else it.label.ifBlank { "Audio" }) } ?: "Auto"
        qualityBtn.visibility = if (e.files.size > 1) View.VISIBLE else View.GONE
        if (audio) view.showController()
        if (::playBtn.isInitialized) updateGhostButtons()
    }

    // ---------------------------------------------------------------- quality

    /** Swap the current entry's file in place, keeping position and play state. */
    private fun switchTo(f: MediaFile) {
        val p = player ?: return
        val i = p.currentMediaItemIndex
        val pos = p.currentPosition.coerceAtLeast(0); val pwr = p.playWhenReady
        dropHq()
        val rearm = loop.armed
        loop.disarm()
        calibrate(pos)
        chosen[i] = f
        p.replaceMediaItem(i, mediaItem(i))
        if (p.currentMediaItemIndex != i || p.currentPosition != pos) p.seekTo(i, pos)
        p.playWhenReady = pwr
        if (p.playbackState == Player.STATE_IDLE) p.prepare()
        if (rearm) loop.arm(loopA, loopB)
        onEntryChanged()
    }

    private fun fileRow(f: MediaFile): String {
        val v = f.video
        val parts = mutableListOf<String>()
        if (v != null) {
            parts += v.codec.uppercase()
            if ((v.width ?: 0) > 0 && (v.height ?: 0) > 0) parts += "${v.width}×${v.height}"
            v.fps?.takeIf { it > 0 }?.let { parts += "%.3g fps".format(it) }
            v.hdr?.let { parts += it.uppercase() }
        } else f.audio.firstOrNull()?.codec?.let { parts += it.uppercase() }
        f.bitrate?.takeIf { it > 0 }?.let { parts += if (it >= 1_000_000) "%.1f Mbps".format(it / 1e6) else "${it / 1000} kbps" }
        val tag = when (Quality.support(f)) {
            Quality.Support.HARDWARE -> if (v != null) "HW" else null
            Quality.Support.SOFTWARE -> "SW"
            Quality.Support.NONE -> "unsupported"
        }
        tag?.let { parts += it }
        val ed = queue.getOrNull(currentIndex())?.edition(f.id)
        val base = f.label.ifBlank { fileTitle(f) }
        val head = (if (ed != null && !base.startsWith(ed)) "$ed · $base" else base) +
            when {
                ed != null || !f.original -> ""
                queue.getOrNull(currentIndex())?.editions?.values?.any { it != null } == true -> "  (main version)"
                else -> "  (original)"
            }
        return head + "\n" + parts.joinToString(" · ")
    }

    private fun showQualityDialog() {
        val i = currentIndex(); val raw = currentFiles(); if (raw.isEmpty()) return
        val autoF = queue.getOrNull(i)?.let { pickFor(it, 0) }
        val cur = chosen.getOrNull(i)
        val isManual = i in manual
        data class Row(val text: String, val enabled: Boolean, val checked: Boolean)
        val rows = mutableListOf(Row("Auto" + (autoF?.let { "\n" + fileTitle(it) } ?: ""), true, !isManual))
        // Group versions: main version first, then each edition; within a group the server's height order.
        val pe = queue[i]
        val files = raw.sortedBy { f -> pe.edition(f.id)?.let { "1$it" } ?: "0" }
        files.forEach { f -> rows += Row(fileRow(f), Quality.playable(f), isManual && f.id == cur?.id) }
        val adapter = object : ArrayAdapter<Row>(this, android.R.layout.simple_list_item_single_choice, rows) {
            override fun isEnabled(position: Int) = rows[position].enabled
            override fun areAllItemsEnabled() = false
            override fun getView(position: Int, convertView: View?, parent: ViewGroup): View {
                val r = rows[position]
                val lines = r.text.split("\n", limit = 2)
                val row = LinearLayout(context).apply {
                    orientation = LinearLayout.HORIZONTAL; gravity = Gravity.CENTER_VERTICAL
                    setPadding(dp(24), dp(12), dp(16), dp(12)); alpha = if (r.enabled) 1f else 0.4f
                    layoutParams = ViewGroup.LayoutParams(ViewGroup.LayoutParams.MATCH_PARENT, ViewGroup.LayoutParams.WRAP_CONTENT)
                }
                val texts = LinearLayout(context).apply { orientation = LinearLayout.VERTICAL }
                texts.addView(TextView(context).apply { text = lines[0]; textSize = 16f; setTextColor(Color.WHITE) })
                lines.getOrNull(1)?.let { texts.addView(TextView(context).apply { text = it; textSize = 13f; setTextColor(0xFFB0B0BC.toInt()) }) }
                row.addView(texts, LinearLayout.LayoutParams(0, ViewGroup.LayoutParams.WRAP_CONTENT, 1f))
                row.addView(android.widget.RadioButton(context).apply { isChecked = r.checked; isClickable = false; isFocusable = false })
                return row
            }
        }
        AlertDialog.Builder(this, android.R.style.Theme_DeviceDefault_Dialog_Alert)
            .setTitle("Quality")
            .setAdapter(adapter) { dlg, which ->
                if (which == 0) {
                    sessionMaxH = 0; manual.remove(i)
                    autoF?.let { if (it.id != cur?.id) switchTo(it) else onEntryChanged() }
                } else {
                    val f = files[which - 1]
                    manual += i
                    sessionMaxH = if (f.id == autoF?.id) 0 else f.h
                    if (f.id != cur?.id) switchTo(f) else onEntryChanged()
                }
                // Apply the session choice to queue entries that haven't been touched explicitly.
                queue.indices.forEach { j ->
                    if (j != i && j !in manual) {
                        val nf = pickFor(queue[j])
                        if (nf?.id != chosen[j]?.id) { chosen[j] = nf; player?.replaceMediaItem(j, mediaItem(j)) }
                    }
                }
                dlg.dismiss()
            }
            .show()
    }

    // ---------------------------------------------------------------- gestures

    override fun zoomEnabled() = !inPip && player != null && !isAudio()
    override fun onSingleTap() {
        if (inPip) return
        if (view.isControllerFullyVisible) { if (view.controllerHideOnTouch) view.hideController() } else view.showController()
    }
    override fun inSeekMode() = System.currentTimeMillis() - lastSeekTapAt < 700
    override fun onDoubleTap(x: Float, y: Float) { seekTap(x, y) }
    override fun onSeekTap(x: Float, y: Float): Boolean { seekTap(x, y); return true }

    private fun seekTap(x: Float, y: Float) {
        val p = player ?: return
        if (inPip) return
        loop.leaveShadow()
        val w = root.width.toFloat()
        val side = if (x < w / 2) -1 else 1
        if (!inSeekMode() || seekOverlay.activeSide != side) seekBase = p.currentPosition
        lastSeekTapAt = System.currentTimeMillis()
        val secs = seekOverlay.tap(side, x, y, SEEK_STEP_S)
        val dur = p.duration.takeIf { it != C.TIME_UNSET && it > 0 }
        var target = seekBase + side * secs * 1000L
        target = target.coerceAtLeast(0)
        if (dur != null) target = target.coerceAtMost(dur - 500)
        p.seekTo(target)
    }

    override fun onScaleBegin() { zoom.begin() }
    override fun onScale(factor: Float, fx: Float, fy: Float, dx: Float, dy: Float) = zoom.scaleBy(factor, fx, fy, dx, dy)
    override fun onScaleEnd() = zoom.end()

    // Vertical level swipes: left third = brightness (this window), right third = media volume.
    private enum class DragMode { NONE, PAN, VOLUME, BRIGHTNESS, IGNORE }
    private var dragMode = DragMode.NONE
    private var dragStartLevel = 0f
    private val audioManager by lazy { getSystemService(Context.AUDIO_SERVICE) as android.media.AudioManager }

    override fun onDrag(down: MotionEvent, e: MotionEvent, dx: Float, dy: Float): Boolean {
        if (dragMode == DragMode.NONE) dragMode = decideDrag(down, e)
        return when (dragMode) {
            DragMode.PAN -> zoom.pan(dx, dy)
            DragMode.VOLUME, DragMode.BRIGHTNESS -> {
                val delta = (down.y - e.y) / root.height.coerceAtLeast(1)
                val v = (dragStartLevel + delta).coerceIn(0f, 1f)
                if (dragMode == DragMode.VOLUME) setVolume(v) else setBrightness(v)
                true
            }
            else -> false
        }
    }

    private fun decideDrag(down: MotionEvent, e: MotionEvent): DragMode {
        if (inPip) return DragMode.IGNORE
        if (zoom.beyondFill) return DragMode.PAN
        val adx = kotlin.math.abs(e.x - down.x); val ady = kotlin.math.abs(e.y - down.y)
        if (ady <= adx) return DragMode.IGNORE
        // Leave the system gesture zones (back swipe from the edges, home/notification bars) alone.
        val g = ViewCompat.getRootWindowInsets(root)?.getInsets(WindowInsetsCompat.Type.systemGestures())
        val w = root.width; val h = root.height
        if (g != null && (down.x < g.left || down.x > w - g.right || down.y < g.top || down.y > h - g.bottom)) return DragMode.IGNORE
        return when {
            down.x < w / 3f -> { dragStartLevel = currentBrightness(); DragMode.BRIGHTNESS }
            down.x > w * 2f / 3f -> {
                val max = audioManager.getStreamMaxVolume(android.media.AudioManager.STREAM_MUSIC).coerceAtLeast(1)
                dragStartLevel = audioManager.getStreamVolume(android.media.AudioManager.STREAM_MUSIC) / max.toFloat()
                DragMode.VOLUME
            }
            else -> DragMode.IGNORE
        }
    }

    override fun onDragEnd() {
        if (dragMode == DragMode.VOLUME || dragMode == DragMode.BRIGHTNESS) levelOverlay.release()
        dragMode = DragMode.NONE
    }

    private fun setVolume(v: Float) {
        val max = audioManager.getStreamMaxVolume(android.media.AudioManager.STREAM_MUSIC)
        val idx = kotlin.math.round(v * max).toInt()
        if (idx != audioManager.getStreamVolume(android.media.AudioManager.STREAM_MUSIC))
            runCatching { audioManager.setStreamVolume(android.media.AudioManager.STREAM_MUSIC, idx, 0) }
        showLevel(LevelOverlay.Kind.VOLUME, v, Gravity.START)
    }

    private fun currentBrightness(): Float {
        val b = window.attributes.screenBrightness
        if (b >= 0f) return b
        val sys = runCatching { android.provider.Settings.System.getInt(contentResolver, android.provider.Settings.System.SCREEN_BRIGHTNESS) }.getOrDefault(128)
        return (sys / 255f).coerceIn(0.01f, 1f)
    }

    private fun setBrightness(v: Float) {
        val b = v.coerceIn(0.01f, 1f)
        window.attributes = window.attributes.apply { screenBrightness = b }
        showLevel(LevelOverlay.Kind.BRIGHTNESS, b, Gravity.END)
    }

    /** Show the level pill on the opposite side of the finger so it stays visible. */
    private fun showLevel(kind: LevelOverlay.Kind, v: Float, side: Int) {
        val lp = levelOverlay.layoutParams as FrameLayout.LayoutParams
        val g = side or Gravity.CENTER_VERTICAL
        if (lp.gravity != g) { lp.gravity = g; levelOverlay.layoutParams = lp }
        levelOverlay.show(kind, v)
    }

    // ---------------------------------------------------------------- lifecycle

    private fun partKey(itemId: String) = "$itemId#part"
    private fun partsOf(i: Int): List<Int> {
        val e = queue.getOrNull(i) ?: return emptyList()
        if (e.part == null) return emptyList()
        return queue.indices.filter { queue[it].item.id == e.item.id && queue[it].part != null }
    }
    private fun partLabel(i: Int): String? {
        val ps = partsOf(i); if (ps.size < 2) return null
        return "Part ${ps.indexOf(i) + 1}/${ps.size}"
    }
    /** Position among distinct items (stack parts count once). */
    private fun itemPosition(i: Int): String? {
        val ids = queue.map { it.item.id }.distinct()
        if (ids.size < 2) return null
        return "${ids.indexOf(queue[i].item.id) + 1}/${ids.size}"
    }

    /** Resume a stacked movie in the part that was playing last time. */
    private fun resumePart() {
        val e = queue.getOrNull(startIndex) ?: return
        if (e.part == null) return
        val saved = settings.position(partKey(e.item.id)).toInt()
        if (saved <= 0) return
        queue.indices.firstOrNull { queue[it].item.id == e.item.id && queue[it].part == saved }?.let { startIndex = it }
    }

    private fun savePos() {
        val p = player ?: return
        val i = p.currentMediaItemIndex
        val e = queue.getOrNull(i) ?: return
        if (p.playbackState == Player.STATE_ENDED) return
        val dur = p.duration
        val nearEnd = dur > 0 && p.currentPosition > dur - 10_000
        if (e.part != null) {
            val ps = partsOf(i); val last = ps.lastOrNull() == i
            when {
                nearEnd && last -> { settings.savePosition(e.item.id, 0); settings.savePosition(partKey(e.item.id), 0) }
                nearEnd -> { settings.savePosition(e.item.id, 0); settings.savePosition(partKey(e.item.id), (queue[ps[ps.indexOf(i) + 1]].part ?: 0).toLong()) }
                else -> { settings.savePosition(e.item.id, p.currentPosition); settings.savePosition(partKey(e.item.id), e.part.toLong()) }
            }
            return
        }
        // Near the end counts as watched.
        settings.savePosition(e.item.id, if (nearEnd) 0 else p.currentPosition)
    }

    private fun newPlayer(): ExoPlayer {
        val renderers = DefaultRenderersFactory(this)
            .setExtensionRendererMode(DefaultRenderersFactory.EXTENSION_RENDERER_MODE_ON)
            .setEnableDecoderFallback(true)
        val http = DefaultHttpDataSource.Factory()
            .setConnectTimeoutMs(10_000).setReadTimeoutMs(30_000).setAllowCrossProtocolRedirects(true)
        // Big buffers: high-bitrate originals over LAN.
        val load = DefaultLoadControl.Builder()
            .setBufferDurationsMs(15_000, 120_000, 1_500, 3_000)
            .setTargetBufferBytes(256 * 1024 * 1024)
            .setPrioritizeTimeOverSizeThresholds(false)
            .setBackBuffer(60_000, true) // keep already-downloaded data so seeking back doesn't refetch
            .build()
        val selector = DefaultTrackSelector(this).apply {
            setParameters(buildUponParameters().setExceedRendererCapabilitiesIfNecessary(true)
                .setExceedVideoConstraintsIfNecessary(true))
        }
        return ExoPlayer.Builder(this, renderers)
            .setMediaSourceFactory(DefaultMediaSourceFactory(http))
            .setLoadControl(load)
            .setTrackSelector(selector)
            .setSeekBackIncrementMs(10_000).setSeekForwardIncrementMs(10_000)
            .setHandleAudioBecomingNoisy(true)
            .build()
    }

    private fun build() {
        if (queue.isEmpty()) return
        val p = newPlayer()
        p.setAudioAttributes(AudioAttributes.Builder().setUsage(C.USAGE_MEDIA).setContentType(C.AUDIO_CONTENT_TYPE_MOVIE).build(), true)
        val idx = if (restoreIndex in queue.indices) restoreIndex else startIndex
        val pos = if (restoreIndex in queue.indices) restorePos else queue[idx].let { e ->
            val savedPart = settings.position(partKey(e.item.id)).toInt()
            if (e.part != null && savedPart != 0 && savedPart != e.part) 0L else settings.position(e.item.id)
        }
        p.setMediaItems(queue.indices.map(::mediaItem), idx, pos)
        calibrate(pos.coerceAtLeast(0))
        p.setVideoFrameMetadataListener { ptsUs, _, _, _ ->
            val t = calibrateTargetUs
            if (t != Long.MIN_VALUE) { ptsOffsetUs = ptsUs - t; calibrateTargetUs = Long.MIN_VALUE }
            lastFramePtsUs = ptsUs
        }
        p.addListener(object : Player.Listener {
            override fun onPlayerError(error: PlaybackException) {
                val i = p.currentMediaItemIndex
                val cur = chosen.getOrNull(i)
                if (cur != null && i !in fellBack) {
                    fellBack += i
                    val ed = queue[i].edition(cur.id)
                    val next = Quality.fallback(queue[i].files.filter { queue[i].edition(it.id) == ed }, cur, setOf(cur.id))
                        ?: Quality.fallback(queue[i].files, cur, setOf(cur.id))
                    if (next != null) {
                        Toast.makeText(this@PlayerActivity, "Couldn't play ${fileTitle(cur)}, switching to ${fileTitle(next)}", Toast.LENGTH_LONG).show()
                        manual.remove(i)
                        switchTo(next); return
                    }
                }
                Toast.makeText(this@PlayerActivity, "Playback error: ${error.errorCodeName}", Toast.LENGTH_LONG).show()
            }
            override fun onMediaItemTransition(mediaItem: MediaItem?, reason: Int) {
                if (reason == Player.MEDIA_ITEM_TRANSITION_REASON_AUTO) {
                    val prev = queue.getOrNull(p.currentMediaItemIndex - 1)
                    val cur = queue.getOrNull(p.currentMediaItemIndex)
                    if (prev != null) {
                        if (prev.part != null && cur?.item?.id == prev.item.id) {
                            settings.savePosition(prev.item.id, 0); settings.savePosition(partKey(prev.item.id), (cur.part ?: 0).toLong())
                        } else { settings.savePosition(prev.item.id, 0); if (prev.part != null) settings.savePosition(partKey(prev.item.id), 0) }
                    }
                }
                if (reason != Player.MEDIA_ITEM_TRANSITION_REASON_PLAYLIST_CHANGED) { seekOverlay.clear(); clearLoop() }
                dropHq()
                onEntryChanged()
            }
            override fun onPlayWhenReadyChanged(playWhenReady: Boolean, reason: Int) { updateGhostButtons(); hqOnStateChange() }
            override fun onPositionDiscontinuity(old: Player.PositionInfo, new: Player.PositionInfo, reason: Int) {
                if (reason == Player.DISCONTINUITY_REASON_SEEK || reason == Player.DISCONTINUITY_REASON_AUTO_TRANSITION ||
                    reason == Player.DISCONTINUITY_REASON_SEEK_ADJUSTMENT || reason == Player.DISCONTINUITY_REASON_REMOVE) calibrate(new.positionMs)
                dropHq(); hqOnStateChange()
            }
            override fun onPlaybackStateChanged(state: Int) {
                updateGhostButtons()
                hqOnStateChange()
                if (state == Player.STATE_ENDED) {
                    queue.getOrNull(p.currentMediaItemIndex)?.let { settings.savePosition(it.item.id, 0); settings.savePosition(partKey(it.item.id), 0) }
                    finish()
                }
            }
        })
        p.playWhenReady = true
        p.prepare()
        view.player = loop.wrap(p)
        if (!inPip) view.useController = true
        player = p
        onEntryChanged()
        updateGhostButtons()
        if (loopB > loopA && loopA >= 0) loop.arm(loopA, loopB)
        ui.postDelayed(saveTick, 5000)
    }

    private fun release() {
        ui.removeCallbacks(saveTick)
        loop.disarm()
        dropHq()
        player?.let {
            savePos()
            restoreIndex = it.currentMediaItemIndex; restorePos = it.currentPosition
            it.release()
        }
        player = null
    }

    override fun onSaveInstanceState(outState: Bundle) {
        super.onSaveInstanceState(outState)
        player?.let { outState.putInt("index", it.currentMediaItemIndex); outState.putLong("pos", it.currentPosition) }
            ?: run { outState.putInt("index", restoreIndex); outState.putLong("pos", restorePos) }
    }

    override fun onDestroy() { super.onDestroy(); hq.release() }

    override fun onStart() { super.onStart(); if (player == null) build() }
    override fun onStop() { super.onStop(); release() }

    private fun pipParams(): PictureInPictureParams {
        val f = player?.videoFormat
        val ratio = if (f != null && f.width > 0 && f.height > 0) {
            val r = f.width.toFloat() / f.height
            when { r > 2.39f -> Rational(239, 100); r < 0.42f -> Rational(42, 100); else -> Rational(f.width, f.height) }
        } else Rational(16, 9)
        return PictureInPictureParams.Builder().setAspectRatio(ratio).build()
    }

    override fun onUserLeaveHint() {
        super.onUserLeaveHint()
        if (player?.isPlaying == true && !isAudio()) runCatching { enterPictureInPictureMode(pipParams()) }
    }

    override fun onPictureInPictureModeChanged(isInPip: Boolean, newConfig: android.content.res.Configuration) {
        super.onPictureInPictureModeChanged(isInPip, newConfig)
        inPip = isInPip
        zoom.enabled = !isInPip
        if (isInPip) { view.hideController(); fadeTopBar(false); seekOverlay.clear(); onDragEnd(); dropHq() }
        view.useController = !isInPip
        updateGhostButtons()
        // PiP window dismissed (not expanded): the activity is already stopped, so close it.
        if (!isInPip && !lifecycle.currentState.isAtLeast(Lifecycle.State.STARTED)) finish()
    }
}
