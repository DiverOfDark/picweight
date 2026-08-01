//! Notification-group settling (PRD §5, §6).
//!
//! A sitting fires **exactly one** notification, not one per photo. A group is
//! settled when:
//!
//! * it has no in-flight jobs, **and**
//! * either the client's `group_size` hint is satisfied — every one of the N
//!   expected meals is terminal, which makes the common case instant — or no
//!   new photo has arrived for [`DEBOUNCE`].
//!
//! The debounce is the fallback for a photo that never arrives, so a group can
//! never hang un-notified. PRD §14.4 warns that WorkManager may deliver photos
//! out of order, so **the hint can arrive before its members**: `expected_size`
//! is stored on the group row rather than inferred from what has landed, and
//! settling re-checks the member count each tick.
//!
//! A *failed* member does not block settling — four dishes still land.

use crate::api::events::{MealEvent, MealEventKind};
use crate::error::AppError;
use crate::models::{MealStatus, NewNotificationGroup, NotificationGroup};
use crate::AppState;
use chrono::NaiveDateTime;
use diesel::prelude::*;
use diesel::SqliteConnection;
use std::time::Duration;

/// Idle window after the last photo before a group settles on its own.
pub const DEBOUNCE: Duration = Duration::from_secs(90);

/// How often the settler wakes to look for settleable groups.
pub const TICK_INTERVAL: Duration = Duration::from_secs(10);

/// Spawn the settler loop.
///
/// Errors are logged, never propagated: a failed tick must not take the settler
/// down, or every subsequent sitting would hang un-notified.
pub fn spawn(state: AppState) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let mut ticker = tokio::time::interval(TICK_INTERVAL);
        // The default `Burst` behaviour would try to catch up after a long tick
        // and fire several passes back to back; skipping is what we want.
        ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        tracing::info!(
            debounce_secs = DEBOUNCE.as_secs(),
            tick_secs = TICK_INTERVAL.as_secs(),
            "notification group settler started"
        );
        loop {
            ticker.tick().await;
            match tick(&state).await {
                Ok(0) => {}
                Ok(n) => tracing::debug!("settled {n} notification group(s)"),
                Err(err) => tracing::error!(error = %err, "group settler tick failed"),
            }
        }
    })
}

/// One pass over un-notified groups. Returns how many settled.
pub async fn tick(state: &AppState) -> Result<usize, AppError> {
    let now = chrono::Utc::now().naive_utc();

    let pending: Vec<String> = state
        .interact(move |conn| {
            use crate::schema::notification_groups::dsl;
            dsl::notification_groups
                .filter(dsl::notified_at.is_null())
                .order(dsl::last_photo_at.asc())
                .select(dsl::group_id)
                .load::<String>(conn)
                .map_err(AppError::from)
        })
        .await?;

    let mut settled = 0usize;
    for group_id in pending {
        let ready = {
            let group_id = group_id.clone();
            state
                .interact(move |conn| is_settled(conn, &group_id, now, DEBOUNCE))
                .await?
        };
        if !ready {
            continue;
        }
        // One bad group must not stop the rest of the pass.
        match settle_group(state, &group_id).await {
            Ok(()) => settled += 1,
            Err(err) => tracing::error!(%group_id, error = %err, "could not settle sitting"),
        }
    }
    Ok(settled)
}

/// Record that a photo joined a sitting.
///
/// Creates the group row on first sight and refreshes `last_photo_at`, which is
/// what restarts the debounce. `expected_size` is only ever set, never cleared,
/// so a hint that arrives with the final shot survives a later out-of-order
/// upload.
///
/// Both halves of PRD §14.4 are handled here: a hint arriving **before** its
/// members creates the row with `expected_size` already set, and a hint arriving
/// **after** them fills the column in without disturbing anything else.
pub fn touch_group(
    conn: &mut SqliteConnection,
    user_id: &str,
    group_id: &str,
    expected_size: Option<i32>,
    at: NaiveDateTime,
) -> Result<(), AppError> {
    use crate::schema::notification_groups::dsl;

    let existing = load_group(conn, group_id)?;

    let Some(existing) = existing else {
        diesel::insert_into(dsl::notification_groups)
            .values(&NewNotificationGroup {
                group_id: group_id.to_string(),
                user_id: user_id.to_string(),
                expected_size: expected_size.filter(|n| *n > 0),
                notified_at: None,
                last_photo_at: at,
                created_at: at,
            })
            .execute(conn)?;
        return Ok(());
    };

    if existing.user_id != user_id {
        // Group ids are client-generated UUIDs; a collision across users would
        // otherwise merge two people's sittings.
        return Err(AppError::Forbidden(format!(
            "sitting {group_id} belongs to another user"
        )));
    }

    // Out-of-order delivery means "the photo that just arrived" is not
    // necessarily the newest one; only ever push the debounce forward.
    let last_photo_at = existing.last_photo_at.max(at);
    // Never clear a hint: the final shot carries it, and WorkManager may deliver
    // that shot before the others.
    let expected_size = expected_size.filter(|n| *n > 0).or(existing.expected_size);

    diesel::update(dsl::notification_groups.filter(dsl::group_id.eq(group_id)))
        .set((
            dsl::last_photo_at.eq(last_photo_at),
            dsl::expected_size.eq(expected_size),
        ))
        .execute(conn)?;
    Ok(())
}

/// Load one group row.
pub fn load_group(
    conn: &mut SqliteConnection,
    group_id: &str,
) -> Result<Option<NotificationGroup>, AppError> {
    use crate::schema::notification_groups::dsl;
    dsl::notification_groups
        .filter(dsl::group_id.eq(group_id))
        .select(NotificationGroup::as_select())
        .first::<NotificationGroup>(conn)
        .optional()
        .map_err(AppError::from)
}

/// How many members of a sitting are still in flight, and how many are terminal.
pub fn member_counts(
    conn: &mut SqliteConnection,
    group_id: &str,
) -> Result<(usize, usize), AppError> {
    use crate::schema::meals::dsl;

    let statuses: Vec<String> = dsl::meals
        .filter(dsl::group_id.eq(group_id))
        .select(dsl::status)
        .load::<String>(conn)?;

    let mut in_flight = 0usize;
    let mut terminal = 0usize;
    for status in statuses {
        match MealStatus::from_str(&status)? {
            MealStatus::Pending | MealStatus::Analyzing => in_flight += 1,
            // `failed` counts as terminal on purpose: one timed-out analysis
            // must not keep the other four dishes from being announced.
            MealStatus::NeedsReview | MealStatus::Confirmed | MealStatus::Failed => terminal += 1,
        }
    }
    Ok((in_flight, terminal))
}

/// Whether a group is ready to notify.
pub fn is_settled(
    conn: &mut SqliteConnection,
    group_id: &str,
    now: NaiveDateTime,
    debounce: Duration,
) -> Result<bool, AppError> {
    let Some(group) = load_group(conn, group_id)? else {
        return Ok(false);
    };
    if group.notified_at.is_some() {
        return Ok(false);
    }

    let (in_flight, terminal) = member_counts(conn, group_id)?;
    let idle = (now - group.last_photo_at)
        .to_std()
        .unwrap_or(Duration::ZERO);

    Ok(settles_with(
        in_flight,
        terminal,
        group.expected_size,
        idle,
        debounce,
    ))
}

/// Fire the single notification for a settled group and stamp `notified_at`.
///
/// Stamping is what makes this idempotent: a group notifies once, ever. The
/// stamp is claimed *before* the notification is built, with a conditional
/// update, so two concurrent ticks cannot both win — and a failure while
/// building the copy costs the wording, never a duplicate notification.
pub async fn settle_group(state: &AppState, group_id: &str) -> Result<(), AppError> {
    let now = chrono::Utc::now().naive_utc();

    let claim = {
        let group_id = group_id.to_string();
        state
            .interact(move |conn| {
                use crate::schema::notification_groups::dsl;
                let claimed = diesel::update(
                    dsl::notification_groups
                        .filter(dsl::group_id.eq(&group_id))
                        .filter(dsl::notified_at.is_null()),
                )
                .set(dsl::notified_at.eq(now))
                .execute(conn)?;
                if claimed == 0 {
                    return Ok(None);
                }
                load_group(conn, &group_id)
            })
            .await?
    };

    let Some(group) = claim else {
        // Somebody else already notified this sitting.
        return Ok(());
    };

    let mut event = MealEvent::new(&group.user_id, MealEventKind::GroupSettled).with_group(group_id);

    match crate::feedback::build_group_feedback(state, &group.user_id, group_id).await {
        Ok(feedback) => {
            event.totals = Some(feedback.day.consumed());
            event.headline = Some(feedback.headline.clone());
            event.body = Some(feedback.body());
            event.day = Some(feedback.day);
        }
        Err(err) => {
            // The stamp is already claimed, so there is no second chance: emit
            // the bare event rather than swallowing the sitting entirely.
            tracing::error!(%group_id, error = %err, "settled a sitting without feedback copy");
            event.error = Some(err.to_string());
        }
    }

    state.events.publish(event);
    tracing::info!(%group_id, "sitting settled; one notification emitted");
    Ok(())
}

/// Pure settle predicate, extracted so the timing rules are testable without a
/// database.
///
/// * `in_flight` — members still `pending` or `analyzing`.
/// * `terminal` — members that reached `needs_review`, `confirmed` or `failed`.
/// * `expected` — the client's `group_size` hint, if any.
/// * `idle` — time since the last photo joined.
pub fn settles(in_flight: usize, terminal: usize, expected: Option<i32>, idle: Duration) -> bool {
    settles_with(in_flight, terminal, expected, idle, DEBOUNCE)
}

/// [`settles`] with an explicit debounce window, so tests (and a future config
/// knob — PRD §14.4 expects the 90s default to be revisited) can vary it.
pub fn settles_with(
    in_flight: usize,
    terminal: usize,
    expected: Option<i32>,
    idle: Duration,
    debounce: Duration,
) -> bool {
    if in_flight > 0 {
        return false;
    }
    if terminal == 0 {
        // The hint arrived before any member did (§14.4). Wait for the members.
        return false;
    }
    match expected {
        Some(n) if terminal >= n.max(0) as usize => true,
        _ => idle >= debounce,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::feedback::state::fixtures::*;

    #[test]
    fn in_flight_members_block_settling() {
        assert!(!settles(1, 2, Some(3), Duration::from_secs(600)));
    }

    #[test]
    fn the_size_hint_fires_immediately_once_all_are_terminal() {
        assert!(settles(0, 3, Some(3), Duration::from_secs(1)));
    }

    #[test]
    fn without_a_hint_the_debounce_carries_it() {
        assert!(!settles(0, 3, None, Duration::from_secs(30)));
        assert!(settles(0, 3, None, DEBOUNCE));
    }

    #[test]
    fn a_failed_member_still_counts_as_terminal() {
        // Three uploads, one failed: all three are terminal, so the hint fires.
        assert!(settles(0, 3, Some(3), Duration::from_secs(1)));
    }

    #[test]
    fn a_hint_arriving_before_its_members_does_not_settle_an_empty_group() {
        assert!(!settles(0, 0, Some(3), Duration::from_secs(600)));
    }

    #[test]
    fn an_unmet_hint_still_falls_back_to_the_debounce() {
        // The fifth photo never arrived. Four terminal dishes must not hang.
        assert!(!settles(0, 4, Some(5), Duration::from_secs(30)));
        assert!(settles(0, 4, Some(5), DEBOUNCE));
    }

    #[test]
    fn an_over_delivered_hint_settles() {
        // The client said three and sent four; the hint is a floor, not a limit.
        assert!(settles(0, 4, Some(3), Duration::from_secs(1)));
    }

    #[test]
    fn a_nonsense_hint_does_not_settle_an_empty_group() {
        assert!(!settles(0, 0, Some(0), Duration::from_secs(600)));
        assert!(!settles(0, 0, Some(-1), Duration::from_secs(600)));
        // With a member present, a zero hint is trivially met.
        assert!(settles(0, 1, Some(0), Duration::from_secs(1)));
    }

    // -----------------------------------------------------------------------
    // Database-backed settling
    // -----------------------------------------------------------------------

    /// Seed a sitting whose members have the given statuses.
    fn sitting(
        conn: &mut SqliteConnection,
        group_id: &str,
        expected: Option<i32>,
        last_photo_at: NaiveDateTime,
        statuses: &[MealStatus],
    ) {
        seed_user(conn, "u1");
        seed_group(conn, group_id, "u1", expected, last_photo_at);
        for (i, status) in statuses.iter().enumerate() {
            seed_meal(
                conn,
                &format!("{group_id}-{i}"),
                "u1",
                last_photo_at,
                0,
                *status,
                1,
                1.0,
                Some(group_id),
            );
        }
    }

    #[test]
    fn a_group_with_an_in_flight_member_is_not_settled() {
        let mut conn = test_conn();
        let t = at(2026, 8, 1, 12, 0);
        sitting(
            &mut conn,
            "g1",
            Some(3),
            t,
            &[MealStatus::NeedsReview, MealStatus::Analyzing],
        );
        let much_later = t + chrono::Duration::hours(1);
        assert!(!is_settled(&mut conn, "g1", much_later, DEBOUNCE).unwrap());
    }

    #[test]
    fn the_hint_settles_the_group_the_moment_the_last_member_is_terminal() {
        let mut conn = test_conn();
        let t = at(2026, 8, 1, 12, 0);
        sitting(
            &mut conn,
            "g1",
            Some(3),
            t,
            &[
                MealStatus::NeedsReview,
                MealStatus::NeedsReview,
                MealStatus::Failed,
            ],
        );
        // One second after the last photo — nowhere near the debounce.
        let now = t + chrono::Duration::seconds(1);
        assert!(is_settled(&mut conn, "g1", now, DEBOUNCE).unwrap());
    }

    #[test]
    fn a_group_without_a_hint_waits_out_the_debounce() {
        let mut conn = test_conn();
        let t = at(2026, 8, 1, 12, 0);
        sitting(
            &mut conn,
            "g1",
            None,
            t,
            &[MealStatus::NeedsReview, MealStatus::Confirmed],
        );
        assert!(!is_settled(&mut conn, "g1", t + chrono::Duration::seconds(30), DEBOUNCE).unwrap());
        assert!(is_settled(&mut conn, "g1", t + chrono::Duration::seconds(90), DEBOUNCE).unwrap());
    }

    #[test]
    fn a_hint_that_lands_before_its_members_does_not_settle_an_empty_group() {
        // §14.4: WorkManager may deliver the final shot — the one carrying
        // `group_size` — before the photos it is counting.
        let mut conn = test_conn();
        let t = at(2026, 8, 1, 12, 0);
        seed_user(&mut conn, "u1");
        seed_group(&mut conn, "g1", "u1", Some(3), t);

        let long_after = t + chrono::Duration::hours(2);
        assert!(
            !is_settled(&mut conn, "g1", long_after, DEBOUNCE).unwrap(),
            "an empty group must never notify, debounce or not"
        );

        // The members then arrive and finish; the stored hint fires at once.
        for i in 0..3 {
            seed_meal(
                &mut conn,
                &format!("m{i}"),
                "u1",
                t,
                0,
                MealStatus::NeedsReview,
                1,
                1.0,
                Some("g1"),
            );
        }
        assert!(is_settled(&mut conn, "g1", t + chrono::Duration::seconds(1), DEBOUNCE).unwrap());
    }

    #[test]
    fn touching_a_group_creates_it_then_only_moves_the_clock_forward() {
        let mut conn = test_conn();
        seed_user(&mut conn, "u1");
        let first = at(2026, 8, 1, 12, 0);
        let later = at(2026, 8, 1, 12, 1);

        touch_group(&mut conn, "u1", "g1", None, first).unwrap();
        let group = load_group(&mut conn, "g1").unwrap().unwrap();
        assert_eq!(group.last_photo_at, first);
        assert_eq!(group.expected_size, None);
        assert_eq!(group.notified_at, None);

        touch_group(&mut conn, "u1", "g1", None, later).unwrap();
        assert_eq!(load_group(&mut conn, "g1").unwrap().unwrap().last_photo_at, later);

        // An out-of-order upload from earlier in the sitting must not rewind the
        // debounce window.
        touch_group(&mut conn, "u1", "g1", None, first).unwrap();
        assert_eq!(load_group(&mut conn, "g1").unwrap().unwrap().last_photo_at, later);
    }

    #[test]
    fn the_hint_is_set_once_and_never_cleared() {
        let mut conn = test_conn();
        seed_user(&mut conn, "u1");
        let t = at(2026, 8, 1, 12, 0);

        // The final shot lands first, carrying the hint (§14.4).
        touch_group(&mut conn, "u1", "g1", Some(3), t).unwrap();
        assert_eq!(load_group(&mut conn, "g1").unwrap().unwrap().expected_size, Some(3));

        // The earlier shots arrive without one; the hint survives.
        touch_group(&mut conn, "u1", "g1", None, t).unwrap();
        assert_eq!(load_group(&mut conn, "g1").unwrap().unwrap().expected_size, Some(3));

        // A nonsensical hint is ignored rather than stored.
        touch_group(&mut conn, "u1", "g1", Some(0), t).unwrap();
        assert_eq!(load_group(&mut conn, "g1").unwrap().unwrap().expected_size, Some(3));
    }

    #[test]
    fn the_hint_can_arrive_after_the_group_exists() {
        let mut conn = test_conn();
        seed_user(&mut conn, "u1");
        let t = at(2026, 8, 1, 12, 0);
        touch_group(&mut conn, "u1", "g1", None, t).unwrap();
        touch_group(&mut conn, "u1", "g1", Some(2), t).unwrap();
        assert_eq!(load_group(&mut conn, "g1").unwrap().unwrap().expected_size, Some(2));
    }

    #[test]
    fn a_group_id_collision_across_users_is_refused() {
        let mut conn = test_conn();
        seed_user(&mut conn, "u1");
        seed_user(&mut conn, "u2");
        let t = at(2026, 8, 1, 12, 0);
        touch_group(&mut conn, "u1", "g1", None, t).unwrap();
        let err = touch_group(&mut conn, "u2", "g1", None, t).unwrap_err();
        assert_eq!(err.status(), axum::http::StatusCode::FORBIDDEN);
    }

    #[test]
    fn an_already_notified_group_never_settles_again() {
        use crate::schema::notification_groups::dsl;
        let mut conn = test_conn();
        let t = at(2026, 8, 1, 12, 0);
        sitting(&mut conn, "g1", Some(1), t, &[MealStatus::NeedsReview]);

        assert!(is_settled(&mut conn, "g1", t, DEBOUNCE).unwrap());
        diesel::update(dsl::notification_groups.filter(dsl::group_id.eq("g1")))
            .set(dsl::notified_at.eq(Some(t)))
            .execute(&mut conn)
            .unwrap();
        assert!(!is_settled(&mut conn, "g1", t, DEBOUNCE).unwrap());
    }

    #[test]
    fn an_unknown_group_is_not_settled() {
        let mut conn = test_conn();
        assert!(!is_settled(&mut conn, "nope", at(2026, 8, 1, 12, 0), DEBOUNCE).unwrap());
    }

    #[test]
    fn a_solo_meal_is_just_a_sitting_of_one() {
        let mut conn = test_conn();
        let t = at(2026, 8, 1, 12, 0);
        sitting(&mut conn, "g1", Some(1), t, &[MealStatus::NeedsReview]);
        assert!(is_settled(&mut conn, "g1", t + chrono::Duration::seconds(1), DEBOUNCE).unwrap());
    }
}
