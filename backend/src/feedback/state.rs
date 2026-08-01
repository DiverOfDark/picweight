//! The rules engine behind per-meal feedback and `GET /api/v1/days/:date`.
//!
//! Everything here is arithmetic over rows. The LLM never sees these numbers
//! except as text to phrase (see [`super::phrasing`]).
//!
//! **Days are local days.** "What did I eat today" is a question about *your*
//! day, not UTC's, so every query is bounded by
//! [`local_day_bounds`] using the per-meal `timezone_offset`.

use crate::agent::schema::MacroTotals;
use crate::error::AppError;
use crate::models::{MealStatus, UserProfile};
use chrono::{NaiveDate, NaiveDateTime};
use diesel::prelude::*;
use diesel::SqliteConnection;
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

/// How a day is going. Computed from the numbers, never asked of a model.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum DayStatus {
    /// Comfortably within budget.
    OnTrack,
    /// Under budget, but with little room left.
    Tight,
    /// Over the energy target.
    Over,
    /// The protein floor can no longer be reached within the remaining kcal —
    /// the constraint that actually binds (§6).
    ProteinUnreachable,
    /// The user has no computed targets yet (onboarding incomplete).
    NoTargets,
}

/// Fraction of the target remaining below which a day counts as `Tight`.
pub const TIGHT_FRACTION: f64 = 0.15;

/// A day's totals against its targets.
///
/// Carried verbatim in the `/meals/events` completion payload so a client
/// renders the notification from one message with no follow-up round trip (§9).
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct DayState {
    /// The local calendar day these figures cover.
    pub date: NaiveDate,
    /// Energy consumed, kcal.
    pub consumed_kcal: f64,
    /// Energy target, kcal. Zero when targets are not computed.
    pub target_kcal: f64,
    /// `target_kcal - consumed_kcal`; may be negative.
    pub remaining_kcal: f64,
    /// Protein consumed, grams.
    pub consumed_protein_g: f64,
    /// Protein floor, grams.
    pub target_protein_g: f64,
    /// Protein still owed, grams; clamped at zero.
    pub remaining_protein_g: f64,
    /// Fat consumed, grams.
    pub consumed_fat_g: f64,
    /// Fat floor, grams.
    pub target_fat_g: f64,
    /// Carbohydrate consumed, grams.
    pub consumed_carbs_g: f64,
    /// Carbohydrate allowance, grams.
    pub target_carbs_g: f64,
    /// Meals logged on this day (all statuses except `failed`).
    pub meals_logged: i64,
    /// Classification derived from the figures above.
    pub status: DayStatus,
}

impl DayState {
    /// An empty day for a user with no computed targets.
    pub fn empty(date: NaiveDate) -> Self {
        DayState {
            date,
            consumed_kcal: 0.0,
            target_kcal: 0.0,
            remaining_kcal: 0.0,
            consumed_protein_g: 0.0,
            target_protein_g: 0.0,
            remaining_protein_g: 0.0,
            consumed_fat_g: 0.0,
            target_fat_g: 0.0,
            consumed_carbs_g: 0.0,
            target_carbs_g: 0.0,
            meals_logged: 0,
            status: DayStatus::NoTargets,
        }
    }

    /// Consumed macros as a single value.
    pub fn consumed(&self) -> MacroTotals {
        MacroTotals {
            kcal: self.consumed_kcal,
            protein_g: self.consumed_protein_g,
            fat_g: self.consumed_fat_g,
            carbs_g: self.consumed_carbs_g,
        }
    }
}

/// Compute a user's day state.
///
/// `tz_offset_minutes` is the offset east of UTC to bucket by — normally the
/// offset the client sent with the most recent meal, falling back to the
/// profile's timezone.
pub fn compute_day_state(
    conn: &mut SqliteConnection,
    user_id: &str,
    date: NaiveDate,
    tz_offset_minutes: i32,
) -> Result<DayState, AppError> {
    let (consumed, meals_logged) = consumed_for_day(conn, user_id, date, tz_offset_minutes)?;
    let targets = load_targets(conn, user_id)?;

    let mut day = DayState {
        date,
        consumed_kcal: consumed.kcal,
        target_kcal: targets.kcal,
        remaining_kcal: targets.kcal - consumed.kcal,
        consumed_protein_g: consumed.protein_g,
        target_protein_g: targets.protein_g,
        remaining_protein_g: (targets.protein_g - consumed.protein_g).max(0.0),
        consumed_fat_g: consumed.fat_g,
        target_fat_g: targets.fat_g,
        consumed_carbs_g: consumed.carbs_g,
        target_carbs_g: targets.carbs_g,
        meals_logged,
        status: DayStatus::NoTargets,
    };
    // "Remaining" is meaningless without a target; leave it at zero rather than
    // reporting a budget of −1,450.
    if targets.kcal <= 0.0 {
        day.remaining_kcal = 0.0;
        day.remaining_protein_g = 0.0;
    }
    day.status = classify(&day);
    Ok(day)
}

/// The four daily figures a day is measured against. All zero when the user has
/// not finished onboarding.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
struct DayTargets {
    kcal: f64,
    protein_g: f64,
    fat_g: f64,
    carbs_g: f64,
}

/// Read the stored targets off `user_profiles`.
///
/// These are computed by [`crate::nutrition::targets`] when the profile is saved
/// and re-read here; recomputing per day view would make a day's numbers move
/// under the user every time they logged a weight.
fn load_targets(conn: &mut SqliteConnection, user_id: &str) -> Result<DayTargets, AppError> {
    use crate::schema::user_profiles::dsl;

    let profile = dsl::user_profiles
        .filter(dsl::user_id.eq(user_id))
        .select(UserProfile::as_select())
        .first::<UserProfile>(conn)
        .optional()?;

    Ok(match profile {
        None => DayTargets::default(),
        Some(profile) => DayTargets {
            kcal: profile.target_kcal.unwrap_or(0.0).max(0.0),
            protein_g: profile.target_protein_g.unwrap_or(0.0).max(0.0),
            fat_g: profile.target_fat_g.unwrap_or(0.0).max(0.0),
            carbs_g: profile.target_carbs_g.unwrap_or(0.0).max(0.0),
        },
    })
}

/// Sum a user's consumed macros over a local day.
///
/// Reads `meal_items` at each meal's **current** revision only — a superseded
/// revision is history, not food — scales every figure by the meal's
/// `portion_scale` ("ate 60% of it"), and skips `failed` meals, which have no
/// numbers anyone should trust.
///
/// The second element is how many meals were logged, counted over meals rather
/// than items so a five-component dish is still one meal.
pub fn consumed_for_day(
    conn: &mut SqliteConnection,
    user_id: &str,
    date: NaiveDate,
    tz_offset_minutes: i32,
) -> Result<(MacroTotals, i64), AppError> {
    use crate::schema::{meal_items, meals};

    let (start, end) = local_day_bounds(date, tz_offset_minutes);
    let failed = MealStatus::Failed.as_str();

    let rows: Vec<(f64, f64, f64, f64, f64)> = meals::table
        .inner_join(meal_items::table)
        .filter(meals::user_id.eq(user_id))
        .filter(meals::eaten_at.ge(start))
        .filter(meals::eaten_at.lt(end))
        .filter(meals::status.ne(failed))
        .filter(meal_items::revision.eq(meals::revision))
        .select((
            meals::portion_scale,
            meal_items::kcal,
            meal_items::protein_g,
            meal_items::fat_g,
            meal_items::carbs_g,
        ))
        .load::<(f64, f64, f64, f64, f64)>(conn)?;

    let totals = rows.into_iter().fold(
        MacroTotals::default(),
        |acc, (scale, kcal, protein_g, fat_g, carbs_g)| {
            acc.plus(
                MacroTotals {
                    kcal,
                    protein_g,
                    fat_g,
                    carbs_g,
                }
                .scaled(scale),
            )
        },
    );

    let meals_logged: i64 = meals::table
        .filter(meals::user_id.eq(user_id))
        .filter(meals::eaten_at.ge(start))
        .filter(meals::eaten_at.lt(end))
        .filter(meals::status.ne(failed))
        .count()
        .get_result(conn)?;

    Ok((totals, meals_logged))
}

/// UTC half-open bounds `[start, end)` of a local calendar day.
///
/// `tz_offset_minutes` is minutes **east** of UTC, matching
/// `meals.timezone_offset`. Half-open because a meal at exactly local midnight
/// belongs to the day starting there, not the one ending.
pub fn local_day_bounds(date: NaiveDate, tz_offset_minutes: i32) -> (NaiveDateTime, NaiveDateTime) {
    let offset = chrono::Duration::minutes(tz_offset_minutes as i64);
    let local_midnight = date
        .and_hms_opt(0, 0, 0)
        .unwrap_or_else(|| date.and_time(chrono::NaiveTime::MIN));
    let start = local_midnight - offset;
    (start, start + chrono::Duration::days(1))
}

/// The local calendar day an instant falls on.
pub fn local_date_of(at: NaiveDateTime, tz_offset_minutes: i32) -> NaiveDate {
    (at + chrono::Duration::minutes(tz_offset_minutes as i64)).date()
}

/// Classify a day from its figures.
///
/// `ProteinUnreachable` wins over `Tight`: it is the constraint that actually
/// binds, and it is actionable in a way "you're a bit low" is not.
pub fn classify(day: &DayState) -> DayStatus {
    if day.target_kcal <= 0.0 {
        return DayStatus::NoTargets;
    }
    if day.remaining_kcal < 0.0 {
        return DayStatus::Over;
    }
    // The cheapest possible protein still costs 4 kcal/g. If the outstanding
    // protein alone exceeds the remaining energy, the floor cannot be met.
    let protein_kcal_needed =
        day.remaining_protein_g * crate::nutrition::targets::KCAL_PER_G_PROTEIN;
    if day.remaining_protein_g > 0.0 && protein_kcal_needed > day.remaining_kcal {
        return DayStatus::ProteinUnreachable;
    }
    if day.remaining_kcal < day.target_kcal * TIGHT_FRACTION {
        return DayStatus::Tight;
    }
    DayStatus::OnTrack
}

/// Minimal database fixtures, shared by the day-state, notification, worker and
/// storage tests.
///
/// Deliberately hand-rolled rather than going through the API layer: these tests
/// are about arithmetic and state transitions, and they must keep passing while
/// the handlers are still being written. It lives here because the day-state
/// tests were the first to need it; anything `pub(crate)` in it is fair game
/// from any other module's `#[cfg(test)]` block.
#[cfg(test)]
pub(crate) mod fixtures {
    use crate::db::{DbConnection, DbPool};
    use crate::models::*;
    use chrono::{NaiveDate, NaiveDateTime};
    use diesel::prelude::*;

    /// A migrated, file-backed temp database.
    ///
    /// The `TempDir` is leaked on purpose: the pool must outlive it for the
    /// duration of the test, and a test process is short-lived.
    pub fn test_pool() -> DbPool {
        let dir = Box::leak(Box::new(tempfile::tempdir().unwrap()));
        let pool = crate::db::establish_pool(dir.path().join("picweight.db")).unwrap();
        crate::db::run_migrations(&pool).unwrap();
        pool
    }

    /// A migrated temp database with one connection checked out.
    pub fn test_conn() -> DbConnection {
        test_pool().get().unwrap()
    }

    /// UTC instant helper.
    pub fn at(y: i32, m: u32, d: u32, hh: u32, mm: u32) -> NaiveDateTime {
        NaiveDate::from_ymd_opt(y, m, d)
            .unwrap()
            .and_hms_opt(hh, mm, 0)
            .unwrap()
    }

    /// Insert a user.
    pub fn seed_user(conn: &mut SqliteConnection, id: &str) {
        diesel::insert_into(crate::schema::users::table)
            .values(&NewUser {
                id: id.to_string(),
                oidc_sub: format!("sub-{id}"),
                oidc_issuer: "https://auth.example.com".into(),
                email: Some(format!("{id}@example.com")),
                display_name: Some(id.to_string()),
                created_at: at(2026, 1, 1, 0, 0),
            })
            .execute(conn)
            .unwrap();
    }

    /// Insert a profile carrying pre-computed targets.
    pub fn seed_profile(
        conn: &mut SqliteConnection,
        user_id: &str,
        kcal: f64,
        protein_g: f64,
    ) {
        let now = at(2026, 1, 1, 0, 0);
        diesel::insert_into(crate::schema::user_profiles::table)
            .values(&NewUserProfile {
                user_id: user_id.to_string(),
                sex: Sex::Male.as_str().to_string(),
                birth_date: NaiveDate::from_ymd_opt(1990, 1, 1).unwrap(),
                height_cm: 180.0,
                activity_factor: 1.55,
                goal_type: GoalType::Lose.as_str().to_string(),
                target_weight_kg: Some(75.0),
                rate_kg_per_week: Some(0.5),
                target_kcal: Some(kcal),
                target_protein_g: Some(protein_g),
                target_fat_g: Some(60.0),
                target_carbs_g: Some(200.0),
                calibration_factor: 1.0,
                targets_computed_at: Some(now),
                timezone: "Europe/Moscow".into(),
                created_at: now,
                updated_at: now,
            })
            .execute(conn)
            .unwrap();
    }

    /// Insert a meal. Returns its id.
    #[allow(clippy::too_many_arguments)]
    pub fn seed_meal(
        conn: &mut SqliteConnection,
        id: &str,
        user_id: &str,
        eaten_at: NaiveDateTime,
        tz_offset: i32,
        status: MealStatus,
        revision: i32,
        portion_scale: f64,
        group_id: Option<&str>,
    ) -> String {
        let now = at(2026, 1, 1, 0, 0);
        diesel::insert_into(crate::schema::meals::table)
            .values(&NewMeal {
                id: id.to_string(),
                user_id: user_id.to_string(),
                client_uuid: format!("client-{id}"),
                thumbnail_id: None,
                group_id: group_id.map(str::to_string),
                group_size: None,
                dish_name: Some(format!("dish {id}")),
                dish_name_normalized: Some(normalize_dish_name(&format!("dish {id}"))),
                name_source: NameSource::Vision.as_str().to_string(),
                user_comment: None,
                revision,
                eaten_at,
                timezone_offset: tz_offset,
                meal_type: None,
                status: status.as_str().to_string(),
                portion_scale,
                created_at: now,
                updated_at: now,
            })
            .execute(conn)
            .unwrap();
        id.to_string()
    }

    /// Insert one item of a meal. `position` is assigned in insertion order, so
    /// callers never have to track it.
    #[allow(clippy::too_many_arguments)]
    pub fn seed_item(
        conn: &mut SqliteConnection,
        meal_id: &str,
        revision: i32,
        kcal: f64,
        protein_g: f64,
        fat_g: f64,
        carbs_g: f64,
    ) {
        use crate::schema::meal_items as mi;
        let position: i64 = mi::table
            .filter(mi::meal_id.eq(meal_id))
            .filter(mi::revision.eq(revision))
            .count()
            .get_result(conn)
            .unwrap();
        let position = position as i32;

        diesel::insert_into(crate::schema::meal_items::table)
            .values(&NewMealItem {
                id: uuid::Uuid::new_v4().to_string(),
                meal_id: meal_id.to_string(),
                revision,
                position,
                name: format!("item {position}"),
                barcode: None,
                grams: 100.0,
                grams_source: GramsSource::Agent.as_str().to_string(),
                kcal,
                protein_g,
                fat_g,
                carbs_g,
                macro_source: MacroSource::Model.as_str().to_string(),
                confidence: Some(0.7),
                reasoning_note: Some("seeded".into()),
                created_at: at(2026, 1, 1, 0, 0),
            })
            .execute(conn)
            .unwrap();
    }

    /// Insert a notification group.
    pub fn seed_group(
        conn: &mut SqliteConnection,
        group_id: &str,
        user_id: &str,
        expected_size: Option<i32>,
        last_photo_at: NaiveDateTime,
    ) {
        diesel::insert_into(crate::schema::notification_groups::table)
            .values(&NewNotificationGroup {
                group_id: group_id.to_string(),
                user_id: user_id.to_string(),
                expected_size,
                notified_at: None,
                last_photo_at,
                created_at: last_photo_at,
            })
            .execute(conn)
            .unwrap();
    }
}

#[cfg(test)]
mod tests {
    use super::fixtures::*;
    use super::*;
    use crate::models::MealStatus;

    fn day(consumed: f64, target: f64, protein_left: f64) -> DayState {
        DayState {
            date: NaiveDate::from_ymd_opt(2026, 8, 1).unwrap(),
            consumed_kcal: consumed,
            target_kcal: target,
            remaining_kcal: target - consumed,
            consumed_protein_g: 0.0,
            target_protein_g: protein_left,
            remaining_protein_g: protein_left,
            consumed_fat_g: 0.0,
            target_fat_g: 0.0,
            consumed_carbs_g: 0.0,
            target_carbs_g: 0.0,
            meals_logged: 1,
            status: DayStatus::OnTrack,
        }
    }

    #[test]
    fn bounds_shift_by_the_offset_and_are_exactly_one_day() {
        let date = NaiveDate::from_ymd_opt(2026, 8, 1).unwrap();
        // UTC+3 (Moscow): the local day starts at 21:00 UTC the day before.
        let (start, end) = local_day_bounds(date, 180);
        assert_eq!(start.to_string(), "2026-07-31 21:00:00");
        assert_eq!(end.to_string(), "2026-08-01 21:00:00");
        assert_eq!((end - start).num_hours(), 24);
    }

    #[test]
    fn bounds_handle_negative_offsets() {
        let date = NaiveDate::from_ymd_opt(2026, 8, 1).unwrap();
        // UTC-8: the local day starts at 08:00 UTC.
        let (start, _) = local_day_bounds(date, -480);
        assert_eq!(start.to_string(), "2026-08-01 08:00:00");
    }

    #[test]
    fn local_date_uses_the_offset_not_utc() {
        // 22:30 UTC on the 1st is already the 2nd in UTC+3.
        let at = NaiveDate::from_ymd_opt(2026, 8, 1)
            .unwrap()
            .and_hms_opt(22, 30, 0)
            .unwrap();
        assert_eq!(
            local_date_of(at, 180),
            NaiveDate::from_ymd_opt(2026, 8, 2).unwrap()
        );
        assert_eq!(
            local_date_of(at, 0),
            NaiveDate::from_ymd_opt(2026, 8, 1).unwrap()
        );
    }

    #[test]
    fn classification_covers_every_state() {
        assert_eq!(classify(&day(0.0, 0.0, 0.0)), DayStatus::NoTargets);
        assert_eq!(classify(&day(2500.0, 2000.0, 0.0)), DayStatus::Over);
        assert_eq!(classify(&day(1000.0, 2000.0, 0.0)), DayStatus::OnTrack);
        assert_eq!(classify(&day(1900.0, 2000.0, 0.0)), DayStatus::Tight);
        // 83g of protein left with 600 kcal to spend needs 332 kcal — reachable.
        assert_eq!(classify(&day(1450.0, 2050.0, 83.0)), DayStatus::OnTrack);
        // 200g of protein left with 600 kcal needs 800 kcal — it is not.
        assert_eq!(
            classify(&day(1450.0, 2050.0, 200.0)),
            DayStatus::ProteinUnreachable
        );
    }

    // -----------------------------------------------------------------------
    // Database-backed bucketing
    // -----------------------------------------------------------------------

    const AUG_1: NaiveDate = match NaiveDate::from_ymd_opt(2026, 8, 1) {
        Some(d) => d,
        None => panic!("2026-08-01 is a date"),
    };

    #[test]
    fn a_days_items_are_summed_at_the_current_revision_only() {
        let mut conn = test_conn();
        seed_user(&mut conn, "u1");
        seed_profile(&mut conn, "u1", 2050.0, 165.0);

        // Meal is at revision 2; revision 1 is superseded history.
        seed_meal(
            &mut conn,
            "m1",
            "u1",
            at(2026, 8, 1, 12, 0),
            0,
            MealStatus::Confirmed,
            2,
            1.0,
            None,
        );
        seed_item(&mut conn, "m1", 1, 900.0, 40.0, 30.0, 90.0);
        seed_item(&mut conn, "m1", 2, 450.0, 30.0, 15.0, 40.0);

        let (totals, count) = consumed_for_day(&mut conn, "u1", AUG_1, 0).unwrap();
        assert_eq!(totals.kcal, 450.0, "the superseded revision must not count");
        assert_eq!(totals.protein_g, 30.0);
        assert_eq!(count, 1);
    }

    #[test]
    fn portion_scale_shrinks_the_whole_meal() {
        let mut conn = test_conn();
        seed_user(&mut conn, "u1");
        seed_meal(
            &mut conn,
            "m1",
            "u1",
            at(2026, 8, 1, 12, 0),
            0,
            MealStatus::Confirmed,
            1,
            0.6,
            None,
        );
        seed_item(&mut conn, "m1", 1, 1000.0, 50.0, 20.0, 100.0);

        let (totals, _) = consumed_for_day(&mut conn, "u1", AUG_1, 0).unwrap();
        assert!((totals.kcal - 600.0).abs() < 1e-9);
        assert!((totals.protein_g - 30.0).abs() < 1e-9);
    }

    #[test]
    fn failed_meals_contribute_nothing_and_are_not_counted() {
        let mut conn = test_conn();
        seed_user(&mut conn, "u1");
        seed_meal(
            &mut conn,
            "ok",
            "u1",
            at(2026, 8, 1, 12, 0),
            0,
            MealStatus::NeedsReview,
            1,
            1.0,
            None,
        );
        seed_item(&mut conn, "ok", 1, 500.0, 20.0, 10.0, 60.0);
        seed_meal(
            &mut conn,
            "bad",
            "u1",
            at(2026, 8, 1, 13, 0),
            0,
            MealStatus::Failed,
            1,
            1.0,
            None,
        );
        seed_item(&mut conn, "bad", 1, 9999.0, 0.0, 0.0, 0.0);

        let (totals, count) = consumed_for_day(&mut conn, "u1", AUG_1, 0).unwrap();
        assert_eq!(totals.kcal, 500.0);
        assert_eq!(count, 1);
    }

    #[test]
    fn one_users_meals_never_leak_into_anothers_day() {
        let mut conn = test_conn();
        seed_user(&mut conn, "u1");
        seed_user(&mut conn, "u2");
        seed_meal(
            &mut conn,
            "mine",
            "u1",
            at(2026, 8, 1, 12, 0),
            0,
            MealStatus::Confirmed,
            1,
            1.0,
            None,
        );
        seed_item(&mut conn, "mine", 1, 400.0, 10.0, 5.0, 50.0);
        seed_meal(
            &mut conn,
            "theirs",
            "u2",
            at(2026, 8, 1, 12, 0),
            0,
            MealStatus::Confirmed,
            1,
            1.0,
            None,
        );
        seed_item(&mut conn, "theirs", 1, 4000.0, 0.0, 0.0, 0.0);

        let (totals, count) = consumed_for_day(&mut conn, "u1", AUG_1, 0).unwrap();
        assert_eq!(totals.kcal, 400.0);
        assert_eq!(count, 1);
    }

    #[test]
    fn a_late_night_meal_belongs_to_the_local_day_not_the_utc_one() {
        let mut conn = test_conn();
        seed_user(&mut conn, "u1");
        // 22:30 UTC on 1 Aug is 01:30 on 2 Aug in Moscow (UTC+3).
        seed_meal(
            &mut conn,
            "late",
            "u1",
            at(2026, 8, 1, 22, 30),
            180,
            MealStatus::Confirmed,
            1,
            1.0,
            None,
        );
        seed_item(&mut conn, "late", 1, 700.0, 30.0, 20.0, 70.0);

        let (aug1, _) = consumed_for_day(&mut conn, "u1", AUG_1, 180).unwrap();
        assert_eq!(aug1.kcal, 0.0, "it is already tomorrow in Moscow");

        let aug2 = NaiveDate::from_ymd_opt(2026, 8, 2).unwrap();
        let (aug2_totals, count) = consumed_for_day(&mut conn, "u1", aug2, 180).unwrap();
        assert_eq!(aug2_totals.kcal, 700.0);
        assert_eq!(count, 1);

        // Bucketed by UTC the same row lands on 1 August instead.
        let (utc_aug1, _) = consumed_for_day(&mut conn, "u1", AUG_1, 0).unwrap();
        assert_eq!(utc_aug1.kcal, 700.0);
    }

    #[test]
    fn a_western_offset_pulls_the_next_utc_day_back() {
        let mut conn = test_conn();
        seed_user(&mut conn, "u1");
        // 03:00 UTC on 2 Aug is 19:00 on 1 Aug in Los Angeles (UTC−8).
        seed_meal(
            &mut conn,
            "dinner",
            "u1",
            at(2026, 8, 2, 3, 0),
            -480,
            MealStatus::Confirmed,
            1,
            1.0,
            None,
        );
        seed_item(&mut conn, "dinner", 1, 800.0, 40.0, 25.0, 80.0);

        let (aug1, count) = consumed_for_day(&mut conn, "u1", AUG_1, -480).unwrap();
        assert_eq!(aug1.kcal, 800.0);
        assert_eq!(count, 1);
    }

    #[test]
    fn the_day_boundary_is_half_open() {
        let mut conn = test_conn();
        seed_user(&mut conn, "u1");
        // Exactly local midnight opening 2 August in UTC+3 = 21:00 UTC on the 1st.
        seed_meal(
            &mut conn,
            "midnight",
            "u1",
            at(2026, 8, 1, 21, 0),
            180,
            MealStatus::Confirmed,
            1,
            1.0,
            None,
        );
        seed_item(&mut conn, "midnight", 1, 100.0, 1.0, 1.0, 1.0);

        let aug2 = NaiveDate::from_ymd_opt(2026, 8, 2).unwrap();
        assert_eq!(consumed_for_day(&mut conn, "u1", AUG_1, 180).unwrap().0.kcal, 0.0);
        assert_eq!(consumed_for_day(&mut conn, "u1", aug2, 180).unwrap().0.kcal, 100.0);
    }

    #[test]
    fn a_dst_transition_keeps_each_meal_on_the_day_it_was_eaten() {
        // Europe/Berlin leaves summer time at 01:00 UTC on 25 October 2026:
        // meals before it carry +120, meals after carry +60. Both bucket by the
        // offset the client sent with them, which is exactly why the offset is
        // stored per meal rather than per user.
        let mut conn = test_conn();
        seed_user(&mut conn, "u1");

        // 23:30 UTC on 24 Oct = 01:30 local on 25 Oct at +120.
        seed_meal(
            &mut conn,
            "before",
            "u1",
            at(2026, 10, 24, 23, 30),
            120,
            MealStatus::Confirmed,
            1,
            1.0,
            None,
        );
        seed_item(&mut conn, "before", 1, 200.0, 10.0, 5.0, 20.0);

        // 12:00 UTC on 25 Oct = 13:00 local on 25 Oct at +60.
        seed_meal(
            &mut conn,
            "after",
            "u1",
            at(2026, 10, 25, 12, 0),
            60,
            MealStatus::Confirmed,
            1,
            1.0,
            None,
        );
        seed_item(&mut conn, "after", 1, 600.0, 30.0, 15.0, 60.0);

        let oct25 = NaiveDate::from_ymd_opt(2026, 10, 25).unwrap();
        assert_eq!(
            local_date_of(at(2026, 10, 24, 23, 30), 120),
            oct25,
            "the pre-switch meal is already 25 October locally"
        );
        assert_eq!(local_date_of(at(2026, 10, 25, 12, 0), 60), oct25);

        // Bucketed at the post-switch offset the whole day is visible: the
        // window [24 Oct 23:00, 25 Oct 23:00) UTC covers both meals.
        let (totals, count) = consumed_for_day(&mut conn, "u1", oct25, 60).unwrap();
        assert_eq!(totals.kcal, 800.0);
        assert_eq!(count, 2);

        // Bucketed at the pre-switch offset the window is [24 Oct 22:00,
        // 25 Oct 22:00) UTC — it still covers both, because the 24h window
        // slides rather than stretching. This is the documented behaviour: the
        // one hour a fall-back day gains is not modelled.
        let (totals, count) = consumed_for_day(&mut conn, "u1", oct25, 120).unwrap();
        assert_eq!(totals.kcal, 800.0);
        assert_eq!(count, 2);
    }

    #[test]
    fn a_spring_forward_day_loses_no_meals() {
        // Europe/Berlin enters summer time at 01:00 UTC on 29 March 2026.
        let mut conn = test_conn();
        seed_user(&mut conn, "u1");
        let mar29 = NaiveDate::from_ymd_opt(2026, 3, 29).unwrap();

        // 00:30 UTC = 01:30 local at +60, before the jump.
        seed_meal(
            &mut conn,
            "early",
            "u1",
            at(2026, 3, 29, 0, 30),
            60,
            MealStatus::Confirmed,
            1,
            1.0,
            None,
        );
        seed_item(&mut conn, "early", 1, 150.0, 5.0, 5.0, 15.0);
        // 20:00 UTC = 22:00 local at +120, after the jump.
        seed_meal(
            &mut conn,
            "late",
            "u1",
            at(2026, 3, 29, 20, 0),
            120,
            MealStatus::Confirmed,
            1,
            1.0,
            None,
        );
        seed_item(&mut conn, "late", 1, 850.0, 45.0, 20.0, 85.0);

        assert_eq!(local_date_of(at(2026, 3, 29, 0, 30), 60), mar29);
        assert_eq!(local_date_of(at(2026, 3, 29, 20, 0), 120), mar29);

        let (totals, count) = consumed_for_day(&mut conn, "u1", mar29, 120).unwrap();
        assert_eq!(totals.kcal, 1000.0);
        assert_eq!(count, 2);
    }

    #[test]
    fn a_day_state_carries_the_targets_and_the_classification() {
        let mut conn = test_conn();
        seed_user(&mut conn, "u1");
        seed_profile(&mut conn, "u1", 2050.0, 165.0);
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
        seed_item(&mut conn, "m1", 1, 1450.0, 82.0, 50.0, 120.0);

        let day = compute_day_state(&mut conn, "u1", AUG_1, 0).unwrap();
        assert_eq!(day.consumed_kcal, 1450.0);
        assert_eq!(day.target_kcal, 2050.0);
        assert_eq!(day.remaining_kcal, 600.0);
        assert_eq!(day.remaining_protein_g, 83.0);
        assert_eq!(day.meals_logged, 1);
        assert_eq!(day.status, DayStatus::OnTrack);
    }

    #[test]
    fn a_user_without_targets_gets_no_targets_not_a_negative_budget() {
        let mut conn = test_conn();
        seed_user(&mut conn, "u1");
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
        seed_item(&mut conn, "m1", 1, 1450.0, 82.0, 50.0, 120.0);

        let day = compute_day_state(&mut conn, "u1", AUG_1, 0).unwrap();
        assert_eq!(day.status, DayStatus::NoTargets);
        assert_eq!(day.consumed_kcal, 1450.0);
        assert_eq!(day.remaining_kcal, 0.0);
        assert_eq!(day.remaining_protein_g, 0.0);
    }

    #[test]
    fn an_empty_day_is_all_zeroes_against_real_targets() {
        let mut conn = test_conn();
        seed_user(&mut conn, "u1");
        seed_profile(&mut conn, "u1", 2050.0, 165.0);

        let day = compute_day_state(&mut conn, "u1", AUG_1, 0).unwrap();
        assert_eq!(day.consumed_kcal, 0.0);
        assert_eq!(day.remaining_kcal, 2050.0);
        assert_eq!(day.remaining_protein_g, 165.0);
        assert_eq!(day.meals_logged, 0);
        // 165g of protein needs 660 kcal and 2,050 are free — reachable.
        assert_eq!(day.status, DayStatus::OnTrack);
    }

    #[test]
    fn overshooting_the_target_reads_as_over() {
        let mut conn = test_conn();
        seed_user(&mut conn, "u1");
        seed_profile(&mut conn, "u1", 2050.0, 165.0);
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
        seed_item(&mut conn, "m1", 1, 2600.0, 100.0, 90.0, 300.0);

        let day = compute_day_state(&mut conn, "u1", AUG_1, 0).unwrap();
        assert_eq!(day.status, DayStatus::Over);
        assert_eq!(day.remaining_kcal, -550.0);
    }
}
