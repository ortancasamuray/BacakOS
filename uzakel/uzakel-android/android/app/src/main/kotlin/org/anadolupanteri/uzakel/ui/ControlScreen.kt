package org.anadolupanteri.uzakel.ui

import androidx.compose.foundation.background
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.text.BasicTextField
import androidx.compose.material3.CircularProgressIndicator
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.TextButton
import androidx.compose.material3.FilterChip
import androidx.compose.material3.OutlinedButton
import androidx.compose.material3.Text
import androidx.compose.material3.TopAppBar
import androidx.compose.runtime.Composable
import androidx.compose.runtime.collectAsState
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.focus.FocusRequester
import androidx.compose.ui.focus.focusRequester
import androidx.compose.ui.platform.LocalSoftwareKeyboardController
import androidx.compose.ui.text.TextRange
import androidx.compose.ui.text.input.TextFieldValue
import androidx.compose.ui.unit.dp
import org.anadolupanteri.uzakel.input.KeyCodes
import org.anadolupanteri.uzakel.input.TrackpadView
import org.anadolupanteri.uzakel.input.typeChar
import org.anadolupanteri.uzakel.network.InputChannel
import org.anadolupanteri.uzakel.network.ConnState
import org.anadolupanteri.uzakel.network.Connection
import org.anadolupanteri.uzakel.ui.theme.BacakColors
import org.anadolupanteri.uzakel.protocol.MouseButton

/** The hidden field's content is always this single placeholder char with the
 * cursor after it — its only job is giving the system IME something to send
 * insert/delete diffs against (see [ControlScreen]'s doc). */
private const val PLACEHOLDER = "​"

/**
 * The main control surface once connected (ARCHITECTURE.md §4): the
 * trackpad fills most of the screen, a modifier-key row sits above it, and
 * a Storage-Access-Framework-invisible text field bridges the system IME
 * into [org.anadolupanteri.uzakel.input.typeChar] calls — rather than the
 * app drawing its own on-screen keyboard, it reuses whatever keyboard the
 * user already has configured (including non-Latin ones, autocorrect,
 * swipe typing, …), translating only the *resulting* characters.
 *
 * Real hardware backspace on an empty system keyboard can't be observed as
 * a text diff, so the field is kept non-empty (one zero-width placeholder
 * char) and a shrinking value is read back as one `KEY_BACKSPACE` press.
 */
@OptIn(ExperimentalMaterial3Api::class)
@Composable
fun ControlScreen(
    connection: Connection,
    onOpenTransfer: () -> Unit,
    onDisconnect: () -> Unit,
    onRepair: () -> Unit,
) {
    // The channel belongs to the Connection (it outlives this screen while
    // the user is on the file-transfer screen, and re-keys in place on a
    // reconnect), so there's nothing to create or close here.
    val channel: InputChannel? = connection.channel
    val state by connection.state.collectAsState()

    var ctrlHeld by remember { mutableStateOf(false) }
    var altHeld by remember { mutableStateOf(false) }
    var superHeld by remember { mutableStateOf(false) }
    var fieldValue by remember { mutableStateOf(TextFieldValue(PLACEHOLDER, TextRange(PLACEHOLDER.length))) }
    val imeFocusRequester = remember { FocusRequester() }
    val keyboardController = LocalSoftwareKeyboardController.current

    fun tapKey(code: Int) {
        channel?.keyPress(code, 0, pressed = true)
        channel?.keyPress(code, 0, pressed = false)
    }

    fun toggleModifier(code: Int, held: Boolean, setHeld: (Boolean) -> Unit) {
        channel?.keyPress(code, 0, pressed = !held)
        setHeld(!held)
    }

    Column(modifier = Modifier.fillMaxSize()) {
        TopAppBar(
            title = { Text(connection.device.name) },
            actions = {
                OutlinedButton(onClick = onOpenTransfer) { Text("Dosyalar") }
                OutlinedButton(onClick = onDisconnect) { Text("Ayır") }
            },
        )

        ConnectionBanner(state, onRetry = connection::retryNow, onRepair = onRepair)

        Row(
            modifier = Modifier.fillMaxWidth().padding(horizontal = 8.dp, vertical = 4.dp),
            horizontalArrangement = Arrangement.spacedBy(6.dp),
        ) {
            FilterChip(selected = ctrlHeld, onClick = { toggleModifier(KeyCodes.LEFT_CTRL, ctrlHeld) { ctrlHeld = it } }, label = { Text("Ctrl") })
            FilterChip(selected = altHeld, onClick = { toggleModifier(KeyCodes.LEFT_ALT, altHeld) { altHeld = it } }, label = { Text("Alt") })
            FilterChip(selected = superHeld, onClick = { toggleModifier(KeyCodes.LEFT_META, superHeld) { superHeld = it } }, label = { Text("Super") })
            OutlinedButton(onClick = { tapKey(KeyCodes.ESC) }) { Text("Esc") }
            OutlinedButton(onClick = { tapKey(KeyCodes.TAB) }) { Text("Tab") }
        }

        Row(
            modifier = Modifier.fillMaxWidth().padding(horizontal = 8.dp, vertical = 4.dp),
            horizontalArrangement = Arrangement.spacedBy(6.dp),
        ) {
            OutlinedButton(onClick = { tapKey(KeyCodes.LEFT) }) { Text("←") }
            OutlinedButton(onClick = { tapKey(KeyCodes.UP) }) { Text("↑") }
            OutlinedButton(onClick = { tapKey(KeyCodes.DOWN) }) { Text("↓") }
            OutlinedButton(onClick = { tapKey(KeyCodes.RIGHT) }) { Text("→") }
            OutlinedButton(onClick = { tapKey(KeyCodes.BACKSPACE) }) { Text("⌫") }
            OutlinedButton(onClick = { tapKey(KeyCodes.ENTER) }) { Text("⏎") }
        }

        // Invisible bridge to the system IME — 0-height so it never draws,
        // but still focusable/editable; a one-finger tap on the trackpad
        // below requests focus on it, which is what actually brings the
        // keyboard up (a 0-sized field can't be tapped directly).
        BasicTextField(
            value = fieldValue,
            onValueChange = { new ->
                val old = fieldValue.text
                when {
                    new.text.length > old.length -> {
                        val inserted = new.text.removePrefix(old)
                        channel?.let { c -> for (ch in inserted) c.typeChar(ch) }
                    }
                    new.text.length < old.length -> {
                        tapKey(KeyCodes.BACKSPACE)
                    }
                }
                fieldValue = TextFieldValue(PLACEHOLDER, TextRange(PLACEHOLDER.length))
            },
            modifier = Modifier.height(0.dp).focusRequester(imeFocusRequester),
        )

        TrackpadView(
            modifier = Modifier.fillMaxWidth().weight(1f),
            onMove = { dx, dy -> channel?.mouseMove(dx, dy) },
            onScroll = { dx, dy -> channel?.mouseScroll(dx, dy) },
            onClick = { button ->
                channel?.mouseClick(button, pressed = true)
                channel?.mouseClick(button, pressed = false)
                if (button == MouseButton.LEFT) {
                    imeFocusRequester.requestFocus()
                    keyboardController?.show()
                }
            },
        )
    }
}

/** Thin status strip under the app bar — hidden while everything is fine. */
@Composable
private fun ConnectionBanner(state: ConnState, onRetry: () -> Unit, onRepair: () -> Unit) {
    val (color, text) = when (state) {
        ConnState.Connected -> return
        ConnState.Reconnecting -> BacakColors.Warn to "Bağlantı koptu — yeniden bağlanılıyor…"
        ConnState.Unreachable -> MaterialTheme.colorScheme.error to "Bilgisayara ulaşılamıyor — arka planda deneniyor"
        ConnState.NeedsPairing -> MaterialTheme.colorScheme.error to "Bu bilgisayar eşleşmeyi tanımıyor"
    }
    Row(
        modifier = Modifier.fillMaxWidth().background(color.copy(alpha = 0.18f)).padding(horizontal = 12.dp, vertical = 6.dp),
        verticalAlignment = Alignment.CenterVertically,
    ) {
        if (state == ConnState.Reconnecting) {
            CircularProgressIndicator(Modifier.size(16.dp), strokeWidth = 2.dp, color = color)
            Spacer(Modifier.width(8.dp))
        }
        Text(text, color = color, style = MaterialTheme.typography.bodyMedium, modifier = Modifier.weight(1f))
        when (state) {
            ConnState.Unreachable -> TextButton(onClick = onRetry) { Text("Şimdi dene") }
            ConnState.NeedsPairing -> TextButton(onClick = onRepair) { Text("Yeniden eşleştir") }
            else -> {}
        }
    }
}
