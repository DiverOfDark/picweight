package dev.picweight.android.data.remote

import dev.picweight.android.data.repository.TokenRefreshManager
import okhttp3.Authenticator
import okhttp3.Request
import okhttp3.Response
import okhttp3.Route
import javax.inject.Inject
import javax.inject.Singleton

/**
 * Reactive safety net: retries a 401 once after forcing a token refresh (covers
 * server-side secret rotation, device clock jumps, a session that expired while the
 * phone was offline). If this gives up, the 401 propagates to [AuthInterceptor],
 * which flags the session expired and surfaces the relogin banner.
 */
@Singleton
class PicweightAuthenticator @Inject constructor(
    private val tokenRefreshManager: dagger.Lazy<TokenRefreshManager>,
) : Authenticator {
    override fun authenticate(route: Route?, response: Response): Request? {
        // Never react to the auth endpoints themselves (avoids refresh recursion).
        if (response.request.url.encodedPath.startsWith("/api/auth/")) return null
        // Only retry once.
        if (response.priorResponse != null) return null

        val oldToken = response.request.header("Authorization")?.removePrefix("Bearer ")
        val newToken = tokenRefreshManager.get().forceRefresh() ?: return null
        if (newToken == oldToken) return null // refresh didn't produce a new token

        return response.request.newBuilder()
            .header("Authorization", "Bearer $newToken")
            .build()
    }
}
