package dev.picweight.android.update

import java.io.File
import java.io.IOException
import java.security.MessageDigest
import javax.inject.Inject
import javax.inject.Singleton

/** The outcome of inspecting a downloaded APK. Only [Ok] may be installed. */
sealed interface ApkVerdict {

    /** Digest matched the advertisement and the signer is the one running on this phone. */
    data object Ok : ApkVerdict

    /** The bytes on disk are not the bytes the server said it was serving. */
    data class DigestMismatch(val expected: String, val actual: String) : ApkVerdict

    /** Signed by a different key. Loud, and terminal. */
    data class SignatureMismatch(val running: Set<String>, val downloaded: Set<String>) : ApkVerdict

    /** A well-signed APK, but for some other app. */
    data class WrongPackage(val expected: String, val actual: String) : ApkVerdict

    /** Not a readable APK, or the platform would not tell us who signed something. */
    data class Unreadable(val reason: String) : ApkVerdict
}

/**
 * The gate between "a file has been downloaded" and "the system installer is invoked".
 *
 * ### Why the signature check exists even though Android already enforces it
 *
 * This feature makes the app fetch executable code over the network and hand it to
 * the package installer. Android will refuse to *update* an installed app with an
 * APK signed by a different key — that guarantee is real and it is the reason the
 * feature is safe at all. This class does not replace it; it front-runs it, for two
 * reasons that are worth the ~40 lines:
 *
 *  1. **The failure is legible.** The platform's rejection surfaces as a generic
 *     "App not installed" with no cause, after the user has waited through a 25MB
 *     download and confirmed a system dialog. Checking first turns that into one
 *     sentence naming the actual problem, at the moment it is still actionable.
 *  2. **It is the check a reviewer looks for.** "Downloads an APK from the network
 *     and installs it" and "downloads an APK, proves it is the same publisher as the
 *     code already running, and installs it" are different features. Relying on a
 *     downstream component to catch the bad case leaves nothing in *this* codebase
 *     that says the bad case was considered.
 *
 * ### Order
 *
 * Digest first, signature second. The digest check is cheap, needs no platform
 * services, and tells you the file is byte-identical to the artefact the server
 * described — so if it fails, nothing else is worth asking, and in particular the
 * platform is never asked to parse a file of unknown provenance. Neither check is
 * skippable: a failure of either returns a non-[ApkVerdict.Ok] verdict and the caller
 * must not install.
 */
@Singleton
class ApkVerifier @Inject constructor(
    private val signatures: ApkSignatures,
) {

    /**
     * Verifies [file] against the [expectedSha256] the server advertised.
     *
     * Never throws for a bad file: an unreadable or truncated download is a verdict,
     * not an exception, because every caller has to handle "it did not check out"
     * anyway and an escaping IOException would just be a second way to say so.
     */
    fun verify(file: File, expectedSha256: String): ApkVerdict {
        // ---- (a) the bytes are the bytes that were advertised --------------
        val actual = try {
            sha256(file)
        } catch (e: IOException) {
            return ApkVerdict.Unreadable("couldn't read the download (${e.message})")
        }
        if (!actual.equals(expectedSha256, ignoreCase = true)) {
            return ApkVerdict.DigestMismatch(expected = expectedSha256.lowercase(), actual = actual)
        }

        // ---- (b) the publisher is the publisher already on this phone ------
        val running = signatures.runningApp()
            ?: return ApkVerdict.Unreadable("couldn't read this app's own signing certificate")
        val downloaded = signatures.archive(file)
            ?: return ApkVerdict.Unreadable("the download isn't a readable APK")

        if (downloaded.packageName != running.packageName) {
            return ApkVerdict.WrongPackage(
                expected = running.packageName,
                actual = downloaded.packageName,
            )
        }
        // Set intersection, not equality: a key rotation leaves the installed app with
        // a certificate history the new APK's contents signer legitimately appears in.
        // Any overlap is the platform's own definition of "same publisher".
        if (downloaded.signerDigests.intersect(running.signerDigests).isEmpty()) {
            return ApkVerdict.SignatureMismatch(
                running = running.signerDigests,
                downloaded = downloaded.signerDigests,
            )
        }

        return ApkVerdict.Ok
    }

    /** Streamed, because the APK is tens of megabytes and this also runs on old phones. */
    private fun sha256(file: File): String {
        val digest = MessageDigest.getInstance("SHA-256")
        file.inputStream().use { input ->
            val buffer = ByteArray(DEFAULT_BUFFER_SIZE)
            while (true) {
                val read = input.read(buffer)
                if (read <= 0) break
                digest.update(buffer, 0, read)
            }
        }
        return digest.digest().toHex()
    }
}

/** The one-line reason shown to the user when a verdict refuses the install. */
fun ApkVerdict.refusalMessage(): String? = when (this) {
    ApkVerdict.Ok -> null

    is ApkVerdict.DigestMismatch ->
        "Refused to install: the download doesn't match the checksum the server " +
            "published. Something altered it in transit, or the server's copy changed " +
            "mid-download."

    is ApkVerdict.SignatureMismatch ->
        "Refused to install: that APK is signed by a different key than the copy of " +
            "picweight running on this phone. Only a build from the same signing key " +
            "can update this app."

    is ApkVerdict.WrongPackage ->
        "Refused to install: that APK is $actual, not $expected."

    is ApkVerdict.Unreadable -> "Refused to install: $reason."
}
