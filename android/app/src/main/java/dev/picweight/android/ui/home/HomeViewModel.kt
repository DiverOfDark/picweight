package dev.picweight.android.ui.home

import androidx.lifecycle.ViewModel
import androidx.lifecycle.viewModelScope
import dagger.hilt.android.lifecycle.HiltViewModel
import dev.picweight.android.data.local.DayStateEntity
import dev.picweight.android.data.local.MealEntity
import dev.picweight.android.data.repository.AuthRepository
import dev.picweight.android.data.repository.MealRepository
import dev.picweight.android.data.repository.ProfileRepository
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

data class HomeUiState(
    val day: DayStateEntity? = null,
    val rows: List<DayRow> = emptyList(),
    val displayName: String? = null,
    val hasProfile: Boolean = true,
    val authExpired: Boolean = false,
    val refreshing: Boolean = false,
    val error: String? = null,
)

@HiltViewModel
class HomeViewModel @Inject constructor(
    private val meals: MealRepository,
    private val profile: ProfileRepository,
    private val authRepository: AuthRepository,
) : ViewModel() {

    private val today = LocalDate.now()
    private val refreshing = MutableStateFlow(false)
    private val error = MutableStateFlow<String?>(null)

    val uiState: StateFlow<HomeUiState> = combine(
        meals.flowDayState(today),
        meals.flowForLocalDay(today),
        profile.me,
        authRepository.authExpired,
        combine(refreshing, error) { r, e -> r to e },
    ) { day, dayMeals, me, expired, (isRefreshing, err) ->
        HomeUiState(
            day = day,
            rows = collapse(dayMeals),
            displayName = me?.user?.displayName ?: me?.user?.email,
            hasProfile = me == null || me.profile != null,
            authExpired = expired,
            refreshing = isRefreshing,
            error = err,
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
                // Room already has yesterday's answer; say so rather than blanking the UI.
                error.value = "Offline — showing what this phone knows."
            }
            refreshing.value = false
        }
    }

    fun thumbnailUrl(meal: MealEntity): String? = authRepository.absoluteUrl(meal.thumbnailUrl)

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
