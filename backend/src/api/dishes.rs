//! `GET /api/v1/dishes/recent` — the recent-dish chips.
//!
//! One of the two zero-keyboard input paths (PRD §1). The capture screen shows
//! the last ~8 confirmed dishes as chips; one tap sets the name, which is both
//! faster than typing and more accurate than vision.
//!
//! Only **confirmed** meals appear here, for the same reason recall only reads
//! confirmed meals: a chip that seeds the agent with an unreviewed guess would
//! launder a hallucination into an input.

use crate::agent::schema::MacroTotals;
use crate::auth::CurrentUser;
use crate::error::AppError;
use crate::models::{MealItem, MealStatus};
use crate::schema::{meal_items, meals};
use crate::AppState;
use axum::extract::{Query, State};
use axum::Json;
use chrono::NaiveDateTime;
use diesel::prelude::*;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use utoipa::{IntoParams, ToSchema};

/// Chips shown on the capture screen by default.
pub const DEFAULT_RECENT_LIMIT: i64 = 8;

/// Largest number of chips a client may ask for.
pub const MAX_RECENT_LIMIT: i64 = 50;

/// How many confirmed meals are scanned to build the chip list.
///
/// Chips are grouped by normalized name, so the scan window has to be wider
/// than the requested chip count — a user who eats the same three dishes all
/// week would otherwise get three chips out of eight.
const SCAN_WINDOW: i64 = 500;

/// One previously confirmed dish, ready to become a chip.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct RecentDish {
    /// The dish name as the user confirmed it.
    pub dish_name: String,
    /// Normalized form — what recall matches on.
    pub dish_name_normalized: String,
    /// Most recent time this dish was eaten.
    pub last_eaten_at: NaiveDateTime,
    /// How many times it has been confirmed. High counts are the flywheel
    /// working.
    pub occurrences: i64,
    /// Totals from the most recent confirmed occurrence, so the chip can show
    /// "≈780 kcal".
    pub totals: MacroTotals,
    /// The meal those totals came from, for a "same as last time" shortcut.
    pub last_meal_id: String,
}

/// Query for `GET /api/v1/dishes/recent`.
#[derive(Debug, Clone, Default, Deserialize, IntoParams)]
pub struct RecentDishesQuery {
    /// How many chips to return. Defaults to [`DEFAULT_RECENT_LIMIT`], capped at
    /// [`MAX_RECENT_LIMIT`].
    pub limit: Option<i64>,
}

/// `GET /api/v1/dishes/recent` — the last N confirmed dishes.
#[utoipa::path(
    get,
    path = "/api/v1/dishes/recent",
    tag = "dishes",
    summary = "Recent confirmed dishes",
    description = "Powers the capture screen's recent-dish chips — one tap sets \
the dish name, no keyboard. Confirmed meals only.",
    params(RecentDishesQuery),
    responses(
        (status = 200, description = "Recent dishes, most recent first", body = Vec<RecentDish>),
    ),
    security(("session" = []))
)]
pub async fn recent_dishes(
    State(state): State<AppState>,
    user: CurrentUser,
    Query(query): Query<RecentDishesQuery>,
) -> Result<Json<Vec<RecentDish>>, AppError> {
    let limit = query
        .limit
        .unwrap_or(DEFAULT_RECENT_LIMIT)
        .clamp(1, MAX_RECENT_LIMIT);
    let user_id = user.id;

    let dishes = state
        .interact(move |conn| {
            // (user_id, status, eaten_at DESC) is exactly `idx_meals_user_status_eaten`.
            let rows = meals::table
                .filter(meals::user_id.eq(&user_id))
                .filter(meals::status.eq(MealStatus::Confirmed.as_str()))
                .filter(meals::dish_name_normalized.is_not_null())
                .order(meals::eaten_at.desc())
                .limit(SCAN_WINDOW)
                .select((
                    meals::id,
                    meals::dish_name,
                    meals::dish_name_normalized,
                    meals::eaten_at,
                    meals::revision,
                    meals::portion_scale,
                ))
                .load::<(
                    String,
                    Option<String>,
                    Option<String>,
                    NaiveDateTime,
                    i32,
                    f64,
                )>(conn)?;

            // Rows arrive newest first, so the first sighting of a normalized
            // name is the occurrence whose numbers the chip should show.
            let mut order: Vec<String> = Vec::new();
            let mut seen: HashMap<String, RecentDish> = HashMap::new();
            let mut sources: HashMap<String, (i32, f64)> = HashMap::new();

            for (meal_id, dish_name, normalized, eaten_at, revision, portion_scale) in rows {
                let Some(normalized) = normalized.filter(|n| !n.is_empty()) else {
                    continue;
                };
                match seen.get_mut(&normalized) {
                    Some(existing) => existing.occurrences += 1,
                    None => {
                        order.push(normalized.clone());
                        sources.insert(normalized.clone(), (revision, portion_scale));
                        seen.insert(
                            normalized.clone(),
                            RecentDish {
                                dish_name: dish_name.unwrap_or_else(|| normalized.clone()),
                                dish_name_normalized: normalized,
                                last_eaten_at: eaten_at,
                                occurrences: 1,
                                totals: MacroTotals::default(),
                                last_meal_id: meal_id,
                            },
                        );
                    }
                }
            }

            order.truncate(limit as usize);
            let mut chips: Vec<RecentDish> = order
                .into_iter()
                .filter_map(|normalized| seen.remove(&normalized))
                .collect();

            // One batched item load for the chips that survived the truncate.
            let meal_ids: Vec<String> = chips.iter().map(|d| d.last_meal_id.clone()).collect();
            if !meal_ids.is_empty() {
                let items = meal_items::table
                    .filter(meal_items::meal_id.eq_any(&meal_ids))
                    .select(MealItem::as_select())
                    .load::<MealItem>(conn)?;
                let mut by_meal: HashMap<String, Vec<MealItem>> = HashMap::new();
                for item in items {
                    by_meal.entry(item.meal_id.clone()).or_default().push(item);
                }
                for chip in &mut chips {
                    let (revision, portion_scale) = sources
                        .get(&chip.dish_name_normalized)
                        .copied()
                        .unwrap_or((1, 1.0));
                    let scale = if portion_scale.is_finite() && portion_scale > 0.0 {
                        portion_scale
                    } else {
                        1.0
                    };
                    chip.totals = by_meal
                        .get(&chip.last_meal_id)
                        .map(|items| {
                            items
                                .iter()
                                .filter(|item| item.revision == revision)
                                .fold(MacroTotals::default(), |acc, item| MacroTotals {
                                    kcal: acc.kcal + item.kcal,
                                    protein_g: acc.protein_g + item.protein_g,
                                    fat_g: acc.fat_g + item.fat_g,
                                    carbs_g: acc.carbs_g + item.carbs_g,
                                })
                                .scaled(scale)
                        })
                        .unwrap_or_default();
                }
            }

            Ok(chips)
        })
        .await?;

    Ok(Json(dishes))
}
