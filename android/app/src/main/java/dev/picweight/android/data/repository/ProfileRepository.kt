package dev.picweight.android.data.repository

import dev.picweight.android.data.remote.PicweightApi
import dev.picweight.android.data.remote.model.GoalType
import dev.picweight.android.data.remote.model.LogWeightRequest
import dev.picweight.android.data.remote.model.MeResponse
import dev.picweight.android.data.remote.model.ProfileResponse
import dev.picweight.android.data.remote.model.Sex
import dev.picweight.android.data.remote.model.UpdateProfileRequest
import dev.picweight.android.data.remote.model.WeightSource
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import java.time.LocalDate
import javax.inject.Inject
import javax.inject.Singleton

/**
 * Identity, body data and the derived targets.
 *
 * Targets are a formula, not a model (PRD §6) — the backend computes them and this
 * class only carries them; nothing here does arithmetic on kcal.
 */
@Singleton
class ProfileRepository @Inject constructor(
    private val api: PicweightApi,
    private val meals: MealRepository,
) {
    private val _me = MutableStateFlow<MeResponse?>(null)

    /** Last known `/api/v1/me`, or null before the first successful load. */
    val me: StateFlow<MeResponse?> = _me.asStateFlow()

    /** True once onboarding has produced a profile. */
    val hasProfile: Boolean
        get() = _me.value?.profile != null

    suspend fun refresh(): MeResponse {
        val response = api.getMe()
        _me.value = response
        meals.cacheDayState(response.today)
        return response
    }

    /** Onboarding and profile edit are the same call: the formulas need every field. */
    suspend fun updateProfile(
        sex: Sex,
        birthDate: LocalDate,
        heightCm: Double,
        activityFactor: Double,
        goalType: GoalType,
        targetWeightKg: Double,
        rateKgPerWeek: Double,
        timezone: String,
        currentWeightKg: Double?,
    ): Pair<ProfileResponse, List<String>> {
        val request = UpdateProfileRequest().apply {
            this.sex = sex
            this.birthDate = birthDate
            this.heightCm = heightCm
            this.activityFactor = activityFactor
            this.goalType = goalType
            this.targetWeightKg = targetWeightKg
            this.rateKgPerWeek = rateKgPerWeek
            this.timezone = timezone
            this.currentWeightKg = currentWeightKg
        }
        val response = api.updateProfile(request)
        refresh()
        // An aggressive deficit is warned about, never silently accepted (§6).
        return response.profile to response.warnings.orEmpty()
    }

    /** Logging a weight recomputes the targets server-side, so refresh after it. */
    suspend fun logWeight(weightKg: Double): Boolean {
        val response = api.logWeight(
            LogWeightRequest().apply {
                this.weightKg = weightKg
                this.source = WeightSource.MANUAL
            }
        )
        refresh()
        return response.targetsRecomputed
    }

    fun clear() {
        _me.value = null
    }
}
