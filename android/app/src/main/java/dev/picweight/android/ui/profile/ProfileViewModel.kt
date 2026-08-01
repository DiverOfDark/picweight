package dev.picweight.android.ui.profile

import androidx.lifecycle.ViewModel
import androidx.lifecycle.viewModelScope
import dagger.hilt.android.lifecycle.HiltViewModel
import dev.picweight.android.data.remote.model.GoalType
import dev.picweight.android.data.remote.model.MeResponse
import dev.picweight.android.data.remote.model.Sex
import dev.picweight.android.data.repository.AuthRepository
import dev.picweight.android.data.repository.MealRepository
import dev.picweight.android.data.repository.ProfileRepository
import dev.picweight.android.sync.SyncScheduler
import dev.picweight.android.ui.common.ApiFailures
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.launch
import java.time.LocalDate
import java.time.ZoneId
import javax.inject.Inject

private const val TAG = "ProfileViewModel"

data class ProfileUiState(
    val me: MeResponse? = null,
    val sex: Sex = Sex.MALE,
    val birthDate: String = "",
    val heightCm: String = "",
    val currentWeightKg: String = "",
    val targetWeightKg: String = "",
    val activityFactor: Float = 1.375f,
    val goalType: GoalType = GoalType.LOSE,
    val rateKgPerWeek: String = "0.5",
    val timezone: String = ZoneId.systemDefault().id,
    val weightEntry: String = "",
    val warnings: List<String> = emptyList(),
    val busy: Boolean = false,
    val saved: Boolean = false,
    val error: String? = null,
    val loggedOut: Boolean = false,
    val serverUrl: String? = null,
    val version: String? = null,
)

/**
 * Onboarding and profile editing (PRD §6).
 *
 * Every field here feeds Mifflin-St Jeor on the server. Nothing on this screen guesses
 * a number — the targets come back computed, together with any warning the inputs
 * earned, such as a deficit steeper than about 1% of bodyweight a week.
 */
@HiltViewModel
class ProfileViewModel @Inject constructor(
    private val profile: ProfileRepository,
    private val meals: MealRepository,
    private val authRepository: AuthRepository,
    private val scheduler: SyncScheduler,
) : ViewModel() {

    private val _uiState = MutableStateFlow(
        ProfileUiState(serverUrl = authRepository.getServerUrl())
    )
    val uiState: StateFlow<ProfileUiState> = _uiState.asStateFlow()

    init {
        viewModelScope.launch {
            runCatching { profile.refresh() }
                .onSuccess { prefill(it) }
                // "Couldn't load your profile." was true but useless: it is the same
                // sentence whether the phone is in a lift, the session expired or the
                // server sent a `/me` body this build can't parse.
                .onFailure { fail("Couldn't load your profile", it) }
        }
    }

    private fun prefill(me: MeResponse) {
        val existing = me.profile
        _uiState.value = _uiState.value.copy(
            me = me,
            version = me.version,
            sex = existing?.sex ?: _uiState.value.sex,
            birthDate = existing?.birthDate?.toString() ?: _uiState.value.birthDate,
            heightCm = existing?.heightCm?.let { fmt(it) } ?: _uiState.value.heightCm,
            currentWeightKg = existing?.currentWeightKg?.let { fmt(it) } ?: _uiState.value.currentWeightKg,
            targetWeightKg = existing?.targetWeightKg?.let { fmt(it) } ?: _uiState.value.targetWeightKg,
            activityFactor = existing?.activityFactor?.toFloat() ?: _uiState.value.activityFactor,
            goalType = existing?.goalType ?: _uiState.value.goalType,
            rateKgPerWeek = existing?.rateKgPerWeek?.let { fmt(it) } ?: _uiState.value.rateKgPerWeek,
            timezone = existing?.timezone ?: _uiState.value.timezone,
        )
    }

    fun setSex(value: Sex) = update { copy(sex = value) }
    fun setBirthDate(value: String) = update { copy(birthDate = value) }
    fun setHeight(value: String) = update { copy(heightCm = value) }
    fun setCurrentWeight(value: String) = update { copy(currentWeightKg = value) }
    fun setTargetWeight(value: String) = update { copy(targetWeightKg = value) }
    fun setActivityFactor(value: Float) = update { copy(activityFactor = value) }
    fun setGoalType(value: GoalType) = update { copy(goalType = value) }
    fun setRate(value: String) = update { copy(rateKgPerWeek = value) }
    fun setTimezone(value: String) = update { copy(timezone = value) }
    fun setWeightEntry(value: String) = update { copy(weightEntry = value) }

    fun save() {
        val state = _uiState.value
        val birthDate = runCatching { LocalDate.parse(state.birthDate) }.getOrNull()
        val height = state.heightCm.toDoubleOrNull()
        val targetWeight = state.targetWeightKg.toDoubleOrNull()
        val rate = state.rateKgPerWeek.toDoubleOrNull()
        if (birthDate == null || height == null || targetWeight == null || rate == null) {
            update { copy(error = "Fill in date of birth (YYYY-MM-DD), height, target weight and rate.") }
            return
        }

        viewModelScope.launch {
            update { copy(busy = true, error = null, saved = false) }
            runCatching {
                profile.updateProfile(
                    sex = state.sex,
                    birthDate = birthDate,
                    heightCm = height,
                    activityFactor = state.activityFactor.toDouble(),
                    goalType = state.goalType,
                    targetWeightKg = targetWeight,
                    rateKgPerWeek = rate,
                    timezone = state.timezone,
                    currentWeightKg = state.currentWeightKg.toDoubleOrNull(),
                )
            }.onSuccess { (_, warnings) ->
                update { copy(busy = false, saved = true, warnings = warnings) }
                profile.me.value?.let { prefill(it) }
            }.onFailure {
                update { copy(busy = false) }
                fail("Couldn't save", it)
            }
        }
    }

    /** Logging a weight recomputes the targets, since they are derived from body data. */
    fun logWeight() {
        val kg = _uiState.value.weightEntry.toDoubleOrNull() ?: run {
            update { copy(error = "That isn't a weight.") }
            return
        }
        viewModelScope.launch {
            update { copy(busy = true, error = null) }
            runCatching { profile.logWeight(kg) }
                .onSuccess { recomputed ->
                    update {
                        copy(
                            busy = false,
                            weightEntry = "",
                            currentWeightKg = fmt(kg),
                            warnings = if (recomputed) listOf("Targets recomputed from the new weight.") else warnings,
                        )
                    }
                }
                .onFailure {
                    update { copy(busy = false) }
                    fail("Couldn't log that", it)
                }
        }
    }

    /** Clears the session, the queue and every cached row on this device. */
    fun logout() {
        viewModelScope.launch {
            scheduler.cancelAll()
            runCatching { meals.wipe() }
            profile.clear()
            authRepository.logout()
            update { copy(loggedOut = true) }
        }
    }

    fun dismissError() = update { copy(error = null, saved = false) }

    /** Names the failure on screen and writes the exception itself to logcat. */
    private fun fail(what: String, t: Throwable) {
        val failure = ApiFailures.report(TAG, what, t)
        update { copy(error = "$what: ${failure.message}") }
    }

    private inline fun update(block: ProfileUiState.() -> ProfileUiState) {
        _uiState.value = _uiState.value.block()
    }

    private companion object {
        fun fmt(value: Double): String =
            if (value % 1.0 == 0.0) value.toInt().toString() else String.format("%.1f", value)
    }
}
