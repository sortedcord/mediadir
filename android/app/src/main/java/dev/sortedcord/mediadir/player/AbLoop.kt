package dev.sortedcord.mediadir.player

import android.os.Handler
import android.os.Looper
import android.view.TextureView
import android.view.ViewGroup
import android.widget.FrameLayout
import androidx.annotation.OptIn
import androidx.media3.common.ForwardingPlayer
import androidx.media3.common.MediaItem
import androidx.media3.common.Player
import androidx.media3.common.util.UnstableApi
import androidx.media3.exoplayer.ExoPlayer
import kotlin.math.abs

/**
 * Gapless A–B loop.
 *
 * A plain `seekTo(A)` at B is slow: the decoder must restart from the keyframe before A and decode
 * forward (keyframes are ~4–7 s apart here, and AV1 is decoded in software), which shows up as a
 * multi-second stall every iteration.
 *
 * Instead two players take turns ("double buffering"): while one plays A→B, the other is parked,
 * paused and fully decoded at A, with its frame already on its surface. At B they swap instantly
 * and the one that just finished re-parks at A in the background, with a whole loop iteration to
 * get ready. The main player renders to PlayerView's surface; the shadow player renders to a
 * TextureView stacked above it inside the content frame, so zoom/pan transforms apply to both.
 * If the waiting player is not ready in time it falls back to a normal seek.
 */
@OptIn(UnstableApi::class)
class AbLoop(
    private val contentFrame: ViewGroup,
    private val newPlayer: () -> ExoPlayer,
    private val main: () -> ExoPlayer?,
    private val currentItem: () -> MediaItem?,
    private val onSwap: () -> Unit,
) {
    private val ui = Handler(Looper.getMainLooper())
    private var shadow: ExoPlayer? = null
    private var texture: TextureView? = null
    private var a = -1L
    private var b = -1L

    /** True while the shadow player is the one being watched. */
    var shadowActive = false
        private set

    val armed get() = a >= 0 && b > a

    private val tick = object : Runnable {
        override fun run() { step(); if (armed) ui.postDelayed(this, 10) }
    }

    fun arm(aMs: Long, bMs: Long) {
        disarm()
        a = aMs; b = bMs
        val tv = TextureView(contentFrame.context).apply { alpha = 0f }
        // Below the HQ still overlay (last child) but above the video surface.
        contentFrame.addView(tv, (contentFrame.childCount - 1).coerceAtLeast(1),
            FrameLayout.LayoutParams(ViewGroup.LayoutParams.MATCH_PARENT, ViewGroup.LayoutParams.MATCH_PARENT))
        texture = tv
        val item = currentItem() ?: return
        shadow = newPlayer().apply {
            volume = 0f
            setVideoTextureView(tv)
            setMediaItem(item, a)
            playWhenReady = false
            prepare()
        }
        ui.post(tick)
    }

    /** Stop looping. Hands playback back to the main player at the shadow's position if needed. */
    fun disarm() {
        ui.removeCallbacks(tick)
        if (waitingSince != 0L) { waitingSince = 0L; (if (shadowActive) shadow else main())?.play() }
        val m = main(); val s = shadow
        if (shadowActive && m != null && s != null) {
            m.seekTo(s.currentPosition); m.volume = 1f; m.playWhenReady = s.playWhenReady
        }
        m?.volume = 1f
        shadowActive = false
        s?.release(); shadow = null
        texture?.let { contentFrame.removeView(it) }; texture = null
        a = -1; b = -1
    }

    private fun parkedAtA(p: ExoPlayer) =
        p.playbackState == Player.STATE_READY && !p.playWhenReady && abs(p.currentPosition - a) < 40

    /** When the waiting player isn't parked at A yet: freeze on B's frame and wait for it (it has already done part of its decode). */
    private var waitingSince = 0L

    private fun step() {
        val m = main() ?: return
        val s = shadow ?: return
        val (playing, parked) = if (shadowActive) s to m else m to s
        if (waitingSince == 0L) {
            if (!playing.playWhenReady || playing.currentPosition < b - 12) return
        }
        if (parkedAtA(parked)) {
            waitingSince = 0L
            if (!shadowActive) texture?.alpha = 1f
            parked.volume = 1f; parked.play()
            if (shadowActive) texture?.alpha = 0f
            playing.volume = 0f; playing.pause(); playing.seekTo(a) // re-park during the next iteration
            shadowActive = !shadowActive
            onSwap()
        } else if (waitingSince == 0L) {
            waitingSince = System.currentTimeMillis()
            playing.pause() // hold B's frame; the other player is still decoding up to A
            if (parked.playbackState == Player.STATE_IDLE || parked.playerError != null) { parked.prepare(); parked.seekTo(a) }
        } else if (System.currentTimeMillis() - waitingSince > 8000) {
            // Parked player is stuck: fall back to a plain seek on the visible player.
            waitingSince = 0L
            playing.seekTo(a); playing.play()
        }
    }

    /** If the shadow player is on screen, hand playback back to the main player (before a user seek etc.). */
    fun leaveShadow() {
        if (waitingSince != 0L) { waitingSince = 0L; (if (shadowActive) shadow else main())?.play() }
        val m = main() ?: return; val s = shadow ?: return
        if (!shadowActive) return
        m.seekTo(s.currentPosition); m.volume = 1f; m.playWhenReady = s.playWhenReady
        texture?.alpha = 0f
        s.volume = 0f; s.pause(); s.seekTo(a)
        shadowActive = false
        onSwap()
    }

    fun release() = disarm()

    /** What PlayerView / the controller should see: positions and play state of whichever player is on screen. */
    fun wrap(p: ExoPlayer): Player = object : ForwardingPlayer(p) {
        private fun live(): Player? = if (shadowActive) shadow else null
        override fun getCurrentPosition() = live()?.currentPosition ?: super.getCurrentPosition()
        override fun getContentPosition() = live()?.contentPosition ?: super.getContentPosition()
        override fun getBufferedPosition() = live()?.bufferedPosition ?: super.getBufferedPosition()
        override fun getPlayWhenReady() = frozen || (live()?.playWhenReady ?: super.getPlayWhenReady())
        override fun isPlaying() = live()?.isPlaying ?: super.isPlaying()
        override fun getPlaybackState() = live()?.playbackState ?: super.getPlaybackState()
        override fun play() { live()?.play() ?: super.play() }
        override fun pause() { if (frozen) { waitingSince = 0L; return }; live()?.pause() ?: super.pause() }
        override fun setPlayWhenReady(v: Boolean) { live()?.setPlayWhenReady(v) ?: super.setPlayWhenReady(v) }
        override fun seekTo(positionMs: Long) { leaveShadow(); super.seekTo(positionMs) }
        override fun seekTo(index: Int, positionMs: Long) { leaveShadow(); super.seekTo(index, positionMs) }
        override fun seekBack() { leaveShadow(); super.seekBack() }
        override fun seekForward() { leaveShadow(); super.seekForward() }
    }

    /** Toggle play/pause on whichever player is on screen. */
    fun togglePlay(): Boolean {
        if (waitingSince != 0L) { waitingSince = 0L; return true } // user pause while held at B: stay paused
        val s = shadow
        if (!shadowActive || s == null) return false
        if (s.playWhenReady) s.pause() else s.play()
        return true
    }

    /** Holding B's frame while the other player finishes parking: counts as "playing" for the UI. */
    val frozen get() = waitingSince != 0L

    fun effectivePlayWhenReady(m: Player) = if (frozen) true else if (shadowActive) shadow?.playWhenReady ?: false else m.playWhenReady
}
