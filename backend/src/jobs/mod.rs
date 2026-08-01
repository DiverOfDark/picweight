//! Background work. Analysis is **never inline** (PRD §7): upload returns 202,
//! a worker runs the loop, the result arrives over SSE or a poll.
//!
//! Two workers:
//!
//! * [`analyzer`] — drains the job queue. One loop per photo, run concurrently,
//!   so a five-dish sitting completes in roughly the time of a single dish.
//! * [`group_settler`] — fires exactly one notification per sitting once it
//!   settles (`group_size` hint, or the 90s idle debounce as fallback).
//!
//! The queue is in-process and unbounded; durability comes from the database,
//! not the channel. [`requeue_unfinished`] re-enqueues anything left `pending`
//! or `analyzing` at startup, which is what makes G5 ("never lose a logged
//! meal") hold across a restart or a crash mid-loop.

pub mod analyzer;
pub mod group_settler;

use crate::error::AppError;
use crate::models::{JobStatus, MealStatus, NewAnalysisJob};
use crate::AppState;
use diesel::prelude::*;
use std::sync::Arc;
use tokio::sync::mpsc;

/// How many analysis loops may run at once.
///
/// Sized for the multi-dish sitting: five concurrent loops is the case the PRD
/// calls out, and the OpenAI key carries a provider-side spend limit so there is
/// no in-app budgeting to respect.
pub const WORKER_CONCURRENCY: usize = 6;

/// What a worker is asked to do.
#[derive(Debug, Clone)]
pub enum JobKind {
    /// First analysis of a freshly uploaded photo (or manual entry).
    Analyze,
    /// A correction turn: resume the persisted session with the user's words.
    Reanalyze {
        /// The user's verbatim feedback, stored on `analysis_jobs.user_feedback`.
        feedback: String,
        /// The job this one continues, for the audit chain.
        parent_job_id: Option<String>,
    },
}

/// One unit of queued work, mirroring an `analysis_jobs` row.
#[derive(Debug, Clone)]
pub struct Job {
    /// `analysis_jobs.id` — the row already exists before the job is enqueued.
    pub job_id: String,
    /// The meal being analysed.
    pub meal_id: String,
    /// Owning user, so the worker can scope the agent's tools.
    pub user_id: String,
    /// Revision this run produces.
    pub revision: i32,
    /// Analyse or re-analyse.
    pub kind: JobKind,
}

/// The producer half of the work queue, held in [`AppState`].
#[derive(Clone)]
pub struct JobQueue {
    tx: mpsc::UnboundedSender<Job>,
}

impl JobQueue {
    /// Create the queue. The receiver is handed to [`spawn_workers`].
    pub fn new() -> (JobQueue, mpsc::UnboundedReceiver<Job>) {
        let (tx, rx) = mpsc::unbounded_channel();
        (JobQueue { tx }, rx)
    }

    /// Enqueue work.
    ///
    /// Fails only if every worker has shut down, which during normal operation
    /// means the process is going away.
    pub fn enqueue(&self, job: Job) -> Result<(), AppError> {
        self.tx.send(job).map_err(|e| {
            AppError::Internal(format!("analysis queue is closed: {}", e.0.meal_id))
        })
    }
}

impl std::fmt::Debug for JobQueue {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("JobQueue")
            .field("closed", &self.tx.is_closed())
            .finish()
    }
}

/// Spawn the analysis worker pool.
///
/// Returns the dispatcher's join handle. Jobs are run concurrently up to
/// [`WORKER_CONCURRENCY`]; each one is independent, so a timed-out analysis
/// never sinks the rest of a sitting.
pub fn spawn_workers(
    state: AppState,
    mut rx: mpsc::UnboundedReceiver<Job>,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let permits = Arc::new(tokio::sync::Semaphore::new(WORKER_CONCURRENCY));
        tracing::info!(concurrency = WORKER_CONCURRENCY, "analysis workers started");

        while let Some(job) = rx.recv().await {
            // Acquired before spawning, so the queue exerts backpressure on
            // itself rather than spawning a task per photo and hammering the
            // provider with a whole sitting at once.
            let permit = match permits.clone().acquire_owned().await {
                Ok(permit) => permit,
                Err(_) => break, // the semaphore is closed: we are shutting down
            };

            let state = state.clone();
            tokio::spawn(async move {
                let _permit = permit;
                let job_id = job.job_id.clone();
                let meal_id = job.meal_id.clone();
                // `run_job` handles its own failures; an error here means even
                // the bookkeeping failed, which is worth shouting about.
                if let Err(err) = analyzer::run_job(&state, job).await {
                    tracing::error!(
                        job = %job_id,
                        meal = %meal_id,
                        error = %err,
                        "analysis job could not be recorded"
                    );
                }
            });
        }

        tracing::info!("analysis queue closed; workers stopping");
    })
}

/// Re-enqueue everything left in flight by a previous process.
///
/// Called once at startup, before the HTTP server binds. Returns how many jobs
/// were recovered.
///
/// This is what makes G5 ("never lose a logged meal") hold across a crash: the
/// in-process channel is not durable, so the database is re-read as the source
/// of truth. Two cases are recovered:
///
/// * a `queued`/`running` `analysis_jobs` row whose meal never reached a
///   terminal status — the common case, a pod killed mid-loop;
/// * a `pending`/`analyzing` meal with **no** live job at all, which can only
///   happen if the process died between the meal insert and the job insert.
pub async fn requeue_unfinished(state: &AppState) -> Result<usize, AppError> {
    let model = state.agent.model().to_string();
    let jobs: Vec<Job> = state
        .interact(move |conn| recover_unfinished(conn, &model))
        .await?;

    let count = jobs.len();
    for job in jobs {
        state.jobs.enqueue(job)?;
    }
    Ok(count)
}

/// The database half of [`requeue_unfinished`]: reset the rows and return the
/// work to re-enqueue.
///
/// Split out so the recovery rules are testable without an agent, a provider key
/// or an HTTP client.
pub fn recover_unfinished(
    conn: &mut diesel::SqliteConnection,
    model: &str,
) -> Result<Vec<Job>, AppError> {
    use crate::schema::{analysis_jobs, meals};

    let live = [JobStatus::Queued.as_str(), JobStatus::Running.as_str()];
    let unfinished = [MealStatus::Pending.as_str(), MealStatus::Analyzing.as_str()];

    let mut recovered: Vec<Job> = Vec::new();

    // --- jobs whose worker never came back ---------------------------------
    /// `(job id, meal id, revision, user feedback, user id, parent job id)`.
    type UnfinishedRow = (String, String, i32, Option<String>, String, Option<String>);

    let rows: Vec<UnfinishedRow> = analysis_jobs::table
        .inner_join(meals::table)
        .filter(analysis_jobs::status.eq_any(live))
        .filter(meals::status.eq_any(unfinished))
        .order(analysis_jobs::created_at.asc())
        .select((
            analysis_jobs::id,
            analysis_jobs::meal_id,
            analysis_jobs::revision,
            analysis_jobs::user_feedback,
            meals::user_id,
            analysis_jobs::parent_job_id,
        ))
        .load(conn)?;

    for (job_id, meal_id, revision, feedback, user_id, parent_job_id) in rows {
        // A `running` row belongs to a worker that no longer exists; put it back
        // in the queue so the stored state matches reality.
        diesel::update(analysis_jobs::table.filter(analysis_jobs::id.eq(&job_id)))
            .set((
                analysis_jobs::status.eq(JobStatus::Queued.as_str()),
                analysis_jobs::started_at.eq(None::<chrono::NaiveDateTime>),
            ))
            .execute(conn)?;
        diesel::update(meals::table.filter(meals::id.eq(&meal_id)))
            .set(meals::status.eq(MealStatus::Pending.as_str()))
            .execute(conn)?;

        recovered.push(Job {
            job_id,
            meal_id,
            user_id,
            revision,
            // `user_feedback` on the row is what distinguishes a correction turn
            // from a first analysis, so a resumed session stays a resumed
            // session across a restart.
            kind: match feedback {
                Some(feedback) => JobKind::Reanalyze {
                    feedback,
                    parent_job_id,
                },
                None => JobKind::Analyze,
            },
        });
    }

    // --- meals that never got a job at all ---------------------------------
    let orphans: Vec<(String, String, i32)> = meals::table
        .filter(meals::status.eq_any(unfinished))
        .filter(diesel::dsl::not(diesel::dsl::exists(
            analysis_jobs::table
                .filter(analysis_jobs::meal_id.eq(meals::id))
                .filter(analysis_jobs::status.eq_any(live)),
        )))
        .order(meals::created_at.asc())
        .select((meals::id, meals::user_id, meals::revision))
        .load(conn)?;

    let now = chrono::Utc::now().naive_utc();
    for (meal_id, user_id, revision) in orphans {
        let job_id = uuid::Uuid::new_v4().to_string();
        diesel::insert_into(analysis_jobs::table)
            .values(&NewAnalysisJob {
                id: job_id.clone(),
                meal_id: meal_id.clone(),
                revision,
                parent_job_id: None,
                status: JobStatus::Queued.as_str().to_string(),
                attempts: 0,
                model: model.to_string(),
                user_feedback: None,
                steps: 0,
                tool_calls: 0,
                prompt_tokens: 0,
                completion_tokens: 0,
                cost_micro_usd: 0,
                error: None,
                started_at: None,
                created_at: now,
                finished_at: None,
            })
            .execute(conn)?;
        diesel::update(meals::table.filter(meals::id.eq(&meal_id)))
            .set(meals::status.eq(MealStatus::Pending.as_str()))
            .execute(conn)?;

        recovered.push(Job {
            job_id,
            meal_id,
            user_id,
            revision,
            kind: JobKind::Analyze,
        });
    }

    Ok(recovered)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn enqueueing_after_every_worker_is_gone_is_an_error_not_a_silent_drop() {
        let (queue, rx) = JobQueue::new();
        drop(rx);
        let err = queue
            .enqueue(Job {
                job_id: "j1".into(),
                meal_id: "m1".into(),
                user_id: "u1".into(),
                revision: 1,
                kind: JobKind::Analyze,
            })
            .unwrap_err();
        assert!(err.to_string().contains("m1"));
    }

    #[tokio::test]
    async fn the_queue_delivers_in_order() {
        let (queue, mut rx) = JobQueue::new();
        for i in 0..3 {
            queue
                .enqueue(Job {
                    job_id: format!("j{i}"),
                    meal_id: format!("m{i}"),
                    user_id: "u1".into(),
                    revision: 1,
                    kind: JobKind::Analyze,
                })
                .unwrap();
        }
        for i in 0..3 {
            assert_eq!(rx.recv().await.unwrap().job_id, format!("j{i}"));
        }
    }

    #[test]
    fn a_reanalysis_job_carries_the_feedback_verbatim() {
        let job = Job {
            job_id: "j1".into(),
            meal_id: "m1".into(),
            user_id: "u1".into(),
            revision: 2,
            kind: JobKind::Reanalyze {
                feedback: "too much rice, it was about half that".into(),
                parent_job_id: Some("j0".into()),
            },
        };
        match job.kind {
            JobKind::Reanalyze {
                feedback,
                parent_job_id,
            } => {
                assert_eq!(feedback, "too much rice, it was about half that");
                assert_eq!(parent_job_id.as_deref(), Some("j0"));
            }
            JobKind::Analyze => panic!("expected a correction turn"),
        }
    }

    // -----------------------------------------------------------------------
    // Startup recovery (G5 — never lose a logged meal)
    // -----------------------------------------------------------------------

    mod recovery {
        use super::super::*;
        use crate::feedback::state::fixtures::*;
        use crate::models::NewAnalysisJob;

        #[allow(clippy::too_many_arguments)]
        fn seed_job(
            conn: &mut diesel::SqliteConnection,
            job_id: &str,
            meal_id: &str,
            status: JobStatus,
            feedback: Option<&str>,
        ) {
            diesel::insert_into(crate::schema::analysis_jobs::table)
                .values(&NewAnalysisJob {
                    id: job_id.to_string(),
                    meal_id: meal_id.to_string(),
                    revision: 1,
                    parent_job_id: None,
                    status: status.as_str().to_string(),
                    attempts: 0,
                    model: "gpt-4.1".into(),
                    user_feedback: feedback.map(str::to_string),
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
        }

        #[test]
        fn a_job_killed_mid_loop_is_reset_and_returned() {
            use crate::schema::analysis_jobs::dsl;
            let mut conn = test_conn();
            seed_user(&mut conn, "u1");
            seed_meal(
                &mut conn,
                "m1",
                "u1",
                at(2026, 8, 1, 12, 0),
                0,
                MealStatus::Analyzing,
                1,
                1.0,
                None,
            );
            seed_job(&mut conn, "j1", "m1", JobStatus::Running, None);

            let jobs = recover_unfinished(&mut conn, "gpt-4.1").unwrap();
            assert_eq!(jobs.len(), 1);
            assert_eq!(jobs[0].job_id, "j1");
            assert_eq!(jobs[0].user_id, "u1");
            assert!(matches!(jobs[0].kind, JobKind::Analyze));

            let (status, started): (String, Option<chrono::NaiveDateTime>) = dsl::analysis_jobs
                .filter(dsl::id.eq("j1"))
                .select((dsl::status, dsl::started_at))
                .first(&mut conn)
                .unwrap();
            assert_eq!(status, "queued");
            assert_eq!(started, None);
        }

        #[test]
        fn a_correction_turn_survives_a_restart_as_a_correction_turn() {
            let mut conn = test_conn();
            seed_user(&mut conn, "u1");
            seed_meal(
                &mut conn,
                "m1",
                "u1",
                at(2026, 8, 1, 12, 0),
                0,
                MealStatus::Analyzing,
                1,
                1.0,
                None,
            );
            seed_job(&mut conn, "j1", "m1", JobStatus::Queued, Some("half the rice"));

            let jobs = recover_unfinished(&mut conn, "gpt-4.1").unwrap();
            match &jobs[0].kind {
                JobKind::Reanalyze { feedback, .. } => assert_eq!(feedback, "half the rice"),
                JobKind::Analyze => panic!("a correction must not degrade into a fresh analysis"),
            }
        }

        #[test]
        fn a_meal_that_never_got_a_job_gets_one() {
            use crate::schema::analysis_jobs::dsl;
            let mut conn = test_conn();
            seed_user(&mut conn, "u1");
            seed_meal(
                &mut conn,
                "m1",
                "u1",
                at(2026, 8, 1, 12, 0),
                0,
                MealStatus::Pending,
                1,
                1.0,
                None,
            );

            let jobs = recover_unfinished(&mut conn, "gpt-4.1").unwrap();
            assert_eq!(jobs.len(), 1, "a pending meal with no job is a lost meal");

            let count: i64 = dsl::analysis_jobs.count().get_result(&mut conn).unwrap();
            assert_eq!(count, 1);
            let model: String = dsl::analysis_jobs.select(dsl::model).first(&mut conn).unwrap();
            assert_eq!(model, "gpt-4.1");
        }

        #[test]
        fn finished_meals_are_left_alone() {
            let mut conn = test_conn();
            seed_user(&mut conn, "u1");
            for (id, status) in [
                ("done", MealStatus::NeedsReview),
                ("confirmed", MealStatus::Confirmed),
                ("failed", MealStatus::Failed),
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
                    None,
                );
                seed_job(&mut conn, &format!("j-{id}"), id, JobStatus::Succeeded, None);
            }
            assert!(recover_unfinished(&mut conn, "gpt-4.1").unwrap().is_empty());
        }

        #[test]
        fn recovery_is_idempotent() {
            use crate::schema::analysis_jobs::dsl;
            let mut conn = test_conn();
            seed_user(&mut conn, "u1");
            seed_meal(
                &mut conn,
                "m1",
                "u1",
                at(2026, 8, 1, 12, 0),
                0,
                MealStatus::Pending,
                1,
                1.0,
                None,
            );

            let first = recover_unfinished(&mut conn, "gpt-4.1").unwrap();
            let second = recover_unfinished(&mut conn, "gpt-4.1").unwrap();
            assert_eq!(first.len(), 1);
            assert_eq!(second.len(), 1, "the same job comes back, not a second one");
            assert_eq!(first[0].job_id, second[0].job_id);

            let count: i64 = dsl::analysis_jobs.count().get_result(&mut conn).unwrap();
            assert_eq!(count, 1);
        }

        #[test]
        fn every_member_of_an_interrupted_sitting_comes_back() {
            let mut conn = test_conn();
            seed_user(&mut conn, "u1");
            seed_group(&mut conn, "g1", "u1", Some(3), at(2026, 8, 1, 12, 0));
            for i in 0..3 {
                let id = format!("m{i}");
                seed_meal(
                    &mut conn,
                    &id,
                    "u1",
                    at(2026, 8, 1, 12, 0),
                    0,
                    MealStatus::Analyzing,
                    1,
                    1.0,
                    Some("g1"),
                );
                seed_job(&mut conn, &format!("j{i}"), &id, JobStatus::Running, None);
            }

            let jobs = recover_unfinished(&mut conn, "gpt-4.1").unwrap();
            assert_eq!(jobs.len(), 3, "one loop per photo, restart included");
        }
    }
}
