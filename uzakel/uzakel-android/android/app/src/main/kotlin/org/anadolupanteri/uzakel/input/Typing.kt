package org.anadolupanteri.uzakel.input

import org.anadolupanteri.uzakel.network.InputChannel
import org.anadolupanteri.uzakel.protocol.Modifiers

/**
 * Sends one character as a full press+release [KeyCodes] pair, wrapped in a
 * Shift press/release when the character needs it. Used by the software
 * keyboard bridge in `ui/TrackpadScreen.kt` — a hidden text field captures
 * whatever the system IME produces and replays it character-by-character
 * through here rather than the app implementing its own on-screen keyboard.
 */
fun InputChannel.typeChar(c: Char) {
    val (keycode, needsShift) = KeyCodes.forChar(c) ?: return
    val mods = if (needsShift) Modifiers.SHIFT else 0
    if (needsShift) keyPress(KeyCodes.LEFT_SHIFT, Modifiers.SHIFT, pressed = true)
    keyPress(keycode, mods, pressed = true)
    keyPress(keycode, mods, pressed = false)
    if (needsShift) keyPress(KeyCodes.LEFT_SHIFT, 0, pressed = false)
}
