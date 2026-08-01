package dev.picweight.android.ui.capture

import android.graphics.Bitmap
import android.graphics.BitmapFactory
import android.graphics.Matrix
import androidx.exifinterface.media.ExifInterface
import java.io.File
import java.io.FileOutputStream
import kotlin.math.max
import kotlin.math.roundToInt

/**
 * Shrinks a capture before it is queued.
 *
 * The backend keeps a 768px derivative and throws the original away (PRD §7), so
 * shipping a 12MP frame over mobile data buys nothing. Downscaling here also bakes in
 * the EXIF rotation, which matters more than it sounds: a sideways plate makes the
 * container — the only reliable scale reference in the frame (§5, step 1) — much harder
 * for the model to read.
 */
object PhotoProcessor {

    /**
     * Long edge of the queued JPEG. Comfortably above the 768px the server keeps, so
     * the derivative is still made from more pixels than it needs.
     */
    const val MAX_EDGE = 1280

    private const val QUALITY = 85

    /**
     * Rewrites [file] in place. Returns the same file; on any failure the original is
     * left untouched, because an unshrunk photo is a slow upload while a lost one is a
     * lost meal.
     */
    fun normalise(file: File): File {
        val bounds = BitmapFactory.Options().apply { inJustDecodeBounds = true }
        BitmapFactory.decodeFile(file.absolutePath, bounds)
        if (bounds.outWidth <= 0 || bounds.outHeight <= 0) return file

        val options = BitmapFactory.Options().apply {
            inSampleSize = sampleSize(bounds.outWidth, bounds.outHeight)
        }
        val decoded = BitmapFactory.decodeFile(file.absolutePath, options) ?: return file

        val rotated = runCatching { applyExif(file, decoded) }.getOrDefault(decoded)
        val scaled = scaleToMaxEdge(rotated)

        val temp = File(file.parentFile, file.name + ".tmp")
        val written = runCatching {
            FileOutputStream(temp).use { out -> scaled.compress(Bitmap.CompressFormat.JPEG, QUALITY, out) }
        }.isSuccess

        if (scaled !== rotated) scaled.recycle()
        if (rotated !== decoded) rotated.recycle()
        decoded.recycle()

        if (!written || temp.length() <= 0L) {
            temp.delete()
            return file
        }
        if (!temp.renameTo(file)) {
            temp.copyTo(file, overwrite = true)
            temp.delete()
        }
        return file
    }

    private fun sampleSize(width: Int, height: Int): Int {
        var sample = 1
        while (max(width, height) / (sample * 2) >= MAX_EDGE) sample *= 2
        return sample
    }

    private fun applyExif(file: File, bitmap: Bitmap): Bitmap {
        val exif = ExifInterface(file.absolutePath)
        val matrix = Matrix()
        when (exif.getAttributeInt(ExifInterface.TAG_ORIENTATION, ExifInterface.ORIENTATION_NORMAL)) {
            ExifInterface.ORIENTATION_ROTATE_90 -> matrix.postRotate(90f)
            ExifInterface.ORIENTATION_ROTATE_180 -> matrix.postRotate(180f)
            ExifInterface.ORIENTATION_ROTATE_270 -> matrix.postRotate(270f)
            ExifInterface.ORIENTATION_FLIP_HORIZONTAL -> matrix.postScale(-1f, 1f)
            ExifInterface.ORIENTATION_FLIP_VERTICAL -> matrix.postScale(1f, -1f)
            else -> return bitmap
        }
        return Bitmap.createBitmap(bitmap, 0, 0, bitmap.width, bitmap.height, matrix, true)
    }

    private fun scaleToMaxEdge(bitmap: Bitmap): Bitmap {
        val longest = max(bitmap.width, bitmap.height)
        if (longest <= MAX_EDGE) return bitmap
        val ratio = MAX_EDGE.toDouble() / longest
        return Bitmap.createScaledBitmap(
            bitmap,
            (bitmap.width * ratio).roundToInt().coerceAtLeast(1),
            (bitmap.height * ratio).roundToInt().coerceAtLeast(1),
            true,
        )
    }
}
