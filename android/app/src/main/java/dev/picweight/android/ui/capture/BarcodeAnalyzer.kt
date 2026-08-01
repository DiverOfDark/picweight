package dev.picweight.android.ui.capture

import androidx.annotation.OptIn
import androidx.camera.core.ExperimentalGetImage
import androidx.camera.core.ImageAnalysis
import androidx.camera.core.ImageProxy
import com.google.mlkit.vision.barcode.BarcodeScanner
import com.google.mlkit.vision.barcode.BarcodeScannerOptions
import com.google.mlkit.vision.barcode.BarcodeScanning
import com.google.mlkit.vision.barcode.common.Barcode
import com.google.mlkit.vision.common.InputImage

/**
 * Reads packaged-goods barcodes off the live preview.
 *
 * Portion estimation is where essentially all the error lives — except for a sealed
 * can or bottle, where the barcode makes it exact for free. ML Kit runs on-device, so
 * this costs no round trip, works offline and never leaves the phone (PRD §5 "Drinks").
 */
class BarcodeAnalyzer(
    private val onBarcode: (String) -> Unit,
) : ImageAnalysis.Analyzer {

    private val scanner: BarcodeScanner = BarcodeScanning.getClient(
        BarcodeScannerOptions.Builder()
            .setBarcodeFormats(
                Barcode.FORMAT_EAN_13,
                Barcode.FORMAT_EAN_8,
                Barcode.FORMAT_UPC_A,
                Barcode.FORMAT_UPC_E,
            )
            .build()
    )

    @OptIn(ExperimentalGetImage::class)
    override fun analyze(image: ImageProxy) {
        val frame = image.image
        if (frame == null) {
            image.close()
            return
        }
        scanner.process(InputImage.fromMediaImage(frame, image.imageInfo.rotationDegrees))
            .addOnSuccessListener { barcodes ->
                barcodes.firstNotNullOfOrNull { it.rawValue?.takeIf(::looksLikeEan) }
                    ?.let(onBarcode)
            }
            .addOnCompleteListener { image.close() }
    }

    private fun looksLikeEan(value: String): Boolean =
        value.length in 8..14 && value.all { it.isDigit() }

    fun close() {
        scanner.close()
    }
}
