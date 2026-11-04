package dev.sortedcord.mediadir.ui.components

import androidx.compose.foundation.background
import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.*
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.automirrored.filled.KeyboardArrowRight
import androidx.compose.material3.*
import androidx.compose.runtime.Composable
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.graphics.vector.ImageVector
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import dev.sortedcord.mediadir.ui.*

/** Small accent-coloured header above a group of settings. */
@Composable
fun SettingsSection(title: String) {
    Text(title, style = MaterialTheme.typography.labelLarge, color = Accent,
        modifier = Modifier.fillMaxWidth().padding(start = 24.dp, end = 24.dp, top = 20.dp, bottom = 6.dp))
}

/** Rounded card holding a group of rows. */
@Composable
fun SettingsGroup(content: @Composable ColumnScope.() -> Unit) {
    Column(Modifier.fillMaxWidth().padding(horizontal = 16.dp).background(CardBg, RoundedCornerShape(20.dp)).padding(vertical = 4.dp), content = content)
}

/** Generic row: optional leading icon, title, optional summary, optional trailing content; clickable when [onClick] set. */
@Composable
fun SettingsRow(
    title: String, summary: String? = null, icon: ImageVector? = null, onClick: (() -> Unit)? = null,
    chevron: Boolean = false, trailing: @Composable (() -> Unit)? = null,
) {
    Row(
        Modifier.fillMaxWidth().then(if (onClick != null) Modifier.clickable(onClick = onClick) else Modifier)
            .padding(horizontal = 16.dp, vertical = 14.dp),
        verticalAlignment = Alignment.CenterVertically,
    ) {
        if (icon != null) { Icon(icon, null, tint = Accent); Spacer(Modifier.width(16.dp)) }
        Column(Modifier.weight(1f)) {
            Text(title, style = MaterialTheme.typography.bodyLarge)
            if (summary != null) Text(summary, style = MaterialTheme.typography.bodySmall, color = Muted, maxLines = 2, overflow = TextOverflow.Ellipsis)
        }
        trailing?.let { Spacer(Modifier.width(12.dp)); it() }
        if (chevron) Icon(Icons.AutoMirrored.Filled.KeyboardArrowRight, null, tint = Muted)
    }
}

@Composable
fun SettingsSwitchRow(title: String, summary: String? = null, checked: Boolean, onChange: (Boolean) -> Unit, icon: ImageVector? = null) {
    SettingsRow(title, summary, icon, onClick = { onChange(!checked) }, trailing = { Switch(checked, onChange) })
}

@Composable
fun SettingsDivider() = HorizontalDivider(Modifier.padding(horizontal = 16.dp), color = Muted.copy(alpha = 0.2f))

/** Free-form content (text fields, buttons) inside a group. */
@Composable
fun SettingsBlock(content: @Composable ColumnScope.() -> Unit) {
    Column(Modifier.fillMaxWidth().padding(16.dp), verticalArrangement = Arrangement.spacedBy(8.dp), content = content)
}
