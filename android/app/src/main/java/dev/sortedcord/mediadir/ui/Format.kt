package dev.sortedcord.mediadir.ui

fun Double.fmtDuration(): String {
    val s = toLong(); val h = s / 3600; val m = (s % 3600) / 60; val sec = s % 60
    return if (h > 0) "%d:%02d:%02d".format(h, m, sec) else "%d:%02d".format(m, sec)
}

fun Long.fmtSize(): String = when {
    this > 1L shl 30 -> "%.2f GB".format(this / (1L shl 30).toDouble())
    else -> "%.0f MB".format(this / (1L shl 20).toDouble())
}

fun String.titleCase() = replace('_', ' ').replaceFirstChar { it.uppercase() }
