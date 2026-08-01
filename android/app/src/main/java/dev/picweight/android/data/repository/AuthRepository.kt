package dev.picweight.android.data.repository

import android.content.SharedPreferences
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import javax.inject.Inject
import javax.inject.Named
import javax.inject.Singleton

/**
 * Session state, backed by EncryptedSharedPreferences.
 *
 * Two secrets live here: the picweight session JWT, and the serialised AppAuth state —
 * which contains the IdP **refresh token**. Both are at rest under the AndroidKeyStore
 * master key (PRD §7 "AppAuth OIDC + PKCE … refresh tokens in EncryptedSharedPreferences").
 */
@Singleton
class AuthRepository @Inject constructor(
    @param:Named("auth") private val prefs: SharedPreferences,
) {
    companion object {
        private const val KEY_TOKEN = "picweight_jwt"
        private const val KEY_EXPIRES_AT = "expires_at"
        private const val KEY_TTL_SECONDS = "ttl_seconds"
        private const val KEY_SERVER_URL = "server_url"
        private const val KEY_OIDC_ISSUER = "oidc_issuer"
        private const val KEY_OIDC_CLIENT_ID = "oidc_client_id"
        private const val KEY_OIDC_SCOPES = "oidc_scopes"
        private const val KEY_APPAUTH_STATE = "appauth_state"

        private const val DEFAULT_TTL_SECONDS = 3600L
    }

    private val _authExpired = MutableStateFlow(false)

    /** True once a 401 has been seen and renewal has not yet succeeded. */
    val authExpired: StateFlow<Boolean> = _authExpired.asStateFlow()

    fun getToken(): String? {
        val token = prefs.getString(KEY_TOKEN, null) ?: return null
        val expiresAt = prefs.getLong(KEY_EXPIRES_AT, 0)
        if (expiresAt > 0 && System.currentTimeMillis() > expiresAt) {
            _authExpired.value = true
            // Return the expired token anyway: the queue should keep trying, and Room
            // keeps rendering, rather than the app pretending to be logged out.
            return token
        }
        return token
    }

    fun isTokenExpired(): Boolean {
        val expiresAt = prefs.getLong(KEY_EXPIRES_AT, 0)
        return expiresAt > 0 && System.currentTimeMillis() > expiresAt
    }

    fun markTokenExpired() {
        _authExpired.value = true
    }

    fun clearAuthExpired() {
        _authExpired.value = false
    }

    fun getExpiresAt(): Long = prefs.getLong(KEY_EXPIRES_AT, 0)

    fun getTtlSeconds(): Long = prefs.getLong(KEY_TTL_SECONDS, DEFAULT_TTL_SECONDS)

    fun saveToken(token: String, expiresInSeconds: Long) {
        prefs.edit()
            .putString(KEY_TOKEN, token)
            .putLong(KEY_EXPIRES_AT, System.currentTimeMillis() + expiresInSeconds * 1000)
            .putLong(KEY_TTL_SECONDS, expiresInSeconds)
            .apply()
        _authExpired.value = false
    }

    /**
     * Drops only the picweight session JWT; the AppAuth state (and with it the IdP
     * refresh token) survives so the session can still be renewed silently.
     */
    fun clearToken() {
        prefs.edit()
            .remove(KEY_TOKEN)
            .remove(KEY_EXPIRES_AT)
            .remove(KEY_TTL_SECONDS)
            .apply()
        _authExpired.value = false
    }

    fun saveAppAuthState(json: String) {
        prefs.edit().putString(KEY_APPAUTH_STATE, json).apply()
    }

    fun getAppAuthState(): String? = prefs.getString(KEY_APPAUTH_STATE, null)

    fun getServerUrl(): String? = prefs.getString(KEY_SERVER_URL, null)

    fun saveServerConfig(serverUrl: String, issuer: String?, clientId: String?) {
        prefs.edit()
            .putString(KEY_SERVER_URL, serverUrl.trimEnd('/'))
            .putString(KEY_OIDC_ISSUER, issuer)
            .putString(KEY_OIDC_CLIENT_ID, clientId)
            .apply()
    }

    fun getOidcIssuer(): String? = prefs.getString(KEY_OIDC_ISSUER, null)

    fun getOidcClientId(): String? = prefs.getString(KEY_OIDC_CLIENT_ID, null)

    fun saveOidcScopes(scopes: List<String>?) {
        if (scopes.isNullOrEmpty()) return
        prefs.edit().putString(KEY_OIDC_SCOPES, scopes.joinToString(" ")).apply()
    }

    fun getOidcScopes(): List<String>? =
        prefs.getString(KEY_OIDC_SCOPES, null)?.split(" ")?.filter { it.isNotBlank() }

    fun isLoggedIn(): Boolean = getToken() != null && getServerUrl() != null

    /**
     * Resolves a server-relative path (a thumbnail URL, say) against the configured
     * server so Coil can fetch it through the authenticated OkHttp client.
     */
    fun absoluteUrl(path: String?): String? {
        val relative = path?.takeIf { it.isNotBlank() } ?: return null
        if (relative.startsWith("http://") || relative.startsWith("https://")) return relative
        val base = getServerUrl()?.trimEnd('/') ?: return null
        return base + "/" + relative.trimStart('/')
    }

    fun logout() {
        prefs.edit().clear().apply()
        _authExpired.value = false
    }
}
