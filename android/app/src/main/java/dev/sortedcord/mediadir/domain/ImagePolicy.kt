package dev.sortedcord.mediadir.domain

/**
 * The single place that decides which image is shown where. The server never picks or falls back between images;
 * it only stores `image` field values ({"hash"} or an ordered list of them). Change the preference lists here.
 *
 * Inputs are an item's/person's raw field values (`ItemDetail.fields`, `ItemSummary.imageValues`) and, optionally,
 * the field definitions (used to ignore non-image fields and to order the "any image field" fallback).
 */
object ImagePolicy {
    val CARD_KEYS = listOf("thumbnail", "thumb", "poster", "cover")
    val BACKDROP_KEYS = listOf("backdrop", "fanart", "background")
    val PERSON_KEYS = listOf("photo", "image", "headshot")

    private val HASH = Regex("[0-9a-f]{16,}")

    /** Hashes of an image field value: one ref, or an ordered list of refs. Empty if the value is not an image ref. */
    fun hashes(value: Any?): List<String> = when (value) {
        is Map<*, *> -> listOfNotNull((value["hash"] as? String)?.lowercase()?.takeIf { HASH.matches(it) })
        is List<*> -> value.flatMap { hashes(it) }
        else -> emptyList()
    }

    /** First image of the first matching preferred key, else of any image field (definition order, then insertion order). */
    private fun pick(keys: List<String>, values: Map<String, Any?>, defs: List<FieldDefinition>): String? {
        val imageKeys = values.keys.filter { k ->
            val d = defs.firstOrNull { it.key == k }
            (d == null || d.type == "image") && hashes(values[k]).isNotEmpty()
        }
        for (k in keys) if (k in imageKeys) return hashes(values[k]).first()
        val order = defs.withIndex().associate { it.value.key to it.index }
        return imageKeys.sortedBy { order[it] ?: Int.MAX_VALUE }.firstOrNull()?.let { hashes(values[it]).first() }
    }

    fun card(values: Map<String, Any?>, defs: List<FieldDefinition> = emptyList()): String? = pick(CARD_KEYS, values, defs)

    /** Backdrop/header image only (no fallback), e.g. to decide whether a separate poster is worth showing. */
    fun backdropOnly(values: Map<String, Any?>, defs: List<FieldDefinition> = emptyList()): String? {
        val ks = values.keys.filter { k -> k in BACKDROP_KEYS && hashes(values[k]).isNotEmpty() && defs.firstOrNull { it.key == k }?.let { it.type == "image" } != false }
        return BACKDROP_KEYS.firstOrNull { it in ks }?.let { hashes(values[it]).first() }
    }

    fun backdrop(values: Map<String, Any?>, defs: List<FieldDefinition> = emptyList()): String? =
        backdropOnly(values, defs) ?: card(values, defs)

    fun person(values: Map<String, Any?>, defs: List<FieldDefinition> = emptyList()): String? = pick(PERSON_KEYS, values, defs)
}
