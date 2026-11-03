package dev.sortedcord.mediadir.domain

import android.media.MediaCodecList
import android.os.Build

/** Device decode capability checks and MediaFile selection (no transcoding: we pick among the server's files). */
object Quality {
    enum class Support { NONE, SOFTWARE, HARDWARE }
    private val cache = HashMap<String, Support>()
    private val codecs by lazy { MediaCodecList(MediaCodecList.REGULAR_CODECS).codecInfos.filter { !it.isEncoder } }

    fun mime(codec: String): String? = when (codec.lowercase()) {
        "av1", "av01" -> "video/av01"
        "hevc", "h265", "hvc1", "hev1" -> "video/hevc"
        "vp9" -> "video/x-vnd.on2.vp9"
        "vp8" -> "video/x-vnd.on2.vp8"
        "h264", "avc", "avc1" -> "video/avc"
        "mpeg4" -> "video/mp4v-es"
        "mpeg2video", "mpeg2" -> "video/mpeg2"
        "h263" -> "video/3gpp"
        else -> null
    }

    fun support(f: MediaFile): Support {
        val v = f.video ?: return Support.HARDWARE // audio: platform or bundled FFmpeg decoders
        val m = mime(v.codec) ?: return Support.SOFTWARE // unknown: let the player try
        val w = v.width; val h = v.height
        val key = "$m:${w}x$h"
        return synchronized(cache) {
            cache.getOrPut(key) {
                var best = Support.NONE
                for (c in codecs) {
                    if (c.supportedTypes.none { it.equals(m, true) }) continue
                    val vc = runCatching { c.getCapabilitiesForType(m).videoCapabilities }.getOrNull() ?: continue
                    val ok = w == null || h == null || w <= 0 || h <= 0 ||
                        runCatching { vc.isSizeSupported(w, h) || vc.isSizeSupported(h, w) }.getOrDefault(false)
                    if (!ok) continue
                    val hw = if (Build.VERSION.SDK_INT >= 29) c.isHardwareAccelerated
                        else !c.name.startsWith("OMX.google.") && !c.name.startsWith("c2.android.")
                    if (hw) { best = Support.HARDWARE; break } else best = Support.SOFTWARE
                }
                best
            }
        }
    }

    fun playable(f: MediaFile) = support(f) != Support.NONE

    fun supportLabel(f: MediaFile) = when (support(f)) {
        Support.HARDWARE -> if (f.video == null) "" else "hardware"
        Support.SOFTWARE -> "software decode"
        Support.NONE -> "not supported on this device"
    }

    private val order = compareByDescending<MediaFile> { it.h }
        .thenByDescending { support(it) == Support.HARDWARE }
        .thenByDescending { it.original }

    /** Highest playable file (hardware preferred on ties), honouring the max-height preference (0 = auto). */
    fun pick(files: List<MediaFile>, maxH: Int = 0): MediaFile? {
        val ok = files.filter { playable(it) }.sortedWith(order)
        if (ok.isEmpty()) return files.firstOrNull()
        if (maxH <= 0) return ok.first()
        return ok.firstOrNull { it.h <= maxH } ?: ok.last()
    }

    /** Next playable file at or below [cur]'s height, used when playback of [cur] fails. */
    fun fallback(files: List<MediaFile>, cur: MediaFile, tried: Set<String>): MediaFile? =
        files.filter { it.id != cur.id && it.id !in tried && playable(it) && it.h <= cur.h }.sortedWith(order).firstOrNull()
}
