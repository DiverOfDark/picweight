//! OIDC login and session handling.
//!
//! Ported from phos `backend/src/auth.rs`. Two OIDC clients, as in phos:
//!
//! * **web** — confidential (`client_id` + `client_secret`), authorization-code
//!   + PKCE, session delivered as an `HttpOnly` cookie.
//! * **mobile** — public/native (`mobile_client_id`, no secret). The Android app
//!   runs the code+PKCE flow itself against the IdP and posts the resulting ID
//!   token to [`token_exchange`], receiving a picweight session JWT it sends as
//!   `Authorization: Bearer`.
//!
//! Everything after login is a picweight-minted HS256 JWT, so request handling
//! never depends on the IdP being reachable.
//!
//! Handlers live under `/api/auth/*` and are **not** behind [`require_auth`];
//! everything under `/api/v1/*` is.
//!
//! # Why there are two ID-token verifiers
//!
//! `openidconnect` enforces OIDC Core §3.1.3.7: the `aud` claim must contain
//! *this client's* id. An ID token minted for the native client has
//! `aud = [mobile_client_id]`, which the web client's verifier rejects outright
//! — `set_other_audience_verifier_fn` only relaxes the *additional* audiences
//! alongside a matching one. So [`AuthState`] keeps the discovered JWKS and
//! builds a second, public-client verifier for `mobile_client_id`;
//! [`token_exchange`] tries the web verifier first and the mobile one second.
//! That is what makes "both the Android public client and the web confidential
//! client authenticate" (PRD §13) actually true.

use crate::error::AppError;
use crate::models::{NewUser, User, UserChangeset};
use crate::AppState;
use axum::extract::{FromRequestParts, Query, State};
use axum::http::request::Parts;
use axum::http::{header, HeaderMap, HeaderValue};
use axum::middleware::Next;
use axum::response::{IntoResponse, Redirect, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use chrono::Utc;
use diesel::prelude::*;
use diesel::SqliteConnection;
use jsonwebtoken::{decode, encode, Algorithm, DecodingKey, EncodingKey, Header, Validation};
use openidconnect::core::{
    CoreClient, CoreIdToken, CoreIdTokenClaims, CoreIdTokenVerifier, CoreJsonWebKeySet,
    CoreProviderMetadata, CoreResponseType,
};
use openidconnect::{
    AuthenticationFlow, AuthorizationCode, ClientId, ClientSecret, CsrfToken, EndpointMaybeSet,
    EndpointNotSet, EndpointSet, IssuerUrl, Nonce, PkceCodeChallenge, PkceCodeVerifier, RedirectUrl,
    Scope, TokenResponse as OidcTokenResponse,
};
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

/// Name of the session cookie set for the web client.
pub const SESSION_COOKIE: &str = "picweight_session";
/// Name of the short-lived cookie carrying PKCE/CSRF state through the OIDC
/// round trip.
pub const OIDC_STATE_COOKIE: &str = "picweight_oidc_state";

/// How long the PKCE/CSRF state cookie stays valid. Ten minutes is long enough
/// for a password + MFA prompt and short enough that a stolen state cookie is
/// worthless.
const OIDC_STATE_TTL_SECS: u64 = 600;

/// Where the browser lands after a successful login.
const POST_LOGIN_REDIRECT: &str = "/";
/// Where the browser lands after logout, or after a provider-side error.
const LOGIN_PAGE: &str = "/login";

/// Claims carried by a picweight session JWT.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct SessionClaims {
    /// OIDC subject — stable per user per issuer.
    pub sub: String,
    /// Display name from the ID token, or empty.
    pub name: String,
    /// Email from the ID token, or empty.
    pub email: String,
    /// Issuer URL the subject came from.
    #[serde(default)]
    pub iss: String,
    /// Expiry, seconds since the epoch.
    pub exp: usize,
    /// Issued-at, seconds since the epoch.
    pub iat: usize,
}

/// Claims of the short-lived cookie that carries login state to the callback.
///
/// Keeping PKCE verifier + nonce + CSRF token in a signed cookie rather than in
/// server memory means a login survives a rolling restart, and the backend needs
/// no session store.
#[derive(Debug, Serialize, Deserialize)]
struct OidcStateClaims {
    /// CSRF token echoed back by the provider as `?state=`.
    csrf_token: String,
    /// Nonce bound into the ID token.
    nonce: String,
    /// PKCE code verifier for the token request.
    pkce_verifier: String,
    /// Expiry, seconds since the epoch.
    exp: usize,
}

/// The OIDC client type after discovery, with the endpoints we set explicitly.
pub type DiscoveredClient = CoreClient<
    EndpointSet,
    EndpointNotSet,
    EndpointNotSet,
    EndpointNotSet,
    EndpointMaybeSet,
    EndpointMaybeSet,
>;

/// Discovered OIDC provider plus the keys used to mint and verify sessions.
#[derive(Clone)]
pub struct AuthState {
    /// Discovered provider client for the confidential web flow.
    pub oidc_client: DiscoveredClient,
    /// HTTP client used for discovery and token exchange.
    pub http_client: openidconnect::reqwest::Client,
    /// HS256 key used to sign session JWTs.
    pub jwt_encoding_key: EncodingKey,
    /// HS256 key used to verify session JWTs.
    pub jwt_decoding_key: DecodingKey,
    /// Session lifetime in seconds.
    pub jwt_ttl_secs: u64,
    /// Scopes requested at authorization time.
    pub scopes: Vec<String>,
    /// Issuer URL, echoed to mobile clients so they self-configure.
    pub issuer_url: String,
    /// Confidential web client id.
    pub client_id: String,
    /// Public/native client id, when configured.
    pub mobile_client_id: Option<String>,
    /// Operator-configured additional `aud` values. Empty means "accept any
    /// extra audience and log it once"; non-empty switches to a strict
    /// allowlist. See [`accept_extra`] for why permissive is the right default.
    pub extra_audiences: Vec<String>,
    /// Signing keys fetched during discovery.
    ///
    /// Retained so a verifier can be built for the *public* client too — the
    /// web client's verifier will not accept an audience it does not appear in.
    pub jwks: CoreJsonWebKeySet,
    /// Typed issuer URL, used when constructing the mobile verifier.
    pub issuer: IssuerUrl,
}

impl AuthState {
    /// Verifier for ID tokens minted for the confidential **web** client.
    fn web_id_token_verifier(&self) -> CoreIdTokenVerifier<'_> {
        let trusted = self.trusted_extra_audiences();
        let strict = !self.extra_audiences.is_empty();
        self.oidc_client
            .id_token_verifier()
            .set_other_audience_verifier_fn(move |aud| accept_extra(&trusted, strict, aud.as_str()))
    }

    /// Verifier for ID tokens minted for the public/native **mobile** client.
    ///
    /// `None` when no `mobile_client_id` is configured.
    fn mobile_id_token_verifier(&self) -> Option<CoreIdTokenVerifier<'static>> {
        let mobile_client_id = self.mobile_client_id.clone()?;
        let trusted = self.trusted_extra_audiences();
        let strict = !self.extra_audiences.is_empty();
        Some(
            CoreIdTokenVerifier::new_public_client(
                ClientId::new(mobile_client_id),
                self.issuer.clone(),
                self.jwks.clone(),
            )
            .set_other_audience_verifier_fn(move |aud| accept_extra(&trusted, strict, aud.as_str())),
        )
    }

    /// Audiences trusted *in addition* to the one the verifier itself requires:
    /// our sibling client id plus anything the operator allowlisted.
    fn trusted_extra_audiences(&self) -> Vec<String> {
        let mut trusted = vec![self.client_id.clone()];
        trusted.extend(self.mobile_client_id.clone());
        trusted.extend(self.extra_audiences.iter().cloned());
        trusted
    }
}

/// Decide whether an *additional* `aud` entry is acceptable.
///
/// The security-critical check — that `aud` contains **this** client's id — is
/// enforced by `openidconnect` before this function is ever consulted (OIDC
/// Core §3.1.3.7). This only governs the extra entries a provider is explicitly
/// permitted to list alongside it.
///
/// Zitadel appends the numeric project id to every ID token, so a real token
/// arrives as `aud = [<client_id>@<project>, <project_id>]`. Rejecting unknown
/// extras therefore breaks login against a stock Zitadel while buying nothing:
/// an attacker cannot forge the primary audience, and a token minted for a
/// *different* RP still fails the primary check.
///
/// So: allowlist configured (`strict`) → accept only what is on it. Nothing
/// configured → accept, and log once at WARN with the exact value so it can be
/// pinned via `PICWEIGHT_OIDC_EXTRA_AUDIENCES`.
fn accept_extra(trusted: &[String], strict: bool, aud: &str) -> bool {
    if trusted.iter().any(|t| t == aud) {
        return true;
    }
    if strict {
        tracing::warn!(
            audience = %aud,
            "rejecting ID token: audience is not in PICWEIGHT_OIDC_EXTRA_AUDIENCES"
        );
        return false;
    }
    static SEEN: std::sync::OnceLock<std::sync::Mutex<std::collections::HashSet<String>>> =
        std::sync::OnceLock::new();
    let seen = SEEN.get_or_init(Default::default);
    if let Ok(mut seen) = seen.lock() {
        if seen.insert(aud.to_string()) {
            tracing::warn!(
                audience = %aud,
                "accepting an unrecognised additional ID-token audience (normal for Zitadel, \
                 which appends the project id). Set PICWEIGHT_OIDC_EXTRA_AUDIENCES={} to \
                 pin it to an allowlist.",
                aud
            );
        }
    }
    true
}

/// Discover the OIDC provider and build [`AuthState`].
///
/// Called once at startup; a failure here is fatal (the app has no local
/// password store to fall back on).
pub async fn init_oidc(
    oidc: &crate::config::OidcConfig,
    jwt_secret: &str,
    jwt_ttl_secs: u64,
) -> Result<AuthState, AppError> {
    let http_client = openidconnect::reqwest::ClientBuilder::new()
        .redirect(openidconnect::reqwest::redirect::Policy::none())
        .build()
        .map_err(|e| AppError::Internal(format!("failed to build the OIDC HTTP client: {e}")))?;

    let issuer = IssuerUrl::new(oidc.issuer.clone()).map_err(|e| {
        AppError::Internal(format!(
            "PICWEIGHT_OIDC_ISSUER {:?} is not a valid URL: {e}",
            oidc.issuer
        ))
    })?;

    let provider_metadata = CoreProviderMetadata::discover_async(issuer.clone(), &http_client)
        .await
        .map_err(|e| AppError::Upstream(format!("OIDC discovery against {issuer:?} failed: {e}")))?;

    // Both are needed after `from_provider_metadata` consumes the metadata.
    let auth_uri = provider_metadata.authorization_endpoint().clone();
    let jwks = provider_metadata.jwks().clone();

    let redirect_uri = RedirectUrl::new(oidc.redirect_uri.clone()).map_err(|e| {
        AppError::Internal(format!(
            "PICWEIGHT_OIDC_REDIRECT_URI {:?} is not a valid URL: {e}",
            oidc.redirect_uri
        ))
    })?;

    let oidc_client = CoreClient::from_provider_metadata(
        provider_metadata,
        ClientId::new(oidc.client_id.clone()),
        Some(ClientSecret::new(oidc.client_secret.clone())),
    )
    .set_auth_uri(auth_uri)
    .set_redirect_uri(redirect_uri);

    tracing::info!(
        issuer = %oidc.issuer,
        client_id = %oidc.client_id,
        mobile_client_id = ?oidc.mobile_client_id,
        extra_audiences = ?oidc.extra_audiences,
        "OIDC discovery complete"
    );

    Ok(AuthState {
        oidc_client,
        http_client,
        jwt_encoding_key: EncodingKey::from_secret(jwt_secret.as_bytes()),
        jwt_decoding_key: DecodingKey::from_secret(jwt_secret.as_bytes()),
        jwt_ttl_secs,
        scopes: oidc.scopes.clone(),
        issuer_url: oidc.issuer.clone(),
        client_id: oidc.client_id.clone(),
        mobile_client_id: oidc.mobile_client_id.clone(),
        extra_audiences: oidc.extra_audiences.clone(),
        jwks,
        issuer,
    })
}

/// Routes that must stay reachable without a session.
pub fn create_auth_router(state: AppState) -> Router {
    Router::new()
        .route("/api/auth/login", get(login))
        .route("/api/auth/callback", get(callback))
        .route("/api/auth/me", get(session_info))
        .route("/api/auth/logout", get(logout))
        .route("/api/auth/token", post(token_exchange))
        .route("/api/auth/refresh", post(refresh))
        .route("/api/auth/config", get(auth_config))
        .with_state(state)
}

// ---------------------------------------------------------------------------
// DTOs
// ---------------------------------------------------------------------------

/// Query parameters the IdP appends to the redirect URI.
#[derive(Debug, Clone, Deserialize)]
pub struct CallbackParams {
    /// Authorization code, present on success.
    pub code: Option<String>,
    /// CSRF state token echoed back by the provider.
    ///
    /// Defaulted rather than required so a provider that drops `state` on an
    /// error response produces a readable redirect instead of a raw 400 from
    /// the extractor.
    #[serde(default)]
    pub state: String,
    /// Error code, present on failure.
    pub error: Option<String>,
    /// Human-readable error detail.
    pub error_description: Option<String>,
}

/// OIDC settings a client needs so it only has to be told the server URL.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct AuthConfigResponse {
    /// Issuer URL for discovery.
    pub issuer: String,
    /// Confidential web client id.
    pub client_id: String,
    /// Public/native client id for the Android app, when configured.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mobile_client_id: Option<String>,
    /// Scopes to request.
    pub scopes: Vec<String>,
}

/// Body of `POST /api/auth/token`.
#[derive(Debug, Clone, Deserialize, ToSchema)]
pub struct TokenExchangeRequest {
    /// ID token obtained by the native client from the IdP.
    pub id_token: String,
}

/// A minted picweight session JWT.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct TokenResponse {
    /// The session JWT, for `Authorization: Bearer`.
    pub token: String,
    /// Lifetime in seconds.
    pub expires_in: u64,
}

// ---------------------------------------------------------------------------
// Handlers
// ---------------------------------------------------------------------------

/// `GET /api/auth/login` — redirect to the IdP with PKCE + CSRF state.
#[utoipa::path(
    get,
    path = "/api/auth/login",
    tag = "auth",
    summary = "Initiate OIDC login",
    description = "Starts the authorization-code + PKCE flow for the confidential web \
client. The PKCE verifier, nonce and CSRF token are stored in a short-lived signed cookie \
so the callback needs no server-side session store.",
    responses((status = 302, description = "Redirect to the OIDC provider"))
)]
pub async fn login(State(state): State<AppState>) -> Result<Response, AppError> {
    let auth = &state.auth;
    let (pkce_challenge, pkce_verifier) = PkceCodeChallenge::new_random_sha256();

    let mut request = auth.oidc_client.authorize_url(
        AuthenticationFlow::<CoreResponseType>::AuthorizationCode,
        CsrfToken::new_random,
        Nonce::new_random,
    );
    request = request.set_pkce_challenge(pkce_challenge);
    for scope in &auth.scopes {
        request = request.add_scope(Scope::new(scope.clone()));
    }
    let (auth_url, csrf_token, nonce) = request.url();

    let now = Utc::now().timestamp() as usize;
    let state_claims = OidcStateClaims {
        csrf_token: csrf_token.secret().clone(),
        nonce: nonce.secret().clone(),
        pkce_verifier: pkce_verifier.secret().clone(),
        exp: now + OIDC_STATE_TTL_SECS as usize,
    };
    let state_jwt = encode(&Header::default(), &state_claims, &auth.jwt_encoding_key)
        .map_err(|e| AppError::Internal(format!("failed to sign the OIDC state cookie: {e}")))?;

    let cookie = format!(
        "{OIDC_STATE_COOKIE}={state_jwt}; HttpOnly; SameSite=Lax; Path=/; Max-Age={OIDC_STATE_TTL_SECS}"
    );

    let mut response = Redirect::to(auth_url.as_str()).into_response();
    append_set_cookie(&mut response, &cookie);
    Ok(response)
}

/// `GET /api/auth/callback` — exchange the code for tokens and mint a session.
#[utoipa::path(
    get,
    path = "/api/auth/callback",
    tag = "auth",
    summary = "Handle the OIDC redirect",
    description = "Verifies the CSRF token against the state cookie, exchanges the \
authorization code with the PKCE verifier, validates the ID token (issuer, audience, nonce, \
signature), upserts the `users` row and sets the `picweight_session` cookie.",
    params(
        ("code" = Option<String>, Query, description = "Authorization code"),
        ("state" = String, Query, description = "CSRF state token"),
        ("error" = Option<String>, Query, description = "Provider error code"),
        ("error_description" = Option<String>, Query, description = "Provider error detail"),
    ),
    responses(
        (status = 302, description = "Redirect to the app with a session cookie set"),
        (status = 400, description = "Invalid callback parameters", body = crate::error::ErrorBody),
        (status = 503, description = "Token exchange failed", body = crate::error::ErrorBody),
    )
)]
pub async fn callback(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(params): Query<CallbackParams>,
) -> Result<Response, AppError> {
    let auth = &state.auth;

    // A provider-side refusal ("user cancelled") is not a server error — send
    // the browser back to the login page with the reason attached.
    if let Some(error) = params.error {
        let detail = params.error_description.unwrap_or_else(|| error.clone());
        tracing::warn!(error = %error, detail = %detail, "OIDC provider returned an error");
        let location = format!("{LOGIN_PAGE}?error={}", percent_encode(&detail));
        let mut response = Redirect::to(&location).into_response();
        append_set_cookie(&mut response, &clear_cookie(OIDC_STATE_COOKIE));
        return Ok(response);
    }

    let code = params
        .code
        .ok_or_else(|| AppError::BadRequest("callback is missing the authorization code".into()))?;

    let state_jwt = get_cookie_value(&headers, OIDC_STATE_COOKIE).ok_or_else(|| {
        AppError::BadRequest("login state cookie is missing or expired; start again".into())
    })?;
    let state_claims = decode::<OidcStateClaims>(
        &state_jwt,
        &auth.jwt_decoding_key,
        &Validation::new(Algorithm::HS256),
    )
    .map_err(|e| AppError::BadRequest(format!("login state cookie is not usable: {e}")))?
    .claims;

    if !constant_time_eq(params.state.as_bytes(), state_claims.csrf_token.as_bytes()) {
        return Err(AppError::BadRequest(
            "CSRF state mismatch; the login was not started by this browser".into(),
        ));
    }

    let token_response = auth
        .oidc_client
        .exchange_code(AuthorizationCode::new(code))
        .map_err(|e| AppError::Internal(format!("failed to build the token request: {e}")))?
        .set_pkce_verifier(PkceCodeVerifier::new(state_claims.pkce_verifier))
        .request_async(&auth.http_client)
        .await
        .map_err(|e| AppError::Upstream(format!("OIDC token exchange failed: {e}")))?;

    let id_token = token_response
        .id_token()
        .ok_or_else(|| AppError::Upstream("token response carried no ID token".into()))?;

    let nonce = Nonce::new(state_claims.nonce);
    let claims = id_token
        .claims(&auth.web_id_token_verifier(), &nonce)
        .map_err(|e| AppError::Upstream(format!("ID token verification failed: {e}")))?;

    let session = mint_session(&state, claims).await?;
    let session_cookie = session_cookie_header(&session, auth.jwt_ttl_secs);

    let mut response = Redirect::to(POST_LOGIN_REDIRECT).into_response();
    append_set_cookie(&mut response, &session_cookie);
    append_set_cookie(&mut response, &clear_cookie(OIDC_STATE_COOKIE));
    Ok(response)
}

/// `GET /api/auth/me` — the raw session claims.
#[utoipa::path(
    get,
    path = "/api/auth/me",
    tag = "auth",
    summary = "Current session claims",
    description = "Returns the claims of the presented session JWT without touching the \
database or the IdP. Use `GET /api/v1/me` for the profile and today's targets.",
    responses(
        (status = 200, description = "Session claims", body = SessionClaims),
        (status = 401, description = "Not authenticated", body = crate::error::ErrorBody),
    ),
    security(("session" = []))
)]
pub async fn session_info(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<SessionClaims>, AppError> {
    let claims = parse_session_token(&headers, &state.auth.jwt_decoding_key)?;
    Ok(Json(claims))
}

/// `GET /api/auth/logout` — clear the session cookie.
#[utoipa::path(
    get,
    path = "/api/auth/logout",
    tag = "auth",
    summary = "Log out",
    description = "Clears the `picweight_session` cookie and redirects to the login page. \
Bearer-token clients simply discard their token; nothing is stored server-side.",
    responses((status = 302, description = "Redirect to the login page"))
)]
pub async fn logout() -> Response {
    let mut response = Redirect::to(LOGIN_PAGE).into_response();
    append_set_cookie(&mut response, &clear_cookie(SESSION_COOKIE));
    append_set_cookie(&mut response, &clear_cookie(OIDC_STATE_COOKIE));
    response
}

/// `POST /api/auth/token` — native clients trade an IdP ID token for a session.
#[utoipa::path(
    post,
    path = "/api/auth/token",
    tag = "auth",
    summary = "Exchange an OIDC ID token for a session JWT",
    description = "The Android app runs authorization-code + PKCE against the **native \
(public)** client itself, then posts the resulting ID token here. The token is verified \
against the provider's JWKS — accepted for either the web or the mobile client id — and a \
picweight session JWT is returned for use as `Authorization: Bearer`.",
    request_body(content = TokenExchangeRequest, description = "OIDC ID token to exchange"),
    responses(
        (status = 200, description = "Session JWT", body = TokenResponse),
        (status = 401, description = "ID token verification failed", body = crate::error::ErrorBody),
    )
)]
pub async fn token_exchange(
    State(state): State<AppState>,
    Json(payload): Json<TokenExchangeRequest>,
) -> Result<Json<TokenResponse>, AppError> {
    let id_token: CoreIdToken =
        serde_json::from_value(serde_json::Value::String(payload.id_token))
            .map_err(|e| AppError::BadRequest(format!("id_token is not a well-formed JWT: {e}")))?;

    // The native client ran its own nonce through its own authorization
    // request, so there is nothing for us to compare against here.
    let skip_nonce = |_: Option<&Nonce>| Ok(());

    let claims = match id_token.claims(&state.auth.web_id_token_verifier(), skip_nonce) {
        Ok(claims) => claims,
        Err(web_err) => {
            let verifier = state.auth.mobile_id_token_verifier().ok_or_else(|| {
                tracing::warn!(error = %web_err, "ID token rejected and no mobile client configured");
                AppError::Unauthorized("ID token verification failed".into())
            })?;
            id_token
                .claims(&verifier, |_: Option<&Nonce>| Ok(()))
                .map_err(|mobile_err| {
                    tracing::warn!(
                        web_error = %web_err,
                        mobile_error = %mobile_err,
                        "ID token rejected by both client verifiers"
                    );
                    AppError::Unauthorized("ID token verification failed".into())
                })?
        }
    };

    let token = mint_session(&state, claims).await?;
    Ok(Json(TokenResponse {
        token,
        expires_in: state.auth.jwt_ttl_secs,
    }))
}

/// `POST /api/auth/refresh` — sliding renewal of a still-valid session.
#[utoipa::path(
    post,
    path = "/api/auth/refresh",
    tag = "auth",
    summary = "Refresh the session JWT",
    description = "Accepts a currently-valid session (bearer header or cookie) and issues a \
new JWT with a fresh expiry. An expired session cannot be resurrected — that path is a full \
re-login. Cookie callers also get the cookie re-set.",
    responses(
        (status = 200, description = "A new session JWT", body = TokenResponse),
        (status = 401, description = "Session missing, invalid or expired", body = crate::error::ErrorBody),
    ),
    security(("session" = []))
)]
pub async fn refresh(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Response, AppError> {
    let auth = &state.auth;
    // `parse_session_token` validates `exp`, so this only ever extends a live
    // session.
    let claims = parse_session_token(&headers, &auth.jwt_decoding_key)?;

    let issuer = if claims.iss.trim().is_empty() {
        auth.issuer_url.as_str()
    } else {
        claims.iss.as_str()
    };
    let token = issue_session_jwt(
        &claims.sub,
        &claims.name,
        &claims.email,
        issuer,
        auth.jwt_ttl_secs,
        &auth.jwt_encoding_key,
    )?;

    let mut response = Json(TokenResponse {
        token: token.clone(),
        expires_in: auth.jwt_ttl_secs,
    })
    .into_response();

    // Cookie-authenticated callers (the web SPA) get the cookie re-set too.
    if headers.get(header::AUTHORIZATION).is_none() {
        append_set_cookie(
            &mut response,
            &session_cookie_header(&token, auth.jwt_ttl_secs),
        );
    }
    Ok(response)
}

/// `GET /api/auth/config` — OIDC settings for self-configuring clients.
#[utoipa::path(
    get,
    path = "/api/auth/config",
    tag = "auth",
    summary = "OIDC configuration",
    description = "Issuer, client ids and scopes, so the Android app and the SPA only need \
to be told the picweight server URL.",
    responses((status = 200, description = "OIDC configuration", body = AuthConfigResponse))
)]
pub async fn auth_config(State(state): State<AppState>) -> Json<AuthConfigResponse> {
    Json(AuthConfigResponse {
        issuer: state.auth.issuer_url.clone(),
        client_id: state.auth.client_id.clone(),
        mobile_client_id: state.auth.mobile_client_id.clone(),
        scopes: state.auth.scopes.clone(),
    })
}

// ---------------------------------------------------------------------------
// Middleware + extractor
// ---------------------------------------------------------------------------

/// Middleware applied to every `/api/v1/*` route.
///
/// Validates the session (bearer header first, then cookie), inserts
/// [`SessionClaims`] and the resolved [`CurrentUser`] into the request
/// extensions, and rejects with 401 otherwise.
pub async fn require_auth(
    State(state): State<AppState>,
    mut request: axum::extract::Request,
    next: Next,
) -> Result<Response, AppError> {
    let claims = parse_session_token(request.headers(), &state.auth.jwt_decoding_key)?;

    // Resolving the row here — rather than in each handler — is what lets every
    // user-scoped query simply filter on `CurrentUser::id`.
    let issuer = state.auth.issuer_url.clone();
    let for_db = claims.clone();
    let user = state
        .interact(move |conn| ensure_user(conn, &for_db, &issuer))
        .await?;

    request.extensions_mut().insert(CurrentUser::from(&user));
    request.extensions_mut().insert(claims);
    Ok(next.run(request).await)
}

/// The authenticated caller, resolved to a `users` row.
///
/// Every user-scoped query filters on [`CurrentUser::id`]; this is where the
/// per-user isolation the PRD requires is anchored.
#[derive(Debug, Clone)]
pub struct CurrentUser {
    /// `users.id` (UUID string).
    pub id: String,
    /// `users.oidc_sub`.
    pub oidc_sub: String,
    /// `users.email`.
    pub email: Option<String>,
    /// `users.display_name`.
    pub display_name: Option<String>,
}

impl From<&User> for CurrentUser {
    fn from(user: &User) -> Self {
        CurrentUser {
            id: user.id.clone(),
            oidc_sub: user.oidc_sub.clone(),
            email: user.email.clone(),
            display_name: user.display_name.clone(),
        }
    }
}

impl FromRequestParts<AppState> for CurrentUser {
    type Rejection = AppError;

    async fn from_request_parts(
        parts: &mut Parts,
        _state: &AppState,
    ) -> Result<Self, Self::Rejection> {
        parts
            .extensions
            .get::<CurrentUser>()
            .cloned()
            .ok_or_else(|| AppError::Unauthorized("no authenticated session".to_string()))
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Turn verified ID-token claims into a picweight session, creating or
/// refreshing the `users` row on the way through.
///
/// Shared by the web callback and the native token exchange so both flows agree
/// on what a session means.
async fn mint_session(state: &AppState, claims: &CoreIdTokenClaims) -> Result<String, AppError> {
    let sub = claims.subject().to_string();
    let name = claims
        .name()
        .and_then(|localized| localized.get(None))
        .map(|name| name.to_string())
        .or_else(|| claims.preferred_username().map(|u| u.to_string()))
        .unwrap_or_default();
    let email = claims.email().map(|e| e.to_string()).unwrap_or_default();
    let issuer = state.auth.issuer_url.clone();

    let token = issue_session_jwt(
        &sub,
        &name,
        &email,
        &issuer,
        state.auth.jwt_ttl_secs,
        &state.auth.jwt_encoding_key,
    )?;

    // Create the row now so the very first `/api/v1` call finds it, and so a
    // login against an unwritable database fails at login rather than later.
    let session_claims = parse_claims(&token, &state.auth.jwt_decoding_key)?;
    let user = state
        .interact(move |conn| ensure_user(conn, &session_claims, &issuer))
        .await?;
    tracing::info!(user_id = %user.id, oidc_sub = %user.oidc_sub, "session issued");

    Ok(token)
}

/// Mint a session JWT for an identity.
pub fn issue_session_jwt(
    sub: &str,
    name: &str,
    email: &str,
    issuer: &str,
    ttl_secs: u64,
    key: &EncodingKey,
) -> Result<String, AppError> {
    let now = Utc::now().timestamp() as usize;
    let claims = SessionClaims {
        sub: sub.to_string(),
        name: name.to_string(),
        email: email.to_string(),
        iss: issuer.to_string(),
        exp: now + ttl_secs as usize,
        iat: now,
    };
    encode(&Header::default(), &claims, key)
        .map_err(|e| AppError::Internal(format!("failed to sign the session token: {e}")))
}

/// Validate a session JWT taken from the `Authorization` header or the cookie.
pub fn parse_session_token(
    headers: &HeaderMap,
    key: &DecodingKey,
) -> Result<SessionClaims, AppError> {
    // Bearer header first (Android), then the cookie (web SPA).
    let token = match headers.get(header::AUTHORIZATION) {
        Some(value) => {
            let value = value
                .to_str()
                .map_err(|_| AppError::Unauthorized("malformed Authorization header".into()))?;
            value
                .strip_prefix("Bearer ")
                .or_else(|| value.strip_prefix("bearer "))
                .ok_or_else(|| {
                    AppError::Unauthorized("Authorization header is not a bearer token".into())
                })?
                .trim()
                .to_string()
        }
        None => get_cookie_value(headers, SESSION_COOKIE)
            .ok_or_else(|| AppError::Unauthorized("no session cookie or bearer token".into()))?,
    };
    parse_claims(&token, key)
}

/// Decode and validate a session JWT.
fn parse_claims(token: &str, key: &DecodingKey) -> Result<SessionClaims, AppError> {
    let data = decode::<SessionClaims>(token, key, &Validation::new(Algorithm::HS256))
        .map_err(|e| AppError::Unauthorized(format!("invalid session token: {e}")))?;
    Ok(data.claims)
}

/// Read a cookie value out of a request's `Cookie` header.
pub fn get_cookie_value(headers: &HeaderMap, name: &str) -> Option<String> {
    let header = headers.get(axum::http::header::COOKIE)?.to_str().ok()?;
    let prefix = format!("{name}=");
    header
        .split(';')
        .map(str::trim)
        .find(|s| s.starts_with(&prefix))?
        .strip_prefix(&prefix)
        .map(str::to_string)
}

/// `Set-Cookie` value that installs a session.
pub fn session_cookie_header(session_jwt: &str, ttl_secs: u64) -> String {
    format!("{SESSION_COOKIE}={session_jwt}; HttpOnly; SameSite=Lax; Path=/; Max-Age={ttl_secs}")
}

/// `Set-Cookie` value that clears a cookie.
pub fn clear_cookie(name: &str) -> String {
    format!("{name}=; HttpOnly; SameSite=Lax; Path=/; Max-Age=0")
}

/// Append a `Set-Cookie` header, dropping it with a log line if it somehow is
/// not a legal header value (JWTs and our fixed attributes always are).
fn append_set_cookie(response: &mut Response, cookie: &str) {
    match HeaderValue::from_str(cookie) {
        Ok(value) => {
            response.headers_mut().append(header::SET_COOKIE, value);
        }
        Err(e) => tracing::error!(error = %e, "refusing to emit a malformed Set-Cookie header"),
    }
}

/// Percent-encode a string for use in a query-string value.
///
/// A single-purpose replacement for the `urlencoding` crate phos uses; picweight
/// only needs it for the login error redirect.
fn percent_encode(input: &str) -> String {
    const HEX: &[u8; 16] = b"0123456789ABCDEF";
    let mut out = String::with_capacity(input.len());
    for byte in input.as_bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(*byte as char)
            }
            other => {
                out.push('%');
                out.push(HEX[(other >> 4) as usize] as char);
                out.push(HEX[(other & 0x0f) as usize] as char);
            }
        }
    }
    out
}

/// Length-independent-ish byte comparison, used for the CSRF token.
fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut diff = 0u8;
    for (x, y) in a.iter().zip(b.iter()) {
        diff |= x ^ y;
    }
    diff == 0
}

/// Trim a claim, mapping the empty string to `None`.
///
/// The IdP may omit `name` or `email` entirely; storing `""` would make
/// "unknown" indistinguishable from "deliberately blank".
fn non_empty(value: &str) -> Option<String> {
    let trimmed = value.trim();
    (!trimmed.is_empty()).then(|| trimmed.to_string())
}

/// Resolve `(issuer, sub)` to a `users` row, inserting it on first login and
/// refreshing the display name / email from the ID token.
///
/// `users.oidc_sub` is globally `UNIQUE` (PRD §8) while `(oidc_issuer, sub)` is
/// merely indexed, so the lookup is: exact identity first, then `sub` alone. The
/// second probe is what lets an operator re-point `PICWEIGHT_OIDC_ISSUER` at a
/// migrated IdP without orphaning everyone's history — the row's issuer is
/// rewritten rather than a duplicate insert failing on the unique index.
pub fn ensure_user(
    conn: &mut SqliteConnection,
    claims: &SessionClaims,
    issuer: &str,
) -> Result<User, AppError> {
    use crate::schema::users::dsl as u;

    // The token's own issuer wins when present; the argument is the fallback
    // for sessions minted before `iss` was a claim.
    let issuer = if claims.iss.trim().is_empty() {
        issuer
    } else {
        claims.iss.as_str()
    };
    let sub = claims.sub.trim();
    if sub.is_empty() {
        return Err(AppError::Unauthorized(
            "session token carries no subject".into(),
        ));
    }
    let email = non_empty(&claims.email);
    let display_name = non_empty(&claims.name);

    let existing: Option<User> = u::users
        .filter(u::oidc_issuer.eq(issuer))
        .filter(u::oidc_sub.eq(sub))
        .select(User::as_select())
        .first(conn)
        .optional()?;
    let existing = match existing {
        Some(user) => Some(user),
        None => u::users
            .filter(u::oidc_sub.eq(sub))
            .select(User::as_select())
            .first(conn)
            .optional()?,
    };

    if let Some(user) = existing {
        let changeset = UserChangeset {
            oidc_issuer: (user.oidc_issuer != issuer).then(|| issuer.to_string()),
            // `email`/`display_name` are `Option<Option<_>>`: outer `None`
            // leaves the column alone, `Some(None)` writes SQL NULL.
            email: (user.email != email).then(|| email.clone()),
            display_name: (user.display_name != display_name).then(|| display_name.clone()),
        };
        if changeset.oidc_issuer.is_none()
            && changeset.email.is_none()
            && changeset.display_name.is_none()
        {
            return Ok(user);
        }
        let updated = diesel::update(u::users.filter(u::id.eq(&user.id)))
            .set(changeset)
            .returning(User::as_returning())
            .get_result(conn)?;
        return Ok(updated);
    }

    let new_user = NewUser {
        id: uuid::Uuid::new_v4().to_string(),
        oidc_sub: sub.to_string(),
        oidc_issuer: issuer.to_string(),
        email,
        display_name,
        created_at: Utc::now().naive_utc(),
    };
    match diesel::insert_into(u::users)
        .values(&new_user)
        .returning(User::as_returning())
        .get_result(conn)
    {
        Ok(user) => {
            tracing::info!(user_id = %user.id, oidc_sub = %user.oidc_sub, "registered a new user");
            Ok(user)
        }
        // Two first-time requests raced; the other one won.
        Err(diesel::result::Error::DatabaseError(
            diesel::result::DatabaseErrorKind::UniqueViolation,
            _,
        )) => u::users
            .filter(u::oidc_sub.eq(sub))
            .select(User::as_select())
            .first(conn)
            .map_err(AppError::from),
        Err(err) => Err(err.into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::{establish_pool, run_migrations, DbPool};

    fn make_keys(secret: &str) -> (EncodingKey, DecodingKey) {
        (
            EncodingKey::from_secret(secret.as_bytes()),
            DecodingKey::from_secret(secret.as_bytes()),
        )
    }

    fn bearer_headers(token: &str) -> HeaderMap {
        let mut headers = HeaderMap::new();
        headers.insert(
            header::AUTHORIZATION,
            format!("Bearer {token}").parse().expect("valid header"),
        );
        headers
    }

    fn claims_for(sub: &str, name: &str, email: &str) -> SessionClaims {
        let now = Utc::now().timestamp() as usize;
        SessionClaims {
            sub: sub.to_string(),
            name: name.to_string(),
            email: email.to_string(),
            iss: "https://idp.example.com".to_string(),
            exp: now + 3600,
            iat: now,
        }
    }

    fn test_pool(dir: &tempfile::TempDir) -> DbPool {
        let pool = establish_pool(dir.path().join("auth-test.db")).expect("pool");
        run_migrations(&pool).expect("migrations");
        pool
    }

    #[test]
    fn issue_and_parse_roundtrip_via_bearer_header() {
        let (enc, dec) = make_keys("test-secret");
        let token = issue_session_jwt(
            "user-1",
            "Alice",
            "alice@example.com",
            "https://idp.example.com",
            3600,
            &enc,
        )
        .expect("token");
        let claims = parse_session_token(&bearer_headers(&token), &dec).expect("claims");
        assert_eq!(claims.sub, "user-1");
        assert_eq!(claims.name, "Alice");
        assert_eq!(claims.email, "alice@example.com");
        assert_eq!(claims.iss, "https://idp.example.com");
        assert!(claims.exp > claims.iat);
    }

    #[test]
    fn parse_falls_back_to_the_cookie() {
        let (enc, dec) = make_keys("test-secret");
        let token = issue_session_jwt("user-1", "", "", "https://idp", 3600, &enc).expect("token");
        let mut headers = HeaderMap::new();
        headers.insert(
            header::COOKIE,
            format!("other=x; {SESSION_COOKIE}={token}; trailing=y")
                .parse()
                .expect("valid header"),
        );
        let claims = parse_session_token(&headers, &dec).expect("claims");
        assert_eq!(claims.sub, "user-1");
    }

    #[test]
    fn parse_rejects_an_expired_token() {
        let (enc, dec) = make_keys("test-secret");
        // Beyond jsonwebtoken's default 60s leeway.
        let now = Utc::now().timestamp() as usize;
        let claims = SessionClaims {
            sub: "user-1".into(),
            name: String::new(),
            email: String::new(),
            iss: String::new(),
            exp: now - 120,
            iat: now - 3720,
        };
            let token = encode(&Header::default(), &claims, &enc).expect("token");
        let err = parse_session_token(&bearer_headers(&token), &dec).expect_err("expired");
        assert_eq!(err.status(), axum::http::StatusCode::UNAUTHORIZED);
    }

    #[test]
    fn parse_rejects_a_token_signed_with_another_secret() {
        let (enc, _) = make_keys("secret-a");
        let (_, dec) = make_keys("secret-b");
        let token = issue_session_jwt("user-1", "", "", "https://idp", 3600, &enc).expect("token");
        let err = parse_session_token(&bearer_headers(&token), &dec).expect_err("bad signature");
        assert_eq!(err.status(), axum::http::StatusCode::UNAUTHORIZED);
    }

    #[test]
    fn parse_rejects_a_missing_session() {
        let (_, dec) = make_keys("test-secret");
        let err = parse_session_token(&HeaderMap::new(), &dec).expect_err("no session");
        assert_eq!(err.status(), axum::http::StatusCode::UNAUTHORIZED);
    }

    #[test]
    fn refresh_extends_the_expiry_and_preserves_the_claims() {
        let (enc, dec) = make_keys("test-secret");
        let now = Utc::now().timestamp() as usize;
        let old = SessionClaims {
            sub: "user-1".into(),
            name: "Alice".into(),
            email: "alice@example.com".into(),
            iss: "https://idp.example.com".into(),
            exp: now + 60,
            iat: now - 3540,
        };
            let old_token = encode(&Header::default(), &old, &enc).expect("token");
        let parsed = parse_session_token(&bearer_headers(&old_token), &dec).expect("claims");

        let new_token = issue_session_jwt(
            &parsed.sub,
            &parsed.name,
            &parsed.email,
            &parsed.iss,
            3600,
            &enc,
        )
        .expect("token");
        let renewed = parse_session_token(&bearer_headers(&new_token), &dec).expect("claims");
        assert_eq!(renewed.sub, old.sub);
        assert_eq!(renewed.name, old.name);
        assert_eq!(renewed.email, old.email);
        assert_eq!(renewed.iss, old.iss);
        assert!(renewed.exp > old.exp);
    }

    #[test]
    fn cookie_headers_carry_the_picweight_prefix() {
        assert!(session_cookie_header("abc", 60).starts_with("picweight_session=abc;"));
        assert!(session_cookie_header("abc", 60).contains("Max-Age=60"));
        assert!(clear_cookie(SESSION_COOKIE).contains("Max-Age=0"));
        assert_eq!(OIDC_STATE_COOKIE, "picweight_oidc_state");
    }

    #[test]
    fn percent_encoding_escapes_query_metacharacters() {
        assert_eq!(percent_encode("a b&c=d"), "a%20b%26c%3Dd");
        assert_eq!(percent_encode("safe-._~"), "safe-._~");
        // Multi-byte input is encoded per UTF-8 byte.
        assert_eq!(percent_encode("é"), "%C3%A9");
    }

    #[test]
    fn ensure_user_inserts_then_reuses_the_same_row() {
        let dir = tempfile::tempdir().expect("tempdir");
        let pool = test_pool(&dir);
        let mut conn = pool.get().expect("conn");

        let claims = claims_for("sub-1", "Alice", "alice@example.com");
        let first = ensure_user(&mut conn, &claims, "https://fallback").expect("insert");
        assert_eq!(first.oidc_sub, "sub-1");
        assert_eq!(first.oidc_issuer, "https://idp.example.com");
        assert_eq!(first.email.as_deref(), Some("alice@example.com"));
        assert_eq!(first.display_name.as_deref(), Some("Alice"));

        let second = ensure_user(&mut conn, &claims, "https://fallback").expect("reuse");
        assert_eq!(second.id, first.id);
    }

    #[test]
    fn ensure_user_refreshes_identity_fields() {
        let dir = tempfile::tempdir().expect("tempdir");
        let pool = test_pool(&dir);
        let mut conn = pool.get().expect("conn");

        let first = ensure_user(
            &mut conn,
            &claims_for("sub-1", "Alice", "alice@example.com"),
            "https://fallback",
        )
        .expect("insert");

        let updated = ensure_user(
            &mut conn,
            &claims_for("sub-1", "Alice Smith", "alice.smith@example.com"),
            "https://fallback",
        )
        .expect("update");

        assert_eq!(updated.id, first.id);
        assert_eq!(updated.display_name.as_deref(), Some("Alice Smith"));
        assert_eq!(updated.email.as_deref(), Some("alice.smith@example.com"));
    }

    #[test]
    fn ensure_user_treats_blank_claims_as_absent() {
        let dir = tempfile::tempdir().expect("tempdir");
        let pool = test_pool(&dir);
        let mut conn = pool.get().expect("conn");

        let user = ensure_user(&mut conn, &claims_for("sub-1", "   ", ""), "https://fallback")
            .expect("insert");
        assert!(user.email.is_none());
        assert!(user.display_name.is_none());
    }

    #[test]
    fn ensure_user_follows_an_issuer_change_instead_of_duplicating() {
        let dir = tempfile::tempdir().expect("tempdir");
        let pool = test_pool(&dir);
        let mut conn = pool.get().expect("conn");

        let first = ensure_user(
            &mut conn,
            &claims_for("sub-1", "Alice", "alice@example.com"),
            "https://fallback",
        )
        .expect("insert");

        let mut moved = claims_for("sub-1", "Alice", "alice@example.com");
        moved.iss = "https://new-idp.example.com".into();
        let after = ensure_user(&mut conn, &moved, "https://fallback").expect("re-home");

        assert_eq!(after.id, first.id);
        assert_eq!(after.oidc_issuer, "https://new-idp.example.com");
    }

    #[test]
    fn ensure_user_falls_back_to_the_configured_issuer() {
        let dir = tempfile::tempdir().expect("tempdir");
        let pool = test_pool(&dir);
        let mut conn = pool.get().expect("conn");

        let mut claims = claims_for("sub-1", "Alice", "alice@example.com");
        claims.iss = String::new();
        let user = ensure_user(&mut conn, &claims, "https://configured.example.com")
            .expect("insert");
        assert_eq!(user.oidc_issuer, "https://configured.example.com");
    }

    #[test]
    fn ensure_user_rejects_a_subjectless_token() {
        let dir = tempfile::tempdir().expect("tempdir");
        let pool = test_pool(&dir);
        let mut conn = pool.get().expect("conn");

        let err = ensure_user(&mut conn, &claims_for("  ", "", ""), "https://idp")
            .expect_err("no subject");
        assert_eq!(err.status(), axum::http::StatusCode::UNAUTHORIZED);
    }

    #[test]
    fn distinct_subjects_get_distinct_users() {
        let dir = tempfile::tempdir().expect("tempdir");
        let pool = test_pool(&dir);
        let mut conn = pool.get().expect("conn");

        let a = ensure_user(&mut conn, &claims_for("sub-a", "A", "a@x"), "https://idp")
            .expect("insert a");
        let b = ensure_user(&mut conn, &claims_for("sub-b", "B", "b@x"), "https://idp")
            .expect("insert b");
        assert_ne!(a.id, b.id);
    }

    #[test]
    fn current_user_is_projected_from_the_row() {
        let user = User {
            id: "u-1".into(),
            oidc_sub: "sub-1".into(),
            oidc_issuer: "https://idp".into(),
            email: Some("a@example.com".into()),
            display_name: Some("Alice".into()),
            created_at: Utc::now().naive_utc(),
        };
        let current = CurrentUser::from(&user);
        assert_eq!(current.id, "u-1");
        assert_eq!(current.oidc_sub, "sub-1");
        assert_eq!(current.email.as_deref(), Some("a@example.com"));
        assert_eq!(current.display_name.as_deref(), Some("Alice"));
    }

    #[test]
    fn constant_time_eq_behaves_like_eq() {
        assert!(constant_time_eq(b"abc", b"abc"));
        assert!(!constant_time_eq(b"abc", b"abd"));
        assert!(!constant_time_eq(b"abc", b"ab"));
        assert!(constant_time_eq(b"", b""));
    }

    // --- additional ID-token audiences ------------------------------------
    //
    // Regression tests for a real deployment failure: logging in against a
    // stock Zitadel returned
    //   "ID token verification failed: Invalid audiences:
    //    `380466054289163197` is not a trusted audience"
    // because Zitadel appends the numeric *project id* to `aud` alongside the
    // client id, and the verifier trusted only the sibling client id.

    /// The exact shape Zitadel mints: `[<client_id>@<project>, <project_id>]`.
    const ZITADEL_PROJECT_ID: &str = "380466054289163197";

    fn trusted() -> Vec<String> {
        vec![
            "380466054289163197@picweight".to_string(),
            "380466054289163199@picweight".to_string(),
        ]
    }

    #[test]
    fn zitadels_project_id_audience_is_accepted_by_default() {
        // The failure that broke a real deployment. With no allowlist
        // configured this must pass, or login is impossible against Zitadel.
        assert!(accept_extra(&trusted(), false, ZITADEL_PROJECT_ID));
    }

    #[test]
    fn our_own_client_ids_are_always_trusted_in_either_mode() {
        for strict in [false, true] {
            assert!(accept_extra(&trusted(), strict, "380466054289163197@picweight"));
            assert!(accept_extra(&trusted(), strict, "380466054289163199@picweight"));
        }
    }

    #[test]
    fn an_allowlist_makes_verification_strict_again() {
        let mut allowlisted = trusted();
        allowlisted.push(ZITADEL_PROJECT_ID.to_string());
        assert!(accept_extra(&allowlisted, true, ZITADEL_PROJECT_ID));
        // Anything not on the list is refused once the operator opts in.
        assert!(!accept_extra(&allowlisted, true, "some-other-rp"));
    }

    #[test]
    fn a_foreign_audience_is_refused_only_in_strict_mode() {
        // Permissive mode accepts it — safe because openidconnect separately
        // enforces that `aud` contains *our* client id (OIDC Core §3.1.3.7),
        // so a token minted for another RP still fails the primary check.
        assert!(accept_extra(&trusted(), false, "some-other-rp"));
        assert!(!accept_extra(&trusted(), true, "some-other-rp"));
    }
}
