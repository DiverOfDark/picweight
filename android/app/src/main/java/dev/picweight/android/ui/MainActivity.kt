package dev.picweight.android.ui

import android.Manifest
import android.content.Intent
import android.os.Build
import android.os.Bundle
import androidx.activity.ComponentActivity
import androidx.activity.compose.setContent
import androidx.activity.enableEdgeToEdge
import androidx.activity.result.contract.ActivityResultContracts
import androidx.compose.runtime.collectAsState
import androidx.compose.runtime.getValue
import dagger.hilt.android.AndroidEntryPoint
import dev.picweight.android.data.repository.AuthRepository
import dev.picweight.android.notifications.MealNotifier
import dev.picweight.android.ui.common.PicweightNavigation
import dev.picweight.android.ui.common.PicweightTheme
import dev.picweight.android.ui.common.StartupRequest
import kotlinx.coroutines.flow.MutableStateFlow
import javax.inject.Inject

@AndroidEntryPoint
class MainActivity : ComponentActivity() {

    @Inject lateinit var authRepository: AuthRepository

    /** The intent that launched (or re-launched) the activity, until it's been acted on. */
    private val pending = MutableStateFlow<StartupRequest?>(null)

    private val notificationPermission =
        registerForActivityResult(ActivityResultContracts.RequestPermission()) { /* declining is fine */ }

    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        enableEdgeToEdge()
        pending.value = parse(intent)

        setContent {
            PicweightTheme {
                val request by pending.collectAsState()
                PicweightNavigation(
                    loggedIn = authRepository.isLoggedIn(),
                    request = request,
                    onRequestHandled = { pending.value = null },
                )
            }
        }

        // Asked for up front rather than at the moment of the first notification: the
        // notification is the whole point of logging a meal (§6), so a user who declines
        // should find that out before they rely on it.
        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.TIRAMISU) {
            notificationPermission.launch(Manifest.permission.POST_NOTIFICATIONS)
        }
    }

    override fun onNewIntent(intent: Intent) {
        super.onNewIntent(intent)
        setIntent(intent)
        parse(intent)?.let { pending.value = it }
    }

    /**
     * Turns a launch intent into something the navigation graph can act on: a shared
     * delivery order, or a tap on a meal's notification.
     */
    private fun parse(intent: Intent?): StartupRequest? {
        if (intent == null) return null

        intent.getStringExtra(MealNotifier.EXTRA_MEAL_ID)?.let {
            return StartupRequest.OpenMeal(serverId = it)
        }

        val shared = when (intent.action) {
            Intent.ACTION_SEND ->
                intent.getStringExtra(Intent.EXTRA_TEXT)

            Intent.ACTION_PROCESS_TEXT ->
                intent.getCharSequenceExtra(Intent.EXTRA_PROCESS_TEXT)?.toString()

            else -> null
        }
        return shared?.takeIf { it.isNotBlank() }?.let { StartupRequest.SharedText(it) }
    }
}
