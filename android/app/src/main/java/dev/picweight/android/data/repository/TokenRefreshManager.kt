package dev.picweight.android.data.repository

import android.content.Context
import android.os.Looper
import android.util.Log
import dagger.hilt.android.qualifiers.ApplicationContext
import dev.picweight.android.data.remote.PicweightApi
import dev.picweight.android.data.remote.model.TokenExchangeRequest
import net.openid.appauth.AuthState
import net.openid.appauth.AuthorizationService
import java.util.concurrent.CountDownLatch
import java.util.concurrent.TimeUnit
import javax.inject.Inject
import javax.inject.Singleton

/**
 * Keeps the picweight session JWT fresh without an interactive relogin.
 *
 * Renewal strategy:
 * 1. While the session is still valid: slide it via `POST /api/auth/refresh`.
 * 2. If the session already expired (or the server refused): use the stored AppAuth
 *    refresh token to get a fresh ID token from the IdP and re-exchange it at
 *    `POST /api/auth/token`.
 *
 * Only when both fail does the 401 → authExpired → login-banner flow kick in. This
 * matters more here than in a browsing app: an upload queued in airplane mode may
 * surface days later, and it must not need the user to log in again first.
 */
@Singleton
class TokenRefreshManager @Inject constructor(
    private val authRepository: AuthRepository,
    // Lazy breaks the Hilt cycle: Manager -> PicweightApi -> OkHttp -> AuthInterceptor -> Manager
    private val api: dagger.Lazy<PicweightApi>,
    @param:ApplicationContext private val appContext: Context,
) {
    companion object {
        private const val TAG = "TokenRefreshManager"

        /** Back off between failed attempts so an offline device doesn't hammer. */
        private const val MIN_ATTEMPT_INTERVAL_MS = 15 * 60 * 1000L

        /** Always refresh once less than this much lifetime remains. */
        private const val MIN_REMAINING_FLOOR_MS = 60 * 60 * 1000L

        private const val IDP_REFRESH_TIMEOUT_S = 30L

        /** True when remaining lifetime is below max(1h, 50% of TTL). */
        internal fun shouldRefresh(nowMs: Long, expiresAtMs: Long, ttlSeconds: Long): Boolean {
            if (expiresAtMs <= 0) return false
            val threshold = maxOf(MIN_REMAINING_FLOOR_MS, ttlSeconds * 1000 / 2)
            return nowMs > expiresAtMs - threshold
        }
    }

    private val lock = Any()

    @Volatile
    private var lastAttemptMs = 0L

    private fun needsRefresh(): Boolean = shouldRefresh(
        System.currentTimeMillis(),
        authRepository.getExpiresAt(),
        authRepository.getTtlSeconds(),
    )

    /**
     * Returns the current best token, refreshing first if it is close to expiry.
     * Blocks on network; must never be called from the main thread.
     */
    fun ensureFreshToken(): String? = refreshInternal(force = false)

    /** Refresh regardless of remaining lifetime (used after an unexpected 401). */
    fun forceRefresh(): String? = refreshInternal(force = true)

    private fun refreshInternal(force: Boolean): String? {
        val entryToken = authRepository.getToken() ?: return null
        if (!force && !needsRefresh()) return entryToken
        assertNotMainThread()
        synchronized(lock) {
            val current = authRepository.getToken()
            // Another thread refreshed while we waited on the lock.
            if (current != entryToken) return current
            if (!force && !needsRefresh()) return current
            val now = System.currentTimeMillis()
            if (now - lastAttemptMs < MIN_ATTEMPT_INTERVAL_MS) return current
            lastAttemptMs = now

            val renewed = if (!authRepository.isTokenExpired()) {
                refreshViaBackend() || refreshViaIdp()
            } else {
                // An expired session can't be slid server-side; go straight to the IdP.
                refreshViaIdp()
            }
            if (renewed) {
                lastAttemptMs = 0L
                authRepository.clearAuthExpired()
            } else {
                Log.w(TAG, "Session renewal failed; will retry later")
            }
            return authRepository.getToken()
        }
    }

    private fun refreshViaBackend(): Boolean = runCatching {
        // AuthInterceptor attaches the (still valid) Bearer token to this call.
        val response = api.get().refreshTokenCall().execute()
        val body = response.body()
        if (response.isSuccessful && body != null) {
            authRepository.saveToken(body.token, body.expiresIn)
            true
        } else {
            false
        }
    }.getOrDefault(false)

    private fun refreshViaIdp(): Boolean {
        val stateJson = authRepository.getAppAuthState() ?: return false
        val authState = runCatching { AuthState.jsonDeserialize(stateJson) }.getOrNull() ?: return false
        if (authState.refreshToken == null) return false

        val latch = CountDownLatch(1)
        var idToken: String? = null
        // AppAuth needs a Context; the app context works for pure token requests (no browser).
        val service = AuthorizationService(appContext)
        try {
            val request = runCatching { authState.createTokenRefreshRequest() }.getOrNull() ?: return false
            val clientAuth = runCatching { authState.clientAuthentication }.getOrNull() ?: return false
            service.performTokenRequest(request, clientAuth) { response, ex ->
                authState.update(response, ex)
                if (response != null) {
                    // Persist immediately: providers like Zitadel rotate refresh tokens,
                    // so the old one is dead the moment this response arrives.
                    authRepository.saveAppAuthState(authState.jsonSerializeString())
                    idToken = response.idToken
                } else {
                    Log.w(TAG, "IdP token refresh failed: ${ex?.message}")
                }
                latch.countDown()
            }
            if (!latch.await(IDP_REFRESH_TIMEOUT_S, TimeUnit.SECONDS)) return false
        } finally {
            service.dispose()
        }

        val token = idToken ?: return false
        return runCatching {
            val response = api.get()
                .exchangeTokenCall(TokenExchangeRequest().apply { this.idToken = token })
                .execute()
            val body = response.body()
            if (response.isSuccessful && body != null) {
                authRepository.saveToken(body.token, body.expiresIn)
                true
            } else {
                false
            }
        }.getOrDefault(false)
    }

    private fun assertNotMainThread() {
        // The IdP path blocks on a latch that AppAuth resolves on the main thread —
        // calling this from the main thread would deadlock.
        val main = Looper.getMainLooper()
        check(main == null || Looper.myLooper() != main) {
            "TokenRefreshManager must not be called on the main thread"
        }
    }
}
