//! `GET /api/v1/days/:date` — the day view.
//!
//! "What did I eat today" is a question about *your* day, not UTC's, so the
//! bucketing uses the per-meal `timezone_offset` (PRD §8). Meals in a sitting
//! come back collapsed into one row, expandable to per-dish rows (§5).

use crate::api::groups::{summarize_groups, GroupSummary};
use crate::api::meals::{build_meal_responses, load_meals_in_local_range, MealResponse};
use crate::auth::CurrentUser;
use crate::error::AppError;
use crate::feedback::phrasing;
use crate::feedback::DayState;
use crate::schema::meals;
use crate::AppState;
use axum::extract::{Path, Query, State};
use axum::Json;
use chrono::NaiveDate;
use diesel::prelude::*;
use serde::{Deserialize, Serialize};
use utoipa::{IntoParams, ToSchema};

/// Widest UTC offset a client can ask for, in minutes.
const MAX_TZ_OFFSET_MINUTES: i32 = 14 * 60;

/// Totals, targets, remaining and the verdict line for one local day.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct DayResponse {
    /// The rules-engine state: consumed, targets, remaining, classification.
    pub state: DayState,
    /// One line of plain-language advice, templated or LLM-phrased.
    pub verdict: String,
    /// Sittings, collapsed.
    pub groups: Vec<GroupSummary>,
    /// Meals not belonging to any sitting, newest first.
    pub meals: Vec<MealResponse>,
}

/// Query for `GET /api/v1/days/{date}`.
#[derive(Debug, Clone, Default, Deserialize, IntoParams)]
pub struct DayQuery {
    /// Minutes east of UTC to bucket by. Defaults to the offset of the user's
    /// most recent meal, then to the profile timezone.
    pub tz_offset: Option<i32>,
}

/// `GET /api/v1/days/{date}` — one local day.
#[utoipa::path(
    get,
    path = "/api/v1/days/{date}",
    tag = "days",
    summary = "Get a day's totals and remaining budget",
    params(
        ("date" = String, Path, description = "Local calendar date, YYYY-MM-DD"),
        DayQuery,
    ),
    responses(
        (status = 200, description = "The day", body = DayResponse),
        (status = 400, description = "Unparseable date", body = crate::error::ErrorBody),
    ),
    security(("session" = []))
)]
pub async fn get_day(
    State(state): State<AppState>,
    user: CurrentUser,
    Path(date): Path<NaiveDate>,
    Query(query): Query<DayQuery>,
) -> Result<Json<DayResponse>, AppError> {
    let user_id = user.id;

    let (day, meals) = state
        .interact(move |conn| {
            let offset = resolve_tz_offset(conn, &user_id, query.tz_offset)?;
            let day = crate::feedback::state::compute_day_state(conn, &user_id, date, offset)?;
            // Each meal is bucketed by the offset *it* was captured in, which
            // is what makes a day correct for a user who travelled (§8).
            let rows = load_meals_in_local_range(conn, &user_id, Some(date), Some(date))?;
            let meals = build_meal_responses(conn, rows)?;
            Ok((day, meals))
        })
        .await?;

    let (groups, ungrouped) = summarize_groups(&meals);

    // Phrasing happens outside the database closure: it may call the model, and
    // it can never fail — an unavailable model degrades to the template (§6).
    let verdict = phrasing::phrase_verdict(&state, &day, &phrasing::standing_line(&day)).await;

    Ok(Json(DayResponse {
        state: day,
        verdict,
        groups,
        meals: ungrouped,
    }))
}

/// Resolve which UTC offset to bucket a user's day by.
///
/// Order: explicit query parameter, then the offset of the user's most recent
/// meal, then the profile timezone, then UTC.
pub fn resolve_tz_offset(
    conn: &mut diesel::SqliteConnection,
    user_id: &str,
    requested: Option<i32>,
) -> Result<i32, AppError> {
    if let Some(offset) = requested {
        if offset.abs() > MAX_TZ_OFFSET_MINUTES {
            return Err(AppError::BadRequest(format!(
                "tz_offset {offset} is outside ±{MAX_TZ_OFFSET_MINUTES} minutes"
            )));
        }
        return Ok(offset);
    }

    // The last meal's offset is the best available guess at where the user is
    // right now, and it is what the client stamped itself.
    if let Some(offset) = meals::table
        .filter(meals::user_id.eq(user_id))
        .order(meals::eaten_at.desc())
        .select(meals::timezone_offset)
        .first::<i32>(conn)
        .optional()?
    {
        return Ok(offset);
    }

    if let Some(profile) = crate::api::profile::load_profile(conn, user_id)? {
        if let Some(offset) = offset_from_timezone(&profile.timezone) {
            return Ok(offset);
        }
    }

    Ok(0)
}

/// Best-effort minutes-east-of-UTC for a stored `timezone` string.
///
/// The crate has no IANA timezone database (`chrono-tz` is not a dependency),
/// and it deliberately does not need one: the per-meal `timezone_offset` the
/// client stamps at capture is the real bucketing mechanism (§8), and this is
/// only the fallback for a user who has logged nothing yet. Fixed-offset forms
/// (`UTC`, `+03:00`, `UTC+3`) resolve; named zones do not and fall back to UTC.
pub fn offset_from_timezone(timezone: &str) -> Option<i32> {
    let raw = timezone.trim();
    if raw.is_empty() {
        return None;
    }
    let upper = raw.to_ascii_uppercase();
    if matches!(upper.as_str(), "UTC" | "GMT" | "Z" | "ETC/UTC" | "ETC/GMT") {
        return Some(0);
    }

    // Strip a leading `UTC`/`GMT` so "UTC+03:00" and "+03:00" take one path.
    let rest = upper
        .strip_prefix("UTC")
        .or_else(|| upper.strip_prefix("GMT"))
        .unwrap_or(&upper);
    let (sign, digits) = match rest.strip_prefix('+') {
        Some(digits) => (1, digits),
        None => match rest.strip_prefix('-') {
            Some(digits) => (-1, digits),
            None => return None,
        },
    };

    let (hours, minutes) = match digits.split_once(':') {
        Some((h, m)) => (h.parse::<i32>().ok()?, m.parse::<i32>().ok()?),
        None if digits.len() == 4 => (
            digits[0..2].parse::<i32>().ok()?,
            digits[2..4].parse::<i32>().ok()?,
        ),
        None => (digits.parse::<i32>().ok()?, 0),
    };
    if !(0..=14).contains(&hours) || !(0..60).contains(&minutes) {
        return None;
    }
    Some(sign * (hours * 60 + minutes))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fixed_offset_zone_names_resolve() {
        assert_eq!(offset_from_timezone("UTC"), Some(0));
        assert_eq!(offset_from_timezone("utc"), Some(0));
        assert_eq!(offset_from_timezone("+03:00"), Some(180));
        assert_eq!(offset_from_timezone("UTC+3"), Some(180));
        assert_eq!(offset_from_timezone("-0530"), Some(-330));
    }

    #[test]
    fn named_zones_fall_back_rather_than_guessing() {
        // No IANA database is linked in, and a wrong guess would silently
        // misbucket days — so this returns None and the caller uses UTC.
        assert_eq!(offset_from_timezone("Europe/Moscow"), None);
        assert_eq!(offset_from_timezone(""), None);
        assert_eq!(offset_from_timezone("+99:00"), None);
    }
}
