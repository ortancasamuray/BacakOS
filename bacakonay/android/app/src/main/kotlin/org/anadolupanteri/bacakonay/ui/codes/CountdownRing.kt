package org.anadolupanteri.bacakonay.ui.codes

import androidx.compose.foundation.Canvas
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.size
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.graphics.StrokeCap
import androidx.compose.ui.graphics.drawscope.Stroke
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.unit.Dp
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import org.anadolupanteri.bacakonay.ui.theme.BacakColors

/** Seconds at which the ring (and the code) switch to the "hurry" colour. */
const val EXPIRY_WARNING_SECONDS = 5

/**
 * Circular countdown for a TOTP period. [fraction] is recomputed every frame
 * from the wall clock by the caller, so the arc drains smoothly instead of
 * jumping once per second, and never "rewinds" visibly at the period edge.
 */
@Composable
fun CountdownRing(fraction: Float, secondsLeft: Int, size: Dp = 44.dp) {
    val warn = secondsLeft <= EXPIRY_WARNING_SECONDS
    val track = MaterialTheme.colorScheme.outline.copy(alpha = 0.35f)
    val arc = if (warn) BacakColors.Rust else MaterialTheme.colorScheme.primary
    Box(contentAlignment = Alignment.Center, modifier = Modifier.size(size)) {
        Canvas(Modifier.size(size)) {
            val stroke = 4.dp.toPx()
            val inset = stroke / 2
            val arcSize = androidx.compose.ui.geometry.Size(this.size.width - stroke, this.size.height - stroke)
            val topLeft = androidx.compose.ui.geometry.Offset(inset, inset)
            drawArc(track, 0f, 360f, false, topLeft, arcSize, style = Stroke(stroke))
            drawArc(
                arc,
                startAngle = -90f,
                sweepAngle = 360f * fraction.coerceIn(0f, 1f),
                useCenter = false,
                topLeft = topLeft,
                size = arcSize,
                style = Stroke(stroke, cap = StrokeCap.Round),
            )
        }
        Text(
            secondsLeft.toString(),
            fontSize = 13.sp,
            fontWeight = FontWeight.SemiBold,
            color = if (warn) BacakColors.Rust else MaterialTheme.colorScheme.onSurfaceVariant,
        )
    }
}
