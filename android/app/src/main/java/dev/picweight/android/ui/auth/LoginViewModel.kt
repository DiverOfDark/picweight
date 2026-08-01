package dev.picweight.android.ui.auth

import android.content.Context
import android.content.Intent
import android.net.Uri
import androidx.lifecycle.ViewModel
import androidx.lifecycle.viewModelScope
import com.fasterxml.jackson.databind.ObjectMapper
import dagger.hilt.android.lifecycle.HiltViewModel
import dagger.hilt.android.qualifiers.ApplicationContext
import dev.picweight.android.data.remote.PicweightApi
import dev.picweight.android.data.remote.model.AuthConfigResponse
import dev.picweight.android.data.remote.model.TokenExchangeRequest
import dev.picweight.android.data.repository.AuthRepository
import dev.picweight.android.ui.common.ApiFailures
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.launch
import kotlinx.coroutines.withContext
import net.openid.appauth.AuthState
import net.openid.appauth.AuthorizationException
import net.openid.appauth.AuthorizationRequest
import net.openid.appauth.AuthorizationResponse
import net.openid.appauth.AuthorizationService
import net.openid.appauth.AuthorizationServiceConfiguration
import net.openid.appauth.ResponseTypeValues
import okhttp3.OkHttpClient
import okhttp3.Request
import javax.inject.Inject

data class LoginUiState(
    val serverUrl: String = "",
    val oidcIssuer: String = "",
    val oidcClientId: String = "",
    val isLoading: Boolean = false,
    val isFetchingConfig: Boolean = false,
    val error: String? = null,
    val info: String? = null,
    val isLoggedIn: Boolean = false,
)

private const val TAG = "LoginViewModel"

/**
 * OIDC login against the **native/public** client (PRD §7).
 *
 * AppAuth runs authorization-code + PKCE in a Custom Tab — no client secret ever
 * reaches the phone, which is the whole reason the backend registers two clients: a
 * confidential one for the web SPA and this public one for the app. The ID token that
 * comes back is exchanged at `POST /api/auth/token` for a picweight session JWT, and
 * the AppAuth state (with its refresh token) is kept so the session can be renewed
 * silently for as long as the IdP allows.
 */
@HiltViewModel
class LoginViewModel @Inject constructor(
    private val authRepository: AuthRepository,
    private val okHttpClient: OkHttpClient,
    private val api: PicweightApi,
    private val mapper: ObjectMapper,
    @param:ApplicationContext private val appContext: Context,
) : ViewModel() {

    private val _uiState = MutableStateFlow(LoginUiState())
    val uiState: StateFlow<LoginUiState> = _uiState.asStateFlow()

    private val _authIntent = MutableStateFlow<Intent?>(null)
    val authIntent: StateFlow<Intent?> = _authIntent.asStateFlow()

    init {
        _uiState.value = LoginUiState(
            serverUrl = authRepository.getServerUrl() ?: "",
            oidcIssuer = authRepository.getOidcIssuer() ?: "",
            oidcClientId = authRepository.getOidcClientId() ?: "",
            isLoggedIn = authRepository.isLoggedIn(),
        )
    }

    fun updateServerUrl(url: String) {
        _uiState.value = _uiState.value.copy(serverUrl = url)
    }

    fun updateOidcIssuer(issuer: String) {
        _uiState.value = _uiState.value.copy(oidcIssuer = issuer)
    }

    fun updateOidcClientId(clientId: String) {
        _uiState.value = _uiState.value.copy(oidcClientId = clientId)
    }

    /**
     * Asks the server for its OIDC settings so the user only ever types a server URL.
     * `mobile_client_id` is preferred over `client_id`: the former is the public client
     * this app is allowed to use.
     */
    fun fetchAuthConfig() {
        val raw = _uiState.value.serverUrl.trim().trimEnd('/')
        if (raw.isBlank()) return
        if (!raw.startsWith("http://") && !raw.startsWith("https://")) {
            _uiState.value = _uiState.value.copy(
                error = "Server URL must start with http:// or https://",
                info = null,
            )
            return
        }

        _uiState.value = _uiState.value.copy(isFetchingConfig = true, error = null, info = null)
        viewModelScope.launch {
            val result = withContext(Dispatchers.IO) {
                runCatching {
                    val request = Request.Builder().url("$raw/api/auth/config").get().build()
                    okHttpClient.newCall(request).execute().use { response ->
                        FetchResult(response.code, response.body.string())
                    }
                }
            }

            result.fold(
                onSuccess = { res ->
                    when {
                        res.code == 404 -> _uiState.value = _uiState.value.copy(
                            isFetchingConfig = false,
                            error = "That server has no OIDC configured — picweight needs one.",
                        )

                        res.code !in 200..299 -> _uiState.value = _uiState.value.copy(
                            isFetchingConfig = false,
                            error = "Server returned HTTP ${res.code}",
                        )

                        else -> {
                            // Parse into the generated model and pass the Class directly:
                            // the reified readValue<T> builds an anonymous TypeReference
                            // whose generic signature R8 strips in release builds.
                            val parsed = res.body?.let {
                                runCatching { mapper.readValue(it, AuthConfigResponse::class.java) }
                            }
                            val config = parsed?.getOrNull()
                            if (config?.issuer.isNullOrBlank()) {
                                // A 2xx we can't read is a contract mismatch, not an
                                // "unexpected response" — name it, because this is the
                                // first call the app ever makes and getting it wrong
                                // sends the user hunting for the wrong problem.
                                val failure = parsed?.exceptionOrNull()
                                    ?.let { ApiFailures.report(TAG, "Parsing /api/auth/config", it) }
                                _uiState.value = _uiState.value.copy(
                                    isFetchingConfig = false,
                                    error = failure?.message
                                        ?: "That server answered, but not with an OIDC config.",
                                )
                            } else {
                                authRepository.saveOidcScopes(config.scopes)
                                _uiState.value = _uiState.value.copy(
                                    oidcIssuer = config.issuer,
                                    oidcClientId = config.mobileClientId ?: config.clientId,
                                    isFetchingConfig = false,
                                    info = "Auth config loaded.",
                                )
                            }
                        }
                    }
                },
                onFailure = { e ->
                    val failure = ApiFailures.report(TAG, "Fetching auth config from $raw", e)
                    _uiState.value = _uiState.value.copy(
                        isFetchingConfig = false,
                        error = failure.message,
                    )
                },
            )
        }
    }

    private data class FetchResult(val code: Int, val body: String?)

    fun startLogin(context: Context) {
        val state = _uiState.value
        if (state.serverUrl.isBlank()) {
            _uiState.value = state.copy(error = "Server URL is required")
            return
        }
        if (state.oidcIssuer.isBlank() || state.oidcClientId.isBlank()) {
            _uiState.value = state.copy(error = "Auto-detect the auth config first, or fill it in.")
            return
        }

        _uiState.value = state.copy(isLoading = true, error = null)
        authRepository.saveServerConfig(state.serverUrl.trim(), state.oidcIssuer, state.oidcClientId)

        viewModelScope.launch {
            try {
                AuthorizationServiceConfiguration.fetchFromIssuer(Uri.parse(state.oidcIssuer)) { config, ex ->
                    if (config == null || ex != null) {
                        _uiState.value = _uiState.value.copy(
                            isLoading = false,
                            error = "OIDC discovery failed: ${ex?.message}",
                        )
                        return@fetchFromIssuer
                    }

                    // Server-configured scopes plus openid (required for an id_token on
                    // refresh) and offline_access (required by Zitadel and others for a
                    // refresh token — a public client asks for it, the web client doesn't).
                    val scopes = (authRepository.getOidcScopes() ?: listOf("openid", "profile", "email"))
                        .toMutableSet()
                        .apply {
                            add("openid")
                            add("offline_access")
                        }

                    // AppAuth defaults to S256 PKCE for a public client; the code verifier
                    // never leaves the device.
                    val authRequest = AuthorizationRequest.Builder(
                        config,
                        state.oidcClientId,
                        ResponseTypeValues.CODE,
                        Uri.parse("dev.picweight.android://callback"),
                    )
                        .setScopes(scopes)
                        .build()

                    val authService = AuthorizationService(context)
                    try {
                        _authIntent.value = authService.getAuthorizationRequestIntent(authRequest)
                    } finally {
                        authService.dispose()
                    }
                }
            } catch (e: Exception) {
                _uiState.value = _uiState.value.copy(
                    isLoading = false,
                    error = "Login failed: ${e.message}",
                )
            }
        }
    }

    fun clearAuthIntent() {
        _authIntent.value = null
    }

    fun handleAuthResult(data: Intent?) {
        val response = data?.let { AuthorizationResponse.fromIntent(it) }
        val exception = data?.let { AuthorizationException.fromIntent(it) }

        if (exception != null) {
            _uiState.value = _uiState.value.copy(isLoading = false, error = "Auth failed: ${exception.message}")
            return
        }
        if (response == null) {
            _uiState.value = _uiState.value.copy(isLoading = false, error = "No auth response received")
            return
        }

        // Exchange the authorization code via AppAuth (PKCE verifier included), then keep
        // the whole AuthState so the refresh token can renew the session later.
        val authState = AuthState(response, exception)
        val authService = AuthorizationService(appContext)
        authService.performTokenRequest(response.createTokenExchangeRequest()) { tokenResponse, tokenEx ->
            authService.dispose()
            authState.update(tokenResponse, tokenEx)
            if (tokenEx != null || tokenResponse == null) {
                _uiState.value = _uiState.value.copy(
                    isLoading = false,
                    error = "Token exchange failed: ${tokenEx?.message}",
                )
                return@performTokenRequest
            }

            val idToken = tokenResponse.idToken
            if (idToken == null) {
                _uiState.value = _uiState.value.copy(
                    isLoading = false,
                    error = "No ID token in token response",
                )
                return@performTokenRequest
            }

            viewModelScope.launch {
                try {
                    val backendResponse = api.exchangeToken(
                        TokenExchangeRequest().apply { this.idToken = idToken }
                    )
                    authRepository.saveToken(backendResponse.token, backendResponse.expiresIn)
                    authRepository.saveAppAuthState(authState.jsonSerializeString())
                    _uiState.value = _uiState.value.copy(isLoading = false, isLoggedIn = true)
                } catch (e: Exception) {
                    // The session JWT comes back from `POST /api/auth/token`; if this
                    // build can't parse that body the user is stuck on the login screen
                    // with a working server, so the distinction matters here too.
                    val failure = ApiFailures.report(TAG, "Exchanging the ID token", e)
                    _uiState.value = _uiState.value.copy(
                        isLoading = false,
                        error = "Backend token exchange failed: ${failure.message}",
                    )
                }
            }
        }
    }
}
