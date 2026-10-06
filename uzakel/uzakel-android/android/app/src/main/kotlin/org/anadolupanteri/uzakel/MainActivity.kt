package org.anadolupanteri.uzakel

import android.os.Bundle
import androidx.activity.ComponentActivity
import androidx.activity.compose.setContent
import org.anadolupanteri.uzakel.network.ActiveConnection
import org.anadolupanteri.uzakel.ui.UzakelApp

class MainActivity : ComponentActivity() {
    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        setContent { UzakelApp() }
    }

    override fun onDestroy() {
        super.onDestroy()
        // Leaving the app ends the remote-control connection; a rotation
        // doesn't.
        if (isFinishing) ActiveConnection.close()
    }
}
