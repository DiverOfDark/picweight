package dev.picweight.android.ui.meal

import androidx.lifecycle.SavedStateHandle
import androidx.lifecycle.ViewModel
import androidx.lifecycle.viewModelScope
import dagger.hilt.android.lifecycle.HiltViewModel
import dev.picweight.android.data.ConnectivityMonitor
import dev.picweight.android.data.local.MealEntity
import dev.picweight.android.data.local.MealItemEntity
import dev.picweight.android.data.remote.model.PatchMealItem
import dev.picweight.android.data.remote.model.RevisionEntry
import dev.picweight.android.data.repository.AuthRepository
import dev.picweight.android.data.repository.MealRepository
import dev.picweight.android.ui.common.ApiFailures
import dev.picweight.android.ui.common.MealImage
import kotlinx.coroutines.ExperimentalCoroutinesApi
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.SharingStarted
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.combine
import kotlinx.coroutines.flow.emptyFlow
import kotlinx.coroutines.flow.flatMapLatest
import kotlinx.coroutines.flow.flowOf
import kotlinx.coroutines.flow.stateIn
import kotlinx.coroutines.launch
import java.net.URLDecoder
import java.nio.charset.StandardCharsets
import javax.inject.Inject

private const val TAG = "MealDetailViewModel"

data class MealDetailUiState(
    val meal: MealEntity? = null,
    val items: List<MealItemEntity> = emptyList(),
    val revisions: List<RevisionEntry> = emptyList(),
    val feedback: String = "",
    val showRevisions: Boolean = false,
    val busy: Boolean = false,
    val error: String? = null,
    val message: String? = null,
    val deleted: Boolean = false,
    /** See `HomeUiState.online`: gates the "waiting for a connection" badge. */
    val online: Boolean = true,
)

/**
 * One meal, and the two ways to fix it.
 *
 * The cheap fix is a gram tweak or a portion scale. The one that actually converges is
 * the free-text field: it resumes the persisted agent session rather than restarting
 * it, so "too much rice, about half that" is interpreted against the model's own prior
 * reasoning and costs one short turn instead of a second full loop (PRD §5).
 */
@OptIn(ExperimentalCoroutinesApi::class)
@HiltViewModel
class MealDetailViewModel @Inject constructor(
    private val meals: MealRepository,
    private val authRepository: AuthRepository,
    connectivity: ConnectivityMonitor,
    savedStateHandle: SavedStateHandle,
) : ViewModel() {

    private val rawId: String = savedStateHandle.get<String>("id")
        ?.let { URLDecoder.decode(it, StandardCharsets.UTF_8.name()) }
        .orEmpty()
    private val byServerId: Boolean = savedStateHandle.get<String>("server") == "true"

    /** Resolved once: a notification carries a server id, navigation carries a local one. */
    private val clientUuid = MutableStateFlow<String?>(if (byServerId) null else rawId)

    private val feedback = MutableStateFlow("")
    private val showRevisions = MutableStateFlow(false)
    private val revisions = MutableStateFlow<List<RevisionEntry>>(emptyList())
    private val busy = MutableStateFlow(false)
    private val error = MutableStateFlow<String?>(null)
    private val message = MutableStateFlow<String?>(null)
    private val deleted = MutableStateFlow(false)

    private val mealFlow = clientUuid.flatMapLatest { uuid ->
        if (uuid == null) flowOf(null) else meals.flowMeal(uuid)
    }
    private val itemsFlow = clientUuid.flatMapLatest { uuid ->
        if (uuid == null) emptyFlow() else meals.flowItems(uuid)
    }

    val uiState: StateFlow<MealDetailUiState> = combine(
        mealFlow,
        itemsFlow,
        combine(feedback, showRevisions, revisions) { f, s, r -> Triple(f, s, r) },
        combine(busy, error, message) { b, e, m -> Triple(b, e, m) },
        combine(deleted, connectivity.online) { d, on -> d to on },
    ) { meal, items, (feedbackText, revisionsOpen, revisionList), (isBusy, err, msg), (isDeleted, isOnline) ->
        MealDetailUiState(
            meal = meal,
            items = items,
            revisions = revisionList,
            feedback = feedbackText,
            showRevisions = revisionsOpen,
            busy = isBusy,
            error = err,
            message = msg,
            deleted = isDeleted,
            online = isOnline,
        )
    }.stateIn(viewModelScope, SharingStarted.WhileSubscribed(5_000), MealDetailUiState())

    init {
        viewModelScope.launch {
            if (byServerId) {
                // Came in from a notification: pull the meal so Room has it, then bind.
                val resolved = runCatching { meals.refreshMeal(rawId) }.getOrNull()
                    ?: runCatching { meals.mealByServerId(rawId) }.getOrNull()
                clientUuid.value = resolved?.clientUuid
                if (resolved == null) error.value = "That meal isn't on this phone yet."
            } else {
                // Room already renders through the Flow; this only pulls the newest
                // revision's items and reasoning notes in behind it.
                meals.mealByClientUuid(rawId)?.serverId
                    ?.let { runCatching { meals.refreshMeal(it) } }
            }
        }
    }

    /**
     * The local capture until the server has a thumbnail of its own. Without the local
     * leg, a meal that has not finished uploading shows no image here at all.
     */
    fun thumbnailModel(meal: MealEntity): Any? = MealImage.model(meal, authRepository::absoluteUrl)

    fun setFeedback(value: String) {
        feedback.value = value
    }

    /** Sends the correction and resumes the agent session. */
    fun submitFeedback() {
        val serverId = uiState.value.meal?.serverId ?: run {
            error.value = "This meal hasn't reached the server yet."
            return
        }
        val text = feedback.value.trim()
        if (text.isEmpty()) return
        viewModelScope.launch {
            busy.value = true
            error.value = null
            runCatching { meals.reanalyze(serverId, text) }
                .onSuccess {
                    feedback.value = ""
                    message.value = "Sent. The agent is picking up where it left off."
                }
                .onFailure { fail("Couldn't send that", it) }
            busy.value = false
        }
    }

    /** Confirming is what feeds the flywheel: only confirmed meals are ever recalled. */
    fun confirm() {
        val serverId = uiState.value.meal?.serverId ?: return
        viewModelScope.launch {
            busy.value = true
            runCatching { meals.confirm(serverId) }
                .onFailure { fail("Couldn't confirm", it) }
            busy.value = false
        }
    }

    fun setPortionScale(scale: Double) {
        val serverId = uiState.value.meal?.serverId ?: return
        viewModelScope.launch {
            busy.value = true
            runCatching { meals.setPortionScale(serverId, scale) }
                .onFailure { fail("Couldn't update the portion", it) }
            busy.value = false
        }
    }

    /** Edits one item's grams, keeping its macros proportional to the new mass. */
    fun setItemGrams(item: MealItemEntity, grams: Double) {
        val serverId = uiState.value.meal?.serverId ?: return
        val current = uiState.value.items
        if (grams <= 0.0 || item.grams <= 0.0) return
        val ratio = grams / item.grams
        val patched = current.map { existing ->
            val scale = if (existing.position == item.position) ratio else 1.0
            PatchMealItem().apply {
                id = existing.serverId
                name = existing.name
                this.grams = existing.grams * scale
                kcal = existing.kcal * scale
                proteinG = existing.proteinG * scale
                fatG = existing.fatG * scale
                carbsG = existing.carbsG * scale
                barcode = existing.barcode
            }
        }
        viewModelScope.launch {
            busy.value = true
            runCatching { meals.replaceItems(serverId, patched) }
                .onFailure { fail("Couldn't save that edit", it) }
            busy.value = false
        }
    }

    fun toggleRevisions() {
        val opening = !showRevisions.value
        showRevisions.value = opening
        if (!opening) return
        val serverId = uiState.value.meal?.serverId ?: return
        viewModelScope.launch {
            runCatching { meals.revisions(serverId) }
                .onSuccess { revisions.value = it.revisions.orEmpty() }
                .onFailure { fail("Couldn't load the revision history", it) }
        }
    }

    fun delete() {
        val uuid = clientUuid.value ?: return
        viewModelScope.launch {
            runCatching { meals.delete(uuid) }
                .onFailure { fail("Couldn't delete", it) }
            deleted.value = true
        }
    }

    fun dismissMessage() {
        message.value = null
        error.value = null
    }

    /**
     * One place where a failure becomes both a logcat line and a sentence.
     *
     * These messages used to end in `${it.message}`, which is null for most transport
     * failures and a wall of Jackson internals for the rest. [ApiFailures] separates a
     * genuine outage from an HTTP status from a response body this build cannot parse,
     * and writes the exception itself to logcat either way.
     */
    private fun fail(what: String, t: Throwable) {
        error.value = "$what: ${ApiFailures.report(TAG, what, t).message}"
    }
}
