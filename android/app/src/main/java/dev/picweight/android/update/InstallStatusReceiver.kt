package dev.picweight.android.update

import android.content.BroadcastReceiver
import android.content.Context
import android.content.Intent
import android.content.pm.PackageInstaller
import android.os.Build
import android.util.Log
import dagger.hilt.EntryPoint
import dagger.hilt.InstallIn
import dagger.hilt.android.EntryPointAccessors
import dagger.hilt.components.SingletonComponent
import kotlinx.coroutines.channels.BufferOverflow
import kotlinx.coroutines.flow.MutableSharedFlow
import kotlinx.coroutines.flow.SharedFlow
import kotlinx.coroutines.flow.asSharedFlow
import javax.inject.Inject
import javax.inject.Singleton

/** What the platform did with a committed install session. */
sealed interface InstallOutcome {

    /** The system is showing its own confirmation dialog. Nothing is installed yet. */
    data object AwaitingUser : InstallOutcome

    /** Installed. The process is normally killed and restarted right after this. */
    data object Installed : InstallOutcome

    /** The user said no at the system dialog. Not an error, and not worth a red banner. */
    data object Declined : InstallOutcome

    /** The platform refused, with whatever it was willing to say about why. */
    data class Failed(val message: String) : InstallOutcome
}

/**
 * The bridge from [InstallStatusReceiver] — which the platform instantiates whenever
 * it feels like it — back into the running UI.
 *
 * A replay buffer of one, because the confirmation dialog covers the app and the
 * screen collecting this may be recomposed (or the process briefly backgrounded)
 * between the broadcast and anyone listening.
 */
@Singleton
class InstallOutcomes @Inject constructor() {

    private val _outcomes = MutableSharedFlow<InstallOutcome>(
        replay = 1,
        extraBufferCapacity = 4,
        onBufferOverflow = BufferOverflow.DROP_OLDEST,
    )

    val outcomes: SharedFlow<InstallOutcome> = _outcomes.asSharedFlow()

    fun publish(outcome: InstallOutcome) {
        _outcomes.tryEmit(outcome)
    }

    /** Drops the replayed value so a new attempt doesn't immediately see the old result. */
    @OptIn(kotlinx.coroutines.ExperimentalCoroutinesApi::class)
    fun reset() {
        _outcomes.resetReplayCache()
    }
}

/**
 * Receives [PackageInstaller] session status.
 *
 * The one piece of real work here is [PackageInstaller.STATUS_PENDING_USER_ACTION]:
 * the platform hands back an Intent that shows *its* install confirmation, and this
 * receiver launches it. That prompt is required and intended — the app is asking to
 * replace its own executable code with something it downloaded — so it is passed
 * through untouched rather than suppressed or pre-answered.
 */
class InstallStatusReceiver : BroadcastReceiver() {

    /**
     * Reached through an entry point rather than `@AndroidEntryPoint` field injection.
     *
     * A receiver the platform instantiates has no constructor injection, and the
     * `@AndroidEntryPoint` form requires calling `super.onReceive` — which is abstract
     * on [BroadcastReceiver] and only becomes callable after Hilt's bytecode transform,
     * so it does not compile from Kotlin source. Resolving the singleton here is the
     * same graph, one line, and no plugin ordering to get wrong.
     */
    @EntryPoint
    @InstallIn(SingletonComponent::class)
    interface Deps {
        fun installOutcomes(): InstallOutcomes
    }

    override fun onReceive(context: Context, intent: Intent) {
        if (intent.action != ACTION_INSTALL_STATUS) return

        val outcomes = EntryPointAccessors
            .fromApplication(context.applicationContext, Deps::class.java)
            .installOutcomes()

        val status = intent.getIntExtra(PackageInstaller.EXTRA_STATUS, Int.MIN_VALUE)
        val message = intent.getStringExtra(PackageInstaller.EXTRA_STATUS_MESSAGE)

        when (status) {
            PackageInstaller.STATUS_PENDING_USER_ACTION -> {
                val confirm = confirmationIntent(intent)
                if (confirm == null) {
                    Log.e(TAG, "Pending user action with no Intent to launch")
                    outcomes.publish(InstallOutcome.Failed("The installer didn't return a confirmation screen."))
                    return
                }
                // NEW_TASK because a BroadcastReceiver has no task of its own to
                // launch into.
                confirm.addFlags(Intent.FLAG_ACTIVITY_NEW_TASK)
                outcomes.publish(InstallOutcome.AwaitingUser)
                context.startActivity(confirm)
            }

            PackageInstaller.STATUS_SUCCESS -> {
                Log.i(TAG, "Update installed")
                outcomes.publish(InstallOutcome.Installed)
            }

            // The user pressed Cancel on the system dialog. Deliberately its own
            // outcome: declining an update is a choice, not a failure, and must not
            // produce an error the user then has to dismiss.
            PackageInstaller.STATUS_FAILURE_ABORTED -> {
                Log.i(TAG, "User declined the update")
                outcomes.publish(InstallOutcome.Declined)
            }

            else -> {
                Log.e(TAG, "Install failed: status=$status message=$message")
                outcomes.publish(InstallOutcome.Failed(explain(status, message)))
            }
        }
    }

    private fun confirmationIntent(intent: Intent): Intent? =
        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.TIRAMISU) {
            intent.getParcelableExtra(Intent.EXTRA_INTENT, Intent::class.java)
        } else {
            @Suppress("DEPRECATION")
            intent.getParcelableExtra(Intent.EXTRA_INTENT)
        }

    /**
     * Turns a status code into something a person can act on.
     *
     * [PackageInstaller.EXTRA_STATUS_MESSAGE] is usually a bare framework string like
     * "INSTALL_FAILED_VERSION_DOWNGRADE", so it is kept but not shown alone.
     */
    private fun explain(status: Int, message: String?): String {
        val head = when (status) {
            PackageInstaller.STATUS_FAILURE_BLOCKED ->
                "Android blocked the install."

            PackageInstaller.STATUS_FAILURE_CONFLICT ->
                "The installed copy of picweight conflicts with this one — most often a " +
                    "different signing key, or a newer version already installed."

            PackageInstaller.STATUS_FAILURE_STORAGE ->
                "Not enough storage to install the update."

            PackageInstaller.STATUS_FAILURE_INCOMPATIBLE ->
                "That build isn't compatible with this device."

            PackageInstaller.STATUS_FAILURE_INVALID ->
                "The installer rejected the package as invalid."

            else -> "The install didn't complete."
        }
        return message?.takeIf { it.isNotBlank() }?.let { "$head ($it)" } ?: head
    }

    companion object {
        private const val TAG = "InstallStatus"

        /** Explicit action so an unrelated broadcast can never be read as install status. */
        const val ACTION_INSTALL_STATUS = "dev.picweight.android.INSTALL_STATUS"
    }
}
