package org.anadolupanteri.uzakel.input

/**
 * Linux evdev keycodes (`<linux/input-event-codes.h>`), matching exactly
 * what `input_manager.rs`'s `Key::from_code` expects on the daemon side —
 * these are kernel constants, not something either side gets to choose, so
 * this table has no daemon-side counterpart to keep in sync with beyond
 * "both read the same kernel header."
 *
 * Only the subset a trackpad + basic text entry session realistically
 * needs; a full hardware-keyboard mapping (function keys, numpad, etc.) is
 * out of scope for the first pass.
 */
object KeyCodes {
    const val ESC = 1
    const val BACKSPACE = 14
    const val TAB = 15
    const val ENTER = 28
    const val LEFT_CTRL = 29
    const val LEFT_SHIFT = 42
    const val LEFT_ALT = 56
    const val SPACE = 57
    const val LEFT_META = 125
    const val UP = 103
    const val LEFT = 105
    const val RIGHT = 106
    const val DOWN = 108
    const val DELETE = 111

    private val letterCodes = mapOf(
        'a' to 30, 'b' to 48, 'c' to 46, 'd' to 32, 'e' to 18, 'f' to 33, 'g' to 34,
        'h' to 35, 'i' to 23, 'j' to 36, 'k' to 37, 'l' to 38, 'm' to 50, 'n' to 49,
        'o' to 24, 'p' to 25, 'q' to 16, 'r' to 19, 's' to 31, 't' to 20, 'u' to 22,
        'v' to 47, 'w' to 17, 'x' to 45, 'y' to 21, 'z' to 44,
    )
    private val digitCodes = mapOf(
        '1' to 2, '2' to 3, '3' to 4, '4' to 5, '5' to 6,
        '6' to 7, '7' to 8, '8' to 9, '9' to 10, '0' to 11,
    )
    // Unshifted punctuation → keycode (US QWERTY physical layout).
    private val punctCodes = mapOf(
        '-' to 12, '=' to 13, '[' to 26, ']' to 27, ';' to 39, '\'' to 40,
        '`' to 41, '\\' to 43, ',' to 51, '.' to 52, '/' to 53,
    )
    // Shifted digit-row symbols → the base digit sharing that physical key.
    private val shiftedDigitSymbols = mapOf(
        '!' to '1', '@' to '2', '#' to '3', '$' to '4', '%' to '5',
        '^' to '6', '&' to '7', '*' to '8', '(' to '9', ')' to '0',
    )
    // Shifted punctuation symbols → the base punctuation char sharing that key.
    private val shiftedPunctSymbols = mapOf(
        '_' to '-', '+' to '=', '{' to '[', '}' to ']', ':' to ';',
        '"' to '\'', '~' to '`', '|' to '\\', '<' to ',', '>' to '.', '?' to '/',
    )

    /** Returns `(keycode, needsShift)` for a printable ASCII char, or `null` if unmapped. */
    fun forChar(c: Char): Pair<Int, Boolean>? {
        letterCodes[c.lowercaseChar()]?.let { code -> return code to c.isUpperCase() }
        digitCodes[c]?.let { code -> return code to false }
        punctCodes[c]?.let { code -> return code to false }
        shiftedDigitSymbols[c]?.let { base -> return digitCodes.getValue(base) to true }
        shiftedPunctSymbols[c]?.let { base -> return punctCodes.getValue(base) to true }
        return when (c) {
            ' ' -> SPACE to false
            '\n' -> ENTER to false
            '\t' -> TAB to false
            else -> null
        }
    }
}
