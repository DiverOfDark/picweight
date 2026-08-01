package dev.picweight.android.ui.common

import android.os.Build
import androidx.compose.foundation.isSystemInDarkTheme
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.darkColorScheme
import androidx.compose.material3.dynamicDarkColorScheme
import androidx.compose.material3.dynamicLightColorScheme
import androidx.compose.material3.lightColorScheme
import androidx.compose.runtime.Composable
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.platform.LocalContext

private val Warm = Color(0xFFF97316)
private val WarmDark = Color(0xFFFFB77A)

private val DarkColorScheme = darkColorScheme(primary = WarmDark)
private val LightColorScheme = lightColorScheme(primary = Warm)

/** Macro accent colours, shared by the ring and the bars so a colour means one thing. */
object MacroColors {
    val Protein = Color(0xFF3B82F6)
    val Fat = Color(0xFFF59E0B)
    val Carbs = Color(0xFF10B981)
    val Over = Color(0xFFDC2626)
}

@Composable
fun PicweightTheme(
    darkTheme: Boolean = isSystemInDarkTheme(),
    content: @Composable () -> Unit,
) {
    val colorScheme = when {
        Build.VERSION.SDK_INT >= Build.VERSION_CODES.S -> {
            val context = LocalContext.current
            if (darkTheme) dynamicDarkColorScheme(context) else dynamicLightColorScheme(context)
        }

        darkTheme -> DarkColorScheme
        else -> LightColorScheme
    }

    MaterialTheme(colorScheme = colorScheme, content = content)
}
