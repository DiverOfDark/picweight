//! picweight — AI-assisted calorie & БЖУ tracker.
//!
//! Photograph what you eat; a server-side estimation agent returns calories and
//! macros; every logged meal immediately tells you what budget is left today.
//!
//! # Module map
//!
//! | Module | Owns |
//! |---|---|
//! | [`config`] | `PICWEIGHT_*` environment, resolved once at startup |
//! | [`db`] | r2d2 pool, WAL/foreign-key pragmas, embedded migrations |
//! | [`schema`] / [`models`] | diesel tables and row structs |
//! | [`error`] | [`AppError`], the single crate-wide error type |
//! | [`auth`] | OIDC discovery, session JWTs, the [`auth::CurrentUser`] extractor |
//! | [`storage`] | 768px content-addressed thumbnails (originals are discarded) |
//! | [`agent`] | the rig 0.41 estimation loop — the swap boundary (§5) |
//! | [`jobs`] | the analysis worker and the notification-group settler |
//! | [`food`] | Open Food Facts barcode cache (barcode only, §1.3) |
//! | [`nutrition`] | Mifflin-St Jeor targets — arithmetic, never an LLM (§6) |
//! | [`feedback`] | day state, verdict phrasing, per-meal notification copy |
//! | [`api`] | the `/api/v1` surface, all utoipa-annotated |
//!
//! Everything hangs off [`AppState`], which is the axum router state and is
//! also what background workers are handed.

pub mod agent;
pub mod api;
pub mod auth;
pub mod config;
pub mod db;
pub mod error;
pub mod feedback;
pub mod food;
pub mod jobs;
pub mod models;
pub mod nutrition;
pub mod schema;
pub mod storage;

use std::sync::Arc;

use axum::Router;
use tower_http::services::{ServeDir, ServeFile};
use utoipa::OpenApi;
use utoipa_scalar::{Scalar, Servable};

pub use config::Config;
pub use db::{DbConnection, DbPool};
pub use error::AppError;

/// Version string stamped in by `build.rs` (git tag, branch, or `sha-…`).
pub const VERSION: &str = env!("PICWEIGHT_VERSION");

/// The API path prefix every versioned route lives under.
pub const API_PREFIX: &str = "/api/v1";

/// Shared application state: the router state, and what every background
/// worker is handed.
///
/// Cheap to clone — every field is either a pool handle, an `Arc`, or a channel
/// sender.
#[derive(Clone)]
pub struct AppState {
    /// SQLite connection pool.
    pub pool: DbPool,
    /// Resolved runtime configuration.
    pub config: Arc<Config>,
    /// OIDC provider metadata and JWT keys.
    pub auth: auth::AuthState,
    /// The rig agent, its tools and its prompt version.
    pub agent: Arc<agent::AgentHandle>,
    /// SSE fan-out for `/api/v1/meals/events`.
    pub events: api::events::EventBus,
    /// Enqueues analysis and re-analysis work for the background workers.
    pub jobs: jobs::JobQueue,
    /// Shared outbound HTTP client (Open Food Facts, web search). rig owns the
    /// OpenAI transport separately.
    pub http: reqwest::Client,
    /// Metadata for the APK this image bundles, resolved once at startup and
    /// served by [`api::client::client_version`].
    ///
    /// Read here rather than per request because neither the APK nor its
    /// sidecar can change without a new image, and `Arc` because `AppState` is
    /// cloned for every request.
    pub client_release: Arc<api::client::ClientVersionResponse>,
}

impl AppState {
    /// Assemble the shared state.
    ///
    /// The [`jobs::JobQueue`] must be created first (its receiver is handed to
    /// [`jobs::spawn_workers`] after this returns), because `AppState` itself
    /// carries the sender that API handlers use to enqueue work.
    ///
    /// The bundled-APK metadata is read here, from `config.static_dir`, rather
    /// than taken as an argument — it is derived state, and
    /// [`api::client::load_bundled_release`] is infallible by construction, so
    /// a missing or broken sidecar cannot stop the server from starting.
    pub fn new(
        pool: DbPool,
        config: Arc<Config>,
        auth: auth::AuthState,
        agent: Arc<agent::AgentHandle>,
        events: api::events::EventBus,
        jobs: jobs::JobQueue,
        http: reqwest::Client,
    ) -> Self {
        let client_release = Arc::new(api::client::load_bundled_release(&config.static_dir));
        Self {
            pool,
            config,
            auth,
            agent,
            events,
            jobs,
            http,
            client_release,
        }
    }

    /// Check out a database connection.
    pub fn conn(&self) -> Result<DbConnection, AppError> {
        self.pool.get().map_err(AppError::from)
    }

    /// Run a blocking diesel closure on the blocking pool without holding up a
    /// tokio worker thread.
    ///
    /// Every handler that touches the database should go through this: diesel
    /// is synchronous, and SQLite writes can block for up to
    /// [`db::BUSY_TIMEOUT_MS`].
    pub async fn interact<F, T>(&self, f: F) -> Result<T, AppError>
    where
        F: FnOnce(&mut DbConnection) -> Result<T, AppError> + Send + 'static,
        T: Send + 'static,
    {
        let pool = self.pool.clone();
        tokio::task::spawn_blocking(move || {
            let mut conn = pool.get().map_err(AppError::from)?;
            f(&mut conn)
        })
        .await?
    }
}

/// Assemble the complete HTTP surface.
///
/// Layering, ported from phos `backend/src/main.rs`:
///
/// ```text
/// /healthz                  → liveness, no auth
/// /api/auth/*               → OIDC login, no auth
/// /api/v1/client/version    → bundled-APK metadata, no auth (see below)
/// /api/v1/*                 → guarded by `auth::require_auth`
/// /api/docs                 → Scalar, rendering `api::ApiDoc`
/// /api/**                   → JSON 404 (an unmatched API path is never the SPA)
/// everything else           → ServeDir(static_dir), falling back to index.html
/// ```
///
/// `/api/v1/client/version` is merged from [`api::create_public_router`], so it
/// never meets the auth layer. It describes the `/picweight.apk` the static
/// fallback already hands to anyone — see [`api::client::client_version`].
///
/// Two deliberate divergences from phos's `main.rs:394`:
///
/// 1. **`ServeDir::fallback`, not `ServeDir::not_found_service`.** The latter
///    wraps the fallback in `SetStatus(404)` — its documented purpose is a
///    custom *error* page. Used as an SPA fallback it serves `index.html` with
///    a `404` status, so every deep link (`/history/2026-08-01`) is a 404 that
///    happens to render. `fallback` preserves the `200`.
/// 2. **An explicit `/api/**` catch-all.** With (1) in place, the static
///    fallback would answer an unmatched `/api/v1/typo` with `200 text/html`,
///    which is precisely the failure phos's APK-availability probe has to
///    content-type-sniff around (PRD §7). A wildcard route below every real API
///    path — matchit ranks static and named segments above catch-alls — turns
///    that back into a JSON `404`.
///
/// This lives in the library rather than in `main.rs` so the integration suite
/// exercises the *same* wiring the binary serves (PRD §13).
///
/// Tracing and CORS are deliberately **not** applied here; `main.rs` adds them,
/// because a test server wants neither a global subscriber nor permissive CORS.
pub fn build_router(state: AppState) -> Router {
    let static_dir = state.config.static_dir.clone();
    let spa_index = state.config.spa_index();

    Router::new()
        .merge(api::create_public_router().with_state(state.clone()))
        .merge(auth::create_auth_router(state.clone()))
        .merge(api::create_router(state))
        .merge(Scalar::with_url("/api/docs", api::ApiDoc::openapi()))
        .route("/api", axum::routing::any(api_not_found))
        .route("/api/{*rest}", axum::routing::any(api_not_found))
        .fallback_service(ServeDir::new(static_dir).fallback(ServeFile::new(spa_index)))
}

/// Answer for any `/api/...` path no route claimed.
///
/// Returns the same [`error::ErrorBody`] shape as every other API failure, so a
/// generated client never has to special-case "the server handed me HTML".
async fn api_not_found(uri: axum::http::Uri) -> AppError {
    AppError::NotFound(format!("no API route matches {}", uri.path()))
}
