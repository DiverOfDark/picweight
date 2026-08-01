package dev.picweight.android.update

import android.content.Context
import android.content.pm.PackageInfo
import android.content.pm.PackageManager
import android.content.pm.Signature
import android.os.Build
import android.util.Log
import dagger.hilt.android.qualifiers.ApplicationContext
import java.io.File
import java.security.MessageDigest
import javax.inject.Inject
import javax.inject.Singleton

/** Who an APK — installed or on disk — claims to be, and who signed it. */
data class AppIdentity(
    val packageName: String,
    val versionCode: Long,
    /** Lowercase hex SHA-256 of each signing certificate. Order is irrelevant; identity is the set. */
    val signerDigests: Set<String>,
)

/**
 * Reads signing identity out of the platform.
 *
 * An interface because both implementations of "read a certificate" go through
 * [PackageManager], which does not exist on a JVM test runner. Everything that
 * *decides* something based on the answer lives on the other side of this boundary,
 * in [ApkVerifier], and is tested with a fake.
 */
interface ApkSignatures {

    /** The app in this process: the certificate any update must match. */
    fun runningApp(): AppIdentity?

    /** The same facts read out of an APK file, or null when the file is not a readable APK. */
    fun archive(file: File): AppIdentity?
}

/**
 * [ApkSignatures] over the real [PackageManager].
 *
 * `GET_SIGNING_CERTIFICATES` (API 28+) is preferred because it reports the v2/v3
 * signing block and the rotation history; on API 26–27 the only thing available is
 * the deprecated `GET_SIGNATURES`, which is why the fallback is here rather than
 * raising `minSdk`.
 */
@Singleton
class PackageManagerApkSignatures @Inject constructor(
    @param:ApplicationContext private val context: Context,
) : ApkSignatures {

    override fun runningApp(): AppIdentity? = try {
        identity(context.packageManager.getPackageInfo(context.packageName, SIGNING_FLAGS))
    } catch (e: PackageManager.NameNotFoundException) {
        // Cannot happen for our own package short of the platform being broken, but a
        // thrown exception here would abort an update the user asked for, so it is
        // reported as "unknown" and refused politely downstream.
        Log.e(TAG, "Could not read this app's own signing certificate", e)
        null
    }

    override fun archive(file: File): AppIdentity? {
        // getPackageArchiveInfo returns null — it does not throw — for anything that is
        // not a parseable APK, which covers a truncated download and an HTML error page
        // saved under an .apk name.
        val info = context.packageManager.getPackageArchiveInfo(file.absolutePath, SIGNING_FLAGS)
        if (info == null) {
            Log.e(TAG, "Downloaded file is not a parseable APK: ${file.name}")
            return null
        }
        return identity(info)
    }

    private fun identity(info: PackageInfo): AppIdentity? {
        val signatures = signaturesOf(info)
        if (signatures.isEmpty()) {
            Log.e(TAG, "No signing certificate on ${info.packageName}")
            return null
        }
        return AppIdentity(
            packageName = info.packageName,
            versionCode = if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.P) {
                info.longVersionCode
            } else {
                @Suppress("DEPRECATION")
                info.versionCode.toLong()
            },
            signerDigests = signatures.map { it.sha256() }.toSet(),
        )
    }

    private fun signaturesOf(info: PackageInfo): List<Signature> {
        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.P) {
            val signing = info.signingInfo ?: return emptyList()
            // A multi-signer APK has no single "current" certificate, so the comparison
            // has to be over the whole set. For the common single-signer case the
            // history is used, which also lets a rotated key still match.
            return if (signing.hasMultipleSigners()) {
                signing.apkContentsSigners?.toList().orEmpty()
            } else {
                signing.signingCertificateHistory?.toList().orEmpty()
            }
        }
        @Suppress("DEPRECATION")
        return info.signatures?.toList().orEmpty()
    }

    private fun Signature.sha256(): String =
        MessageDigest.getInstance("SHA-256").digest(toByteArray()).toHex()

    private companion object {
        const val TAG = "ApkSignatures"

        val SIGNING_FLAGS: Int
            get() = if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.P) {
                PackageManager.GET_SIGNING_CERTIFICATES
            } else {
                @Suppress("DEPRECATION")
                PackageManager.GET_SIGNATURES
            }
    }
}

/** Lowercase hex, which is the form the sidecar's `sha256` is written in. */
internal fun ByteArray.toHex(): String {
    val out = StringBuilder(size * 2)
    for (b in this) {
        val v = b.toInt() and 0xFF
        out.append(HEX[v ushr 4]).append(HEX[v and 0x0F])
    }
    return out.toString()
}

private const val HEX = "0123456789abcdef"
