//! Weight logging — `POST /api/v1/weights`, `GET /api/v1/weights`.
//!
//! Logging a weight recomputes the user's targets (G6: "targets derived from
//! body data, recomputed on weight change"). The history feeds the weight trend
//! chart in the dashboard.

use crate::auth::CurrentUser;
use crate::error::AppError;
use crate::models::{NewWeightLog, WeightSource};
use crate::schema::weight_logs;
use crate::AppState;
use axum::extract::{Query, State};
use axum::http::StatusCode;
use axum::Json;
use chrono::{NaiveDate, NaiveDateTime, Utc};
use diesel::prelude::*;
use serde::{Deserialize, Serialize};
use utoipa::{IntoParams, ToSchema};

/// Default number of weight readings returned.
pub const DEFAULT_WEIGHT_LIMIT: i64 = 365;

/// Hard cap on one page of weight history.
pub const MAX_WEIGHT_LIMIT: i64 = 5000;

/// Body of `POST /api/v1/weights`.
#[derive(Debug, Clone, Deserialize, ToSchema)]
pub struct LogWeightRequest {
    /// Weight in kilograms.
    pub weight_kg: f64,
    /// When it was measured. Defaults to now.
    pub logged_at: Option<NaiveDateTime>,
    /// How it was captured. Defaults to `manual`.
    pub source: Option<WeightSource>,
}

/// One weight reading.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct WeightLogResponse {
    /// `weight_logs.id`.
    pub id: String,
    /// When it was measured.
    pub logged_at: NaiveDateTime,
    /// Weight in kilograms.
    pub weight_kg: f64,
    /// How it was captured.
    pub source: WeightSource,
}

/// Result of logging a weight.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct LogWeightResponse {
    /// The stored reading.
    pub weight: WeightLogResponse,
    /// True when this reading caused targets to be recomputed.
    pub targets_recomputed: bool,
}

/// Query for `GET /api/v1/weights`.
#[derive(Debug, Clone, Default, Deserialize, IntoParams)]
pub struct WeightHistoryQuery {
    /// Inclusive start date.
    pub from: Option<NaiveDate>,
    /// Inclusive end date.
    pub to: Option<NaiveDate>,
    /// Maximum readings to return. Defaults to [`DEFAULT_WEIGHT_LIMIT`].
    pub limit: Option<i64>,
}

/// `POST /api/v1/weights` — log a weight and recompute targets.
#[utoipa::path(
    post,
    path = "/api/v1/weights",
    tag = "weights",
    summary = "Log a weight",
    description = "Recomputes the user's targets from the new weight, since \
targets are derived from body data and must track it.",
    request_body = LogWeightRequest,
    responses(
        (status = 201, description = "The stored reading", body = LogWeightResponse),
        (status = 400, description = "Implausible weight", body = crate::error::ErrorBody),
    ),
    security(("session" = []))
)]
pub async fn log_weight(
    State(state): State<AppState>,
    user: CurrentUser,
    Json(body): Json<LogWeightRequest>,
) -> Result<(StatusCode, Json<LogWeightResponse>), AppError> {
    validate_weight(body.weight_kg)?;

    let user_id = user.id;
    let now = Utc::now().naive_utc();
    let logged_at = body.logged_at.unwrap_or(now);
    let source = body.source.unwrap_or(WeightSource::Manual);

    let response = state
        .interact(move |conn| {
            conn.transaction::<_, AppError, _>(|conn| {
                let row = NewWeightLog {
                    id: uuid::Uuid::new_v4().to_string(),
                    user_id: user_id.clone(),
                    logged_at,
                    weight_kg: body.weight_kg,
                    source: source.as_str().to_string(),
                    created_at: now,
                };
                diesel::insert_into(weight_logs::table)
                    .values(&row)
                    .execute(conn)?;

                // Targets track body data (G6). A user who has not onboarded
                // yet has no profile to recompute, which is not an error.
                let has_profile =
                    crate::api::profile::load_profile(conn, &user_id)?.is_some();
                if has_profile {
                    crate::api::profile::recompute_targets(conn, &user_id)?;
                }

                Ok(LogWeightResponse {
                    weight: WeightLogResponse {
                        id: row.id,
                        logged_at: row.logged_at,
                        weight_kg: row.weight_kg,
                        source,
                    },
                    targets_recomputed: has_profile,
                })
            })
        })
        .await?;

    Ok((StatusCode::CREATED, Json(response)))
}

/// `GET /api/v1/weights` — weight history for the trend chart.
#[utoipa::path(
    get,
    path = "/api/v1/weights",
    tag = "weights",
    summary = "Weight history",
    params(WeightHistoryQuery),
    responses(
        (status = 200, description = "Readings, newest first", body = Vec<WeightLogResponse>),
    ),
    security(("session" = []))
)]
pub async fn list_weights(
    State(state): State<AppState>,
    user: CurrentUser,
    Query(query): Query<WeightHistoryQuery>,
) -> Result<Json<Vec<WeightLogResponse>>, AppError> {
    if let (Some(from), Some(to)) = (query.from, query.to) {
        if from > to {
            return Err(AppError::BadRequest(
                "`from` must not be after `to`".to_string(),
            ));
        }
    }
    let limit = query
        .limit
        .unwrap_or(DEFAULT_WEIGHT_LIMIT)
        .clamp(1, MAX_WEIGHT_LIMIT);
    let user_id = user.id;

    let rows = state
        .interact(move |conn| load_weights(conn, &user_id, query.from, query.to, Some(limit)))
        .await?;

    Ok(Json(rows))
}

/// Load a user's weight readings, newest first.
pub fn load_weights(
    conn: &mut diesel::SqliteConnection,
    user_id: &str,
    from: Option<NaiveDate>,
    to: Option<NaiveDate>,
    limit: Option<i64>,
) -> Result<Vec<WeightLogResponse>, AppError> {
    let mut query = weight_logs::table
        .filter(weight_logs::user_id.eq(user_id))
        .into_boxed();
    if let Some(from) = from {
        query = query.filter(weight_logs::logged_at.ge(start_of_day(from)));
    }
    if let Some(to) = to {
        query = query.filter(
            weight_logs::logged_at.lt(start_of_day(to + chrono::Duration::days(1))),
        );
    }
    if let Some(limit) = limit {
        query = query.limit(limit);
    }

    let rows = query
        .order(weight_logs::logged_at.desc())
        .select((
            weight_logs::id,
            weight_logs::logged_at,
            weight_logs::weight_kg,
            weight_logs::source,
        ))
        .load::<(String, NaiveDateTime, f64, String)>(conn)?;

    rows.into_iter()
        .map(|(id, logged_at, weight_kg, source)| {
            Ok(WeightLogResponse {
                id,
                logged_at,
                weight_kg,
                source: WeightSource::from_str(&source)?,
            })
        })
        .collect()
}

/// Midnight at the start of a date.
fn start_of_day(date: NaiveDate) -> NaiveDateTime {
    date.and_hms_opt(0, 0, 0)
        .unwrap_or_else(|| date.and_time(chrono::NaiveTime::MIN))
}

/// Reject implausible readings before they poison the target computation.
pub fn validate_weight(weight_kg: f64) -> Result<(), AppError> {
    if !(20.0..=400.0).contains(&weight_kg) {
        return Err(AppError::BadRequest(
            "weight_kg must be between 20 and 400".to_string(),
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn implausible_weights_are_rejected() {
        assert!(validate_weight(80.0).is_ok());
        assert!(validate_weight(0.0).is_err());
        assert!(validate_weight(1000.0).is_err());
    }

    #[test]
    fn a_nan_reading_never_reaches_the_formula() {
        assert!(validate_weight(f64::NAN).is_err());
    }
}
