package org.anadolupanteri.uzakel.ui

import androidx.compose.material3.Surface
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.LocalContext
import org.anadolupanteri.uzakel.discovery.SavedHostsStore
import org.anadolupanteri.uzakel.network.NetworkClient
import org.anadolupanteri.uzakel.network.PairedSession
import org.anadolupanteri.uzakel.transfer.FileTransferManager
import org.anadolupanteri.uzakel.ui.theme.UzakelTheme

/**
 * Root composable. No navigation library — three screens, switched by hand
 * with a small sealed class, is simpler than pulling in Navigation-Compose
 * for a graph this shallow (device list → control → transfer, and back).
 *
 * [Screen.Control]/[Screen.Transfer] carry a [PairedSession], not just an
 * address — the session keys a successful pairing derives (ARCHITECTURE.md
 * §2.3.1) are what [ControlScreen]/[TransferScreen] need to actually
 * encrypt anything; there's no separate "reconnect" step that could
 * re-derive them later.
 */
private sealed class Screen {
    object DeviceList : Screen()
    data class Control(val name: String, val session: PairedSession) : Screen()
    data class Transfer(val name: String, val session: PairedSession) : Screen()
}

@Composable
fun UzakelApp() {
    val context = LocalContext.current
    val client = remember { NetworkClient() }
    val savedHosts = remember { SavedHostsStore(context) }
    val transferManager = remember { FileTransferManager(context) }

    var screen by remember { mutableStateOf<Screen>(Screen.DeviceList) }

    UzakelTheme {
        Surface(modifier = Modifier) {
            when (val current = screen) {
                is Screen.DeviceList -> DeviceListScreen(
                    client = client,
                    savedHosts = savedHosts,
                    onConnected = { name, session -> screen = Screen.Control(name, session) },
                )
                is Screen.Control -> ControlScreen(
                    session = current.session,
                    hostName = current.name,
                    onOpenTransfer = { screen = Screen.Transfer(current.name, current.session) },
                    onDisconnect = { screen = Screen.DeviceList },
                )
                is Screen.Transfer -> TransferScreen(
                    session = current.session,
                    manager = transferManager,
                    onBack = { screen = Screen.Control(current.name, current.session) },
                )
            }
        }
    }
}
