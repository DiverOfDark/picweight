package dev.picweight.android.update

import dev.picweight.android.ui.common.ApiFailure
import dev.picweight.android.ui.common.FailureKind

/**
 * The build running in this process.
 *
 * A wrapper around `BuildConfig.VERSION_CODE` / `VERSION_NAME` rather than a direct
 * read of them, so [UpdateCheck.decide] — the one rule that decides whether the user
 * is offered code fetched over the network — can be exercised on the JVM against
 * every ordering, instead of only against whatever this particular build happens to
 * be numbered.
 */
data class RunningVersion(val versionCode: Int, val versionName: String)

/**
 * What the server advertises about the APK it ships, as read from
 * `GET /api/v1/client/version`.
 *
 * [sha256] and [downloadPath] are part of the advertisement and not derived here: the
 * client must never hardcode `/picweight.apk`, and the digest has to come from the
 * same response that named the version, or "verified" would only mean "downloaded
 * whatever the download said it was".
 */
data class ServerBuild(
    val versionName: String,
    val versionCode: Int,
    val sha256: String,
    val sizeBytes: Long,
    val downloadPath: String,
)

/** Where the update check got to. */
sealed interface UpdateState {

    /** Nothing has been checked yet — no server configured, or the app just started. */
    data object Unknown : UpdateState

    /** A check is in flight. */
    data object Checking : UpdateState

    /**
     * The server ships nothing newer. This is also the answer for a backend-only
     * deployment that bundles no APK at all, and for a server too old to serve the
     * endpoint: "there is no update" is true in every one of those cases, and calling
     * any of them an error would put a red banner in front of a working app.
     */
    data object UpToDate : UpdateState

    /** A strictly newer build is on the server, verifiable and ready to fetch. */
    data class Available(
        val versionName: String,
        val versionCode: Int,
        val sizeBytes: Long,
        /** Digest the downloaded file must hash to before it is allowed anywhere near the installer. */
        val sha256: String,
        /** Server-supplied download location, so the APK path lives in exactly one place. */
        val downloadPath: String,
    ) : UpdateState

    /**
     * The check itself failed. Carries the classified [ApiFailure] so the UI can say
     * what actually went wrong instead of falling back to "Offline" for everything —
     * the confusion this codebase has already paid to remove once.
     */
    data class Failed(val failure: ApiFailure) : UpdateState
}

/**
 * The rule that decides whether an update is offered.
 *
 * Pure, and deliberately its own object: this is the security-relevant half of the
 * feature that can be tested without a device. Everything downstream — download,
 * digest check, signature check, install — only ever runs because this function
 * returned [UpdateState.Available].
 */
object UpdateCheck {

    /** A sha-256 digest, lowercase or upper, is exactly 64 hex characters. Nothing else is one. */
    private val SHA256 = Regex("^[0-9a-fA-F]{64}$")

    /**
     * Compares [running] against what the server [advertised].
     *
     * **Strictly greater, or nothing.** Equality must not prompt (the user is already
     * on that build and a dialog offering it is noise), and a lower code must never be
     * offered — a server rolled back to an older image would otherwise walk every
     * client backwards, and Android would reject the downgrade anyway after the user
     * had already sat through a download.
     *
     * [advertised] is null when the server has no APK to talk about; that is "no
     * update available", never a failure.
     */
    fun decide(running: RunningVersion, advertised: ServerBuild?): UpdateState {
        if (advertised == null) return UpdateState.UpToDate
        if (advertised.versionCode <= running.versionCode) return UpdateState.UpToDate

        // Newer, but the advertisement is not something this app can verify. Refusing
        // is not optional: without a well-formed digest and a size to bound the
        // download, "verify before install" has nothing to verify against. Reported as
        // a CONTRACT failure because that is precisely what it is — the server replied
        // and the two halves disagree about the payload — and never as UpToDate, which
        // would hide a broken release pipeline behind a reassuring green tick.
        unverifiable(advertised)?.let { why ->
            return UpdateState.Failed(
                ApiFailure(
                    FailureKind.CONTRACT,
                    "The server advertised build ${advertised.versionCode} but $why, " +
                        "so this app can't verify the download. Not installing it.",
                )
            )
        }

        return UpdateState.Available(
            versionName = advertised.versionName,
            versionCode = advertised.versionCode,
            sizeBytes = advertised.sizeBytes,
            sha256 = advertised.sha256,
            downloadPath = advertised.downloadPath,
        )
    }

    /** Why [build] cannot be installed safely, or null when it can. */
    private fun unverifiable(build: ServerBuild): String? = when {
        !SHA256.matches(build.sha256) -> "its sha256 is not a digest"
        build.sizeBytes <= 0L -> "it declares no size"
        build.downloadPath.isBlank() -> "it names no download location"
        build.versionName.isBlank() -> "it has no version name"
        else -> null
    }
}
