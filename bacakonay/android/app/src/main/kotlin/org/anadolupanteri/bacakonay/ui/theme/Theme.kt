package org.anadolupanteri.bacakonay.ui.theme

import androidx.compose.foundation.isSystemInDarkTheme
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.darkColorScheme
import androidx.compose.material3.lightColorScheme
import androidx.compose.runtime.Composable
import androidx.compose.ui.graphics.Color

/** BacakOS palette (bacak/DESIGN_SYSTEM.md): Aegean blues + Rust accent. */
object BacakColors {
    val AegeanDeep = Color(0xFF07334A)
    val AegeanMid = Color(0xFF1A6C8A)
    val AegeanSoft = Color(0xFF5CB0C4)
    val AegeanPale = Color(0xFFA8DDE6)
    val AegeanFoam = Color(0xFFE6F6F8)
    val Rust = Color(0xFFD96C2D)
    val RustLight = Color(0xFFEF8348)
    val Night = Color(0xFF041C29)
    val NightCard = Color(0xFF0A2B3D)
}

private val Dark = darkColorScheme(
    primary = BacakColors.AegeanSoft,
    onPrimary = BacakColors.AegeanDeep,
    primaryContainer = BacakColors.AegeanMid,
    onPrimaryContainer = BacakColors.AegeanFoam,
    secondary = BacakColors.RustLight,
    onSecondary = Color.White,
    tertiary = BacakColors.Rust,
    background = BacakColors.Night,
    onBackground = BacakColors.AegeanFoam,
    surface = BacakColors.Night,
    onSurface = BacakColors.AegeanFoam,
    surfaceContainer = BacakColors.NightCard,
    surfaceContainerHigh = Color(0xFF0E3550),
    onSurfaceVariant = BacakColors.AegeanPale,
    outline = Color(0xFF2F6378),
    error = Color(0xFFFF8A80),
)

private val Light = lightColorScheme(
    primary = BacakColors.AegeanMid,
    onPrimary = Color.White,
    primaryContainer = BacakColors.AegeanPale,
    onPrimaryContainer = BacakColors.AegeanDeep,
    secondary = BacakColors.Rust,
    onSecondary = Color.White,
    tertiary = BacakColors.Rust,
    background = BacakColors.AegeanFoam,
    onBackground = BacakColors.AegeanDeep,
    surface = BacakColors.AegeanFoam,
    onSurface = BacakColors.AegeanDeep,
    surfaceContainer = Color.White,
    surfaceContainerHigh = Color(0xFFD7EEF2),
    onSurfaceVariant = BacakColors.AegeanMid,
)

/**
 * Brand colours rather than Material You dynamic colour: the rust "about to
 * expire" accent must stay recognisable on every phone.
 */
@Composable
fun BacakOnayTheme(darkTheme: Boolean = isSystemInDarkTheme(), content: @Composable () -> Unit) {
    MaterialTheme(colorScheme = if (darkTheme) Dark else Light, content = content)
}
