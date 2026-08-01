//! The HTTP surface.
//!
//! Everything versioned lives under [`crate::API_PREFIX`] (`/api/v1`) and is
//! behind [`crate::auth::require_auth`]. `/api/auth/*` (see [`crate::auth`]) and
//! `/healthz` are the only unauthenticated routes; everything else falls
//! through to the static SPA (PRD §7).
//!
//! Every route is `utoipa`-annotated so `android/openapi.json` regenerates
//! cleanly — the Android Retrofit client is *generated* from that spec, so an
//! un-annotated route is a route the phone cannot call.

pub mod barcode;
pub mod days;
pub mod dishes;
pub mod events;
pub mod export;
pub mod groups;
pub mod meals;
pub mod profile;
pub mod weights;

use crate::AppState;
use axum::extract::DefaultBodyLimit;
use axum::routing::{get, post, put};
use axum::{Json, Router};
use serde::{Deserialize, Serialize};
use utoipa::openapi::security::{HttpAuthScheme, HttpBuilder, SecurityScheme};
use utoipa::{Modify, OpenApi, ToSchema};

/// The generated OpenAPI document.
#[derive(OpenApi)]
#[openapi(
    info(
        title = "picweight API",
        description = "AI-assisted calorie & БЖУ tracker. Photograph what you eat; \
a server-side estimation agent returns calories and macros.",
        version = env!("PICWEIGHT_VERSION"),
        license(name = "MIT"),
    ),
    paths(
        // Auth
        crate::auth::login,
        crate::auth::callback,
        crate::auth::session_info,
        crate::auth::logout,
        crate::auth::token_exchange,
        crate::auth::refresh,
        crate::auth::auth_config,
        // Profile & targets
        profile::get_me,
        profile::update_profile,
        // Meals
        meals::create_meal,
        meals::list_meals,
        meals::get_meal,
        meals::patch_meal,
        meals::delete_meal,
        meals::reanalyze_meal,
        meals::meal_revisions,
        meals::get_meal_thumbnail,
        events::meal_events,
        // Sittings
        groups::get_group,
        // Days
        days::get_day,
        // Zero-keyboard inputs
        dishes::recent_dishes,
        // Barcode
        barcode::resolve_barcode,
        // Weight
        weights::log_weight,
        weights::list_weights,
        // Export
        export::export_data,
        // System
        healthz,
    ),
    components(schemas(
        crate::error::ErrorBody,
        // Auth
        crate::auth::SessionClaims,
        crate::auth::AuthConfigResponse,
        crate::auth::TokenExchangeRequest,
        crate::auth::TokenResponse,
        // Domain enums
        crate::models::MealStatus,
        crate::models::NameSource,
        crate::models::GramsSource,
        crate::models::MacroSource,
        crate::models::Sex,
        crate::models::GoalType,
        crate::models::WeightSource,
        // Agent output
        crate::agent::schema::MealEstimate,
        crate::agent::schema::EstimatedItem,
        crate::agent::schema::MacroTotals,
        // Feedback
        crate::feedback::state::DayState,
        crate::feedback::state::DayStatus,
        crate::feedback::MealFeedback,
        // Nutrition
        crate::nutrition::targets::Targets,
        crate::nutrition::targets::TargetInputs,
        // Meals
        meals::MealResponse,
        meals::MealItemResponse,
        meals::MealAcceptedResponse,
        meals::CreateMealForm,
        meals::PatchMealRequest,
        meals::PatchMealItem,
        meals::ReanalyzeRequest,
        meals::RevisionEntry,
        meals::RevisionsResponse,
        // Events
        events::MealEvent,
        events::MealEventKind,
        // Groups
        groups::GroupResponse,
        groups::GroupSummary,
        // Profile
        profile::MeResponse,
        profile::UserResponse,
        profile::ProfileResponse,
        profile::UpdateProfileRequest,
        profile::UpdateProfileResponse,
        // Days
        days::DayResponse,
        // Dishes
        dishes::RecentDish,
        // Barcode
        barcode::FoodResponse,
        crate::food::FoodFacts,
        // Weights
        weights::LogWeightRequest,
        weights::WeightLogResponse,
        weights::LogWeightResponse,
        // Export
        export::ExportDocument,
        export::ExportFormat,
        // System
        HealthResponse,
    )),
    modifiers(&SecurityAddon),
    tags(
        (name = "auth", description = "OIDC login and session management"),
        (name = "profile", description = "Body data and computed targets"),
        (name = "meals", description = "Ingest, correction and history"),
        (name = "groups", description = "Multi-dish sittings"),
        (name = "days", description = "Local-day totals and remaining budget"),
        (name = "dishes", description = "Zero-keyboard input paths"),
        (name = "barcode", description = "Packaged-goods lookup"),
        (name = "weights", description = "Weight logging"),
        (name = "export", description = "Data export"),
        (name = "system", description = "Health and version"),
    ),
)]
pub struct ApiDoc;

/// Declares the bearer/cookie session scheme referenced by every guarded route.
struct SecurityAddon;

impl Modify for SecurityAddon {
    fn modify(&self, openapi: &mut utoipa::openapi::OpenApi) {
        let components = openapi.components.get_or_insert_with(Default::default);
        components.add_security_scheme(
            "session",
            SecurityScheme::Http(
                HttpBuilder::new()
                    .scheme(HttpAuthScheme::Bearer)
                    .bearer_format("JWT")
                    .description(Some(
                        "Session JWT. Web clients receive it as the HttpOnly \
`picweight_session` cookie after OIDC login; native clients obtain it from \
`POST /api/auth/token` and send it as `Authorization: Bearer`.",
                    ))
                    .build(),
            ),
        );
    }
}

/// `GET /healthz` response.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct HealthResponse {
    /// Always `"ok"` when the process is serving.
    pub status: &'static str,
    /// Backend version string.
    pub version: &'static str,
}

/// `GET /healthz` — liveness and readiness probe target.
///
/// Deliberately does *not* touch the database: a probe that fails because
/// SQLite is briefly locked would restart a pod that is merely busy.
#[utoipa::path(
    get,
    path = "/healthz",
    tag = "system",
    summary = "Health check",
    responses((status = 200, description = "The service is up", body = HealthResponse))
)]
pub async fn healthz() -> Json<HealthResponse> {
    Json(HealthResponse {
        status: "ok",
        version: crate::VERSION,
    })
}

/// Build the authenticated `/api/v1` router.
///
/// The auth middleware is applied here rather than in `main`, so a route added
/// to this function is guarded by construction.
pub fn create_router(state: AppState) -> Router {
    Router::new()
        // Profile & targets
        .route("/api/v1/me", get(profile::get_me))
        .route("/api/v1/me/profile", put(profile::update_profile))
        // Zero-keyboard inputs
        .route("/api/v1/dishes/recent", get(dishes::recent_dishes))
        // Meals. The static `/meals/events` segment is declared before the
        // dynamic `/meals/{id}` so the SSE route is unambiguous.
        .route(
            "/api/v1/meals",
            post(meals::create_meal)
                .get(meals::list_meals)
                .layer(DefaultBodyLimit::max(meals::MAX_BODY_BYTES)),
        )
        .route("/api/v1/meals/events", get(events::meal_events))
        .route(
            "/api/v1/meals/{id}",
            get(meals::get_meal)
                .patch(meals::patch_meal)
                .delete(meals::delete_meal),
        )
        .route(
            "/api/v1/meals/{id}/thumbnail",
            get(meals::get_meal_thumbnail),
        )
        .route(
            "/api/v1/meals/{id}/reanalyze",
            post(meals::reanalyze_meal),
        )
        .route("/api/v1/meals/{id}/revisions", get(meals::meal_revisions))
        // Sittings
        .route("/api/v1/groups/{group_id}", get(groups::get_group))
        // Days
        .route("/api/v1/days/{date}", get(days::get_day))
        // Barcode
        .route("/api/v1/barcode/{ean}", post(barcode::resolve_barcode))
        // Weight
        .route(
            "/api/v1/weights",
            post(weights::log_weight).get(weights::list_weights),
        )
        // Export
        .route("/api/v1/export", get(export::export_data))
        .layer(axum::middleware::from_fn_with_state(
            state.clone(),
            crate::auth::require_auth,
        ))
        .with_state(state)
}

/// Public routes that must work without a session.
pub fn create_public_router() -> Router<AppState> {
    Router::new().route("/healthz", get(healthz))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_openapi_document_builds_and_declares_the_prd_routes() {
        let doc = ApiDoc::openapi();
        let json = doc.to_json().expect("spec serializes");
        for path in [
            "/api/v1/me",
            "/api/v1/me/profile",
            "/api/v1/dishes/recent",
            "/api/v1/meals",
            "/api/v1/meals/{id}",
            "/api/v1/meals/{id}/reanalyze",
            "/api/v1/meals/{id}/revisions",
            "/api/v1/meals/events",
            "/api/v1/groups/{group_id}",
            "/api/v1/days/{date}",
            "/api/v1/barcode/{ean}",
            "/api/v1/weights",
            "/api/v1/export",
            "/healthz",
        ] {
            assert!(json.contains(path), "spec is missing {path}");
        }
    }
}
