//! The analysis worker: one job, one agent loop, one persisted revision.
//!
//! Lifecycle of a job:
//!
//! 1. mark `analysis_jobs.status = running`, `meals.status = analyzing`,
//!    stamp `started_at`, emit an `Analyzing` event;
//! 2. assemble the [`crate::agent::AnalysisContext`] (thumbnail path, dish name,
//!    recent dishes, calibration factor);
//! 3. run [`crate::agent::AgentHandle::analyze`] — or, for a correction,
//!    [`crate::agent::reanalyze::reanalyze`];
//! 4. persist items at the new revision, write `agent_steps`, save the session;
//! 5. set `meals.status = needs_review`, emit a `Completed` event carrying the
//!    day state, and touch the notification group.
//!
//! Failure handling is deliberately loud. A retryable error ([`AppError::is_retryable`])
//! is re-queued up to [`MAX_ATTEMPTS`]; anything else — and any retryable error
//! that exhausts its attempts — sets `meals.status = failed` with a visible
//! reason. A quota-exhausted key must never look like a stalled upload (§5).

use crate::agent::{AgentOutcome, AnalysisContext};
use crate::api::events::{MealEvent, MealEventKind};
use crate::error::AppError;
use crate::jobs::{Job, JobKind};
use crate::models::{
    AgentSessionChangeset, AnalysisJobChangeset, JobStatus, Meal, MealChangeset, MealStatus,
    NewAgentStep, NewMealItem,
};
use crate::AppState;
use diesel::prelude::*;
use diesel::SqliteConnection;

/// How many times a retryable failure is retried before the meal is failed.
pub const MAX_ATTEMPTS: i32 = 3;

/// Delay before a retryable job is re-queued.
pub const RETRY_DELAY_SECS: u64 = 5;

/// How many of the user's recent confirmed dishes are handed to the agent as
/// context. Matches the capture screen's recent-dish chip row (§7).
pub const RECENT_DISHES: i64 = 8;

/// Longest tool output stored per `agent_steps` row.
///
/// A recall hit with many items serializes to several kilobytes; the row exists
/// to make a bad estimate debuggable, not to be a second copy of the data.
pub const MAX_STEP_PAYLOAD: usize = 8_000;

/// Run one job end to end.
///
/// Never panics and never propagates: every failure path ends with either a
/// re-queue or a `failed` meal plus a `Failed` event, because a dropped error
/// here is a meal the user silently loses.
pub async fn run_job(state: &AppState, job: Job) -> Result<(), AppError> {
    let started = std::time::Instant::now();

    // ---- 1. claim the job -------------------------------------------------
    let claimed = {
        let state2 = state.clone();
        let job2 = job.clone();
        state
            .interact(move |conn| {
                // Read before flipping: a *correction* that fails must put the
                // meal back where it was, not throw the good revision away.
                let prepared = prepare(conn, &state2, &job2)?;
                mark_running(conn, &job2)?;
                let attempts = job_attempts(conn, &job2.job_id)?;
                Ok((attempts, prepared))
            })
            .await
    };
    let (attempts, prepared) = match claimed {
        Ok(claimed) => claimed,
        // A meal that cannot even be set up must fail visibly. Left as
        // `analyzing` it would stall the UI forever *and* keep its sitting from
        // ever settling, since an in-flight member blocks the group (§5).
        Err(err) => return abandon(state, &job, None, err).await,
    };

    state.events.publish(
        event(&prepared, MealEventKind::Analyzing, &job).with_status(MealStatus::Analyzing),
    );

    // ---- 2. run the agent -------------------------------------------------
    let outcome = match &job.kind {
        JobKind::Analyze => state.agent.analyze(&prepared.ctx).await,
        JobKind::Reanalyze { feedback, .. } => {
            crate::agent::reanalyze::reanalyze(state, &prepared.ctx, feedback).await
        }
    };

    let outcome = match outcome {
        Ok(outcome) => outcome,
        Err(err) => return handle_failure(state, &job, &prepared, err, attempts).await,
    };

    // ---- 3. persist -------------------------------------------------------
    // One transaction: items, steps, session and the two status flips either all
    // land or none do, so a half-written revision can never be read back.
    let persisted = {
        let job2 = job.clone();
        let outcome2 = outcome.clone();
        state
            .interact(move |conn| {
                conn.transaction(|conn| {
                    persist_outcome(conn, &job2.meal_id, job2.revision, &outcome2)?;
                    persist_steps(conn, &job2.job_id, &outcome2)?;
                    save_session(conn, &job2.meal_id, &outcome2)?;
                    mark_succeeded(conn, &job2, &outcome2)
                })
            })
            .await
    };
    if let Err(err) = persisted {
        return abandon(state, &job, restore_target(&job, &prepared), err).await;
    }

    tracing::info!(
        job = %job.job_id,
        meal = %job.meal_id,
        revision = job.revision,
        elapsed_ms = started.elapsed().as_millis() as u64,
        model_calls = outcome.usage.model_calls,
        tool_calls = outcome.usage.tool_calls,
        fallback = outcome.fallback_used,
        "analysis finished"
    );

    // ---- 4. announce ------------------------------------------------------
    let kind = match job.kind {
        JobKind::Analyze => MealEventKind::Completed,
        JobKind::Reanalyze { .. } => MealEventKind::Reanalyzed,
    };
    let mut completed = event(&prepared, kind, &job).with_status(MealStatus::NeedsReview);
    completed.totals = Some(outcome.estimate.totals());
    // The stored name (chip, share intent or comment) outranks the agent's
    // visual read, exactly as `persist_outcome` decided a moment ago.
    completed.dish_name = non_empty(prepared.dish_name.clone())
        .or_else(|| non_empty(Some(outcome.estimate.dish_name.clone())));

    match crate::feedback::build_meal_feedback(state, &prepared.user_id, &job.meal_id).await {
        Ok(feedback) => {
            completed.day = Some(feedback.day);
            // One notification per sitting, not per photo (§5): a grouped meal
            // updates the UI, but the notification *copy* is emitted once, by
            // the settler, when the whole sitting is done.
            if prepared.group_id.is_none() {
                completed.body = Some(feedback.body());
                completed.headline = Some(feedback.headline);
            }
        }
        Err(err) => {
            // The estimate is already durable; failing the meal over
            // notification copy would be exactly backwards.
            tracing::warn!(meal = %job.meal_id, error = %err, "could not build meal feedback");
        }
    }
    state.events.publish(completed);

    Ok(())
}

/// Discard blank strings so an empty dish name never reaches a notification.
fn non_empty(value: Option<String>) -> Option<String> {
    value.filter(|text| !text.trim().is_empty())
}

/// Where a failed job should leave its meal.
///
/// A first analysis has nothing to lose, so it goes to `failed`. A **correction**
/// does: the meal already carries a revision the user has seen, and destroying it
/// because a follow-up turn timed out would take those calories out of the day
/// (`consumed_for_day` skips `failed` meals). The correction fails; the meal
/// returns to the status it had.
pub fn restore_target(job: &Job, prepared: &Prepared) -> Option<MealStatus> {
    match job.kind {
        JobKind::Analyze => None,
        JobKind::Reanalyze { .. } => match prepared.previous_status {
            MealStatus::NeedsReview | MealStatus::Confirmed => Some(prepared.previous_status),
            _ => None,
        },
    }
}

/// Fail a job that broke outside the agent run — before its context could be
/// assembled, or while its result was being written.
///
/// Never retried: both failures are bookkeeping problems, and retrying a
/// database that just refused a write would only stall the sitting further.
async fn abandon(
    state: &AppState,
    job: &Job,
    restore: Option<MealStatus>,
    err: AppError,
) -> Result<(), AppError> {
    let reason = err.to_string();
    tracing::error!(
        job = %job.job_id,
        meal = %job.meal_id,
        error = %reason,
        "analysis job could not be set up or recorded; failing the meal"
    );

    {
        let job2 = job.clone();
        let reason = reason.clone();
        // Best effort: if this fails too, startup recovery re-queues the meal.
        if let Err(e) = state
            .interact(move |conn| mark_failed_restoring(conn, &job2, &reason, restore))
            .await
        {
            tracing::error!(job = %job.job_id, error = %e, "could not even mark the job failed");
        }
    }

    let mut failed =
        MealEvent::new(&job.user_id, MealEventKind::Failed).with_meal(&job.meal_id, job.revision);
    failed.status = Some(restore.unwrap_or(MealStatus::Failed));
    failed.error = Some(reason);
    state.events.publish(failed);
    Ok(())
}

/// Retry or fail, loudly, and always emit something the user can see.
async fn handle_failure(
    state: &AppState,
    job: &Job,
    prepared: &Prepared,
    err: AppError,
    attempts: i32,
) -> Result<(), AppError> {
    let reason = err.to_string();

    if should_retry(&err, attempts) {
        let delay = std::time::Duration::from_secs(RETRY_DELAY_SECS * attempts.max(1) as u64);
        tracing::warn!(
            job = %job.job_id,
            attempt = attempts,
            retry_in_secs = delay.as_secs(),
            error = %reason,
            "analysis failed; retrying"
        );
        {
            let job2 = job.clone();
            let reason = reason.clone();
            state
                .interact(move |conn| mark_requeued(conn, &job2, &reason))
                .await?;
        }
        // Detached so the retry delay does not hold a worker slot.
        let queue = state.jobs.clone();
        let job2 = job.clone();
        tokio::spawn(async move {
            tokio::time::sleep(delay).await;
            if let Err(e) = queue.enqueue(job2) {
                tracing::error!(error = %e, "could not re-queue a retryable analysis job");
            }
        });
        return Ok(());
    }

    // Terminal. PRD §5: a quota-exhausted key goes to `failed` with a visible
    // reason; it must never look like a stalled upload.
    let restore = restore_target(job, prepared);
    tracing::error!(
        job = %job.job_id,
        meal = %job.meal_id,
        attempt = attempts,
        error = %reason,
        restored = ?restore,
        "analysis failed permanently"
    );
    {
        let job2 = job.clone();
        let reason = reason.clone();
        state
            .interact(move |conn| mark_failed_restoring(conn, &job2, &reason, restore))
            .await?;
    }

    let mut failed = event(prepared, MealEventKind::Failed, job)
        .with_status(restore.unwrap_or(MealStatus::Failed));
    failed.error = Some(reason);
    failed.dish_name = prepared.dish_name.clone();
    state.events.publish(failed);
    Ok(())
}

/// Everything [`run_job`] needs that is not in the [`Job`] itself.
#[derive(Debug, Clone)]
pub struct Prepared {
    /// The agent's view of the meal.
    pub ctx: AnalysisContext,
    /// Owning user, for event routing.
    pub user_id: String,
    /// The sitting this meal belongs to, if any.
    pub group_id: Option<String>,
    /// Dish name as stored, for the event payload.
    pub dish_name: Option<String>,
    /// The meal's status *before* this job claimed it, so a failed correction
    /// can put it back (see [`restore_target`]).
    pub previous_status: MealStatus,
}

/// Load the meal and assemble both the agent context and the event metadata.
///
/// Must run **before** [`mark_running`], which is what captures the meal's
/// pre-job status.
fn prepare(conn: &mut SqliteConnection, state: &AppState, job: &Job) -> Result<Prepared, AppError> {
    let meal = load_meal(conn, &job.meal_id)?;
    let previous_status = meal.status()?;
    let ctx = context_for(conn, state, job, &meal)?;
    Ok(Prepared {
        user_id: meal.user_id.clone(),
        group_id: meal.group_id.clone(),
        dish_name: meal.dish_name.clone(),
        previous_status,
        ctx,
    })
}

/// Build the event skeleton shared by every emission for one job.
fn event(prepared: &Prepared, kind: MealEventKind, job: &Job) -> MealEvent {
    let event = MealEvent::new(&prepared.user_id, kind).with_meal(&job.meal_id, job.revision);
    match &prepared.group_id {
        Some(group_id) => event.with_group(group_id),
        None => event,
    }
}

/// Small extension so the event-building above reads as one chain.
trait WithStatus {
    fn with_status(self, status: MealStatus) -> Self;
}

impl WithStatus for MealEvent {
    fn with_status(mut self, status: MealStatus) -> Self {
        self.status = Some(status);
        self
    }
}

/// Assemble everything the agent needs for a meal.
pub fn build_context(
    conn: &mut SqliteConnection,
    state: &AppState,
    job: &Job,
) -> Result<AnalysisContext, AppError> {
    let meal = load_meal(conn, &job.meal_id)?;
    context_for(conn, state, job, &meal)
}

/// [`build_context`] with the meal row already in hand.
fn context_for(
    conn: &mut SqliteConnection,
    state: &AppState,
    job: &Job,
    meal: &Meal,
) -> Result<AnalysisContext, AppError> {
    // Images are re-attached from disk on every run (§5 caveats), so the worker
    // resolves the path rather than trusting anything in a serialized thread.
    let image_path = match &meal.thumbnail_id {
        None => None,
        Some(thumbnail_id) => match crate::storage::thumbs::find_by_id(conn, thumbnail_id)? {
            None => None,
            Some(thumbnail) => Some(crate::storage::thumbs::absolute_path(
                &state.config.thumbs_root(),
                &thumbnail.path,
            )?),
        },
    };

    Ok(AnalysisContext {
        user_id: meal.user_id.clone(),
        meal_id: meal.id.clone(),
        job_id: job.job_id.clone(),
        revision: job.revision,
        dish_name: meal.dish_name.clone(),
        user_comment: meal.user_comment.clone(),
        image_path,
        eaten_at: meal.eaten_at,
        recent_dishes: recent_confirmed_dishes(conn, &meal.user_id, RECENT_DISHES)?,
        calibration_factor: calibration_factor(conn, &meal.user_id)?,
    })
}

/// Load a meal or report it missing.
pub fn load_meal(conn: &mut SqliteConnection, meal_id: &str) -> Result<Meal, AppError> {
    use crate::schema::meals::dsl;
    dsl::meals
        .filter(dsl::id.eq(meal_id))
        .select(Meal::as_select())
        .first::<Meal>(conn)
        .optional()?
        .ok_or_else(|| AppError::NotFound(format!("meal {meal_id}")))
}

/// The user's most recently confirmed dish names, newest first, de-duplicated.
pub fn recent_confirmed_dishes(
    conn: &mut SqliteConnection,
    user_id: &str,
    limit: i64,
) -> Result<Vec<String>, AppError> {
    use crate::schema::meals::dsl;

    // Over-fetch, then de-duplicate in Rust: SQLite's DISTINCT would fight the
    // ORDER BY, and the row count here is trivially small.
    let names: Vec<Option<String>> = dsl::meals
        .filter(dsl::user_id.eq(user_id))
        .filter(dsl::status.eq(MealStatus::Confirmed.as_str()))
        .filter(dsl::dish_name.is_not_null())
        .order(dsl::eaten_at.desc())
        .limit(limit * 4)
        .select(dsl::dish_name)
        .load::<Option<String>>(conn)?;

    let mut seen = std::collections::HashSet::new();
    let mut out = Vec::with_capacity(limit as usize);
    for name in names.into_iter().flatten() {
        let trimmed = name.trim();
        if trimmed.is_empty() {
            continue;
        }
        if seen.insert(crate::models::normalize_dish_name(trimmed)) {
            out.push(trimmed.to_string());
        }
        if out.len() >= limit as usize {
            break;
        }
    }
    Ok(out)
}

/// The user's learned calibration multiplier, defaulting to 1.0.
pub fn calibration_factor(conn: &mut SqliteConnection, user_id: &str) -> Result<f64, AppError> {
    use crate::schema::user_profiles::dsl;
    let factor: Option<f64> = dsl::user_profiles
        .filter(dsl::user_id.eq(user_id))
        .select(dsl::calibration_factor)
        .first::<f64>(conn)
        .optional()?;
    // A zero or negative factor would silently erase every estimate.
    Ok(factor.filter(|f| f.is_finite() && *f > 0.0).unwrap_or(1.0))
}

/// Persist an agent run as `meal_items` at `revision`.
///
/// Replaces any existing rows at that revision (a retry must be idempotent) and
/// leaves earlier revisions untouched.
pub fn persist_outcome(
    conn: &mut SqliteConnection,
    meal_id: &str,
    revision: i32,
    outcome: &AgentOutcome,
) -> Result<(), AppError> {
    use crate::schema::{meal_items, meals};

    diesel::delete(
        meal_items::table
            .filter(meal_items::meal_id.eq(meal_id))
            .filter(meal_items::revision.eq(revision)),
    )
    .execute(conn)?;

    let now = chrono::Utc::now().naive_utc();
    let rows: Vec<NewMealItem> = outcome
        .estimate
        .items
        .iter()
        .enumerate()
        .map(|(index, item)| NewMealItem {
            id: uuid::Uuid::new_v4().to_string(),
            meal_id: meal_id.to_string(),
            revision,
            position: index as i32,
            name: item.name.clone(),
            barcode: item.barcode.clone(),
            grams: sane(item.grams),
            grams_source: item.grams_source.as_str().to_string(),
            kcal: sane(item.kcal),
            protein_g: sane(item.protein_g),
            fat_g: sane(item.fat_g),
            carbs_g: sane(item.carbs_g),
            macro_source: item.macro_source.as_str().to_string(),
            confidence: Some(item.confidence.clamp(0.0, 1.0)),
            reasoning_note: Some(item.reasoning_note.clone()),
            created_at: now,
        })
        .collect();

    if !rows.is_empty() {
        diesel::insert_into(meal_items::table)
            .values(&rows)
            .execute(conn)?;
    }

    // The dish name the user supplied outranks the agent's visual read (§5), so
    // it is only filled in when the meal does not already have one.
    let meal = load_meal(conn, meal_id)?;
    let agent_name = outcome.estimate.dish_name.trim();
    let take_agent_name = meal
        .dish_name
        .as_deref()
        .map(|name| name.trim().is_empty())
        .unwrap_or(true)
        && !agent_name.is_empty();

    let changeset = MealChangeset {
        revision: Some(revision),
        updated_at: Some(now),
        dish_name: take_agent_name.then(|| Some(agent_name.to_string())),
        dish_name_normalized: take_agent_name
            .then(|| Some(crate::models::normalize_dish_name(agent_name))),
        ..Default::default()
    };
    diesel::update(meals::table.filter(meals::id.eq(meal_id)))
        .set(&changeset)
        .execute(conn)?;

    Ok(())
}

/// Replace non-finite and negative model output with zero.
///
/// A `NaN` from the model would poison every day total that ever touches it, and
/// SQLite would happily store it.
fn sane(value: f64) -> f64 {
    if value.is_finite() && value > 0.0 {
        value
    } else {
        0.0
    }
}

/// Write the run's tool calls to `agent_steps`.
pub fn persist_steps(
    conn: &mut SqliteConnection,
    job_id: &str,
    outcome: &AgentOutcome,
) -> Result<(), AppError> {
    use crate::schema::agent_steps;

    // Idempotent: a retry rewrites its own steps rather than colliding with the
    // previous attempt's `(job_id, step_no)` rows.
    diesel::delete(agent_steps::table.filter(agent_steps::job_id.eq(job_id))).execute(conn)?;

    if outcome.steps.is_empty() {
        return Ok(());
    }

    let now = chrono::Utc::now().naive_utc();
    let rows: Vec<NewAgentStep> = outcome
        .steps
        .iter()
        .map(|step| NewAgentStep {
            id: uuid::Uuid::new_v4().to_string(),
            job_id: job_id.to_string(),
            step_no: step.step_no,
            tool_name: step.tool_name.clone(),
            tool_input: step.tool_input.as_deref().map(truncate),
            tool_output: step.tool_output.as_deref().map(truncate),
            latency_ms: step.latency_ms,
            created_at: now,
        })
        .collect();

    diesel::insert_into(agent_steps::table)
        .values(&rows)
        .execute(conn)?;
    Ok(())
}

/// Cap a stored payload at [`MAX_STEP_PAYLOAD`], on a character boundary.
fn truncate(payload: &str) -> String {
    if payload.len() <= MAX_STEP_PAYLOAD {
        return payload.to_string();
    }
    let mut end = MAX_STEP_PAYLOAD;
    while end > 0 && !payload.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}… [truncated]", &payload[..end])
}

/// Upsert the meal's `agent_sessions` row from a finished run.
///
/// The thread arrives already serialized on [`AgentOutcome::serialized_messages`]
/// precisely so nothing outside `src/agent/` has to name a rig type.
pub fn save_session(
    conn: &mut SqliteConnection,
    meal_id: &str,
    outcome: &AgentOutcome,
) -> Result<(), AppError> {
    use crate::schema::agent_sessions::dsl;

    let Some(messages) = outcome.serialized_messages.clone() else {
        // A single-shot fallback has no resumable thread; leaving the previous
        // session in place is better than replacing it with nothing.
        return Ok(());
    };

    let changeset = AgentSessionChangeset {
        messages: Some(messages.clone()),
        model: Some(outcome.model.clone()),
        prompt_version: Some(outcome.prompt_version.clone()),
        turn_count: Some(outcome.turn_count),
        updated_at: Some(chrono::Utc::now().naive_utc()),
    };

    let updated = diesel::update(dsl::agent_sessions.filter(dsl::meal_id.eq(meal_id)))
        .set(&changeset)
        .execute(conn)?;
    if updated > 0 {
        return Ok(());
    }

    diesel::insert_into(dsl::agent_sessions)
        .values(&crate::agent::session::new_session_row(
            meal_id,
            messages,
            outcome.turn_count,
            &outcome.model,
            &outcome.prompt_version,
        ))
        .execute(conn)?;
    Ok(())
}

/// Mark a job running and its meal `analyzing`.
pub fn mark_running(conn: &mut SqliteConnection, job: &Job) -> Result<(), AppError> {
    use crate::schema::{analysis_jobs, meals};

    let now = chrono::Utc::now().naive_utc();
    diesel::update(analysis_jobs::table.filter(analysis_jobs::id.eq(&job.job_id)))
        .set((
            analysis_jobs::status.eq(JobStatus::Running.as_str()),
            analysis_jobs::started_at.eq(Some(now)),
            // `created_at → finished_at` includes queue time; `started_at` is
            // what §13's p95 wall clock is measured from.
            analysis_jobs::attempts.eq(analysis_jobs::attempts + 1),
            analysis_jobs::error.eq(None::<String>),
        ))
        .execute(conn)?;

    diesel::update(meals::table.filter(meals::id.eq(&job.meal_id)))
        .set((
            meals::status.eq(MealStatus::Analyzing.as_str()),
            meals::updated_at.eq(now),
        ))
        .execute(conn)?;
    Ok(())
}

/// Read a job's attempt counter.
pub fn job_attempts(conn: &mut SqliteConnection, job_id: &str) -> Result<i32, AppError> {
    use crate::schema::analysis_jobs::dsl;
    dsl::analysis_jobs
        .filter(dsl::id.eq(job_id))
        .select(dsl::attempts)
        .first::<i32>(conn)
        .optional()?
        .ok_or_else(|| AppError::NotFound(format!("analysis job {job_id}")))
}

/// Mark a job succeeded and its meal `needs_review`.
pub fn mark_succeeded(
    conn: &mut SqliteConnection,
    job: &Job,
    outcome: &AgentOutcome,
) -> Result<(), AppError> {
    use crate::schema::{analysis_jobs, meals};

    let now = chrono::Utc::now().naive_utc();
    let changeset = AnalysisJobChangeset {
        status: Some(JobStatus::Succeeded.as_str().to_string()),
        model: Some(outcome.model.clone()),
        // `steps` counts model calls — rig's `max_turns` unit, not tool calls.
        steps: Some(outcome.usage.model_calls),
        tool_calls: Some(outcome.usage.tool_calls),
        prompt_tokens: Some(outcome.usage.prompt_tokens),
        completion_tokens: Some(outcome.usage.completion_tokens),
        cost_micro_usd: Some(estimate_cost_micro_usd(
            &outcome.model,
            outcome.usage.prompt_tokens,
            outcome.usage.completion_tokens,
        )),
        error: Some(None),
        finished_at: Some(Some(now)),
        ..Default::default()
    };
    diesel::update(analysis_jobs::table.filter(analysis_jobs::id.eq(&job.job_id)))
        .set(&changeset)
        .execute(conn)?;

    diesel::update(meals::table.filter(meals::id.eq(&job.meal_id)))
        .set((
            meals::status.eq(MealStatus::NeedsReview.as_str()),
            meals::revision.eq(job.revision),
            meals::updated_at.eq(now),
        ))
        .execute(conn)?;
    Ok(())
}

/// Mark a job and its meal failed, with a reason the user can read.
pub fn mark_failed(conn: &mut SqliteConnection, job: &Job, reason: &str) -> Result<(), AppError> {
    mark_failed_restoring(conn, job, reason, None)
}

/// Mark a job failed, optionally putting its meal back to a previous status
/// instead of failing it too.
///
/// `restore` is `Some` only for a **correction** over a meal that already had a
/// usable revision: the job failed, the meal did not (see [`restore_target`]).
pub fn mark_failed_restoring(
    conn: &mut SqliteConnection,
    job: &Job,
    reason: &str,
    restore: Option<MealStatus>,
) -> Result<(), AppError> {
    use crate::schema::{analysis_jobs, meals};

    let now = chrono::Utc::now().naive_utc();
    diesel::update(analysis_jobs::table.filter(analysis_jobs::id.eq(&job.job_id)))
        .set((
            analysis_jobs::status.eq(JobStatus::Failed.as_str()),
            analysis_jobs::error.eq(Some(reason.to_string())),
            analysis_jobs::finished_at.eq(Some(now)),
        ))
        .execute(conn)?;

    diesel::update(meals::table.filter(meals::id.eq(&job.meal_id)))
        .set((
            meals::status.eq(restore.unwrap_or(MealStatus::Failed).as_str()),
            meals::updated_at.eq(now),
        ))
        .execute(conn)?;
    Ok(())
}

/// Put a job back on the queue after a transient failure.
///
/// The error is kept on the row so an operator can see *why* it is being
/// retried, and the meal drops back to `pending` so the UI shows work pending
/// rather than a stuck `analyzing`.
pub fn mark_requeued(
    conn: &mut SqliteConnection,
    job: &Job,
    reason: &str,
) -> Result<(), AppError> {
    use crate::schema::{analysis_jobs, meals};

    let now = chrono::Utc::now().naive_utc();
    diesel::update(analysis_jobs::table.filter(analysis_jobs::id.eq(&job.job_id)))
        .set((
            analysis_jobs::status.eq(JobStatus::Queued.as_str()),
            analysis_jobs::error.eq(Some(reason.to_string())),
            analysis_jobs::finished_at.eq(None::<chrono::NaiveDateTime>),
        ))
        .execute(conn)?;

    diesel::update(meals::table.filter(meals::id.eq(&job.meal_id)))
        .set((
            meals::status.eq(MealStatus::Pending.as_str()),
            meals::updated_at.eq(now),
        ))
        .execute(conn)?;
    Ok(())
}

/// Whether a failed attempt should be retried.
pub fn should_retry(error: &AppError, attempts: i32) -> bool {
    error.is_retryable() && attempts < MAX_ATTEMPTS
}

// ---------------------------------------------------------------------------
// Cost accounting
// ---------------------------------------------------------------------------

/// Micro-USD per million input/output tokens, by model prefix.
///
/// An **estimate**, kept deliberately small and easy to edit: PRD §3 puts the
/// real spend limit on the provider side, so this column exists to answer "which
/// meals are expensive to analyse", not to bill anyone. An unknown model falls
/// back to [`DEFAULT_PRICING`] rather than recording zero, because a silent zero
/// would make the whole column untrustworthy.
pub const MODEL_PRICING: &[(&str, i64, i64)] = &[
    ("gpt-4.1-nano", 100_000, 400_000),
    ("gpt-4.1-mini", 400_000, 1_600_000),
    ("gpt-4.1", 2_000_000, 8_000_000),
    ("gpt-4o-mini", 150_000, 600_000),
    ("gpt-4o", 2_500_000, 10_000_000),
];

/// Fallback rate for an unrecognized model, in micro-USD per million tokens.
pub const DEFAULT_PRICING: (i64, i64) = (2_000_000, 8_000_000);

/// Estimated cost of one run, in micro-USD.
pub fn estimate_cost_micro_usd(model: &str, prompt_tokens: i64, completion_tokens: i64) -> i64 {
    let (input_rate, output_rate) = MODEL_PRICING
        .iter()
        // Longest prefix wins, so `gpt-4.1-mini` never matches `gpt-4.1` first.
        .filter(|(prefix, _, _)| model.starts_with(prefix))
        .max_by_key(|(prefix, _, _)| prefix.len())
        .map(|(_, input, output)| (*input, *output))
        .unwrap_or(DEFAULT_PRICING);

    let input = prompt_tokens.max(0).saturating_mul(input_rate) / 1_000_000;
    let output = completion_tokens.max(0).saturating_mul(output_rate) / 1_000_000;
    input.saturating_add(output)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::schema::{EstimatedItem, MealEstimate};
    use crate::agent::{RunUsage, StepRecord};
    use crate::feedback::state::fixtures::*;
    use crate::models::{GramsSource, MacroSource, NewAnalysisJob};

    fn item(name: &str, grams: f64, kcal: f64) -> EstimatedItem {
        EstimatedItem {
            name: name.to_string(),
            grams,
            kcal,
            protein_g: 10.0,
            fat_g: 5.0,
            carbs_g: 20.0,
            confidence: 0.7,
            reasoning_note: "because".into(),
            barcode: None,
            grams_source: GramsSource::Agent,
            macro_source: MacroSource::Model,
        }
    }

    fn outcome(dish: &str, items: Vec<EstimatedItem>) -> AgentOutcome {
        AgentOutcome {
            estimate: MealEstimate {
                dish_name: dish.to_string(),
                cuisine: None,
                container: Some("delivery bowl".into()),
                from_recall: false,
                items,
                overall_confidence: 0.65,
                notes: None,
            },
            serialized_messages: Some("[{\"role\":\"user\"}]".to_string()),
            turn_count: 4,
            steps: vec![StepRecord {
                step_no: 1,
                tool_name: "recall_similar_meals".into(),
                tool_input: Some("{\"query\":\"шаурма\"}".into()),
                tool_output: Some("{\"hits\":[]}".into()),
                latency_ms: Some(12),
            }],
            usage: RunUsage {
                prompt_tokens: 1_500,
                completion_tokens: 400,
                model_calls: 3,
                tool_calls: 1,
            },
            fallback_used: false,
            model: "gpt-4.1".into(),
            prompt_version: "2026-08-01.1".into(),
        }
    }

    fn seed_job(conn: &mut SqliteConnection, job_id: &str, meal_id: &str, revision: i32) -> Job {
        diesel::insert_into(crate::schema::analysis_jobs::table)
            .values(&NewAnalysisJob {
                id: job_id.to_string(),
                meal_id: meal_id.to_string(),
                revision,
                parent_job_id: None,
                status: JobStatus::Queued.as_str().to_string(),
                attempts: 0,
                model: "gpt-4.1".into(),
                user_feedback: None,
                steps: 0,
                tool_calls: 0,
                prompt_tokens: 0,
                completion_tokens: 0,
                cost_micro_usd: 0,
                error: None,
                started_at: None,
                created_at: at(2026, 8, 1, 12, 0),
                finished_at: None,
            })
            .execute(conn)
            .unwrap();
        Job {
            job_id: job_id.to_string(),
            meal_id: meal_id.to_string(),
            user_id: "u1".to_string(),
            revision,
            kind: JobKind::Analyze,
        }
    }

    fn pending_meal(conn: &mut SqliteConnection, meal_id: &str) {
        seed_user(conn, "u1");
        seed_meal(
            conn,
            meal_id,
            "u1",
            at(2026, 8, 1, 12, 0),
            180,
            MealStatus::Pending,
            1,
            1.0,
            None,
        );
    }

    #[test]
    fn retries_stop_at_the_attempt_cap() {
        let transient = AppError::Upstream("connection reset".into());
        assert!(should_retry(&transient, 1));
        assert!(!should_retry(&transient, MAX_ATTEMPTS));
    }

    #[test]
    fn a_bad_request_is_never_retried() {
        assert!(!should_retry(&AppError::BadRequest("no image".into()), 0));
    }

    #[test]
    fn quota_exhaustion_is_retried_then_failed_loudly() {
        let quota = AppError::RateLimited("insufficient_quota".into());
        assert!(should_retry(&quota, 1));
        assert!(!should_retry(&quota, MAX_ATTEMPTS));
    }

    #[test]
    fn marking_running_stamps_the_clock_and_counts_the_attempt() {
        use crate::schema::analysis_jobs::dsl;
        let mut conn = test_conn();
        pending_meal(&mut conn, "m1");
        let job = seed_job(&mut conn, "j1", "m1", 1);

        mark_running(&mut conn, &job).unwrap();
        assert_eq!(job_attempts(&mut conn, "j1").unwrap(), 1);

        let (status, started): (String, Option<chrono::NaiveDateTime>) = dsl::analysis_jobs
            .filter(dsl::id.eq("j1"))
            .select((dsl::status, dsl::started_at))
            .first(&mut conn)
            .unwrap();
        assert_eq!(status, "running");
        assert!(started.is_some());
        assert_eq!(load_meal(&mut conn, "m1").unwrap().status, "analyzing");

        // A retry increments rather than resetting.
        mark_running(&mut conn, &job).unwrap();
        assert_eq!(job_attempts(&mut conn, "j1").unwrap(), 2);
    }

    #[test]
    fn a_run_persists_items_and_moves_the_meal_to_needs_review() {
        use crate::schema::{analysis_jobs::dsl as jobs, meal_items::dsl as items};
        let mut conn = test_conn();
        pending_meal(&mut conn, "m1");
        let job = seed_job(&mut conn, "j1", "m1", 1);
        let out = outcome("шаурма с курицей", vec![item("лаваш", 90.0, 250.0), item("курица", 120.0, 200.0)]);

        mark_running(&mut conn, &job).unwrap();
        persist_outcome(&mut conn, "m1", 1, &out).unwrap();
        persist_steps(&mut conn, "j1", &out).unwrap();
        save_session(&mut conn, "m1", &out).unwrap();
        mark_succeeded(&mut conn, &job, &out).unwrap();

        let rows: Vec<(i32, String, f64)> = items::meal_items
            .filter(items::meal_id.eq("m1"))
            .order(items::position.asc())
            .select((items::position, items::name, items::kcal))
            .load(&mut conn)
            .unwrap();
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0], (0, "лаваш".to_string(), 250.0));
        assert_eq!(rows[1], (1, "курица".to_string(), 200.0));

        let meal = load_meal(&mut conn, "m1").unwrap();
        assert_eq!(meal.status, "needs_review");
        assert_eq!(meal.revision, 1);

        let (steps, tool_calls, prompt, completion, cost): (i32, i32, i64, i64, i64) =
            jobs::analysis_jobs
                .filter(jobs::id.eq("j1"))
                .select((
                    jobs::steps,
                    jobs::tool_calls,
                    jobs::prompt_tokens,
                    jobs::completion_tokens,
                    jobs::cost_micro_usd,
                ))
                .first(&mut conn)
                .unwrap();
        assert_eq!((steps, tool_calls), (3, 1));
        assert_eq!((prompt, completion), (1_500, 400));
        assert!(cost > 0, "wall clock and spend must both be recorded");
    }

    #[test]
    fn the_agent_name_only_fills_a_blank_one() {
        let mut conn = test_conn();
        pending_meal(&mut conn, "m1");
        // `seed_meal` names it "dish m1"; a user-supplied name outranks the
        // agent's visual read (§5), so it must survive.
        persist_outcome(&mut conn, "m1", 1, &outcome("what the model saw", vec![])).unwrap();
        assert_eq!(load_meal(&mut conn, "m1").unwrap().dish_name.unwrap(), "dish m1");

        // With no stored name the agent's read is adopted, normalized included.
        use crate::schema::meals::dsl;
        diesel::update(dsl::meals.filter(dsl::id.eq("m1")))
            .set((dsl::dish_name.eq(None::<String>), dsl::dish_name_normalized.eq(None::<String>)))
            .execute(&mut conn)
            .unwrap();
        persist_outcome(&mut conn, "m1", 1, &outcome("Шаурма из Фарша", vec![])).unwrap();
        let meal = load_meal(&mut conn, "m1").unwrap();
        assert_eq!(meal.dish_name.unwrap(), "Шаурма из Фарша");
        assert_eq!(meal.dish_name_normalized.unwrap(), "шаурма из фарша");
    }

    #[test]
    fn persisting_twice_at_one_revision_is_idempotent() {
        use crate::schema::{agent_steps::dsl as steps, meal_items::dsl as items};
        let mut conn = test_conn();
        pending_meal(&mut conn, "m1");
        seed_job(&mut conn, "j1", "m1", 1);
        let out = outcome("шаурма", vec![item("лаваш", 90.0, 250.0)]);

        for _ in 0..3 {
            persist_outcome(&mut conn, "m1", 1, &out).unwrap();
            persist_steps(&mut conn, "j1", &out).unwrap();
        }

        let item_count: i64 = items::meal_items.count().get_result(&mut conn).unwrap();
        let step_count: i64 = steps::agent_steps.count().get_result(&mut conn).unwrap();
        assert_eq!(item_count, 1, "a retry must not duplicate items");
        assert_eq!(step_count, 1);
    }

    #[test]
    fn a_new_revision_leaves_the_old_one_intact() {
        use crate::schema::meal_items::dsl as items;
        let mut conn = test_conn();
        pending_meal(&mut conn, "m1");

        persist_outcome(&mut conn, "m1", 1, &outcome("шаурма", vec![item("рис", 200.0, 300.0)])).unwrap();
        persist_outcome(&mut conn, "m1", 2, &outcome("шаурма", vec![item("рис", 100.0, 150.0)])).unwrap();

        let by_revision: Vec<(i32, f64)> = items::meal_items
            .filter(items::meal_id.eq("m1"))
            .order(items::revision.asc())
            .select((items::revision, items::grams))
            .load(&mut conn)
            .unwrap();
        assert_eq!(by_revision, vec![(1, 200.0), (2, 100.0)]);
        assert_eq!(load_meal(&mut conn, "m1").unwrap().revision, 2);
    }

    #[test]
    fn non_finite_model_output_is_neutralized_rather_than_stored() {
        use crate::schema::meal_items::dsl as items;
        let mut conn = test_conn();
        pending_meal(&mut conn, "m1");

        let mut bad = item("mystery", f64::NAN, -20.0);
        bad.protein_g = f64::INFINITY;
        bad.confidence = 7.0;
        persist_outcome(&mut conn, "m1", 1, &outcome("x", vec![bad])).unwrap();

        let (grams, kcal, protein, confidence): (f64, f64, f64, Option<f64>) = items::meal_items
            .select((items::grams, items::kcal, items::protein_g, items::confidence))
            .first(&mut conn)
            .unwrap();
        assert_eq!((grams, kcal, protein), (0.0, 0.0, 0.0));
        assert_eq!(confidence, Some(1.0));
    }

    #[test]
    fn a_failed_job_says_why_on_both_rows() {
        use crate::schema::analysis_jobs::dsl;
        let mut conn = test_conn();
        pending_meal(&mut conn, "m1");
        let job = seed_job(&mut conn, "j1", "m1", 1);

        mark_running(&mut conn, &job).unwrap();
        mark_failed(&mut conn, &job, "rate limited: insufficient_quota").unwrap();

        let (status, error, finished): (String, Option<String>, Option<chrono::NaiveDateTime>) =
            dsl::analysis_jobs
                .filter(dsl::id.eq("j1"))
                .select((dsl::status, dsl::error, dsl::finished_at))
                .first(&mut conn)
                .unwrap();
        assert_eq!(status, "failed");
        assert!(error.unwrap().contains("insufficient_quota"));
        assert!(finished.is_some());
        assert_eq!(load_meal(&mut conn, "m1").unwrap().status, "failed");
    }

    #[test]
    fn a_failed_correction_puts_the_meal_back_instead_of_destroying_it() {
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
        let mut job = seed_job(&mut conn, "j2", "m1", 2);
        job.kind = JobKind::Reanalyze {
            feedback: "half the rice".into(),
            parent_job_id: Some("j1".into()),
        };

        mark_running(&mut conn, &job).unwrap();
        mark_failed_restoring(&mut conn, &job, "upstream timeout", Some(MealStatus::Confirmed))
            .unwrap();

        // The job is failed and says why...
        use crate::schema::analysis_jobs::dsl;
        let status: String = dsl::analysis_jobs
            .filter(dsl::id.eq("j2"))
            .select(dsl::status)
            .first(&mut conn)
            .unwrap();
        assert_eq!(status, "failed");
        // ...but the meal keeps the revision the user already saw, so its
        // calories stay in the day.
        assert_eq!(load_meal(&mut conn, "m1").unwrap().status, "confirmed");
    }

    #[test]
    fn the_restore_target_only_applies_to_corrections_over_usable_meals() {
        let base = |status: MealStatus| Prepared {
            ctx: AnalysisContext {
                user_id: "u1".into(),
                meal_id: "m1".into(),
                job_id: "j1".into(),
                revision: 1,
                dish_name: None,
                user_comment: None,
                image_path: None,
                eaten_at: at(2026, 8, 1, 12, 0),
                recent_dishes: vec![],
                calibration_factor: 1.0,
            },
            user_id: "u1".into(),
            group_id: None,
            dish_name: None,
            previous_status: status,
        };
        let analyze = Job {
            job_id: "j1".into(),
            meal_id: "m1".into(),
            user_id: "u1".into(),
            revision: 1,
            kind: JobKind::Analyze,
        };
        let correct = Job {
            kind: JobKind::Reanalyze {
                feedback: "less rice".into(),
                parent_job_id: None,
            },
            ..analyze.clone()
        };

        // A first analysis has nothing to preserve.
        assert_eq!(restore_target(&analyze, &base(MealStatus::Pending)), None);
        assert_eq!(restore_target(&analyze, &base(MealStatus::Confirmed)), None);
        // A correction over a usable revision preserves it.
        assert_eq!(
            restore_target(&correct, &base(MealStatus::Confirmed)),
            Some(MealStatus::Confirmed)
        );
        assert_eq!(
            restore_target(&correct, &base(MealStatus::NeedsReview)),
            Some(MealStatus::NeedsReview)
        );
        // A correction over a meal that never produced one does not.
        assert_eq!(restore_target(&correct, &base(MealStatus::Failed)), None);
        assert_eq!(restore_target(&correct, &base(MealStatus::Pending)), None);
    }

    #[test]
    fn a_requeued_job_goes_back_to_pending_with_the_reason_visible() {
        use crate::schema::analysis_jobs::dsl;
        let mut conn = test_conn();
        pending_meal(&mut conn, "m1");
        let job = seed_job(&mut conn, "j1", "m1", 1);

        mark_running(&mut conn, &job).unwrap();
        mark_requeued(&mut conn, &job, "upstream unavailable: timeout").unwrap();

        let (status, error): (String, Option<String>) = dsl::analysis_jobs
            .filter(dsl::id.eq("j1"))
            .select((dsl::status, dsl::error))
            .first(&mut conn)
            .unwrap();
        assert_eq!(status, "queued");
        assert!(error.unwrap().contains("timeout"));
        assert_eq!(load_meal(&mut conn, "m1").unwrap().status, "pending");
        // The attempt already counted, so the cap still bites.
        assert_eq!(job_attempts(&mut conn, "j1").unwrap(), 1);
    }

    #[test]
    fn a_session_is_written_once_and_then_updated() {
        use crate::schema::agent_sessions::dsl;
        let mut conn = test_conn();
        pending_meal(&mut conn, "m1");

        let mut first = outcome("шаурма", vec![]);
        first.serialized_messages = Some("[1]".into());
        save_session(&mut conn, "m1", &first).unwrap();

        let mut second = outcome("шаурма", vec![]);
        second.serialized_messages = Some("[1,2]".into());
        second.turn_count = 6;
        save_session(&mut conn, "m1", &second).unwrap();

        let rows: Vec<(String, i32, String)> = dsl::agent_sessions
            .filter(dsl::meal_id.eq("m1"))
            .select((dsl::messages, dsl::turn_count, dsl::prompt_version))
            .load(&mut conn)
            .unwrap();
        assert_eq!(rows.len(), 1, "meal_id is unique: one live thread per meal");
        assert_eq!(rows[0], ("[1,2]".to_string(), 6, "2026-08-01.1".to_string()));
    }

    #[test]
    fn a_fallback_run_does_not_wipe_the_stored_thread() {
        use crate::schema::agent_sessions::dsl;
        let mut conn = test_conn();
        pending_meal(&mut conn, "m1");

        save_session(&mut conn, "m1", &outcome("шаурма", vec![])).unwrap();
        let mut fallback = outcome("шаурма", vec![]);
        fallback.serialized_messages = None;
        save_session(&mut conn, "m1", &fallback).unwrap();

        let messages: String = dsl::agent_sessions
            .filter(dsl::meal_id.eq("m1"))
            .select(dsl::messages)
            .first(&mut conn)
            .unwrap();
        assert_eq!(messages, "[{\"role\":\"user\"}]");
    }

    #[test]
    fn a_long_tool_payload_is_truncated_on_a_character_boundary() {
        let payload = "ы".repeat(MAX_STEP_PAYLOAD);
        let truncated = truncate(&payload);
        assert!(truncated.ends_with("… [truncated]"));
        assert!(truncated.len() < payload.len() + 20);
        assert_eq!(truncate("short"), "short");
    }

    #[test]
    fn recent_dishes_are_confirmed_deduplicated_and_newest_first() {
        let mut conn = test_conn();
        seed_user(&mut conn, "u1");
        let names = [
            ("a", "Шаурма", MealStatus::Confirmed, 1),
            ("b", "шаурма ", MealStatus::Confirmed, 2),
            ("c", "Пад тай", MealStatus::Confirmed, 3),
            ("d", "Никогда", MealStatus::NeedsReview, 4),
            ("e", "Тоже нет", MealStatus::Pending, 5),
        ];
        for (id, name, status, hour) in names {
            seed_meal(
                &mut conn,
                id,
                "u1",
                at(2026, 8, 1, 10 + hour, 0),
                0,
                status,
                1,
                1.0,
                None,
            );
            use crate::schema::meals::dsl;
            diesel::update(dsl::meals.filter(dsl::id.eq(id)))
                .set(dsl::dish_name.eq(Some(name.to_string())))
                .execute(&mut conn)
                .unwrap();
        }

        let recent = recent_confirmed_dishes(&mut conn, "u1", 8).unwrap();
        assert_eq!(recent, vec!["Пад тай".to_string(), "шаурма".to_string()]);
    }

    #[test]
    fn recent_dishes_are_scoped_to_one_user() {
        let mut conn = test_conn();
        seed_user(&mut conn, "u1");
        seed_user(&mut conn, "u2");
        seed_meal(&mut conn, "mine", "u1", at(2026, 8, 1, 12, 0), 0, MealStatus::Confirmed, 1, 1.0, None);
        seed_meal(&mut conn, "theirs", "u2", at(2026, 8, 1, 13, 0), 0, MealStatus::Confirmed, 1, 1.0, None);

        let recent = recent_confirmed_dishes(&mut conn, "u1", 8).unwrap();
        assert_eq!(recent, vec!["dish mine".to_string()]);
    }

    #[test]
    fn the_calibration_factor_defaults_to_one_and_rejects_nonsense() {
        use crate::schema::user_profiles::dsl;
        let mut conn = test_conn();
        seed_user(&mut conn, "u1");
        assert_eq!(calibration_factor(&mut conn, "u1").unwrap(), 1.0);

        seed_profile(&mut conn, "u1", 2050.0, 165.0);
        diesel::update(dsl::user_profiles.filter(dsl::user_id.eq("u1")))
            .set(dsl::calibration_factor.eq(1.15))
            .execute(&mut conn)
            .unwrap();
        assert_eq!(calibration_factor(&mut conn, "u1").unwrap(), 1.15);

        diesel::update(dsl::user_profiles.filter(dsl::user_id.eq("u1")))
            .set(dsl::calibration_factor.eq(0.0))
            .execute(&mut conn)
            .unwrap();
        assert_eq!(calibration_factor(&mut conn, "u1").unwrap(), 1.0);
    }

    #[test]
    fn cost_uses_the_longest_matching_model_prefix() {
        // 1M in + 1M out on gpt-4.1 = $2 + $8 = 10,000,000 micro-USD.
        assert_eq!(estimate_cost_micro_usd("gpt-4.1", 1_000_000, 1_000_000), 10_000_000);
        // `gpt-4.1-mini` must not be priced as `gpt-4.1`.
        assert_eq!(
            estimate_cost_micro_usd("gpt-4.1-mini", 1_000_000, 1_000_000),
            2_000_000
        );
        // An unknown model is estimated, never silently zero.
        assert!(estimate_cost_micro_usd("some-future-model", 1_000, 1_000) > 0);
        assert_eq!(estimate_cost_micro_usd("gpt-4.1", 0, 0), 0);
        assert_eq!(estimate_cost_micro_usd("gpt-4.1", -5, -5), 0);
    }
}
