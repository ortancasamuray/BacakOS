package org.anadolupanteri.bacakonay

import android.os.Bundle
import android.view.WindowManager
import androidx.activity.compose.setContent
import androidx.activity.enableEdgeToEdge
import androidx.fragment.app.FragmentActivity
import org.anadolupanteri.bacakonay.ui.BacakOnayRoot
import org.anadolupanteri.bacakonay.ui.theme.BacakOnayTheme

/**
 * Single activity. A [FragmentActivity] because `BiometricPrompt` needs one.
 */
class MainActivity : FragmentActivity() {

    private val container get() = (application as BacakOnayApplication).container

    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        // Codes must never end up in screenshots, screen recordings, casts or
        // the recent-apps thumbnail.
        window.setFlags(WindowManager.LayoutParams.FLAG_SECURE, WindowManager.LayoutParams.FLAG_SECURE)
        enableEdgeToEdge()
        setContent {
            BacakOnayTheme {
                BacakOnayRoot(activity = this, container = container)
            }
        }
    }

    override fun onStop() {
        super.onStop()
        // Leaving the app (home, app switch, screen off) re-locks the vault and
        // scrubs decrypted secrets; a rotation is not "leaving".
        if (!isChangingConfigurations) container.repository.lock()
    }
}
