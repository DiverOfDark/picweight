package dev.picweight.android.ui.common

import dev.picweight.android.data.local.MealEntity
import java.io.File

/**
 * Picks what Coil should load for a meal.
 *
 * A meal has two possible images and they take turns: `photoPath`, the JPEG this phone
 * captured, and `thumbnailUrl`, the one the server produces. Every screen used to ask
 * only for the server's, which does not exist until the upload has landed and been
 * analysed — so a meal still in the queue rendered as a grey camera icon, and a meal
 * that could never upload rendered as one forever. The picture was sitting on disk the
 * whole time.
 *
 * Local first, deliberately: while a local file exists it is the freshest thing we have
 * and it needs no network. `MealRepository.applyServerMeal` deletes it once the server
 * has a thumbnail to take over, so the two are handed off rather than overlapping
 * indefinitely.
 */
object MealImage {

    /**
     * Returns a Coil model — a [File] for the local capture, a [String] URL for the
     * server's thumbnail, or null when there is genuinely no image yet.
     *
     * [absoluteUrl] resolves a server-relative path against the configured host.
     */
    fun model(meal: MealEntity, absoluteUrl: (String?) -> String?): Any? {
        // takeIf { isFile }: the path outlives the file across a cache wipe, and handing
        // Coil a path to nothing renders an error drawable rather than the placeholder.
        meal.photoPath?.let(::File)?.takeIf { it.isFile }?.let { return it }
        return absoluteUrl(meal.thumbnailUrl)
    }
}
