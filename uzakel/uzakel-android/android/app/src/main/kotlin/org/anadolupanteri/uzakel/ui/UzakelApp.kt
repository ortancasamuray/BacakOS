package org.anadolupanteri.uzakel.ui

import androidx.compose.material3.Surface
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.LocalContext
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.material3.MaterialTheme
import org.anadolupanteri.uzakel.discovery.PairingStore
import androidx.activity.compose.BackHandler
import org.anadolupanteri.uzakel.network.ActiveConnection
import org.anadolupanteri.uzakel.network.Connection
import org.anadolupanteri.uzakel.network.NetworkClient
import org.anadolupanteri.uzakel.transfer.FileTransferManager
import org.anadolupanteri.uzakel.ui.theme.UzakelTheme

/**
 * Root composable. No navigation library — three screens, switched by hand
 * with a small sealed class, is simpler than pulling in Navigation-Compose
 * for a graph this shallow (device list → control → transfer, and back).
 *
 * [Screen.Control]/[Screen.Transfer] share one [Connection]: it keeps the
 * session alive (PING/PONG) and re-keys it in place after a drop
 * (ARCHITECTURE.md §2.3.3), so both screens always use current keys.
 */
private sealed class Screen {
    data class DeviceList(val repairName: String? = null) : Screen()
    data class Control(val connection: Connection) : Screen()
    data class Transfer(val connection: Connection) : Screen()
}

@Composable
fun UzakelApp() {
    val context = LocalContext.current
    val client = remember { NetworkClient() }
    val store = remember { PairingStore(context) }
    val transferManager = remember { FileTransferManager(context) }

    var screen by remember { mutableStateOf<Screen>(Screen.DeviceList()) }

    UzakelTheme {
        Surface(modifier = Modifier.fillMaxSize(), color = MaterialTheme.colorScheme.background) {
            when (val current = screen) {
                is Screen.DeviceList -> DeviceListScreen(
                    client = client,
                    store = store,
                    repairName = current.repairName,
                    onConnected = { conn ->
                        ActiveConnection.set(conn)
                        screen = Screen.Control(conn)
                    },
                )
                is Screen.Control -> {
                    BackHandler {
                        ActiveConnection.close()
                        screen = Screen.DeviceList()
                    }
                    ControlScreen(
                    connection = current.connection,
                    onOpenTransfer = { screen = Screen.Transfer(current.connection) },
                    onDisconnect = {
                        ActiveConnection.close()
                        screen = Screen.DeviceList()
                    },
                    onRepair = {
                        ActiveConnection.close()
                        screen = Screen.DeviceList(repairName = current.connection.device.name)
                    },
                    )
                }
                is Screen.Transfer -> {
                    BackHandler { screen = Screen.Control(current.connection) }
                    TransferScreen(
                    session = { current.connection.session },
                    manager = transferManager,
                    onBack = { screen = Screen.Control(current.connection) },
                    )
                }
            }
        }
    }
}
