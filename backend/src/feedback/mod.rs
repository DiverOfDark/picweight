//! Per-meal feedback (PRD §6).
//!
//! **Event-driven, never scheduled.** The only trigger is "you just logged
//! something". There is no daily cron and no periodic nudge anywhere in this
//! crate — by design, and G-non-goal §4.
//!
//! The split is deliberate: [`state`] is a rules engine that computes the
//! numbers (consumed / remaining / protein gap / verdict class), and
//! [`phrasing`] supplies only the wording. If the LLM is unavailable you get
//! the templated string — never a hard fail.

pub mod phrasing;
pub mod state;

use crate::agent::schema::MacroTotals;
use crate::error::AppError;
use crate::models::{Meal, MealStatus};
use crate::AppState;
use diesel::prelude::*;
use diesel::SqliteConnection;
use serde::{Deserialize, Serialize};
pub use state::{DayState, DayStatus};
use utoipa::ToSchema;

/// A self-contained notification payload.
///
/// Self-contained is the requirement, not a nicety: the notification fires
/// ~20–30s after capture, by which time the phone may be pocketed, so it has to
/// make sense read cold minutes later. Lead with the dish name (§6 timing).
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct MealFeedback {
    /// Line 1 — what was logged: "Шаурма с курицей — 780 kcal".
    pub headline: String,
    /// Line 2 — where you stand: "1,450 / 2,050 today · 600 left".
    pub standing: String,
    /// Line 3 — macro status against the floor that actually binds.
    pub macro_status: String,
    /// Line 4 — the one-line verdict, LLM-phrased from the numbers with a
    /// templated fallback.
    pub verdict: String,
    /// The full day state, so a client renders from one message with no
    /// follow-up round trip.
    pub day: DayState,
}

impl MealFeedback {
    /// Lines 2–4 joined, ready to be the body of a push notification.
    pub fn body(&self) -> String {
        format!("{}\n{}\n{}", self.standing, self.macro_status, self.verdict)
    }
}

/// What the rules engine needs to know about a meal before it can be phrased.
///
/// Deliberately not `MealResponse`: the notification path must not depend on the
/// API layer's response shapes.
#[derive(Debug, Clone)]
pub struct MealSummary {
    /// `meals.id`.
    pub id: String,
    /// The dish as named, or a neutral stand-in when the agent never got that far.
    pub dish_name: String,
    /// Totals at the meal's current revision, already scaled by `portion_scale`.
    pub totals: MacroTotals,
    /// The local day the meal belongs to.
    pub date: chrono::NaiveDate,
    /// Minutes east of UTC the client captured it at.
    pub tz_offset_minutes: i32,
}

/// Fallback dish name for a meal that failed before it was identified.
pub const UNNAMED_DISH: &str = "Meal";

/// Build the notification for one completed meal.
///
/// Self-contained by construction: the caller gets the four rendered lines *and*
/// the full [`DayState`], so a client can render the notification and the home
/// screen from this one value with no follow-up request (§9).
pub async fn build_meal_feedback(
    state: &AppState,
    user_id: &str,
    meal_id: &str,
) -> Result<MealFeedback, AppError> {
    let (summary, day) = {
        let user_id = user_id.to_string();
        let meal_id = meal_id.to_string();
        state
            .interact(move |conn| {
                let summary = meal_summary(conn, &user_id, &meal_id)?;
                let day = state::compute_day_state(
                    conn,
                    &user_id,
                    summary.date,
                    summary.tz_offset_minutes,
                )?;
                Ok((summary, day))
            })
            .await?
    };

    let headline = phrasing::notification_headline(&summary.dish_name, summary.totals.kcal);
    Ok(finish(state, headline, day).await)
}

/// Build the single notification for a settled sitting.
///
/// Leads with a summary line ("5 dishes — 2,140 kcal") rather than one
/// notification per photo (§6).
pub async fn build_group_feedback(
    state: &AppState,
    user_id: &str,
    group_id: &str,
) -> Result<MealFeedback, AppError> {
    let (members, day) = {
        let user_id = user_id.to_string();
        let group_id = group_id.to_string();
        state
            .interact(move |conn| {
                let members = group_summaries(conn, &user_id, &group_id)?;
                // Every member of a sitting was eaten at the same time by
                // definition, so the newest member's local day is the sitting's.
                let (date, offset) = members
                    .last()
                    .map(|m| (m.date, m.tz_offset_minutes))
                    .unwrap_or_else(|| (chrono::Utc::now().date_naive(), 0));
                let day = state::compute_day_state(conn, &user_id, date, offset)?;
                Ok((members, day))
            })
            .await?
    };

    if members.is_empty() {
        return Err(AppError::NotFound(format!(
            "sitting {group_id} has no meals"
        )));
    }

    let totals = members
        .iter()
        .fold(MacroTotals::default(), |acc, member| acc.plus(member.totals));
    let headline = if members.len() == 1 {
        phrasing::notification_headline(&members[0].dish_name, totals.kcal)
    } else {
        phrasing::group_notification_headline(members.len(), totals.kcal)
    };

    Ok(finish(state, headline, day).await)
}

/// Assemble the four lines once the headline and the day state are settled.
async fn finish(state: &AppState, headline: String, day: DayState) -> MealFeedback {
    let verdict = phrasing::phrase_verdict(state, &day, &headline).await;
    MealFeedback {
        headline,
        standing: phrasing::standing_line(&day),
        macro_status: phrasing::macro_line(&day),
        verdict,
        day,
    }
}

/// Load one meal's totals at its current revision.
pub fn meal_summary(
    conn: &mut SqliteConnection,
    user_id: &str,
    meal_id: &str,
) -> Result<MealSummary, AppError> {
    use crate::schema::meals::dsl;

    let meal = dsl::meals
        .filter(dsl::id.eq(meal_id))
        // Scoping the read to the owner is what makes a cross-user id a 404
        // rather than a leak.
        .filter(dsl::user_id.eq(user_id))
        .select(Meal::as_select())
        .first::<Meal>(conn)
        .optional()?
        .ok_or_else(|| AppError::NotFound(format!("meal {meal_id}")))?;

    summarize(conn, meal)
}

/// Load every member of a sitting, oldest first.
pub fn group_summaries(
    conn: &mut SqliteConnection,
    user_id: &str,
    group_id: &str,
) -> Result<Vec<MealSummary>, AppError> {
    use crate::schema::meals::dsl;

    let meals = dsl::meals
        .filter(dsl::group_id.eq(group_id))
        .filter(dsl::user_id.eq(user_id))
        // A failed member is still part of the sitting; it simply contributes
        // no numbers. Excluding it here would make "4 dishes" read as "3".
        .order((dsl::created_at.asc(), dsl::id.asc()))
        .select(Meal::as_select())
        .load::<Meal>(conn)?;

    meals.into_iter().map(|meal| summarize(conn, meal)).collect()
}

/// Turn a loaded meal row into a [`MealSummary`].
fn summarize(conn: &mut SqliteConnection, meal: Meal) -> Result<MealSummary, AppError> {
    use crate::schema::meal_items::dsl as items;

    let totals = if meal.status == MealStatus::Failed.as_str() {
        MacroTotals::default()
    } else {
        let rows: Vec<(f64, f64, f64, f64)> = items::meal_items
            .filter(items::meal_id.eq(&meal.id))
            .filter(items::revision.eq(meal.revision))
            .select((items::kcal, items::protein_g, items::fat_g, items::carbs_g))
            .load::<(f64, f64, f64, f64)>(conn)?;
        rows.into_iter()
            .fold(
                MacroTotals::default(),
                |acc, (kcal, protein_g, fat_g, carbs_g)| {
                    acc.plus(MacroTotals {
                        kcal,
                        protein_g,
                        fat_g,
                        carbs_g,
                    })
                },
            )
            .scaled(meal.portion_scale)
    };

    Ok(MealSummary {
        dish_name: meal
            .dish_name
            .clone()
            .filter(|name| !name.trim().is_empty())
            .unwrap_or_else(|| UNNAMED_DISH.to_string()),
        date: state::local_date_of(meal.eaten_at, meal.timezone_offset),
        tz_offset_minutes: meal.timezone_offset,
        totals,
        id: meal.id,
    })
}

#[cfg(test)]
mod tests {
    use super::state::fixtures::*;
    use super::*;

    #[test]
    fn a_meal_summary_scales_by_portion_and_reads_the_current_revision() {
        let mut conn = test_conn();
        seed_user(&mut conn, "u1");
        seed_meal(
            &mut conn,
            "m1",
            "u1",
            at(2026, 8, 1, 12, 0),
            180,
            MealStatus::NeedsReview,
            2,
            0.5,
            None,
        );
        seed_item(&mut conn, "m1", 1, 1000.0, 50.0, 20.0, 100.0);
        seed_item(&mut conn, "m1", 2, 800.0, 40.0, 16.0, 80.0);

        let summary = meal_summary(&mut conn, "u1", "m1").unwrap();
        assert_eq!(summary.totals.kcal, 400.0);
        assert_eq!(summary.tz_offset_minutes, 180);
        assert_eq!(summary.date, chrono::NaiveDate::from_ymd_opt(2026, 8, 1).unwrap());
        assert_eq!(summary.dish_name, "dish m1");
    }

    #[test]
    fn another_users_meal_is_a_404_not_a_leak() {
        let mut conn = test_conn();
        seed_user(&mut conn, "u1");
        seed_user(&mut conn, "u2");
        seed_meal(
            &mut conn,
            "m1",
            "u1",
            at(2026, 8, 1, 12, 0),
            0,
            MealStatus::Confirmed,
            1,
            1.0,
            None,
        );

        let err = meal_summary(&mut conn, "u2", "m1").unwrap_err();
        assert_eq!(err.status(), axum::http::StatusCode::NOT_FOUND);
    }

    #[test]
    fn a_failed_meal_contributes_no_numbers_but_keeps_its_name() {
        let mut conn = test_conn();
        seed_user(&mut conn, "u1");
        seed_meal(
            &mut conn,
            "m1",
            "u1",
            at(2026, 8, 1, 12, 0),
            0,
            MealStatus::Failed,
            1,
            1.0,
            None,
        );
        seed_item(&mut conn, "m1", 1, 9999.0, 0.0, 0.0, 0.0);

        let summary = meal_summary(&mut conn, "u1", "m1").unwrap();
        assert_eq!(summary.totals, MacroTotals::default());
        assert_eq!(summary.dish_name, "dish m1");
    }

    #[test]
    fn a_sitting_sums_its_members_and_keeps_a_failed_one_in_the_count() {
        let mut conn = test_conn();
        seed_user(&mut conn, "u1");
        seed_group(&mut conn, "g1", "u1", Some(3), at(2026, 8, 1, 12, 0));
        for (id, kcal, status) in [
            ("a", 700.0, MealStatus::NeedsReview),
            ("b", 900.0, MealStatus::Confirmed),
            ("c", 0.0, MealStatus::Failed),
        ] {
            seed_meal(
                &mut conn,
                id,
                "u1",
                at(2026, 8, 1, 12, 0),
                0,
                status,
                1,
                1.0,
                Some("g1"),
            );
            if kcal > 0.0 {
                seed_item(&mut conn, id, 1, kcal, 10.0, 5.0, 50.0);
            }
        }

        let members = group_summaries(&mut conn, "u1", "g1").unwrap();
        assert_eq!(members.len(), 3, "a failed dish is still part of the sitting");
        let total: f64 = members.iter().map(|m| m.totals.kcal).sum();
        assert_eq!(total, 1600.0);
    }

    #[test]
    fn a_sitting_never_reaches_across_users() {
        let mut conn = test_conn();
        seed_user(&mut conn, "u1");
        seed_user(&mut conn, "u2");
        seed_group(&mut conn, "g1", "u1", None, at(2026, 8, 1, 12, 0));
        seed_meal(
            &mut conn,
            "mine",
            "u1",
            at(2026, 8, 1, 12, 0),
            0,
            MealStatus::Confirmed,
            1,
            1.0,
            Some("g1"),
        );
        seed_meal(
            &mut conn,
            "theirs",
            "u2",
            at(2026, 8, 1, 12, 0),
            0,
            MealStatus::Confirmed,
            1,
            1.0,
            Some("g1"),
        );

        assert_eq!(group_summaries(&mut conn, "u1", "g1").unwrap().len(), 1);
        assert_eq!(group_summaries(&mut conn, "u2", "g1").unwrap().len(), 1);
    }

    #[test]
    fn the_notification_body_is_three_lines() {
        let feedback = MealFeedback {
            headline: "Шаурма — 780 kcal".into(),
            standing: "1,450 / 2,050 today · 600 left".into(),
            macro_status: "Protein 82/165g — 83g short with 600 kcal to spend".into(),
            verdict: "Doable, but it has to be mostly protein.".into(),
            day: DayState::empty(chrono::NaiveDate::from_ymd_opt(2026, 8, 1).unwrap()),
        };
        assert_eq!(feedback.body().lines().count(), 3);
    }
}
