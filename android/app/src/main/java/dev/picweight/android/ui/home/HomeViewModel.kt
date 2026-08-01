package dev.picweight.android.ui.home

import androidx.lifecycle.ViewModel
import androidx.lifecycle.viewModelScope
import dagger.hilt.android.lifecycle.HiltViewModel
import dev.picweight.android.data.ConnectivityMonitor
import dev.picweight.android.data.local.DayStateEntity
import dev.picweight.android.data.local.MealEntity
import dev.picweight.android.data.repository.AuthRepository
import dev.picweight.android.data.repository.MealRepository
import dev.picweight.android.data.repository.ProfileRepository
import dev.picweight.android.ui.common.ApiFailure
import dev.picweight.android.ui.common.ApiFailures
import dev.picweight.android.ui.common.FailureKind
import dev.picweight.android.ui.common.MealImage
import dev.picweight.android.ui.common.MealRetries
import dev.picweight.android.ui.common.MealRetry
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.SharingStarted
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.flow.combine
import kotlinx.coroutines.flow.stateIn
import kotlinx.coroutines.launch
import java.time.LocalDate
import javax.inject.Inject

/** One row of the day list: a solo meal, or a sitting collapsed into a single line. */
sealed interface DayRow {
    val sortKey: Long

    data class Single(val meal: MealEntity) : DayRow {
        override val sortKey: Long get() = meal.eatenAt
    }

    /**
     * A sitting. N photos are N independent meals with N independent agent loops; this
     * row exists purely so the day view reads as one restaurant order (PRD §5).
     */
    data class Sitting(val groupId: String, val meals: List<MealEntity>) : DayRow {
        override val sortKey: Long get() = meals.minOf { it.eatenAt }
        val kcal: Double get() = meals.sumOf { it.kcal }
        val hasFailures: Boolean
            get() = meals.any { it.status == dev.picweight.android.data.local.LocalMealStatus.FAILED }
    }
}

private const val TAG = "HomeViewModel"

/**
 * The banner text for a refresh that failed.
 *
 * Split out of the ViewModel because the rule it encodes is worth pinning in a test:
 * "showing what this phone knows" is a promise that Room holds the best available
 * answer, and that is only true when the request never reached the server. Everything
 * else — a status code, a body this build can't read, an expired session — is named for
 * what it is. Reporting all of them as "Offline" is what made a serialisation mismatch
 * in `/api/v1/me` take hours to find: the app looked disconnected while the server saw a
 * clean 200 and no follow-up requests at all.
 */
object HomeErrorCopy {
    fun forFailure(failure: ApiFailure): String = when (failure.kind) {
        FailureKind.OFFLINE -> "Offline — showing what this phone knows."
        else -> failure.message
    }
}

data class HomeUiState(
    val day: DayStateEntity? = null,
    val rows: List<DayRow> = emptyList(),
    val displayName: String? = null,
    val hasProfile: Boolean = true,
    val authExpired: Boolean = false,
    val refreshing: Boolean = false,
    val error: String? = null,
    /**
     * Whether the phone has a usable route out. Drives whether a queued meal is allowed
     * to claim it is "waiting for a connection"; assumed true until told otherwise, so
     * an unknown answer never produces that claim.
     */
    val online: Boolean = true,
    /** `clientUuid`s whose retry is in flight — those rows' buttons are disabled. */
    val retrying: Set<String> = emptySet(),
    /** Why the last retry was refused, if it was. Separate from [error]: a refresh that
     *  worked must not clear it, and a failed retry must not read as a failed refresh. */
    val retryError: String? = null,
) {
    fun isRetrying(meal: MealEntity): Boolean = meal.clientUuid in retrying

    /** Whether this row should offer the affordance at all. See [MealRetry]. */
    fun canRetry(meal: MealEntity): Boolean = MealRetry.isRetryable(meal)
}

@HiltViewModel
class HomeViewModel @Inject constructor(
    private val meals: MealRepository,
    private val profile: ProfileRepository,
    private val authRepository: AuthRepository,
    connectivity: ConnectivityMonitor,
) : ViewModel() {

    private val today = LocalDate.now()
    private val refreshing = MutableStateFlow(false)
    private val error = MutableStateFlow<String?>(null)

    /**
     * Retrying a failed analysis, per row. Owned here rather than in the screen so a
     * recomposition — or a navigation to the meal and back — cannot forget that a request
     * is already in flight and re-enable the button under the user's thumb.
     */
    private val retries = MealRetries(viewModelScope, TAG) { meals.retryAnalysis(it) }

    /** Everything that is about the screen rather than about a meal. */
    private data class Chrome(
        val refreshing: Boolean,
        val error: String?,
        val online: Boolean,
        val retries: MealRetries.State,
    )

    val uiState: StateFlow<HomeUiState> = combine(
        meals.flowDayState(today),
        meals.flowForLocalDay(today),
        profile.me,
        authRepository.authExpired,
        combine(refreshing, error, connectivity.online, retries.state, ::Chrome),
    ) { day, dayMeals, me, expired, chrome ->
        HomeUiState(
            day = day,
            rows = collapse(dayMeals),
            displayName = me?.user?.displayName ?: me?.user?.email,
            hasProfile = me == null || me.profile != null,
            authExpired = expired,
            refreshing = chrome.refreshing,
            error = chrome.error,
            online = chrome.online,
            retrying = chrome.retries.inFlight,
            retryError = chrome.retries.error,
        )
    }.stateIn(viewModelScope, SharingStarted.WhileSubscribed(5_000), HomeUiState())

    private val _serverUrl = MutableStateFlow(authRepository.getServerUrl())
    val serverUrl: StateFlow<String?> = _serverUrl.asStateFlow()

    init {
        refresh()
    }

    fun refresh() {
        if (refreshing.value) return
        viewModelScope.launch {
            refreshing.value = true
            error.value = null
            runCatching {
                profile.refresh()
                meals.refreshDay(today)
                meals.refreshRecentDishes()
            }.onFailure {
                // The three calls above run in sequence, so whichever one died is the one
                // that matters — and it goes to logcat with its stack trace regardless of
                // how short the banner ends up being.
                error.value = HomeErrorCopy.forFailure(
                    ApiFailures.report(TAG, "Refreshing today", it)
                )
            }
            refreshing.value = false
        }
    }

    /**
     * Re-runs the analysis for a meal the server marked failed.
     *
     * A second tap while the first is still in flight does nothing at all — [MealRetries]
     * holds the key until the response lands, which is what stops a frustrated double tap
     * from asking for two analyses of the same photo.
     */
    fun retry(meal: MealEntity) {
        if (!MealRetry.isRetryable(meal)) return
        retries.start(meal.clientUuid)
    }

    fun dismissRetryError() = retries.dismissError()

    /**
     * What to render for this meal — the local capture while it is still the only copy,
     * the server's thumbnail once there is one. See [MealImage].
     */
    fun thumbnailModel(meal: MealEntity): Any? = MealImage.model(meal, authRepository::absoluteUrl)

    private fun collapse(dayMeals: List<MealEntity>): List<DayRow> {
        val (grouped, solo) = dayMeals.partition { it.groupId != null }
        val rows = mutableListOf<DayRow>()
        solo.forEach { rows += DayRow.Single(it) }
        grouped.groupBy { it.groupId!! }.forEach { (groupId, members) ->
            if (members.size == 1) {
                // A sitting that ended up with one landed member reads better as itself.
                rows += DayRow.Single(members.first())
            } else {
                rows += DayRow.Sitting(groupId, members.sortedBy { it.eatenAt })
            }
        }
        return rows.sortedByDescending { it.sortKey }
    }
}
