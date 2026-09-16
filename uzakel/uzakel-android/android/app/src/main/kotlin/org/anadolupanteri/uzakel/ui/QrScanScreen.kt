package org.anadolupanteri.uzakel.ui

import android.Manifest
import android.content.pm.PackageManager
import android.os.Handler
import android.os.Looper
import androidx.activity.compose.rememberLauncherForActivityResult
import androidx.activity.result.contract.ActivityResultContracts
import androidx.camera.core.CameraSelector
import androidx.camera.core.ImageAnalysis
import androidx.camera.core.ImageProxy
import androidx.camera.core.Preview
import androidx.camera.lifecycle.ProcessCameraProvider
import androidx.camera.view.PreviewView
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.material3.Button
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.OutlinedButton
import androidx.compose.material3.Text
import androidx.compose.material3.TopAppBar
import androidx.compose.runtime.Composable
import androidx.compose.runtime.DisposableEffect
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.platform.LocalLifecycleOwner
import androidx.compose.ui.viewinterop.AndroidView
import androidx.core.content.ContextCompat
import com.google.zxing.BarcodeFormat
import com.google.zxing.BinaryBitmap
import com.google.zxing.DecodeHintType
import com.google.zxing.MultiFormatReader
import com.google.zxing.NotFoundException
import com.google.zxing.PlanarYUVLuminanceSource
import com.google.zxing.common.HybridBinarizer
import org.anadolupanteri.uzakel.network.ScannedPairingInfo
import org.anadolupanteri.uzakel.network.parsePairingUri
import java.util.concurrent.ExecutorService
import java.util.concurrent.Executors

/**
 * Camera-based pairing scan (ARCHITECTURE.md §2.3.1's "QR pairing" note) —
 * an alternative to [DeviceListScreen]'s manual PIN entry, not a
 * replacement: this screen never talks to the network itself, it only
 * decodes a `uzakel://pair?...` QR and hands the result back to the
 * caller, which runs the real pairing handshake exactly the way a manually
 * typed PIN would.
 *
 * Decoding is ZXing's raw `MultiFormatReader` over the analysis frame's Y
 * (luminance) plane directly — no bitmap conversion needed, since QR
 * decoding only looks at brightness, not colour, and `ImageAnalysis`'s
 * default output format (YUV_420_888) puts luminance in plane 0 uncompressed.
 */
@OptIn(ExperimentalMaterial3Api::class)
@Composable
fun QrScanScreen(onScanned: (ScannedPairingInfo) -> Unit, onCancel: () -> Unit) {
    val context = LocalContext.current

    var hasPermission by remember {
        mutableStateOf(
            ContextCompat.checkSelfPermission(context, Manifest.permission.CAMERA) == PackageManager.PERMISSION_GRANTED,
        )
    }
    val permissionLauncher = rememberLauncherForActivityResult(ActivityResultContracts.RequestPermission()) { granted ->
        hasPermission = granted
    }
    LaunchedEffect(Unit) {
        if (!hasPermission) permissionLauncher.launch(Manifest.permission.CAMERA)
    }

    Column(modifier = Modifier.fillMaxSize()) {
        TopAppBar(
            title = { Text("QR ile Eşleştir") },
            navigationIcon = { OutlinedButton(onClick = onCancel) { Text("İptal") } },
        )
        if (!hasPermission) {
            Box(modifier = Modifier.fillMaxSize(), contentAlignment = Alignment.Center) {
                Column(horizontalAlignment = Alignment.CenterHorizontally) {
                    Text("Taramak için kamera izni gerekiyor")
                    Button(onClick = { permissionLauncher.launch(Manifest.permission.CAMERA) }) { Text("İzin Ver") }
                }
            }
        } else {
            CameraPreview(onScanned = onScanned)
        }
    }
}

@Composable
private fun CameraPreview(onScanned: (ScannedPairingInfo) -> Unit) {
    val lifecycleOwner = LocalLifecycleOwner.current
    // `handled` lives outside Compose state on purpose: it's read/written
    // from the analyzer's background thread on every frame, and flipping
    // it must never wait on a recomposition — it just needs to stop new
    // frames from re-triggering onScanned after the first hit while the
    // camera unbinds.
    val handled = remember { java.util.concurrent.atomic.AtomicBoolean(false) }
    val executor: ExecutorService = remember { Executors.newSingleThreadExecutor() }
    val mainHandler = remember { Handler(Looper.getMainLooper()) }

    DisposableEffect(Unit) {
        onDispose {
            executor.shutdown()
        }
    }

    AndroidView(
        modifier = Modifier.fillMaxSize(),
        factory = { ctx ->
            val previewView = PreviewView(ctx)
            val cameraProviderFuture = ProcessCameraProvider.getInstance(ctx)
            cameraProviderFuture.addListener({
                val cameraProvider = cameraProviderFuture.get()
                val preview = Preview.Builder().build().also {
                    it.setSurfaceProvider(previewView.surfaceProvider)
                }
                val analysis = ImageAnalysis.Builder()
                    .setBackpressureStrategy(ImageAnalysis.STRATEGY_KEEP_ONLY_LATEST)
                    .build()
                analysis.setAnalyzer(executor) { imageProxy ->
                    if (handled.get()) {
                        imageProxy.close()
                        return@setAnalyzer
                    }
                    val info = decodeQr(imageProxy)
                    imageProxy.close()
                    if (info != null && handled.compareAndSet(false, true)) {
                        mainHandler.post {
                            runCatching { cameraProvider.unbindAll() }
                            onScanned(info)
                        }
                    }
                }
                try {
                    cameraProvider.unbindAll()
                    cameraProvider.bindToLifecycle(
                        lifecycleOwner,
                        CameraSelector.DEFAULT_BACK_CAMERA,
                        preview,
                        analysis,
                    )
                } catch (_: Exception) {
                    // No back camera, camera in use elsewhere, etc. — the
                    // preview just stays blank; manual PIN entry on the
                    // previous screen is always the fallback.
                }
            }, ContextCompat.getMainExecutor(ctx))
            previewView
        },
    )
}

/** Decodes one analysis frame's luminance plane as a QR code and parses it
 * as a pairing URI. Returns `null` on anything that isn't a decodable
 * `uzakel://pair?...` QR — a blank frame, an out-of-focus one, or a QR
 * that means something else entirely — which is the overwhelmingly common
 * case for any one frame, not an error. */
private fun decodeQr(imageProxy: ImageProxy): ScannedPairingInfo? {
    val plane = imageProxy.planes.getOrNull(0) ?: return null
    val buffer = plane.buffer
    val data = ByteArray(buffer.remaining())
    buffer.get(data)
    val source = PlanarYUVLuminanceSource(
        data,
        imageProxy.width,
        imageProxy.height,
        0,
        0,
        imageProxy.width,
        imageProxy.height,
        false,
    )
    val bitmap = BinaryBitmap(HybridBinarizer(source))
    val reader = MultiFormatReader().apply {
        setHints(mapOf(DecodeHintType.POSSIBLE_FORMATS to listOf(BarcodeFormat.QR_CODE)))
    }
    return try {
        parsePairingUri(reader.decode(bitmap).text)
    } catch (_: NotFoundException) {
        null
    } catch (_: Exception) {
        null
    }
}
