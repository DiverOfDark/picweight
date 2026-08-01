package dev.picweight.android.ui.common

import dev.picweight.android.data.local.LocalMealStatus
import dev.picweight.android.data.local.MealEntity

/**
 * What a meal's state is called on screen.
 *
 * Split out of the screens and kept pure so the rules below can be pinned in a test,
 * because they encode a lesson rather than a preference: a queued meal used to read
 * "Waiting for a connection" unconditionally. When the upload queue was broken outright
 * — the worker could not even be constructed, so not one request was ever attempted —
 * every phone with a perfect signal still said it was waiting for one. The UI was the
 * last place the bug could have been spotted, and it actively pointed away from it.
 *
 * So: that sentence is now only printed when the phone genuinely has no route out, and
 * a queue that is failing says how it is failing.
 */
object MealStatusCopy {

    /**
     * The short form, for a title line. Never longer than a few words: a title is not
     * where a stack trace goes.
     */
    fun title(status: LocalMealStatus, online: Boolean): String = when (status) {
        LocalMealStatus.HELD -> "Saving…"
        // The one honest split. Offline is a state the user can fix; online-and-queued
        // means the upload is our problem, not theirs.
        LocalMealStatus.QUEUED -> if (online) "Uploading…" else "Waiting for a connection"
        LocalMealStatus.PENDING -> "Queued for analysis"
        LocalMealStatus.ANALYZING -> "Analysing…"
        LocalMealStatus.NEEDS_REVIEW -> "Needs review"
        LocalMealStatus.CONFIRMED -> "Confirmed"
        LocalMealStatus.FAILED -> "Failed"
    }

    /**
     * The long form, for a subtitle — the specific reason when there is one.
     *
     * A queued meal carries an `error` only once its upload has bounced repeatedly
     * (see `MealRepository.noteRetry`), so this reads as the plain status right up until
     * the moment something is genuinely wrong, and then says what.
     */
    fun detail(status: LocalMealStatus, error: String?, online: Boolean): String = when {
        status == LocalMealStatus.FAILED -> error ?: "Analysis failed"
        status == LocalMealStatus.QUEUED && !error.isNullOrBlank() -> error
        else -> title(status, online)
    }

    /**
     * The lowercase badge the meal detail header sets beside the calorie count. Same
     * rule as [title] — this screen carried its own duplicate of the copy, and therefore
     * its own copy of the lie.
     */
    fun badge(status: LocalMealStatus, online: Boolean): String = when (status) {
        LocalMealStatus.HELD -> "saving"
        LocalMealStatus.QUEUED -> if (online) "uploading" else "waiting for a connection"
        LocalMealStatus.PENDING -> "queued for analysis"
        LocalMealStatus.ANALYZING -> "analysing"
        LocalMealStatus.NEEDS_REVIEW -> "needs review"
        LocalMealStatus.CONFIRMED -> "confirmed"
        LocalMealStatus.FAILED -> "failed"
    }

    fun title(meal: MealEntity, online: Boolean): String = title(meal.status, online)

    fun detail(meal: MealEntity, online: Boolean): String = detail(meal.status, meal.error, online)

    /** True when [detail] is reporting a problem, so the caller can colour it as one. */
    fun isProblem(meal: MealEntity): Boolean =
        meal.status == LocalMealStatus.FAILED ||
            (meal.status == LocalMealStatus.QUEUED && !meal.error.isNullOrBlank())
}
