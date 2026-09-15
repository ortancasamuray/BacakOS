package org.anadolupanteri.uzakel.input

import androidx.compose.foundation.background
import androidx.compose.foundation.gestures.awaitEachGesture
import androidx.compose.foundation.gestures.awaitFirstDown
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.material3.MaterialTheme
import androidx.compose.runtime.Composable
import androidx.compose.ui.Modifier
import androidx.compose.ui.input.pointer.pointerInput
import androidx.compose.ui.input.pointer.positionChange
import kotlin.math.hypot
import kotlin.math.roundToInt
import org.anadolupanteri.uzakel.protocol.MouseButton

private const val TAP_SLOP_PX = 18f
private const val TAP_TIMEOUT_MS = 300L

/**
 * The main touch surface (ARCHITECTURE.md §4): one-finger drag → `MOUSE_MOVE`,
 * one-finger tap → left click, two-finger tap → right click, two-finger
 * drag → `MOUSE_SCROLL`. Sensitivity is applied here, client-side, before a
 * packet is ever built — the daemon's own curve (`input_manager.rs`) is
 * only a safety net on top of this, not the primary feel-tuning knob.
 *
 * Uses Compose's low-level pointer API (`awaitEachGesture`) rather than
 * `detectDragGestures` because that one only tracks a single pointer;
 * distinguishing a one- vs two-finger drag needs to see every active
 * pointer each frame.
 */
@Composable
fun TrackpadView(
    modifier: Modifier = Modifier,
    sensitivity: Float = 0.4f,
    onMove: (dx: Int, dy: Int) -> Unit,
    onScroll: (dx: Int, dy: Int) -> Unit,
    onClick: (button: MouseButton) -> Unit,
) {
    Box(
        modifier = modifier
            .fillMaxSize()
            .background(MaterialTheme.colorScheme.surfaceVariant)
            .pointerInput(sensitivity) {
                awaitEachGesture {
                    awaitFirstDown(requireUnconsumed = false)
                    var maxPointers = 1
                    var totalMovement = 0f
                    val downTime = System.currentTimeMillis()

                    while (true) {
                        val event = awaitPointerEvent()
                        val pressed = event.changes.filter { it.pressed }
                        maxPointers = maxOf(maxPointers, pressed.size)

                        if (pressed.isNotEmpty()) {
                            var sumDx = 0f
                            var sumDy = 0f
                            for (change in pressed) {
                                val delta = change.positionChange()
                                sumDx += delta.x
                                sumDy += delta.y
                                change.consume()
                            }
                            val avgDx = sumDx / pressed.size
                            val avgDy = sumDy / pressed.size
                            totalMovement += hypot(avgDx, avgDy)

                            val scaledDx = (avgDx * sensitivity).roundToInt()
                            val scaledDy = (avgDy * sensitivity).roundToInt()
                            if (scaledDx != 0 || scaledDy != 0) {
                                if (pressed.size >= 2) {
                                    onScroll(scaledDx, scaledDy)
                                } else {
                                    onMove(scaledDx, scaledDy)
                                }
                            }
                        }

                        if (event.changes.all { !it.pressed }) {
                            val elapsed = System.currentTimeMillis() - downTime
                            if (totalMovement < TAP_SLOP_PX && elapsed < TAP_TIMEOUT_MS) {
                                onClick(if (maxPointers >= 2) MouseButton.RIGHT else MouseButton.LEFT)
                            }
                            break
                        }
                    }
                }
            },
    )
}
