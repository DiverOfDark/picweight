package dev.picweight.android.data.repository

import android.util.Log
import dev.picweight.android.data.remote.PicweightApi
import dev.picweight.android.data.remote.model.ClientVersionResponse
import dev.picweight.android.ui.common.ApiFailures
import dev.picweight.android.update.ApkDownloader
import dev.picweight.android.update.ApkInstaller
import dev.picweight.android.update.ApkVerdict
import dev.picweight.android.update.ApkVerifier
import dev.picweight.android.update.InstallOutcome
import dev.picweight.android.update.InstallOutcomes
import dev.picweight.android.update.InstallState
import dev.picweight.android.update.RunningVersion
import dev.picweight.android.update.ServerBuild
import dev.picweight.android.update.UpdateCheck
import dev.picweight.android.update.UpdateState
import dev.picweight.android.update.refusalMessage
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.Job
import kotlinx.coroutines.SupervisorJob
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.launch
import retrofit2.HttpException
import javax.inject.Inject
import javax.inject.Singleton

/**
 * In-app update: is the server shipping a newer APK than the one running, and if so,
 * fetch it, prove it is ours, and hand it to the platform installer.
 *
 * The whole feature is one repository because the steps are not independent — the
 * digest that verifies the download is part of the same advertisement that decided an
 * update exists, and skipping a step means installing code from the network on the
 * strength of a filename.
 *
 * Nothing here is ever allowed to break the app. A server that has no such endpoint,
 * no APK, or no network at all resolves to "no update", and the check runs off the
 * hot path so a slow answer delays nothing.
 */
@Singleton
class UpdateRepository @Inject constructor(
    private val api: PicweightApi,
    private val auth: AuthRepository,
    private val running: RunningVersion,
    private val downloader: ApkDownloader,
    private val verifier: ApkVerifier,
    private val installer: ApkInstaller,
    private val outcomes: InstallOutcomes,
) {

    private val _state = MutableStateFlow<UpdateState>(UpdateState.Unknown)

    /** The last answer to "is there an update". Survives navigation; shared app-wide. */
    val state: StateFlow<UpdateState> = _state.asStateFlow()

    private val _install = MutableStateFlow<InstallState>(InstallState.Idle)

    /** Progress of an update the user accepted. */
    val install: StateFlow<InstallState> = _install.asStateFlow()

    /** What this build is, for the "you're running X" line. */
    val runningVersion: RunningVersion get() = running

    private var lastCheckedAt: Long = 0L

    /**
     * The install runs here, not in a ViewModel's scope.
     *
     * Backing out of the settings screen mid-download must not cancel a 25MB transfer
     * the user explicitly asked for — and must not leave [install] frozen on a
     * progress value that will never advance again.
     */
    private val scope = CoroutineScope(SupervisorJob() + Dispatchers.IO)

    private var installJob: Job? = null

    /**
     * The app-start check.
     *
     * Throttled and silent: it exists so the profile screen already knows the answer
     * when the user opens it, not to interrupt anyone. Every failure is swallowed
     * after classification — an update check is the least important thing the app
     * does, and a red banner because the server was briefly unreachable is exactly
     * the "everything is offline" noise this codebase already removed once.
     */
    suspend fun checkQuietly() {
        if (auth.getServerUrl() == null) return
        val now = System.currentTimeMillis()
        if (_state.value !is UpdateState.Unknown && now - lastCheckedAt < MIN_INTERVAL_MS) return
        check()
    }

    /**
     * The "Check for updates" button.
     *
     * Always talks to the server, and reports what happened — including failure,
     * which the manual path should show because the user just asked.
     */
    suspend fun check(): UpdateState {
        _state.value = UpdateState.Checking
        val result = try {
            UpdateCheck.decide(running, advertised(api.getClientVersion()))
        } catch (e: HttpException) {
            // 404 is the answer from a server predating this endpoint. "Your server is
            // too old to tell you about updates" is still "no update available", and
            // failing here would put an error in front of every user of an older
            // deployment for no actionable reason.
            if (e.code() == 404) {
                Log.i(TAG, "Server has no /api/v1/client/version — treating as up to date")
                UpdateState.UpToDate
            } else {
                UpdateState.Failed(ApiFailures.report(TAG, "Checking for updates", e))
            }
        } catch (e: Exception) {
            UpdateState.Failed(ApiFailures.report(TAG, "Checking for updates", e))
        }
        lastCheckedAt = System.currentTimeMillis()
        _state.value = result
        return result
    }

    /**
     * Projects the generated wire model onto the domain, or null when the server has
     * nothing to offer.
     *
     * `available == false` is the backend's explicit "this image bundles no APK"; the
     * `version_code <= 0` arm covers a sidecar that was written but is nonsense, which
     * must not be dressed up as a real release.
     *
     * Every field is `required` in the spec and `@Nonnull` in the generated model, so
     * they are read straight through. A server that omitted one would blow up here on
     * unboxing — which [check] catches and classifies like any other bad payload,
     * rather than quietly producing a half-built advertisement.
     */
    private fun advertised(response: ClientVersionResponse): ServerBuild? {
        if (!response.available || response.versionCode <= 0) return null
        return ServerBuild(
            versionName = response.versionName,
            versionCode = response.versionCode,
            sha256 = response.sha256,
            sizeBytes = response.sizeBytes,
            downloadPath = response.downloadPath,
        )
    }

    /**
     * Starts [downloadAndInstall] on the repository's own scope, at most once at a time.
     *
     * A second tap while one is already running is a double-tap, not a request for two
     * downloads into the same file.
     */
    fun startInstall(available: UpdateState.Available) {
        if (installJob?.isActive == true) return
        installJob = scope.launch { downloadAndInstall(available) }
    }

    /**
     * Downloads, verifies and installs [available] — in that order, refusing at the
     * first thing that does not check out.
     *
     * The two verification steps are not advisory. `PackageInstaller` is only reached
     * from the single branch below where the digest matched *and* the APK is signed by
     * the key running on this phone; there is no path around either.
     */
    suspend fun downloadAndInstall(available: UpdateState.Available) {
        if (!installer.canInstall()) {
            // Nothing to fail yet — the platform simply will not show its dialog until
            // the user has granted "install unknown apps", so say that rather than
            // downloading 25MB and then discovering it.
            _install.value = InstallState.PermissionRequired
            return
        }

        outcomes.reset()
        _install.value = InstallState.Downloading(0L, available.sizeBytes)

        val file = try {
            downloader.download(available) { read, total ->
                _install.value = InstallState.Downloading(read, total)
            }
        } catch (e: Exception) {
            val failure = ApiFailures.report(TAG, "Downloading update ${available.versionName}", e)
            _install.value = InstallState.Failed(failure.message)
            return
        }

        _install.value = InstallState.Verifying
        val verdict = verifier.verify(file, available.sha256)
        if (verdict != ApkVerdict.Ok) {
            // Loud, and the file goes: keeping an APK that failed verification around
            // is keeping an artefact whose provenance is unknown.
            Log.e(TAG, "Refusing to install ${available.versionName}: $verdict")
            file.delete()
            _install.value = InstallState.Refused(
                verdict.refusalMessage() ?: "Refused to install: the download didn't verify."
            )
            return
        }

        try {
            // Published *before* the commit, not after. The platform's status broadcast
            // can land while `install` is still returning, and setting this afterwards
            // would overwrite a result that had already arrived — turning a completed
            // install back into "waiting for confirmation" that never resolves.
            _install.value = InstallState.AwaitingConfirmation
            installer.install(file)
        } catch (e: Exception) {
            val failure = ApiFailures.report(TAG, "Installing update ${available.versionName}", e)
            _install.value = InstallState.Failed(failure.message)
        }
    }

    /** Folds a platform install callback into [install]. */
    fun onInstallOutcome(outcome: InstallOutcome) {
        _install.value = when (outcome) {
            InstallOutcome.AwaitingUser -> InstallState.AwaitingConfirmation
            InstallOutcome.Installed -> InstallState.Installed
            InstallOutcome.Declined -> InstallState.Declined
            is InstallOutcome.Failed -> InstallState.Failed(outcome.message)
        }
    }

    /** Returns to [InstallState.Idle] after the user has read whatever happened. */
    fun dismissInstall() {
        outcomes.reset()
        _install.value = InstallState.Idle
    }

    /** The Settings screen that turns on "install unknown apps" for this app. */
    fun permissionSettingsIntent() = installer.permissionSettingsIntent()

    private companion object {
        const val TAG = "UpdateRepository"

        /**
         * The app is foregrounded many times a day; the server ships a new APK at most
         * a few times a week. An hour between automatic checks is generous.
         */
        const val MIN_INTERVAL_MS = 60L * 60L * 1000L
    }
}
