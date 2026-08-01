//! `GET /api/v1/me` and `PUT /api/v1/me/profile`.
//!
//! Targets are recomputed here, deterministically, from
//! [`crate::nutrition::targets`] — never asked of a model (PRD §6). A deficit
//! steeper than ~1% bodyweight/week is returned as a **warning** alongside the
//! saved profile; it is not silently accepted and not rejected either.

use crate::auth::CurrentUser;
use crate::error::AppError;
use crate::feedback::DayState;
use crate::models::{
    GoalType, NewUserProfile, NewWeightLog, Sex, User, UserProfile, UserProfileChangeset,
    WeightSource,
};
use crate::nutrition::targets::{age_years, compute_targets, deficit_warning, TargetInputs};
use crate::schema::{user_profiles, users, weight_logs};
use crate::AppState;
use axum::extract::State;
use axum::Json;
use chrono::{NaiveDate, NaiveDateTime, Utc};
use diesel::prelude::*;
use diesel::SqliteConnection;
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

/// The authenticated user's identity.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct UserResponse {
    /// `users.id`.
    pub id: String,
    /// Email from the ID token, when the IdP supplied one.
    pub email: Option<String>,
    /// Display name from the ID token.
    pub display_name: Option<String>,
    /// When the account was first seen.
    pub created_at: NaiveDateTime,
}

/// Body data and the targets derived from it.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct ProfileResponse {
    /// Biological sex — changes the BMR constant by 166 kcal.
    pub sex: Sex,
    /// Date of birth.
    pub birth_date: NaiveDate,
    /// Height in centimetres.
    pub height_cm: f64,
    /// Activity multiplier, 1.2 sedentary … 1.9 very active.
    pub activity_factor: f64,
    /// Direction of the goal.
    pub goal_type: GoalType,
    /// Target weight in kilograms.
    pub target_weight_kg: Option<f64>,
    /// Intended rate of change, kg/week.
    pub rate_kg_per_week: Option<f64>,
    /// Daily energy target.
    pub target_kcal: Option<f64>,
    /// Daily protein floor.
    pub target_protein_g: Option<f64>,
    /// Daily fat floor.
    pub target_fat_g: Option<f64>,
    /// Daily carbohydrate allowance.
    pub target_carbs_g: Option<f64>,
    /// Per-user multiplier learned from correction history (§5 hidden fat).
    pub calibration_factor: f64,
    /// When the targets were last recomputed.
    pub targets_computed_at: Option<NaiveDateTime>,
    /// IANA timezone name, the default for meals that arrive without an offset.
    pub timezone: String,
    /// Most recent logged weight, for convenience.
    pub current_weight_kg: Option<f64>,
}

/// `GET /api/v1/me` response.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct MeResponse {
    /// Identity.
    pub user: UserResponse,
    /// Body data and targets. `None` until onboarding completes.
    pub profile: Option<ProfileResponse>,
    /// Today's state, so the home screen renders from one request.
    pub today: DayState,
    /// Backend version string, for the about screen and drift diagnosis.
    pub version: String,
}

/// Body of `PUT /api/v1/me/profile`.
///
/// This is the onboarding payload and the edit payload both; every field is
/// required because the formulas need all of them.
#[derive(Debug, Clone, Deserialize, ToSchema)]
pub struct UpdateProfileRequest {
    /// Biological sex.
    pub sex: Sex,
    /// Date of birth.
    pub birth_date: NaiveDate,
    /// Height in centimetres.
    pub height_cm: f64,
    /// Activity multiplier, 1.2–1.9.
    pub activity_factor: f64,
    /// Direction of the goal.
    pub goal_type: GoalType,
    /// Target weight in kilograms.
    pub target_weight_kg: f64,
    /// Intended rate of change, kg/week (always positive).
    pub rate_kg_per_week: f64,
    /// IANA timezone name.
    pub timezone: String,
    /// Current weight. When present, also written to `weight_logs` with source
    /// `onboarding`, since targets recompute on weight change (G6).
    pub current_weight_kg: Option<f64>,
}

/// `PUT /api/v1/me/profile` response.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct UpdateProfileResponse {
    /// The saved profile with freshly computed targets.
    pub profile: ProfileResponse,
    /// Non-fatal advice — e.g. an aggressive deficit. Warned about, not
    /// silently accepted (§6).
    pub warnings: Vec<String>,
}

/// `GET /api/v1/me` — profile plus today's targets.
#[utoipa::path(
    get,
    path = "/api/v1/me",
    tag = "profile",
    summary = "Current user, profile and today's state",
    responses(
        (status = 200, description = "The current user", body = MeResponse),
        (status = 401, description = "Not authenticated", body = crate::error::ErrorBody),
    ),
    security(("session" = []))
)]
pub async fn get_me(
    State(state): State<AppState>,
    user: CurrentUser,
) -> Result<Json<MeResponse>, AppError> {
    let user_id = user.id;

    let (row, profile, today) = state
        .interact(move |conn| {
            let row = users::table
                .filter(users::id.eq(&user_id))
                .select(User::as_select())
                .first::<User>(conn)
                .optional()?
                .ok_or_else(|| {
                    AppError::Unauthorized("this session's user no longer exists".to_string())
                })?;
            let profile = load_profile_response(conn, &user_id)?;

            // "Today" is the user's today, not UTC's.
            let offset = crate::api::days::resolve_tz_offset(conn, &user_id, None)?;
            let date = crate::feedback::state::local_date_of(Utc::now().naive_utc(), offset);
            let today = crate::feedback::state::compute_day_state(conn, &user_id, date, offset)?;
            Ok((row, profile, today))
        })
        .await?;

    Ok(Json(MeResponse {
        user: UserResponse {
            id: row.id,
            email: row.email,
            display_name: row.display_name,
            created_at: row.created_at,
        },
        profile,
        today,
        version: crate::VERSION.to_string(),
    }))
}

/// `PUT /api/v1/me/profile` — save body data and recompute targets.
#[utoipa::path(
    put,
    path = "/api/v1/me/profile",
    tag = "profile",
    summary = "Update body data and recompute targets",
    description = "Targets are computed with Mifflin-St Jeor — deterministic, \
offline and auditable. A deficit steeper than ~1% bodyweight/week is returned \
as a warning, not an error.",
    request_body = UpdateProfileRequest,
    responses(
        (status = 200, description = "The saved profile", body = UpdateProfileResponse),
        (status = 400, description = "Invalid body data", body = crate::error::ErrorBody),
    ),
    security(("session" = []))
)]
pub async fn update_profile(
    State(state): State<AppState>,
    user: CurrentUser,
    Json(body): Json<UpdateProfileRequest>,
) -> Result<Json<UpdateProfileResponse>, AppError> {
    validate_profile(&body)?;

    let user_id = user.id;
    let now = Utc::now().naive_utc();

    let response = state
        .interact(move |conn| {
            conn.transaction::<_, AppError, _>(|conn| {
                let exists = user_profiles::table
                    .filter(user_profiles::user_id.eq(&user_id))
                    .select(user_profiles::user_id)
                    .first::<String>(conn)
                    .optional()?
                    .is_some();

                if exists {
                    diesel::update(
                        user_profiles::table.filter(user_profiles::user_id.eq(&user_id)),
                    )
                    .set(&UserProfileChangeset {
                        sex: Some(body.sex.as_str().to_string()),
                        birth_date: Some(body.birth_date),
                        height_cm: Some(body.height_cm),
                        activity_factor: Some(body.activity_factor),
                        goal_type: Some(body.goal_type.as_str().to_string()),
                        target_weight_kg: Some(Some(body.target_weight_kg)),
                        rate_kg_per_week: Some(Some(body.rate_kg_per_week)),
                        timezone: Some(body.timezone.trim().to_string()),
                        updated_at: Some(now),
                        ..Default::default()
                    })
                    .execute(conn)?;
                } else {
                    diesel::insert_into(user_profiles::table)
                        .values(&NewUserProfile {
                            user_id: user_id.clone(),
                            sex: body.sex.as_str().to_string(),
                            birth_date: body.birth_date,
                            height_cm: body.height_cm,
                            activity_factor: body.activity_factor,
                            goal_type: body.goal_type.as_str().to_string(),
                            target_weight_kg: Some(body.target_weight_kg),
                            rate_kg_per_week: Some(body.rate_kg_per_week),
                            target_kcal: None,
                            target_protein_g: None,
                            target_fat_g: None,
                            target_carbs_g: None,
                            calibration_factor: 1.0,
                            targets_computed_at: None,
                            timezone: body.timezone.trim().to_string(),
                            created_at: now,
                            updated_at: now,
                        })
                        .execute(conn)?;
                }

                // Log the weight before recomputing: targets are derived from
                // body data and must track the newest reading (G6).
                if let Some(weight_kg) = body.current_weight_kg {
                    diesel::insert_into(weight_logs::table)
                        .values(&NewWeightLog {
                            id: uuid::Uuid::new_v4().to_string(),
                            user_id: user_id.clone(),
                            logged_at: now,
                            weight_kg,
                            source: WeightSource::Onboarding.as_str().to_string(),
                            created_at: now,
                        })
                        .execute(conn)?;
                }

                recompute_targets(conn, &user_id)?;

                let profile = load_profile_response(conn, &user_id)?.ok_or_else(|| {
                    AppError::Internal("profile vanished immediately after saving".to_string())
                })?;

                // The warning is about the plan the user just saved, so it is
                // computed from the same weight the targets were.
                let mut warnings = Vec::new();
                if let Some(weight_kg) = latest_weight(conn, &user_id)?
                    .or(body.current_weight_kg)
                    .or(Some(body.target_weight_kg))
                {
                    let inputs = TargetInputs {
                        sex: body.sex,
                        age_years: age_years(body.birth_date, now.date()),
                        height_cm: body.height_cm,
                        weight_kg,
                        activity_factor: body.activity_factor,
                        goal_type: body.goal_type,
                        target_weight_kg: body.target_weight_kg,
                        rate_kg_per_week: body.rate_kg_per_week,
                    };
                    if let Some(warning) = deficit_warning(&inputs) {
                        warnings.push(warning);
                    }
                }
                if profile.current_weight_kg.is_none() {
                    warnings.push(
                        "No weight logged yet — targets are computed from your target weight \
until you log one."
                            .to_string(),
                    );
                }

                Ok(UpdateProfileResponse { profile, warnings })
            })
        })
        .await?;

    Ok(Json(response))
}

/// Recompute and persist a user's targets from their current profile and
/// latest weight.
///
/// Called on profile edit and whenever a new weight is logged (G6). A user with
/// no profile is a no-op rather than an error: weight logging must work before
/// onboarding completes.
pub fn recompute_targets(
    conn: &mut diesel::SqliteConnection,
    user_id: &str,
) -> Result<(), AppError> {
    let Some(profile) = load_profile(conn, user_id)? else {
        return Ok(());
    };

    // Without a logged weight the target weight is the only figure available;
    // it is a defensible stand-in and keeps onboarding from producing nulls.
    let Some(weight_kg) = latest_weight(conn, user_id)?.or(profile.target_weight_kg) else {
        return Ok(());
    };

    let now = Utc::now().naive_utc();
    let inputs = target_inputs(&profile, weight_kg, now.date())?;
    let targets = compute_targets(&inputs);

    diesel::update(user_profiles::table.filter(user_profiles::user_id.eq(user_id)))
        .set(&UserProfileChangeset {
            target_kcal: Some(Some(targets.kcal)),
            target_protein_g: Some(Some(targets.protein_g)),
            target_fat_g: Some(Some(targets.fat_g)),
            target_carbs_g: Some(Some(targets.carbs_g)),
            targets_computed_at: Some(Some(now)),
            updated_at: Some(now),
            ..Default::default()
        })
        .execute(conn)?;
    Ok(())
}

/// Load a user's profile row.
pub fn load_profile(
    conn: &mut SqliteConnection,
    user_id: &str,
) -> Result<Option<UserProfile>, AppError> {
    user_profiles::table
        .filter(user_profiles::user_id.eq(user_id))
        .select(UserProfile::as_select())
        .first::<UserProfile>(conn)
        .optional()
        .map_err(AppError::from)
}

/// Load a user's profile in its response shape, with the latest weight folded in.
pub fn load_profile_response(
    conn: &mut SqliteConnection,
    user_id: &str,
) -> Result<Option<ProfileResponse>, AppError> {
    let Some(profile) = load_profile(conn, user_id)? else {
        return Ok(None);
    };
    let current_weight_kg = latest_weight(conn, user_id)?;
    Ok(Some(ProfileResponse {
        sex: profile.sex()?,
        birth_date: profile.birth_date,
        height_cm: profile.height_cm,
        activity_factor: profile.activity_factor,
        goal_type: profile.goal_type()?,
        target_weight_kg: profile.target_weight_kg,
        rate_kg_per_week: profile.rate_kg_per_week,
        target_kcal: profile.target_kcal,
        target_protein_g: profile.target_protein_g,
        target_fat_g: profile.target_fat_g,
        target_carbs_g: profile.target_carbs_g,
        calibration_factor: profile.calibration_factor,
        targets_computed_at: profile.targets_computed_at,
        timezone: profile.timezone,
        current_weight_kg,
    }))
}

/// The most recent logged weight for a user.
pub fn latest_weight(conn: &mut SqliteConnection, user_id: &str) -> Result<Option<f64>, AppError> {
    weight_logs::table
        .filter(weight_logs::user_id.eq(user_id))
        .order(weight_logs::logged_at.desc())
        .select(weight_logs::weight_kg)
        .first::<f64>(conn)
        .optional()
        .map_err(AppError::from)
}

/// Assemble the formula inputs from a stored profile.
fn target_inputs(
    profile: &UserProfile,
    weight_kg: f64,
    today: NaiveDate,
) -> Result<TargetInputs, AppError> {
    Ok(TargetInputs {
        sex: profile.sex()?,
        age_years: age_years(profile.birth_date, today),
        height_cm: profile.height_cm,
        weight_kg,
        activity_factor: profile.activity_factor,
        goal_type: profile.goal_type()?,
        target_weight_kg: profile.target_weight_kg.unwrap_or(weight_kg),
        rate_kg_per_week: profile.rate_kg_per_week.unwrap_or(0.0),
    })
}

/// Reject body data that would produce a nonsensical target before it is saved.
pub fn validate_profile(body: &UpdateProfileRequest) -> Result<(), AppError> {
    if !(80.0..=250.0).contains(&body.height_cm) {
        return Err(AppError::BadRequest(
            "height_cm must be between 80 and 250".to_string(),
        ));
    }
    if !(1.0..=2.0).contains(&body.activity_factor) {
        return Err(AppError::BadRequest(
            "activity_factor must be between 1.0 and 2.0".to_string(),
        ));
    }
    if !(20.0..=400.0).contains(&body.target_weight_kg) {
        return Err(AppError::BadRequest(
            "target_weight_kg must be between 20 and 400".to_string(),
        ));
    }
    if body.rate_kg_per_week < 0.0 || body.rate_kg_per_week > 2.0 {
        return Err(AppError::BadRequest(
            "rate_kg_per_week must be between 0 and 2".to_string(),
        ));
    }
    if let Some(weight) = body.current_weight_kg {
        if !(20.0..=400.0).contains(&weight) {
            return Err(AppError::BadRequest(
                "current_weight_kg must be between 20 and 400".to_string(),
            ));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn valid() -> UpdateProfileRequest {
        UpdateProfileRequest {
            sex: Sex::Male,
            birth_date: NaiveDate::from_ymd_opt(1990, 5, 1).unwrap(),
            height_cm: 180.0,
            activity_factor: 1.55,
            goal_type: GoalType::Lose,
            target_weight_kg: 75.0,
            rate_kg_per_week: 0.5,
            timezone: "Europe/Moscow".to_string(),
            current_weight_kg: Some(80.0),
        }
    }

    #[test]
    fn a_sane_profile_validates() {
        assert!(validate_profile(&valid()).is_ok());
    }

    #[test]
    fn absurd_body_data_is_rejected() {
        let mut body = valid();
        body.height_cm = 12.0;
        assert!(validate_profile(&body).is_err());

        let mut body = valid();
        body.activity_factor = 4.0;
        assert!(validate_profile(&body).is_err());

        let mut body = valid();
        body.rate_kg_per_week = -1.0;
        assert!(validate_profile(&body).is_err());
    }
}
