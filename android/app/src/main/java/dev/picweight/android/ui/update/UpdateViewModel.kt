package dev.picweight.android.ui.update

import android.content.Intent
import androidx.lifecycle.ViewModel
import androidx.lifecycle.viewModelScope
import dagger.hilt.android.lifecycle.HiltViewModel
import dev.picweight.android.data.repository.UpdateRepository
import dev.picweight.android.ui.common.ApiFailure
import dev.picweight.android.ui.common.FailureKind
import dev.picweight.android.update.InstallState
import dev.picweight.android.update.UpdateState
import kotlinx.coroutines.flow.SharingStarted
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.combine
import kotlinx.coroutines.flow.stateIn
import kotlinx.coroutines.launch
import javax.inject.Inject

/** Everything the update row needs to draw itself. */
data class UpdateUiState(
    val runningVersionName: String,
    val runningVersionCode: Int,
    val update: UpdateState,
    val install: InstallState,
) {
    /** True while a check or a download is in flight, so the button can be disabled. */
    val busy: Boolean
        get() = update is UpdateState.Checking ||
            install is InstallState.Downloading ||
            install is InstallState.Verifying
}

/**
 * Copy for a failed update check.
 *
 * Every line names the update check specifically. That is the point: the app has one
 * "Offline — showing what this phone knows" sentence and it belongs to the home
 * screen, where Room genuinely is the best available answer. An update check has no
 * cached answer and nothing degrades when it fails, so it says so in its own words and
 * never borrows the app-wide outage phrasing.
 */
object UpdateCopy {

    fun forFailure(failure: ApiFailure): String = when (failure.kind) {
        FailureKind.OFFLINE -> "Couldn't reach the server to check for updates."
        FailureKind.TIMEOUT -> "The server didn't answer the update check in time."
        FailureKind.UNAUTHORIZED -> "The server wouldn't answer the update check for this session."
        // A contract mismatch on *this* endpoint is faintly funny — the app is too old
        // to understand the answer to "are you too old" — so it is worth saying plainly.
        FailureKind.CONTRACT -> failure.message
        else -> "Couldn't check for updates: ${failure.message}"
    }
}

/**
 * The "Check for updates" row.
 *
 * Reads shared state from [UpdateRepository] rather than checking on its own, so the
 * app-start check has usually already answered by the time this screen opens and the
 * row is populated instantly.
 */
@HiltViewModel
class UpdateViewModel @Inject constructor(
    private val updates: UpdateRepository,
) : ViewModel() {

    val uiState: StateFlow<UpdateUiState> = combine(
        updates.state,
        updates.install,
    ) { update, install ->
        UpdateUiState(
            runningVersionName = updates.runningVersion.versionName,
            runningVersionCode = updates.runningVersion.versionCode,
            update = update,
            install = install,
        )
    }.stateIn(
        scope = viewModelScope,
        started = SharingStarted.WhileSubscribed(5_000),
        initialValue = UpdateUiState(
            runningVersionName = updates.runningVersion.versionName,
            runningVersionCode = updates.runningVersion.versionCode,
            update = updates.state.value,
            install = updates.install.value,
        ),
    )

    /** The manual check. Reports failure, unlike the app-start one. */
    fun check() {
        viewModelScope.launch { updates.check() }
    }

    /**
     * Accepts the update: download, verify, then Android's own confirmation.
     *
     * Handed to the repository's own scope rather than [viewModelScope] — navigating
     * away mid-download must not cancel a transfer the user asked for.
     */
    fun install(available: UpdateState.Available) = updates.startInstall(available)

    fun dismiss() = updates.dismissInstall()

    /** Where to send the user when "install unknown apps" is off. */
    fun permissionSettingsIntent(): Intent = updates.permissionSettingsIntent()
}
