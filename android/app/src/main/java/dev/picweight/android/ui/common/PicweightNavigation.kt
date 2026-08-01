package dev.picweight.android.ui.common

import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.navigation.NavType
import androidx.navigation.compose.NavHost
import androidx.navigation.compose.composable
import androidx.navigation.compose.rememberNavController
import androidx.navigation.navArgument
import dev.picweight.android.ui.auth.LoginScreen
import dev.picweight.android.ui.capture.CaptureScreen
import dev.picweight.android.ui.home.HomeScreen
import dev.picweight.android.ui.meal.MealDetailScreen
import dev.picweight.android.ui.profile.ProfileScreen
import java.net.URLEncoder
import java.nio.charset.StandardCharsets

/** Something the launching intent asked us to do once the graph is up. */
sealed interface StartupRequest {
    /** A delivery order shared into picweight — straight to capture, name prefilled. */
    data class SharedText(val text: String) : StartupRequest

    /** A tap on a meal notification. */
    data class OpenMeal(val serverId: String) : StartupRequest
}

object Routes {
    const val LOGIN = "login"
    const val HOME = "home"
    const val CAPTURE = "capture?shared={shared}"
    const val MEAL = "meal/{id}?server={server}"
    const val PROFILE = "profile"

    fun capture(sharedText: String? = null): String =
        "capture?shared=" + (sharedText?.let { encode(it) } ?: "")

    fun meal(clientUuid: String): String = "meal/${encode(clientUuid)}?server=false"

    fun mealByServerId(serverId: String): String = "meal/${encode(serverId)}?server=true"

    private fun encode(value: String): String =
        URLEncoder.encode(value, StandardCharsets.UTF_8.name())
}

@Composable
fun PicweightNavigation(
    loggedIn: Boolean,
    request: StartupRequest?,
    onRequestHandled: () -> Unit,
) {
    val navController = rememberNavController()

    LaunchedEffect(request, loggedIn) {
        val pending = request ?: return@LaunchedEffect
        if (!loggedIn) {
            // Nothing to open until there's a session; the login screen is already up.
            onRequestHandled()
            return@LaunchedEffect
        }
        when (pending) {
            is StartupRequest.SharedText -> navController.navigate(Routes.capture(pending.text))
            is StartupRequest.OpenMeal -> navController.navigate(Routes.mealByServerId(pending.serverId))
        }
        onRequestHandled()
    }

    NavHost(
        navController = navController,
        startDestination = if (loggedIn) Routes.HOME else Routes.LOGIN,
    ) {
        composable(Routes.LOGIN) {
            LoginScreen(
                onLoginSuccess = {
                    navController.navigate(Routes.HOME) {
                        popUpTo(Routes.LOGIN) { inclusive = true }
                    }
                },
            )
        }

        composable(Routes.HOME) {
            HomeScreen(
                onCapture = { navController.navigate(Routes.capture()) },
                onMealClick = { clientUuid -> navController.navigate(Routes.meal(clientUuid)) },
                onProfile = { navController.navigate(Routes.PROFILE) },
                onReLogin = {
                    navController.navigate(Routes.LOGIN) { popUpTo(0) { inclusive = true } }
                },
            )
        }

        composable(
            route = Routes.CAPTURE,
            arguments = listOf(
                navArgument("shared") {
                    type = NavType.StringType
                    defaultValue = ""
                },
            ),
        ) {
            CaptureScreen(
                onDone = { navController.popBackStack() },
                onMealClick = { clientUuid -> navController.navigate(Routes.meal(clientUuid)) },
            )
        }

        composable(
            route = Routes.MEAL,
            arguments = listOf(
                navArgument("id") { type = NavType.StringType },
                navArgument("server") {
                    type = NavType.StringType
                    defaultValue = "false"
                },
            ),
        ) {
            MealDetailScreen(onBack = { navController.popBackStack() })
        }

        composable(Routes.PROFILE) {
            ProfileScreen(
                onBack = { navController.popBackStack() },
                onLoggedOut = {
                    navController.navigate(Routes.LOGIN) { popUpTo(0) { inclusive = true } }
                },
            )
        }
    }
}
