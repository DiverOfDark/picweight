package dev.picweight.android.ui.capture

import android.util.Log
import androidx.lifecycle.SavedStateHandle
import androidx.lifecycle.ViewModel
import androidx.lifecycle.viewModelScope
import dagger.hilt.android.lifecycle.HiltViewModel
import dev.picweight.android.data.local.MealEntity
import dev.picweight.android.data.local.RecentDishEntity
import dev.picweight.android.data.remote.model.NameSource
import dev.picweight.android.data.repository.MealRepository
import dev.picweight.android.di.ApplicationScope
import dev.picweight.android.ui.common.ApiFailures
import dev.picweight.android.ui.common.FailureKind
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.SharingStarted
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.combine
import kotlinx.coroutines.flow.stateIn
import kotlinx.coroutines.launch
import kotlinx.coroutines.withContext
import java.io.File
import java.net.URLDecoder
import java.nio.charset.StandardCharsets
import javax.inject.Inject

private const val TAG = "CaptureViewModel"

data class CaptureUiState(
    val dishName: String = "",
    val comment: String = "",
    val commentOpen: Boolean = false,
    val nameSource: NameSource = NameSource.VISION,
    val chips: List<RecentDishEntity> = emptyList(),
    val shots: List<MealEntity> = emptyList(),
    val barcode: String? = null,
    val barcodeProduct: String? = null,
    val busy: Boolean = false,
    val error: String? = null,
    val finished: Boolean = false,
)

/**
 * One sitting, from the first shutter press to "Done".
 *
 * The sitting id is generated up front and every shot in it is stamped with the same
 * one; whether it is actually *sent* is decided at the end, because a sitting of one
 * should get the per-meal notification that leads with its dish name rather than a
 * one-line group summary (PRD §5, §6).
 */
@HiltViewModel
class CaptureViewModel @Inject constructor(
    private val meals: MealRepository,
    @param:ApplicationScope private val appScope: CoroutineScope,
    savedStateHandle: SavedStateHandle,
) : ViewModel() {

    private val sittingId: String = meals.newSitting()

    private val form = MutableStateFlow(FormState())
    private val busy = MutableStateFlow(false)
    private val error = MutableStateFlow<String?>(null)
    private val finished = MutableStateFlow(false)

    val uiState: StateFlow<CaptureUiState> = combine(
        form,
        meals.flowSitting(sittingId),
        meals.flowRecentDishes(CHIP_COUNT),
        combine(busy, error, finished) { b, e, f -> Triple(b, e, f) },
    ) { formState, shots, chips, (isBusy, err, isFinished) ->
        CaptureUiState(
            dishName = formState.dishName,
            comment = formState.comment,
            commentOpen = formState.commentOpen,
            nameSource = formState.nameSource,
            chips = chips,
            shots = shots,
            barcode = formState.barcode,
            barcodeProduct = formState.barcodeProduct,
            busy = isBusy,
            error = err,
            finished = isFinished,
        )
    }.stateIn(viewModelScope, SharingStarted.WhileSubscribed(5_000), CaptureUiState())

    init {
        // A delivery order shared in from another app: the dish name arrives with no
        // typing at all, which is the whole point of the share-sheet path (§1).
        savedStateHandle.get<String>("shared")
            ?.takeIf { it.isNotBlank() }
            ?.let { applySharedText(URLDecoder.decode(it, StandardCharsets.UTF_8.name())) }

        viewModelScope.launch {
            // Stale chips are not worth a banner over — the capture path must never be
            // blocked by the network — but a silent failure here is how "the chips went
            // away" becomes unexplainable, so it always reaches logcat.
            runCatching { meals.refreshRecentDishes(CHIP_COUNT.toLong()) }
                .onFailure { ApiFailures.report(TAG, "Refreshing recent dishes", it) }
        }
    }

    // ---- the zero-keyboard inputs ----------------------------------------

    /** One tap on a chip sets the name. Faster than typing, better than vision (§1). */
    fun pickChip(dish: RecentDishEntity) {
        form.value = form.value.copy(
            dishName = dish.dishName,
            nameSource = NameSource.RECENT_CHIP,
        )
    }

    fun clearDishName() {
        form.value = form.value.copy(dishName = "", nameSource = NameSource.VISION)
    }

    /** Typing the name by hand is the rare path, but it is the highest-signal one. */
    fun setDishName(value: String) {
        form.value = form.value.copy(
            dishName = value,
            nameSource = if (value.isBlank()) NameSource.VISION else NameSource.COMMENT,
        )
    }

    fun toggleComment() {
        form.value = form.value.copy(commentOpen = !form.value.commentOpen)
    }

    fun setComment(value: String) {
        form.value = form.value.copy(comment = value)
    }

    private fun applySharedText(text: String) {
        val trimmed = text.trim()
        val firstLine = trimmed.lineSequence().firstOrNull { it.isNotBlank() }?.trim().orEmpty()
        form.value = form.value.copy(
            dishName = firstLine.take(MAX_SHARED_NAME),
            // Anything past the first line is order detail — worth handing to the agent
            // verbatim rather than throwing away.
            comment = trimmed.substringAfter(firstLine, "").trim(),
            commentOpen = trimmed.length > firstLine.length,
            nameSource = NameSource.SHARE_INTENT,
        )
    }

    /**
     * A scanned barcode is exact where vision is not. The EAN is carried into the
     * comment verbatim so the agent's `lookup_barcode` tool can use the real number
     * rather than re-reading it from a photo.
     */
    fun onBarcodeScanned(ean: String) {
        if (form.value.barcode == ean) return
        form.value = form.value.copy(barcode = ean)
        viewModelScope.launch {
            val food = runCatching { meals.resolveBarcode(ean) }
                .onFailure {
                    val failure = ApiFailures.report(TAG, "Resolving barcode $ean", it)
                    // The EAN is already in the comment, so the agent can still look it
                    // up server-side and an outage costs nothing. Anything else means the
                    // lookup is broken rather than merely unavailable — say so.
                    if (failure.kind != FailureKind.OFFLINE) error.value = failure.message
                }
                .getOrNull()
            val product = food?.let { listOfNotNull(it.brand, it.name).joinToString(" ").trim() }
            form.value = form.value.copy(
                barcodeProduct = product,
                dishName = product?.takeIf { it.isNotBlank() } ?: form.value.dishName,
                nameSource = if (product.isNullOrBlank()) form.value.nameSource else NameSource.COMMENT,
                comment = appendBarcode(form.value.comment, ean),
            )
        }
    }

    fun dismissBarcode() {
        form.value = form.value.copy(barcode = null, barcodeProduct = null)
    }

    // ---- capture ----------------------------------------------------------

    /** A fresh file for CameraX to write into. */
    fun newCaptureFile(): File =
        File(meals.captureDir(), "capture-${System.currentTimeMillis()}.jpg")

    /**
     * Records one shot. Returns immediately once the row is in Room — the upload is
     * WorkManager's problem from here, and the user can shoot the next dish.
     */
    fun onPhotoCaptured(file: File) {
        viewModelScope.launch {
            busy.value = true
            runCatching {
                withContext(Dispatchers.IO) { PhotoProcessor.normalise(file) }
                val state = form.value
                meals.addShot(
                    sittingId = sittingId,
                    photo = file,
                    dishName = state.dishName,
                    comment = state.comment,
                    nameSource = state.effectiveNameSource(hasPhoto = true),
                )
                // Per-shot fields reset; the sitting's own state does not.
                form.value = state.copy(dishName = "", comment = "", commentOpen = false, barcode = null, barcodeProduct = null, nameSource = NameSource.VISION)
            }.onFailure {
                // Deliberately NOT routed through ApiFailures: nothing here touches the
                // network. An IOException on this path is the phone's own storage, and
                // calling that "offline" would be the same lie in a new place.
                Log.e(TAG, "Saving a shot of sitting $sittingId failed", it)
                error.value = "Couldn't save that shot — this phone's storage refused it."
                file.delete()
            }
            busy.value = false
        }
    }

    fun onCaptureFailed(message: String) {
        error.value = message
    }

    /**
     * The no-photo path: "ate something, roughly this". Keeps unphotographed meals in
     * the dataset instead of leaving a systematic hole where they should be (§5).
     */
    fun logWithoutPhoto() {
        val state = form.value
        if (state.dishName.isBlank() && state.comment.isBlank()) {
            error.value = "Give it a name (or tap a recent dish) first."
            return
        }
        viewModelScope.launch {
            busy.value = true
            runCatching {
                meals.addShot(
                    sittingId = sittingId,
                    photo = null,
                    dishName = state.dishName,
                    comment = state.comment,
                    nameSource = state.effectiveNameSource(hasPhoto = false),
                )
                meals.closeSitting(sittingId)
            }.onFailure {
                // Local write again — Room, not the server. The upload is WorkManager's
                // problem and happens later.
                Log.e(TAG, "Saving a no-photo entry failed", it)
                error.value = "Couldn't save that — this phone's storage refused it."
            }
            busy.value = false
            finished.value = true
        }
    }

    /** Ends the sitting: the last shot carries the `group_size` settle hint. */
    fun finish() {
        viewModelScope.launch {
            runCatching { meals.closeSitting(sittingId) }
            finished.value = true
        }
    }

    fun dismissError() {
        error.value = null
    }

    override fun onCleared() {
        super.onCleared()
        // Leaving by the back gesture must still release the held shot, and
        // viewModelScope is already cancelled by the time this runs.
        appScope.launch { runCatching { meals.closeSitting(sittingId) } }
    }

    private data class FormState(
        val dishName: String = "",
        val comment: String = "",
        val commentOpen: Boolean = false,
        val nameSource: NameSource = NameSource.VISION,
        val barcode: String? = null,
        val barcodeProduct: String? = null,
    ) {
        /**
         * `name_source` is instrumentation, not decoration: it is the only way to find
         * out whether the chips and the share sheet were worth building (PRD §14.2).
         */
        fun effectiveNameSource(hasPhoto: Boolean): NameSource = when {
            dishName.isBlank() && !hasPhoto -> NameSource.MANUAL
            dishName.isBlank() -> NameSource.VISION
            // Typed by hand with no photo — that is the manual-entry path, not a comment
            // attached to an image the model is also looking at.
            !hasPhoto && nameSource == NameSource.COMMENT -> NameSource.MANUAL
            else -> nameSource
        }
    }

    private companion object {
        const val CHIP_COUNT = 8
        const val MAX_SHARED_NAME = 80

        fun appendBarcode(comment: String, ean: String): String =
            if (comment.contains(ean)) comment
            else listOf(comment.trim(), "barcode $ean").filter { it.isNotBlank() }.joinToString(", ")
    }
}
