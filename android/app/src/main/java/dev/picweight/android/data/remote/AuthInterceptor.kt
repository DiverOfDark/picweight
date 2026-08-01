package dev.picweight.android.data.remote

import dev.picweight.android.data.repository.AuthRepository
import dev.picweight.android.data.repository.TokenRefreshManager
import okhttp3.Interceptor
import okhttp3.Response
import javax.inject.Inject
import javax.inject.Singleton

/** Attaches the picweight session JWT and flags an expired session on a 401. */
@Singleton
class AuthInterceptor @Inject constructor(
    private val authRepository: dagger.Lazy<AuthRepository>,
    private val tokenRefreshManager: dagger.Lazy<TokenRefreshManager>,
) : Interceptor {
    override fun intercept(chain: Interceptor.Chain): Response {
        val original = chain.request()
        // Auth endpoints get the raw token and never trigger a refresh — the
        // refresh/exchange calls go through this same client (recursion guard).
        val isAuthEndpoint = original.url.encodedPath.startsWith("/api/auth/")
        val token = if (isAuthEndpoint) {
            authRepository.get().getToken()
        } else {
            // Proactively renews when close to expiry; on failure this returns the old
            // (possibly expired) token so the app keeps working against its Room cache.
            tokenRefreshManager.get().ensureFreshToken()
        }

        val request = if (token != null) {
            original.newBuilder().header("Authorization", "Bearer $token").build()
        } else {
            original
        }

        val response = chain.proceed(request)

        // Mark the token expired on a 401 but don't clear it — Room stays readable and
        // queued uploads stay queued. PicweightAuthenticator has already tried a forced
        // refresh by the time a 401 reaches here.
        if (response.code == 401 && !isAuthEndpoint) {
            authRepository.get().markTokenExpired()
        }

        return response
    }
}
