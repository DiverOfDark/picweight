package dev.picweight.android.ui.common

import dev.picweight.android.data.local.LocalMealStatus
import dev.picweight.android.data.local.MealEntity
import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.launch

/**
 * When a failed meal can be retried at all.
 *
 * [LocalMealStatus.FAILED] covers two different disasters. One is the server's: the
 * upload landed, the thumbnail is stored, and the estimation agent died — a spent quota,
 * a rate limit, a provider hiccup. That one the server can be asked to do again, with
 * nothing at all from the phone. The other is the queue's: the upload gave up before the
 * server ever heard of the meal (see `MealRepository.uploadOne`), so there is no
 * `serverId` and no analysis to re-run — offering "Retry" there would be a button that
 * can only 404.
 */
object MealRetry {
    fun isRetryable(meal: MealEntity): Boolean =
        meal.status == LocalMealStatus.FAILED && meal.serverId != null
}

/**
 * The sentence a failed *retry* gets.
 *
 * Deliberately not [dev.picweight.android.ui.home.HomeErrorCopy]: "showing what this
 * phone knows" is a promise about stale data being the best answer available, and a
 * retry that never left has no data to show — it simply did not happen. Everything the
 * server actually said keeps [ApiFailures]' own wording, so a refusal reads as a refusal
 * with its status code attached rather than being laundered into an outage.
 */
object RetryErrorCopy {
    fun forFailure(failure: ApiFailure): String = when {
        // The one status this endpoint returns by design: the meal is no longer `failed`,
        // or an analysis for it is already queued or running. Nothing is broken; the tap
        // was simply against a stale row.
        failure.status == 409 ->
            "Nothing to retry — the server says this meal isn't failed any more, or an " +
                "analysis for it is already running."

        FailureKind.OFFLINE == failure.kind ->
            "Couldn't reach the server, so the retry wasn't sent. Try again when you're back on."

        else -> failure.message
    }
}

/**
 * The retry affordance's state machine, shared by the day list and the meal detail
 * screen.
 *
 * Per meal: `idle → retrying → idle`, and on the way back either a cleared error (the
 * server accepted it) or the sentence for what refused it. A [start] for a meal that is
 * already retrying is a **no-op** — the whole point, because the user tapping this button
 * is by definition a frustrated one, and the endpoint answers 409 to the second request
 * rather than enqueueing a second analysis. Losing the race locally is cheaper than
 * explaining a 409 that only a double tap could have produced.
 *
 * The in-flight key is claimed *synchronously* inside [start], before the coroutine is
 * launched, so two taps in the same frame cannot both get through on a dispatcher that
 * has not run yet.
 */
class MealRetries(
    private val scope: CoroutineScope,
    private val tag: String,
    private val retry: suspend (String) -> Unit,
) {
    /** What the screens render: which meals are mid-retry, and the last refusal. */
    data class State(
        val inFlight: Set<String> = emptySet(),
        val error: String? = null,
    ) {
        fun isRetrying(key: String?): Boolean = key != null && key in inFlight
    }

    private val lock = Any()
    private val _state = MutableStateFlow(State())
    val state: StateFlow<State> = _state.asStateFlow()

    /**
     * Starts a retry for [key], unless one is already running for it.
     *
     * Returns true when this tap actually started one — false is the double-tap case, and
     * is returned rather than ignored so a test can prove the second tap did nothing.
     */
    fun start(key: String): Boolean {
        synchronized(lock) {
            val current = _state.value
            if (key in current.inFlight) return false
            _state.value = current.copy(inFlight = current.inFlight + key, error = null)
        }

        scope.launch {
            try {
                retry(key)
            } catch (e: CancellationException) {
                // The screen went away, not the retry. Clear the flag so a returning
                // screen is not left with a permanently disabled button, but never call
                // this a failure — and let cancellation keep propagating.
                finish(key, null)
                throw e
            } catch (t: Throwable) {
                finish(key, t)
                return@launch
            }
            finish(key, null)
        }
        return true
    }

    /** Drops a stale refusal — e.g. when the user dismisses the message. */
    fun dismissError() {
        synchronized(lock) { _state.value = _state.value.copy(error = null) }
    }

    /**
     * Ends one meal's retry: the button comes back, and the banner either names what
     * refused it or goes away. The failure is classified by [ApiFailures] and written to
     * logcat there — a retry that fails silently is the bug this whole feature exists to
     * undo, so it is never merely swallowed.
     */
    private fun finish(key: String, failure: Throwable?) {
        val message = failure?.let {
            RetryErrorCopy.forFailure(ApiFailures.report(tag, "Retrying the analysis", it))
        }
        synchronized(lock) {
            val current = _state.value
            _state.value = current.copy(inFlight = current.inFlight - key, error = message)
        }
    }
}
