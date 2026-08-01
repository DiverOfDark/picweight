//! Sittings — `GET /api/v1/groups/:group_id`.
//!
//! A group is a **display and notification** concern only (PRD §5). Every meal
//! in it was analysed by its own independent loop; grouping never affects
//! estimation. It exists so N dishes produce one notification and collapse into
//! one row in the day view.

use crate::agent::schema::MacroTotals;
use crate::api::meals::{build_meal_responses, MealResponse};
use crate::auth::CurrentUser;
use crate::error::AppError;
use crate::models::{Meal, MealStatus};
use crate::schema::{meals, notification_groups};
use crate::AppState;
use axum::extract::{Path, State};
use axum::Json;
use chrono::{DateTime, NaiveDateTime, Utc};
use diesel::prelude::*;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use utoipa::ToSchema;

/// One sitting: its member meals and their combined totals.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct GroupResponse {
    /// Client-generated sitting id.
    pub group_id: String,
    /// The client's `group_size` hint, when it sent one.
    pub expected_size: Option<i32>,
    /// Meals that have actually landed.
    pub member_count: usize,
    /// True once no member is still `pending` or `analyzing`.
    pub settled: bool,
    /// When the single grouped notification fired, if it has.
    pub notified_at: Option<DateTime<Utc>>,
    /// When the most recent photo joined; the debounce runs from here.
    pub last_photo_at: DateTime<Utc>,
    /// Sum across every member meal.
    pub totals: MacroTotals,
    /// The member meals, oldest first — the expanded per-dish rows.
    pub meals: Vec<MealResponse>,
}

/// A group as it appears collapsed in the day view.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct GroupSummary {
    /// Client-generated sitting id.
    pub group_id: String,
    /// How many meals it contains.
    pub member_count: usize,
    /// Combined totals.
    pub totals: MacroTotals,
    /// Dish names, for the collapsed row's label.
    pub dish_names: Vec<String>,
    /// Earliest `eaten_at` among the members.
    pub eaten_at: DateTime<Utc>,
    /// True when a member failed — the row should say so rather than quietly
    /// under-report.
    pub has_failures: bool,
}

/// `GET /api/v1/groups/{group_id}` — the sitting and its combined totals.
#[utoipa::path(
    get,
    path = "/api/v1/groups/{group_id}",
    tag = "groups",
    summary = "Get a sitting",
    description = "Member meals plus combined totals. Each meal was analysed by \
its own independent loop and stays independently correctable.",
    params(("group_id" = String, Path, description = "Client-generated sitting id")),
    responses(
        (status = 200, description = "The sitting", body = GroupResponse),
        (status = 404, description = "No such group for this user", body = crate::error::ErrorBody),
    ),
    security(("session" = []))
)]
pub async fn get_group(
    State(state): State<AppState>,
    user: CurrentUser,
    Path(group_id): Path<String>,
) -> Result<Json<GroupResponse>, AppError> {
    let user_id = user.id;

    let response = state
        .interact(move |conn| {
            // Scoped by user_id: a group id is client-generated, so it must not
            // be a handle onto another account's sitting.
            let (expected_size, notified_at, last_photo_at) = notification_groups::table
                .filter(notification_groups::group_id.eq(&group_id))
                .filter(notification_groups::user_id.eq(&user_id))
                .select((
                    notification_groups::expected_size,
                    notification_groups::notified_at,
                    notification_groups::last_photo_at,
                ))
                .first::<(Option<i32>, Option<NaiveDateTime>, NaiveDateTime)>(conn)
                .optional()?
                .ok_or_else(|| AppError::NotFound("group".to_string()))?;

            let rows = meals::table
                .filter(meals::user_id.eq(&user_id))
                .filter(meals::group_id.eq(&group_id))
                .order(meals::created_at.asc())
                .select(Meal::as_select())
                .load::<Meal>(conn)?;

            // A failed member does not stop a sitting from being settled —
            // four dishes still land (§5).
            let settled = rows.iter().all(|meal| meal.is_terminal());
            let meals = build_meal_responses(conn, rows)?;
            let totals = meals
                .iter()
                .fold(MacroTotals::default(), |acc, meal| acc.plus(meal.totals));

            Ok(GroupResponse {
                group_id,
                expected_size,
                member_count: meals.len(),
                settled,
                // SQLite stores naive UTC; the wire format must carry the offset.
                notified_at: notified_at.map(|at| at.and_utc()),
                last_photo_at: last_photo_at.and_utc(),
                totals,
                meals,
            })
        })
        .await?;

    Ok(Json(response))
}

/// Collapse a user's meals into group summaries plus the ungrouped remainder.
///
/// Used by the day view and by history; a solo meal is simply a group of one,
/// which is why there is no special case for it.
///
/// Groups come back in the order their first member appears in `meals`, so a
/// newest-first input yields newest-first sittings.
pub fn summarize_groups(meals: &[MealResponse]) -> (Vec<GroupSummary>, Vec<MealResponse>) {
    let mut order: Vec<String> = Vec::new();
    let mut members: HashMap<String, Vec<&MealResponse>> = HashMap::new();
    let mut ungrouped: Vec<MealResponse> = Vec::new();

    for meal in meals {
        match meal.group_id.as_deref() {
            Some(group_id) => {
                let entry = members.entry(group_id.to_string()).or_insert_with(|| {
                    order.push(group_id.to_string());
                    Vec::new()
                });
                entry.push(meal);
            }
            None => ungrouped.push(meal.clone()),
        }
    }

    let summaries = order
        .into_iter()
        .filter_map(|group_id| {
            let group = members.remove(&group_id)?;
            let totals = group
                .iter()
                .fold(MacroTotals::default(), |acc, meal| acc.plus(meal.totals));
            let eaten_at = group
                .iter()
                .map(|meal| meal.eaten_at)
                .min()
                .unwrap_or_else(Utc::now);
            Some(GroupSummary {
                group_id,
                member_count: group.len(),
                totals,
                dish_names: group
                    .iter()
                    .filter_map(|meal| meal.dish_name.clone())
                    .collect(),
                eaten_at,
                has_failures: group.iter().any(|meal| meal.status == MealStatus::Failed),
            })
        })
        .collect();

    (summaries, ungrouped)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::meals::MealResponse;
    use crate::models::NameSource;
    use chrono::NaiveDate;

    fn meal(id: &str, group: Option<&str>, kcal: f64, status: MealStatus) -> MealResponse {
        let at = NaiveDate::from_ymd_opt(2026, 8, 1)
            .unwrap()
            .and_hms_opt(12, 0, 0)
            .unwrap()
            .and_utc();
        MealResponse {
            id: id.to_string(),
            client_uuid: format!("uuid-{id}"),
            group_id: group.map(str::to_string),
            group_size: None,
            dish_name: Some(format!("dish {id}")),
            name_source: NameSource::Vision,
            user_comment: None,
            revision: 1,
            status,
            eaten_at: at,
            timezone_offset: 180,
            meal_type: None,
            portion_scale: 1.0,
            thumbnail_url: None,
            items: Vec::new(),
            totals: MacroTotals {
                kcal,
                protein_g: 0.0,
                fat_g: 0.0,
                carbs_g: 0.0,
            },
            error: None,
            created_at: at,
            updated_at: at,
        }
    }

    #[test]
    fn a_sitting_collapses_and_solo_meals_stay_out() {
        let meals = vec![
            meal("1", Some("g1"), 780.0, MealStatus::Confirmed),
            meal("2", Some("g1"), 420.0, MealStatus::NeedsReview),
            meal("3", None, 300.0, MealStatus::Confirmed),
        ];
        let (groups, ungrouped) = summarize_groups(&meals);
        assert_eq!(groups.len(), 1);
        assert_eq!(groups[0].member_count, 2);
        assert_eq!(groups[0].totals.kcal, 1200.0);
        assert!(!groups[0].has_failures);
        assert_eq!(ungrouped.len(), 1);
        assert_eq!(ungrouped[0].id, "3");
    }

    #[test]
    fn a_failed_member_is_flagged_rather_than_hidden() {
        let meals = vec![
            meal("1", Some("g1"), 780.0, MealStatus::Confirmed),
            meal("2", Some("g1"), 0.0, MealStatus::Failed),
        ];
        let (groups, _) = summarize_groups(&meals);
        assert!(groups[0].has_failures);
        assert_eq!(groups[0].member_count, 2);
    }
}
