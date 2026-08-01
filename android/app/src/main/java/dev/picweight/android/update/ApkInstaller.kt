package dev.picweight.android.update

import android.app.PendingIntent
import android.content.Context
import android.content.Intent
import android.content.pm.PackageInstaller
import android.os.Build
import android.util.Log
import dagger.hilt.android.qualifiers.ApplicationContext
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.withContext
import java.io.File
import java.io.IOException
import javax.inject.Inject
import javax.inject.Singleton

/**
 * Hands a verified APK to the platform.
 *
 * An interface for the same reason [ApkSignatures] is one — there is no
 * [PackageInstaller] on a JVM test runner — and so that the tests can assert the
 * thing that actually matters: that a file which failed verification never reaches
 * this call.
 */
interface ApkInstaller {

    /** True when the user has granted this app the "install unknown apps" permission. */
    fun canInstall(): Boolean

    /**
     * Opens a [PackageInstaller] session, streams [file] into it and commits.
     *
     * Returns as soon as the session is committed — the install itself is asynchronous
     * and its result arrives on [InstallOutcomes]. Throws [IOException] if the session
     * could not be created or written.
     */
    suspend fun install(file: File)

    /** An Intent that takes the user to the "install unknown apps" toggle for this app. */
    fun permissionSettingsIntent(): Intent
}

/**
 * The real one, over [PackageInstaller]'s session API.
 *
 * The session API — rather than `ACTION_VIEW` on a `content://` URI — is used because
 * the APK never has to leave the app's private cache: the bytes are streamed into the
 * session from this process, so there is no window in which a world-readable file
 * could be swapped between verification and install.
 *
 * The system still shows its own confirmation dialog before anything is installed
 * (delivered as [PackageInstaller.STATUS_PENDING_USER_ACTION], which
 * [InstallStatusReceiver] turns into an activity). That dialog is the point, not an
 * obstacle: this app downloads code, and the user gets the final say on whether it
 * runs. Nothing here tries to route around it.
 */
@Singleton
class PackageInstallerApkInstaller @Inject constructor(
    @param:ApplicationContext private val context: Context,
) : ApkInstaller {

    override fun canInstall(): Boolean = context.packageManager.canRequestPackageInstalls()

    override fun permissionSettingsIntent(): Intent =
        Intent(android.provider.Settings.ACTION_MANAGE_UNKNOWN_APP_SOURCES)
            .setData(android.net.Uri.parse("package:${context.packageName}"))
            .addFlags(Intent.FLAG_ACTIVITY_NEW_TASK)

    override suspend fun install(file: File): Unit = withContext(Dispatchers.IO) {
        val installer = context.packageManager.packageInstaller
        val params = PackageInstaller.SessionParams(PackageInstaller.SessionParams.MODE_FULL_INSTALL)
        // Naming the package lets the platform reject a session whose contents turn out
        // to be some other app before it ever prompts the user.
        params.setAppPackageName(context.packageName)
        // `setRequireUserAction(USER_ACTION_NOT_REQUIRED)` is deliberately NOT called.
        // Leaving it unset is what keeps Android's own confirmation dialog in the flow,
        // which is the whole reason a user is safe letting an app update itself.

        val sessionId = installer.createSession(params)
        try {
            installer.openSession(sessionId).use { session ->
                session.openWrite(WRITE_NAME, 0, file.length()).use { output ->
                    file.inputStream().use { input -> input.copyTo(output) }
                    // fsync before commit, or the session can be committed over bytes
                    // the kernel has not written yet and fails with a corrupt APK.
                    session.fsync(output)
                }
                session.commit(statusIntent(sessionId).intentSender)
            }
            Log.i(TAG, "Committed install session $sessionId for ${file.name}")
        } catch (e: IOException) {
            // Abandon rather than leak the session; a stranded session keeps its staging
            // space until the platform garbage-collects it.
            runCatching { installer.abandonSession(sessionId) }
            throw e
        }
    }

    /**
     * The callback the platform reports session status to.
     *
     * Must be MUTABLE on API 31+: [PackageInstaller] fills in the status extras (and,
     * for the confirmation step, the Intent to launch) on this very PendingIntent. An
     * immutable one silently arrives with no extras.
     */
    private fun statusIntent(sessionId: Int): PendingIntent {
        val intent = Intent(context, InstallStatusReceiver::class.java)
            .setAction(InstallStatusReceiver.ACTION_INSTALL_STATUS)
            .setPackage(context.packageName)
        val flags = if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.S) {
            PendingIntent.FLAG_UPDATE_CURRENT or PendingIntent.FLAG_MUTABLE
        } else {
            PendingIntent.FLAG_UPDATE_CURRENT
        }
        // The session id is the request code so two sessions can never share (and
        // overwrite) one PendingIntent.
        return PendingIntent.getBroadcast(context, sessionId, intent, flags)
    }

    private companion object {
        const val TAG = "ApkInstaller"
        const val WRITE_NAME = "picweight.apk"
    }
}
