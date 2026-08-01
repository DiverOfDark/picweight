//! Feedback-driven continuation (PRD §5, "Correction by conversation").
//!
//! `POST /api/v1/meals/:id/reanalyze { feedback }` does not restart the agent.
//! It loads the persisted thread, appends the feedback as a new user turn, and
//! continues the same conversation — so "half the rice" is interpreted against
//! the agent's own prior reasoning instead of re-derived from scratch, and the
//! tool results it already has are reused rather than re-fetched.
//!
//! The output is a **new revision**; prior revisions are retained, and the
//! corrected revision is what `recall_similar_meals` returns next time.

use crate::agent::schema::{EstimatedItem, MealEstimate};
use crate::agent::session::{self, ResumeDecision};
use crate::agent::{AgentOutcome, AnalysisContext, StepRecord};
use crate::error::AppError;
use crate::models::{GramsSource, MacroSource, Meal, MealItem, NewItemCorrection, NewMealItem};
use crate::AppState;
use diesel::prelude::*;
use diesel::SqliteConnection;

/// `agent_steps.tool_name` used for the synthetic row that records whether a
/// correction continued its session or reseeded.
///
/// §5 asks for the chosen path to be *recorded*, and `agent_steps` is the audit
/// table that already exists for "why did the agent do that" — so the decision
/// rides there rather than growing a column nothing else would read.
pub const RESUME_STEP: &str = "session_resume";

/// Run one correction turn for a meal.
///
/// Loads the session, applies [`crate::agent::session::decide`], and either
/// continues the thread or starts a fresh one seeded with the last confirmed
/// result. Which path was taken is recorded on the new `analysis_jobs` row.
pub async fn reanalyze(
    state: &AppState,
    ctx: &AnalysisContext,
    feedback: &str,
) -> Result<AgentOutcome, AppError> {
    let meal_id = ctx.meal_id.clone();
    let stored = state
        .interact(move |conn| session::load(conn, &meal_id))
        .await?;

    let model = state.agent.model().to_string();
    let prompt_version = state.agent.prompt_version().to_string();

    // A session that never existed (or no longer deserializes) reseeds for the
    // same reason a stale one does: there is no thread to continue.
    let decision = stored
        .as_ref()
        .map(|session| session::decide(session, &model, &prompt_version))
        .unwrap_or(ResumeDecision::Reseed);

    let stored_model = stored.as_ref().map(|s| s.model.clone());
    let stored_prompt_version = stored.as_ref().map(|s| s.prompt_version.clone());

    tracing::info!(
        meal_id = %ctx.meal_id,
        job_id = %ctx.job_id,
        revision = ctx.revision,
        decision = %decision,
        stored_model = ?stored_model,
        stored_prompt_version = ?stored_prompt_version,
        "resuming an agent session for a correction"
    );

    let mut outcome = match (decision, stored) {
        (ResumeDecision::Continue, Some(session)) => {
            state
                .agent
                .continue_session(ctx, session.messages, feedback)
                .await?
        }
        _ => {
            // Seed the fresh conversation with the estimate the user is
            // reacting to, so "half the rice" still has a rice figure to halve.
            let meal_id = ctx.meal_id.clone();
            let previous_revision = ctx.revision.saturating_sub(1).max(1);
            let previous = state
                .interact(move |conn| estimate_at_revision(conn, &meal_id, previous_revision))
                .await?;
            state.agent.reseed_session(ctx, &previous, feedback).await?
        }
    };

    outcome.steps.insert(
        0,
        resume_step(decision, stored_model.as_deref(), stored_prompt_version.as_deref()),
    );
    Ok(outcome)
}

/// The synthetic `agent_steps` row recording the continue-vs-reseed decision.
///
/// `step_no` is 0 because tool calls are numbered from 1: the marker sorts ahead
/// of the run's real steps without renumbering them.
pub fn resume_step(
    decision: ResumeDecision,
    stored_model: Option<&str>,
    stored_prompt_version: Option<&str>,
) -> StepRecord {
    let input = serde_json::json!({
        "decision": decision.as_str(),
        "stored_model": stored_model,
        "stored_prompt_version": stored_prompt_version,
    });
    StepRecord {
        step_no: 0,
        tool_name: RESUME_STEP.to_string(),
        tool_input: Some(input.to_string()),
        tool_output: Some(match decision {
            ResumeDecision::Continue => {
                "continued the persisted thread; tool results were reused".to_string()
            }
            ResumeDecision::Reseed => {
                "started a fresh session seeded with the previous revision".to_string()
            }
        }),
        latency_ms: None,
    }
}

/// Next revision number for a meal: `max(meals.revision, max(jobs.revision)) + 1`.
///
/// Both tables are consulted because a job row is written *before* its run
/// finishes: taking `meals.revision` alone would hand two concurrent corrections
/// the same number.
pub fn next_revision(conn: &mut SqliteConnection, meal_id: &str) -> Result<i32, AppError> {
    use crate::schema::{analysis_jobs, meals};
    use diesel::dsl::max;

    let meal_revision: i32 = meals::table
        .filter(meals::id.eq(meal_id))
        .select(meals::revision)
        .first(conn)
        .optional()?
        .ok_or_else(|| AppError::NotFound(format!("meal {meal_id}")))?;

    let job_revision: Option<i32> = analysis_jobs::table
        .filter(analysis_jobs::meal_id.eq(meal_id))
        .select(max(analysis_jobs::revision))
        .first(conn)?;

    Ok(meal_revision.max(job_revision.unwrap_or(0)) + 1)
}

/// Load the estimate of a meal at a given revision, reconstructed from
/// `meal_items`.
///
/// Used to seed a fresh conversation when the session cannot be continued, and
/// to render revision history.
pub fn estimate_at_revision(
    conn: &mut SqliteConnection,
    meal_id: &str,
    revision: i32,
) -> Result<MealEstimate, AppError> {
    use crate::schema::{meal_items, meals};

    let meal: Meal = meals::table
        .filter(meals::id.eq(meal_id))
        .select(Meal::as_select())
        .first(conn)
        .optional()?
        .ok_or_else(|| AppError::NotFound(format!("meal {meal_id}")))?;

    let rows: Vec<MealItem> = meal_items::table
        .filter(meal_items::meal_id.eq(meal_id))
        .filter(meal_items::revision.eq(revision))
        .order(meal_items::position.asc())
        .select(MealItem::as_select())
        .load(conn)?;

    if rows.is_empty() {
        return Err(AppError::NotFound(format!(
            "meal {meal_id} has no items at revision {revision}"
        )));
    }

    Ok(to_estimate(&meal, &rows))
}

/// Rebuild a [`MealEstimate`] from persisted rows.
///
/// Kept here rather than reused from `api::meals` on purpose: the agent module
/// is the swap boundary and must not depend on the HTTP layer above it.
pub fn to_estimate(meal: &Meal, items: &[MealItem]) -> MealEstimate {
    let from_recall = items
        .iter()
        .any(|item| matches!(item.macro_source(), Ok(MacroSource::Recall)));

    MealEstimate {
        dish_name: meal
            .dish_name
            .clone()
            .unwrap_or_else(|| "unnamed meal".to_string()),
        cuisine: None,
        container: None,
        from_recall,
        items: items
            .iter()
            .map(|item| EstimatedItem {
                name: item.name.clone(),
                grams: item.grams,
                kcal: item.kcal,
                protein_g: item.protein_g,
                fat_g: item.fat_g,
                carbs_g: item.carbs_g,
                confidence: item.confidence.unwrap_or(0.5),
                reasoning_note: item.reasoning_note.clone().unwrap_or_default(),
                barcode: item.barcode.clone(),
                grams_source: item.grams_source().unwrap_or(GramsSource::Agent),
                macro_source: item.macro_source().unwrap_or(MacroSource::Model),
            })
            .collect(),
        overall_confidence: average_confidence(items),
        notes: meal.user_comment.clone(),
    }
}

/// Mean of the per-item confidences, or 0.5 when none were recorded.
fn average_confidence(items: &[MealItem]) -> f64 {
    let recorded: Vec<f64> = items.iter().filter_map(|item| item.confidence).collect();
    if recorded.is_empty() {
        return 0.5;
    }
    recorded.iter().sum::<f64>() / recorded.len() as f64
}

/// Persist a correction turn's result as a new revision.
///
/// Writes the new `meal_items` rows, bumps `meals.revision`, and records the
/// per-field deltas in `item_corrections` so the correction is auditable.
///
/// Idempotent: a retried job replaces the rows at `revision` rather than
/// doubling them, and earlier revisions are never touched — that is what makes
/// "prior revisions are retained" true.
pub fn persist_revision(
    conn: &mut SqliteConnection,
    meal_id: &str,
    revision: i32,
    outcome: &AgentOutcome,
) -> Result<(), AppError> {
    use crate::schema::{meal_items, meals};

    conn.transaction(|conn| {
        let previous: Vec<MealItem> = meal_items::table
            .filter(meal_items::meal_id.eq(meal_id))
            .filter(meal_items::revision.eq(revision - 1))
            .order(meal_items::position.asc())
            .select(MealItem::as_select())
            .load(conn)?;

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
            .map(|(position, item)| NewMealItem {
                id: uuid::Uuid::new_v4().to_string(),
                meal_id: meal_id.to_string(),
                revision,
                position: position as i32,
                name: item.name.clone(),
                barcode: item.barcode.clone(),
                grams: item.grams,
                grams_source: item.grams_source.as_str().to_string(),
                kcal: item.kcal,
                protein_g: item.protein_g,
                fat_g: item.fat_g,
                carbs_g: item.carbs_g,
                macro_source: item.macro_source.as_str().to_string(),
                confidence: Some(item.confidence),
                reasoning_note: Some(item.reasoning_note.clone()),
                created_at: now,
            })
            .collect();

        let corrections = diff_corrections(&previous, &rows, now);

        diesel::insert_into(meal_items::table)
            .values(&rows)
            .execute(conn)?;
        if !corrections.is_empty() {
            diesel::insert_into(crate::schema::item_corrections::table)
                .values(&corrections)
                .execute(conn)?;
        }

        diesel::update(meals::table.filter(meals::id.eq(meal_id)))
            .set((
                meals::revision.eq(revision),
                meals::dish_name.eq(Some(outcome.estimate.dish_name.clone())),
                meals::dish_name_normalized.eq(Some(crate::models::normalize_dish_name(
                    &outcome.estimate.dish_name,
                ))),
                meals::updated_at.eq(now),
            ))
            .execute(conn)?;

        Ok(())
    })
}

/// Diff the previous revision's items against the new ones, item by item.
///
/// Matched by name, because that is what the user is actually correcting ("too
/// much rice"): positions shift when the agent adds or drops a component, so
/// pairing on position would attribute the rice delta to the chicken. An item
/// with no counterpart in the previous revision is a genuine addition and has no
/// delta to record.
pub fn diff_corrections(
    previous: &[MealItem],
    current: &[NewMealItem],
    at: chrono::NaiveDateTime,
) -> Vec<NewItemCorrection> {
    let mut corrections = Vec::new();

    for item in current {
        let normalized = crate::models::normalize_dish_name(&item.name);
        let Some(before) = previous
            .iter()
            .find(|p| crate::models::normalize_dish_name(&p.name) == normalized)
        else {
            continue;
        };

        for (field, original, corrected) in [
            (MEAL_ITEM_FIELD_GRAMS, before.grams, item.grams),
            (MEAL_ITEM_FIELD_KCAL, before.kcal, item.kcal),
            (MEAL_ITEM_FIELD_PROTEIN, before.protein_g, item.protein_g),
            (MEAL_ITEM_FIELD_FAT, before.fat_g, item.fat_g),
            (MEAL_ITEM_FIELD_CARBS, before.carbs_g, item.carbs_g),
        ] {
            if (original - corrected).abs() < CORRECTION_EPSILON {
                continue;
            }
            corrections.push(NewItemCorrection {
                id: uuid::Uuid::new_v4().to_string(),
                // The correction belongs to the row that now holds the value, so
                // the audit trail hangs off the revision the user can see.
                meal_item_id: item.id.clone(),
                field: field.to_string(),
                original_value: Some(format!("{original}")),
                corrected_value: Some(format!("{corrected}")),
                corrected_at: at,
            });
        }
    }

    corrections
}

/// Smaller than this and the "change" is float noise, not a correction.
pub const CORRECTION_EPSILON: f64 = 0.01;

/// `item_corrections.field` values written by a re-analysis.
pub const MEAL_ITEM_FIELD_GRAMS: &str = "grams";
/// See [`MEAL_ITEM_FIELD_GRAMS`].
pub const MEAL_ITEM_FIELD_KCAL: &str = "kcal";
/// See [`MEAL_ITEM_FIELD_GRAMS`].
pub const MEAL_ITEM_FIELD_PROTEIN: &str = "protein_g";
/// See [`MEAL_ITEM_FIELD_GRAMS`].
pub const MEAL_ITEM_FIELD_FAT: &str = "fat_g";
/// See [`MEAL_ITEM_FIELD_GRAMS`].
pub const MEAL_ITEM_FIELD_CARBS: &str = "carbs_g";

#[cfg(test)]
mod tests {
    use super::*;

    fn now() -> chrono::NaiveDateTime {
        chrono::NaiveDate::from_ymd_opt(2026, 8, 1)
            .expect("valid date")
            .and_hms_opt(12, 0, 0)
            .expect("valid time")
    }

    fn previous_item(name: &str, grams: f64, kcal: f64) -> MealItem {
        MealItem {
            id: format!("old-{name}"),
            meal_id: "m1".into(),
            revision: 1,
            position: 0,
            name: name.into(),
            barcode: None,
            grams,
            grams_source: GramsSource::Agent.as_str().into(),
            kcal,
            protein_g: 0.0,
            fat_g: 0.0,
            carbs_g: 0.0,
            macro_source: MacroSource::Model.as_str().into(),
            confidence: Some(0.6),
            reasoning_note: None,
            created_at: now(),
        }
    }

    fn current_item(name: &str, grams: f64, kcal: f64) -> NewMealItem {
        NewMealItem {
            id: format!("new-{name}"),
            meal_id: "m1".into(),
            revision: 2,
            position: 0,
            name: name.into(),
            barcode: None,
            grams,
            grams_source: GramsSource::Agent.as_str().into(),
            kcal,
            protein_g: 0.0,
            fat_g: 0.0,
            carbs_g: 0.0,
            macro_source: MacroSource::Model.as_str().into(),
            confidence: Some(0.6),
            reasoning_note: None,
            created_at: now(),
        }
    }

    #[test]
    fn halving_the_rice_is_recorded_against_the_new_row() {
        let previous = vec![
            previous_item("rice", 300.0, 400.0),
            previous_item("curry", 250.0, 500.0),
        ];
        // The agent re-ordered the items and halved the rice.
        let current = vec![
            current_item("curry", 250.0, 500.0),
            current_item("rice", 150.0, 200.0),
        ];

        let corrections = diff_corrections(&previous, &current, now());

        // Only the rice moved, and only its grams and kcal.
        assert_eq!(corrections.len(), 2);
        assert!(corrections.iter().all(|c| c.meal_item_id == "new-rice"));
        let fields: Vec<&str> = corrections.iter().map(|c| c.field.as_str()).collect();
        assert!(fields.contains(&MEAL_ITEM_FIELD_GRAMS));
        assert!(fields.contains(&MEAL_ITEM_FIELD_KCAL));
        let grams = corrections
            .iter()
            .find(|c| c.field == MEAL_ITEM_FIELD_GRAMS)
            .expect("a grams correction");
        assert_eq!(grams.original_value.as_deref(), Some("300"));
        assert_eq!(grams.corrected_value.as_deref(), Some("150"));
    }

    #[test]
    fn a_newly_added_item_has_nothing_to_diff_against() {
        let previous = vec![previous_item("rice", 300.0, 400.0)];
        let current = vec![
            current_item("rice", 300.0, 400.0),
            current_item("sour cream", 30.0, 60.0),
        ];
        assert!(diff_corrections(&previous, &current, now()).is_empty());
    }

    #[test]
    fn item_names_are_matched_after_normalization() {
        let previous = vec![previous_item("Рис", 300.0, 400.0)];
        let current = vec![current_item("рис", 150.0, 400.0)];
        let corrections = diff_corrections(&previous, &current, now());
        assert_eq!(corrections.len(), 1);
        assert_eq!(corrections[0].field, MEAL_ITEM_FIELD_GRAMS);
    }

    #[test]
    fn float_noise_is_not_a_correction() {
        let previous = vec![previous_item("rice", 300.0, 400.0)];
        let current = vec![current_item("rice", 300.000_001, 400.0)];
        assert!(diff_corrections(&previous, &current, now()).is_empty());
    }

    #[test]
    fn the_resume_step_records_the_path_and_what_it_compared_against() {
        let step = resume_step(
            ResumeDecision::Reseed,
            Some("gpt-4.1"),
            Some("2026-01-01.1"),
        );
        assert_eq!(step.step_no, 0);
        assert_eq!(step.tool_name, RESUME_STEP);
        let input = step.tool_input.expect("the decision is recorded");
        assert!(input.contains("\"decision\":\"reseed\""), "{input}");
        assert!(input.contains("2026-01-01.1"), "{input}");

        let step = resume_step(ResumeDecision::Continue, Some("gpt-4.1"), Some("x"));
        assert!(step
            .tool_input
            .expect("the decision is recorded")
            .contains("\"decision\":\"continue\""));
    }

    // -- revision persistence against a real database ----------------------

    use crate::agent::schema::{EstimatedItem, MealEstimate};
    use crate::agent::RunUsage;
    use crate::feedback::state::fixtures::{seed_user, test_conn};
    use crate::models::{MealStatus, NameSource, NewAnalysisJob, NewMeal};

    fn seed_meal(conn: &mut SqliteConnection, id: &str, user_id: &str, revision: i32) {
        diesel::insert_into(crate::schema::meals::table)
            .values(&NewMeal {
                id: id.to_string(),
                user_id: user_id.to_string(),
                client_uuid: format!("client-{id}"),
                thumbnail_id: None,
                group_id: None,
                group_size: None,
                dish_name: Some("карри с рисом".into()),
                dish_name_normalized: Some("карри с рисом".into()),
                name_source: NameSource::Vision.as_str().to_string(),
                user_comment: None,
                revision,
                eaten_at: now(),
                timezone_offset: 180,
                meal_type: None,
                status: MealStatus::NeedsReview.as_str().to_string(),
                portion_scale: 1.0,
                created_at: now(),
                updated_at: now(),
            })
            .execute(conn)
            .expect("seed meal");
    }

    fn seed_items(conn: &mut SqliteConnection, meal_id: &str, revision: i32, items: &[MealItem]) {
        for (position, item) in items.iter().enumerate() {
            diesel::insert_into(crate::schema::meal_items::table)
                .values(&NewMealItem {
                    // A fresh id per row: the same fixture item is seeded at
                    // more than one revision, and `meal_items.id` is unique.
                    id: uuid::Uuid::new_v4().to_string(),
                    meal_id: meal_id.to_string(),
                    revision,
                    position: position as i32,
                    name: item.name.clone(),
                    barcode: None,
                    grams: item.grams,
                    grams_source: item.grams_source.clone(),
                    kcal: item.kcal,
                    protein_g: item.protein_g,
                    fat_g: item.fat_g,
                    carbs_g: item.carbs_g,
                    macro_source: item.macro_source.clone(),
                    confidence: item.confidence,
                    reasoning_note: item.reasoning_note.clone(),
                    created_at: now(),
                })
                .execute(conn)
                .expect("seed item");
        }
    }

    fn seed_job(conn: &mut SqliteConnection, id: &str, meal_id: &str, revision: i32) {
        diesel::insert_into(crate::schema::analysis_jobs::table)
            .values(&NewAnalysisJob {
                id: id.to_string(),
                meal_id: meal_id.to_string(),
                revision,
                parent_job_id: None,
                status: crate::models::JobStatus::Succeeded.as_str().to_string(),
                attempts: 1,
                model: "gpt-4.1".into(),
                user_feedback: None,
                steps: 0,
                tool_calls: 0,
                prompt_tokens: 0,
                completion_tokens: 0,
                cost_micro_usd: 0,
                error: None,
                started_at: Some(now()),
                created_at: now(),
                finished_at: Some(now()),
            })
            .execute(conn)
            .expect("seed job");
    }

    fn correction_outcome(items: Vec<EstimatedItem>) -> AgentOutcome {
        AgentOutcome {
            estimate: MealEstimate {
                dish_name: "карри с рисом".into(),
                cuisine: None,
                container: Some("delivery bowl".into()),
                from_recall: false,
                items,
                overall_confidence: 0.7,
                notes: None,
            },
            serialized_messages: None,
            turn_count: 4,
            steps: Vec::new(),
            usage: RunUsage::default(),
            fallback_used: false,
            model: "gpt-4.1".into(),
            prompt_version: "2026-08-01.2".into(),
        }
    }

    fn estimated(name: &str, grams: f64, kcal: f64) -> EstimatedItem {
        EstimatedItem {
            name: name.into(),
            grams,
            kcal,
            protein_g: 0.0,
            fat_g: 0.0,
            carbs_g: 0.0,
            confidence: 0.8,
            reasoning_note: "corrected".into(),
            barcode: None,
            grams_source: GramsSource::Agent,
            macro_source: MacroSource::Model,
        }
    }

    #[test]
    fn the_next_revision_clears_both_the_meal_and_its_jobs() {
        let mut conn = test_conn();
        seed_user(&mut conn, "u1");
        seed_meal(&mut conn, "m1", "u1", 1);
        assert_eq!(next_revision(&mut conn, "m1").expect("computes"), 2);

        // A job for revision 2 is already in flight, so 2 is taken.
        seed_job(&mut conn, "j2", "m1", 2);
        assert_eq!(next_revision(&mut conn, "m1").expect("computes"), 3);
    }

    #[test]
    fn the_next_revision_of_an_unknown_meal_is_a_404() {
        let mut conn = test_conn();
        let err = next_revision(&mut conn, "nope").expect_err("no such meal");
        assert!(matches!(err, AppError::NotFound(_)), "{err:?}");
    }

    #[test]
    fn an_estimate_can_be_rebuilt_from_a_specific_revision() {
        let mut conn = test_conn();
        seed_user(&mut conn, "u1");
        seed_meal(&mut conn, "m1", "u1", 2);
        seed_items(
            &mut conn,
            "m1",
            1,
            &[previous_item("рис", 300.0, 400.0), previous_item("карри", 250.0, 500.0)],
        );
        seed_items(&mut conn, "m1", 2, &[previous_item("рис", 150.0, 200.0)]);

        let first = estimate_at_revision(&mut conn, "m1", 1).expect("revision 1 is retained");
        assert_eq!(first.items.len(), 2);
        assert_eq!(first.totals().kcal, 900.0);

        let second = estimate_at_revision(&mut conn, "m1", 2).expect("revision 2 exists");
        assert_eq!(second.items.len(), 1);
        assert_eq!(second.items[0].grams, 150.0);

        assert!(estimate_at_revision(&mut conn, "m1", 9).is_err());
    }

    #[test]
    fn a_correction_writes_a_new_revision_keeps_the_old_and_records_the_delta() {
        use crate::schema::{item_corrections, meal_items, meals};

        let mut conn = test_conn();
        seed_user(&mut conn, "u1");
        seed_meal(&mut conn, "m1", "u1", 1);
        seed_items(
            &mut conn,
            "m1",
            1,
            &[previous_item("рис", 300.0, 400.0), previous_item("карри", 250.0, 500.0)],
        );

        let outcome = correction_outcome(vec![
            estimated("рис", 150.0, 200.0),
            estimated("карри", 250.0, 500.0),
        ]);
        persist_revision(&mut conn, "m1", 2, &outcome).expect("persists");

        // The prior revision is untouched...
        let old: Vec<MealItem> = meal_items::table
            .filter(meal_items::meal_id.eq("m1"))
            .filter(meal_items::revision.eq(1))
            .select(MealItem::as_select())
            .load(&mut conn)
            .expect("loads");
        assert_eq!(old.len(), 2);
        assert_eq!(old.iter().find(|i| i.name == "рис").unwrap().grams, 300.0);

        // ...the new one holds the corrected figures...
        let new: Vec<MealItem> = meal_items::table
            .filter(meal_items::meal_id.eq("m1"))
            .filter(meal_items::revision.eq(2))
            .select(MealItem::as_select())
            .load(&mut conn)
            .expect("loads");
        assert_eq!(new.len(), 2);
        assert_eq!(new.iter().find(|i| i.name == "рис").unwrap().grams, 150.0);

        // ...the meal points at it...
        let revision: i32 = meals::table
            .filter(meals::id.eq("m1"))
            .select(meals::revision)
            .first(&mut conn)
            .expect("loads");
        assert_eq!(revision, 2);

        // ...and the correction is auditable.
        let corrections: i64 = item_corrections::table
            .count()
            .get_result(&mut conn)
            .expect("counts");
        assert_eq!(corrections, 2, "expected grams and kcal deltas for the rice");
    }

    #[test]
    fn persisting_a_revision_twice_is_idempotent() {
        use crate::schema::meal_items;

        let mut conn = test_conn();
        seed_user(&mut conn, "u1");
        seed_meal(&mut conn, "m1", "u1", 1);
        seed_items(&mut conn, "m1", 1, &[previous_item("рис", 300.0, 400.0)]);

        let outcome = correction_outcome(vec![estimated("рис", 150.0, 200.0)]);
        persist_revision(&mut conn, "m1", 2, &outcome).expect("persists");
        persist_revision(&mut conn, "m1", 2, &outcome).expect("persists again");

        let count: i64 = meal_items::table
            .filter(meal_items::meal_id.eq("m1"))
            .filter(meal_items::revision.eq(2))
            .count()
            .get_result(&mut conn)
            .expect("counts");
        assert_eq!(count, 1, "a retry doubled the items");
    }

    #[test]
    fn a_rebuilt_estimate_flags_recall_sourced_items() {
        let meal = Meal {
            id: "m1".into(),
            user_id: "u1".into(),
            client_uuid: "c1".into(),
            thumbnail_id: None,
            group_id: None,
            group_size: None,
            dish_name: Some("шаурма".into()),
            dish_name_normalized: Some("шаурма".into()),
            name_source: crate::models::NameSource::Vision.as_str().into(),
            user_comment: None,
            revision: 2,
            eaten_at: now(),
            timezone_offset: 180,
            meal_type: None,
            status: crate::models::MealStatus::Confirmed.as_str().into(),
            portion_scale: 1.0,
            created_at: now(),
            updated_at: now(),
        };
        let mut recalled = previous_item("lavash", 90.0, 250.0);
        recalled.macro_source = MacroSource::Recall.as_str().into();

        let estimate = to_estimate(&meal, &[recalled, previous_item("chicken", 120.0, 200.0)]);
        assert_eq!(estimate.dish_name, "шаурма");
        assert!(estimate.from_recall);
        assert_eq!(estimate.items.len(), 2);
        assert_eq!(estimate.totals().kcal, 450.0);
        assert!((estimate.overall_confidence - 0.6).abs() < 1e-9);
    }
}
