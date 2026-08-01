package dev.picweight.android.update

/**
 * Where an accepted update has got to.
 *
 * Separate from [UpdateState] on purpose: "is there an update" and "what is happening
 * to the one I accepted" have different lifetimes, and folding them together would
 * mean a failed install erased the knowledge that an update exists.
 */
sealed interface InstallState {

    /** Nothing in flight. */
    data object Idle : InstallState

    /** Streaming the APK into the app's private cache. */
    data class Downloading(val bytesRead: Long, val totalBytes: Long) : InstallState {
        /** 0f..1f, or 0f while the total is still unknown. */
        val fraction: Float
            get() = if (totalBytes > 0L) (bytesRead.toFloat() / totalBytes).coerceIn(0f, 1f) else 0f
    }

    /** Checking the digest, then the signing certificate. Brief, but not instant on a big APK. */
    data object Verifying : InstallState

    /** Committed to [android.content.pm.PackageInstaller]; Android's own dialog is up. */
    data object AwaitingConfirmation : InstallState

    /** Done. Shown briefly, if the app survives long enough to render it. */
    data object Installed : InstallState

    /** The user said no at the system prompt. Their call; stated plainly, not as an error. */
    data object Declined : InstallState

    /**
     * A verification check said no.
     *
     * Distinct from [Failed] because the two mean different things to the user: this
     * one says the artefact is not what it claimed to be and *must not* be retried
     * blindly, where [Failed] usually just means "try again on better Wi-Fi".
     */
    data class Refused(val reason: String) : InstallState

    /** The download or the installer didn't work. Retryable. */
    data class Failed(val reason: String) : InstallState

    /**
     * "Install unknown apps" is off for this app.
     *
     * Its own state because the fix is one toggle in Settings, and saying so beats a
     * generic failure — the platform gives no prompt of its own until the permission
     * is granted.
     */
    data object PermissionRequired : InstallState
}
