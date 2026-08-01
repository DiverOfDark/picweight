//! The HTTP surface.
//!
//! Everything versioned lives under [`crate::API_PREFIX`] (`/api/v1`) and is
//! behind [`crate::auth::require_auth`]. `/api/auth/*` (see [`crate::auth`]),
//! `/healthz` and [`client::client_version`] are the only unauthenticated
//! routes; everything else falls through to the static SPA (PRD §7).
//!
//! Every route is `utoipa`-annotated so `android/openapi.json` regenerates
//! cleanly — the Android Retrofit client is *generated* from that spec, so an
//! un-annotated route is a route the phone cannot call.

pub mod barcode;
pub mod client;
pub mod days;
pub mod dishes;
pub mod events;
pub mod export;
pub mod groups;
pub mod meals;
pub mod profile;
pub mod usage;
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
        usage::get_usage,
        profile::update_profile,
        // Meals
        meals::create_meal,
        meals::list_meals,
        meals::get_meal,
        meals::patch_meal,
        meals::delete_meal,
        meals::reanalyze_meal,
        meals::retry_meal,
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
        // Android client distribution
        client::client_version,
        // System
        healthz,
    ),
    components(schemas(
        usage::UsageResponse,
        usage::ModelUsage,
        usage::DailyUsage,
        crate::jobs::analyzer::PricingSource,
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
        // Android client distribution
        client::ClientVersionResponse,
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
        (name = "client", description = "Android client distribution and in-app update"),
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
        .route("/api/v1/usage", get(usage::get_usage))
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
        // The one way back from `failed`. The photo is not lost — only the
        // analysis failed — so this needs no body at all.
        .route("/api/v1/meals/{id}/retry", post(meals::retry_meal))
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
///
/// `/api/v1/client/version` is here rather than in [`create_router`] on
/// purpose: it describes `/picweight.apk`, which the static fallback already
/// serves to anyone, and an app whose session has expired still has to be able
/// to discover that a newer build exists. See [`client::client_version`] for
/// the full argument that this discloses nothing the public download does not.
pub fn create_public_router() -> Router<AppState> {
    Router::new()
        .route("/healthz", get(healthz))
        .route("/api/v1/client/version", get(client::client_version))
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
            "/api/v1/meals/{id}/retry",
            "/api/v1/meals/{id}/revisions",
            "/api/v1/meals/events",
            "/api/v1/groups/{group_id}",
            "/api/v1/days/{date}",
            "/api/v1/barcode/{ean}",
            "/api/v1/weights",
            "/api/v1/export",
            "/api/v1/client/version",
            "/healthz",
        ] {
            assert!(json.contains(path), "spec is missing {path}");
        }
    }
}

/// The wire-format contract for timestamps.
///
/// The OpenAPI document declares every instant as `{"type":"string","format":
/// "date-time"}`, which is RFC 3339 — where the offset is **mandatory**. The
/// generated Android client therefore models those fields as
/// `java.time.OffsetDateTime`, and Jackson's deserialiser throws on an
/// offset-less string. A DTO that serialises `chrono::NaiveDateTime` emits
/// `"2026-08-01T13:32:33.441427539"`, which kills the whole response — the
/// phone showed "Offline" after a successful login because `GET /api/v1/me`
/// could not be parsed.
///
/// The bug was uniform across every DTO, so a test that pinned one field would
/// not have caught it. These tests serialise a fully populated instance of
/// every response DTO that carries a timestamp and walk the resulting JSON.
#[cfg(test)]
mod timestamp_contract {
    use super::*;
    use crate::agent::schema::MacroTotals;
    use crate::feedback::state::DayState;
    use crate::models::{
        GoalType, GramsSource, MacroSource, MealStatus, NameSource, Sex, WeightSource,
    };
    use chrono::{DateTime, NaiveDate, Utc};
    use serde::Serialize;
    use serde_json::Value;

    /// The exact instant the deployed pod emitted, sub-second digits and all —
    /// so the fixtures exercise the precision that actually ships.
    fn instant() -> DateTime<Utc> {
        NaiveDate::from_ymd_opt(2026, 8, 1)
            .expect("a valid calendar date")
            .and_hms_nano_opt(13, 32, 33, 441_427_539)
            .expect("a valid wall-clock time")
            .and_utc()
    }

    fn a_date() -> NaiveDate {
        NaiveDate::from_ymd_opt(1990, 5, 1).expect("a valid calendar date")
    }

    fn user() -> profile::UserResponse {
        profile::UserResponse {
            id: "u1".to_string(),
            email: Some("eater@example.com".to_string()),
            display_name: Some("Eater".to_string()),
            created_at: instant(),
        }
    }

    fn user_profile() -> profile::ProfileResponse {
        profile::ProfileResponse {
            sex: Sex::Male,
            birth_date: a_date(),
            height_cm: 180.0,
            activity_factor: 1.55,
            goal_type: GoalType::Lose,
            target_weight_kg: Some(75.0),
            rate_kg_per_week: Some(0.5),
            target_kcal: Some(2050.0),
            target_protein_g: Some(165.0),
            target_fat_g: Some(65.0),
            target_carbs_g: Some(200.0),
            calibration_factor: 1.0,
            targets_computed_at: Some(instant()),
            timezone: "Europe/Moscow".to_string(),
            current_weight_kg: Some(80.0),
        }
    }

    fn day() -> DayState {
        DayState::empty(NaiveDate::from_ymd_opt(2026, 8, 1).expect("a valid calendar date"))
    }

    fn meal_item() -> meals::MealItemResponse {
        meals::MealItemResponse {
            id: "i1".to_string(),
            position: 0,
            name: "rice".to_string(),
            barcode: Some("4600000000000".to_string()),
            grams: 200.0,
            grams_source: GramsSource::Agent,
            kcal: 260.0,
            protein_g: 5.0,
            fat_g: 1.0,
            carbs_g: 57.0,
            macro_source: MacroSource::Model,
            confidence: Some(0.6),
            reasoning_note: Some("standard delivery bowl".to_string()),
        }
    }

    fn meal() -> meals::MealResponse {
        meals::MealResponse {
            id: "m1".to_string(),
            client_uuid: "c1".to_string(),
            group_id: Some("g1".to_string()),
            group_size: Some(2),
            dish_name: Some("Шаурма с курицей".to_string()),
            name_source: NameSource::RecentChip,
            user_comment: Some("half the rice".to_string()),
            revision: 2,
            status: MealStatus::Confirmed,
            eaten_at: instant(),
            timezone_offset: 180,
            meal_type: Some("lunch".to_string()),
            portion_scale: 1.0,
            thumbnail_url: Some("/api/v1/meals/m1/thumbnail".to_string()),
            items: vec![meal_item()],
            totals: MacroTotals::default(),
            error: Some("quota exhausted".to_string()),
            created_at: instant(),
            updated_at: instant(),
        }
    }

    fn revision_entry() -> meals::RevisionEntry {
        meals::RevisionEntry {
            revision: 2,
            user_feedback: Some("too much rice".to_string()),
            model: "gpt-test".to_string(),
            reseeded: false,
            items: vec![meal_item()],
            totals: MacroTotals::default(),
            tool_calls: 0,
            created_at: instant(),
        }
    }

    fn weight() -> weights::WeightLogResponse {
        weights::WeightLogResponse {
            id: "w1".to_string(),
            logged_at: instant(),
            weight_kg: 80.0,
            source: WeightSource::Manual,
        }
    }

    fn group_summary() -> groups::GroupSummary {
        groups::GroupSummary {
            group_id: "g1".to_string(),
            member_count: 2,
            totals: MacroTotals::default(),
            dish_names: vec!["Шаурма".to_string()],
            eaten_at: instant(),
            has_failures: false,
        }
    }

    fn meal_event() -> events::MealEvent {
        events::MealEvent {
            user_id: "u1".to_string(),
            kind: events::MealEventKind::Completed,
            meal_id: Some("m1".to_string()),
            group_id: Some("g1".to_string()),
            revision: 2,
            status: Some(MealStatus::NeedsReview),
            dish_name: Some("Шаурма".to_string()),
            totals: Some(MacroTotals::default()),
            day: Some(day()),
            headline: Some("Шаурма — 780 kcal".to_string()),
            body: Some("1,450 / 2,050 today".to_string()),
            error: None,
            emitted_at: instant(),
        }
    }

    /// Serialise one DTO, tagged with the name that will appear in a failure.
    fn dto<T: Serialize>(name: &'static str, value: T) -> (&'static str, Value) {
        (
            name,
            serde_json::to_value(value).unwrap_or_else(|e| panic!("{name} serialises: {e}")),
        )
    }

    /// Every response DTO that carries at least one timestamp, fully populated.
    ///
    /// Adding a DTO with a timestamp means adding it here — the alternative
    /// (reflecting over the schema registry) cannot produce an *instance*, and
    /// only an instance proves what serde actually writes.
    fn every_dto_with_a_timestamp() -> Vec<(&'static str, Value)> {
        vec![
            dto("UserResponse", user()),
            dto("ProfileResponse", user_profile()),
            dto(
                "MeResponse",
                profile::MeResponse {
                    user: user(),
                    profile: Some(user_profile()),
                    today: day(),
                    version: "test".to_string(),
                },
            ),
            dto(
                "UpdateProfileResponse",
                profile::UpdateProfileResponse {
                    profile: user_profile(),
                    warnings: vec!["steep deficit".to_string()],
                },
            ),
            dto("MealResponse", meal()),
            dto("RevisionEntry", revision_entry()),
            dto(
                "RevisionsResponse",
                meals::RevisionsResponse {
                    meal_id: "m1".to_string(),
                    revisions: vec![revision_entry()],
                },
            ),
            dto(
                "GroupResponse",
                groups::GroupResponse {
                    group_id: "g1".to_string(),
                    expected_size: Some(2),
                    member_count: 1,
                    settled: true,
                    notified_at: Some(instant()),
                    last_photo_at: instant(),
                    totals: MacroTotals::default(),
                    meals: vec![meal()],
                },
            ),
            dto("GroupSummary", group_summary()),
            dto(
                "DayResponse",
                days::DayResponse {
                    state: day(),
                    verdict: "600 left".to_string(),
                    groups: vec![group_summary()],
                    meals: vec![meal()],
                },
            ),
            dto(
                "RecentDish",
                dishes::RecentDish {
                    dish_name: "Шаурма".to_string(),
                    dish_name_normalized: "шаурма".to_string(),
                    last_eaten_at: instant(),
                    occurrences: 3,
                    totals: MacroTotals::default(),
                    last_meal_id: "m1".to_string(),
                },
            ),
            dto(
                "FoodResponse",
                barcode::FoodResponse {
                    id: "f1".to_string(),
                    source: "openfoodfacts".to_string(),
                    name: "Cola".to_string(),
                    brand: Some("Brand".to_string()),
                    barcode: Some("4600000000000".to_string()),
                    kcal_100g: Some(42.0),
                    protein_100g: Some(0.0),
                    fat_100g: Some(0.0),
                    carbs_100g: Some(10.6),
                    fetched_at: instant(),
                },
            ),
            dto("WeightLogResponse", weight()),
            dto(
                "LogWeightResponse",
                weights::LogWeightResponse {
                    weight: weight(),
                    targets_recomputed: true,
                },
            ),
            dto("MealEvent", meal_event()),
            dto(
                "ExportDocument",
                export::ExportDocument {
                    export_version: export::EXPORT_VERSION,
                    app_version: "test".to_string(),
                    exported_at: instant(),
                    profile: Some(user_profile()),
                    weights: vec![weight()],
                    meals: vec![meal()],
                },
            ),
        ]
    }

    /// True for a string shaped like a calendar date followed by a time.
    ///
    /// A bare `YYYY-MM-DD` (`birth_date`, `DayState::date`) is `format: date`,
    /// where an offset would be wrong, so it is deliberately not matched.
    fn looks_like_a_timestamp(value: &str) -> bool {
        let bytes = value.as_bytes();
        // "YYYY-MM-DDThh:mm" is the shortest thing worth calling an instant.
        if bytes.len() < 16 {
            return false;
        }
        let date_shaped = bytes[..10]
            .iter()
            .zip(b"0000-00-00")
            .all(|(byte, pattern)| match pattern {
                b'0' => byte.is_ascii_digit(),
                _ => byte == pattern,
            });
        date_shaped && matches!(bytes[10], b'T' | b't' | b' ')
    }

    /// Walk a serialised DTO and assert every timestamp-shaped string is a
    /// valid RFC 3339 instant. Returns how many were checked.
    fn assert_offsets(path: &str, value: &Value) -> usize {
        match value {
            Value::String(text) if looks_like_a_timestamp(text) => {
                assert!(
                    DateTime::parse_from_rfc3339(text).is_ok(),
                    "{path} serialised as {text:?}, which is not RFC 3339. The \
OpenAPI document declares this field `format: date-time`, so the generated \
Android client deserialises it into java.time.OffsetDateTime and Jackson \
throws on a missing offset — the entire response is lost, not just this \
field. Emit chrono::DateTime<Utc> (`.and_utc()` at construction), never \
NaiveDateTime."
                );
                1
            }
            Value::Array(items) => items
                .iter()
                .enumerate()
                .map(|(index, item)| assert_offsets(&format!("{path}[{index}]"), item))
                .sum(),
            Value::Object(fields) => fields
                .iter()
                .map(|(name, field)| assert_offsets(&format!("{path}.{name}"), field))
                .sum(),
            _ => 0,
        }
    }

    #[test]
    fn every_api_timestamp_is_rfc3339_with_an_offset() {
        let mut total = 0;
        for (name, json) in every_dto_with_a_timestamp() {
            let found = assert_offsets(name, &json);
            // A fixture that forgot to populate its timestamps would make this
            // guard pass while proving nothing.
            assert!(
                found > 0,
                "{name} is listed as carrying a timestamp but its fixture \
serialised none — populate it, or drop it from the list"
            );
            total += found;
        }
        assert!(
            total >= 30,
            "only {total} timestamps were checked; the walk is no longer \
reaching the nested DTOs"
        );
    }

    #[test]
    fn the_guard_recognises_the_payload_the_pod_actually_emitted() {
        // Proves the walk above is not vacuous: this is the literal string
        // `GET /api/v1/me` returned, and it must be both *detected* as a
        // timestamp and *rejected* as RFC 3339.
        let broken = "2026-08-01T13:32:33.441427539";
        assert!(looks_like_a_timestamp(broken));
        assert!(DateTime::parse_from_rfc3339(broken).is_err());

        // A bare date is `format: date` and must not be dragged into the check.
        assert!(!looks_like_a_timestamp("2026-08-01"));
    }

    #[test]
    #[should_panic(expected = "not RFC 3339")]
    fn the_walk_rejects_a_naive_instant_nested_inside_a_document() {
        // The payload the pod returned, at the depth it actually sat: this
        // proves the walk descends into nested objects and arrays rather than
        // inspecting top-level fields, which is why one regressed DTO cannot
        // slip through.
        let broken = serde_json::json!({
            "user": { "created_at": "2026-08-01T13:32:33.441427539" },
            "meals": [{ "eaten_at": "2026-08-01T13:32:33Z" }],
        });
        assert_offsets("MeResponse", &broken);
    }

    #[test]
    fn a_user_responses_created_at_ends_with_z() {
        let json = serde_json::to_value(user()).expect("UserResponse serialises");
        let created_at = json
            .get("created_at")
            .and_then(Value::as_str)
            .expect("created_at is a JSON string");
        assert!(
            created_at.ends_with('Z'),
            "GET /api/v1/me emitted created_at={created_at:?} with no UTC \
designator; this is the exact field whose offset-less form made a logged-in \
Android client report \"Offline — showing what this phone knows.\""
        );
        assert_eq!(created_at, "2026-08-01T13:32:33.441427539Z");
    }

    #[test]
    fn the_spec_still_declares_every_converted_field_as_date_time() {
        let spec = serde_json::to_value(ApiDoc::openapi()).expect("spec serialises");
        let schemas = spec
            .pointer("/components/schemas")
            .expect("the document has schemas");

        for (schema, property) in [
            ("UserResponse", "created_at"),
            ("ProfileResponse", "targets_computed_at"),
            ("MealResponse", "eaten_at"),
            ("MealResponse", "created_at"),
            ("MealResponse", "updated_at"),
            ("RevisionEntry", "created_at"),
            ("GroupResponse", "notified_at"),
            ("GroupResponse", "last_photo_at"),
            ("GroupSummary", "eaten_at"),
            ("RecentDish", "last_eaten_at"),
            ("FoodResponse", "fetched_at"),
            ("WeightLogResponse", "logged_at"),
            ("LogWeightRequest", "logged_at"),
            ("MealEvent", "emitted_at"),
            ("ExportDocument", "exported_at"),
        ] {
            let declared = schemas
                .pointer(&format!("/{schema}/properties/{property}"))
                .unwrap_or_else(|| panic!("{schema}.{property} is missing from the spec"));
            // Nullable fields render as a one-of/`type: [string, null]`, so the
            // format is what identifies the contract in every case.
            assert_eq!(
                declared.get("format").and_then(Value::as_str),
                Some("date-time"),
                "{schema}.{property} is no longer declared `format: date-time`, \
so generated clients will stop modelling it as an instant: {declared}"
            );
        }
    }

    #[test]
    fn a_logged_weight_accepts_an_rfc3339_instant_from_a_generated_client() {
        // Retrofit/Jackson serialises OffsetDateTime with an offset and cannot
        // be made to emit a naive one, so the inbound direction has to accept
        // exactly what `format: date-time` promises.
        let body = serde_json::json!({
            "weight_kg": 80.0,
            "logged_at": "2026-08-01T13:32:33.441427539Z",
        });
        let parsed: weights::LogWeightRequest =
            serde_json::from_value(body).expect("an RFC 3339 logged_at parses");
        assert_eq!(parsed.logged_at, Some(instant()));
    }
}
