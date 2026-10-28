// SPDX-License-Identifier: MPL-2.0

package app.koru.mobile.ui

import androidx.compose.ui.graphics.Color

internal data class EntityTileColors(val background: Color, val foreground: Color)

private val workspaceLight = mapOf(
    "blue" to Color(0xFF2F6FB5),
    "purple" to Color(0xFF6B4FC4),
    "teal" to Color(0xFF1F7D75),
    "green" to Color(0xFF3B7D44),
    "olive" to Color(0xFF746A2C),
    "orange" to Color(0xFFC06A1E),
    "red" to Color(0xFFB33D35),
    "magenta" to Color(0xFFA83C77),
    "slate" to Color(0xFF555F6B),
    "stone" to Color(0xFF6E6660),
)

private val workspaceDark = mapOf(
    "blue" to Color(0xFF5E9CE0),
    "purple" to Color(0xFF9B86EA),
    "teal" to Color(0xFF45ADA2),
    "green" to Color(0xFF66AE70),
    "olive" to Color(0xFFA69B4F),
    "orange" to Color(0xFFE0964A),
    "red" to Color(0xFFE06C63),
    "magenta" to Color(0xFFD46EA6),
    "slate" to Color(0xFF8B96A4),
    "stone" to Color(0xFF9E948C),
)

private val profileLight = listOf(
    EntityTileColors(Color(0xFFF2F2F2), Color(0xFF565656)),
    EntityTileColors(Color(0xFFE7EFF8), Color(0xFF2C5983)),
    EntityTileColors(Color(0xFFFBF0DB), Color(0xFF86601D)),
    EntityTileColors(Color(0xFFE8F2EC), Color(0xFF2F6B4A)),
)

private val profileDark = listOf(
    EntityTileColors(Color(0xFF2C2C2C), Color(0xFFC9C9C9)),
    EntityTileColors(Color(0xFF1E2E3E), Color(0xFF9FC6E8)),
    EntityTileColors(Color(0xFF382C18), Color(0xFFE3BE77)),
    EntityTileColors(Color(0xFF1D3026), Color(0xFF8FCBA8)),
)

internal fun workspaceTileColor(token: String?, dark: Boolean): Color? =
    (if (dark) workspaceDark else workspaceLight)[token?.trim()?.lowercase()]

// koru-core's `AccentColor` tokens — what a callout's `color` property holds.
private val accentLight = mapOf(
    "red" to Color(0xFFB33D35),
    "orange" to Color(0xFFC06A1E),
    "yellow" to Color(0xFFB08A1A),
    "green" to Color(0xFF3B7D44),
    "teal" to Color(0xFF1F7D75),
    "blue" to Color(0xFF2F6FB5),
    "purple" to Color(0xFF6B4FC4),
    "pink" to Color(0xFFA83C77),
    "gray" to Color(0xFF555F6B),
)

private val accentDark = mapOf(
    "red" to Color(0xFFE06C63),
    "orange" to Color(0xFFE0964A),
    "yellow" to Color(0xFFD9B44A),
    "green" to Color(0xFF66AE70),
    "teal" to Color(0xFF45ADA2),
    "blue" to Color(0xFF5E9CE0),
    "purple" to Color(0xFF9B86EA),
    "pink" to Color(0xFFD46EA6),
    "gray" to Color(0xFF8B96A4),
)

/**
 * The background a callout box draws for its accent token: the token's
 * color washed over the surface, so the text keeps the surface's contrast.
 * `null` is the default tint — the token is absent, or comes from a newer
 * palette than this build knows, which reads as the default rather than as
 * an error, as koru-core's `callout::tint` does.
 */
// No token is the default tint, which the desktop draws as the yellow tone;
// a token this build does not know draws the surface color instead.
internal fun calloutTint(token: String?, dark: Boolean): Color? =
    (if (dark) accentDark else accentLight)[token?.trim()?.lowercase() ?: "yellow"]
        ?.copy(alpha = if (dark) 0.24f else 0.14f)

internal fun profileTileColors(
    colorToken: String?,
    paletteKey: String,
    dark: Boolean,
    defaultForeground: Color,
): EntityTileColors {
    workspaceTileColor(colorToken, dark)?.let { return EntityTileColors(it, defaultForeground) }
    val palettes = if (dark) profileDark else profileLight
    return palettes[profileChipSlot(paletteKey)]
}

internal fun profileChipSlot(key: String): Int {
    var hash = -3750763034362895579L // FNV-1a offset basis as a signed JVM Long.
    for (byte in key.trim().lowercase().encodeToByteArray()) {
        hash = hash xor (byte.toLong() and 0xFF)
        hash *= 1099511628211L
    }
    return hash.toInt() and 3
}
