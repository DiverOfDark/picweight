//! `GET /api/v1/usage` — what the agent has cost you.
//!
//! Every analysis already records its model, token counts and wall clock on
//! `analysis_jobs` (PRD §13 asks for exactly that, so p95 latency and spend are
//! answerable). Until now nothing read those columns back. This is the reader.
//!
//! # Why the cost is recomputed rather than summed
//!
//! `analysis_jobs.cost_micro_usd` is written once, when the job finishes, using
//! whatever rates were configured *then*. It is kept for forensics, but this
//! endpoint deliberately does **not** sum it. Instead it sums tokens — which are
//! a measured fact that never changes — and applies the rates in force right
//! now.
//!
//! The difference matters the first time someone sets `PICWEIGHT_MODEL_PRICING`.
//! Summing the stored column would leave every historical row priced at the old
//! guess forever, so the total would be a blend of two pricing regimes that
//! matches neither. Recomputing means correcting your rates retroactively
//! corrects the history, which is the behaviour anyone would expect from a
//! screen labelled "estimate".
//!
//! # Why the provenance is in the payload
//!
//! A figure derived from a rate the operator supplied is worth acting on. One
//! derived from [`DEFAULT_PRICING`](crate::jobs::analyzer::DEFAULT_PRICING)
//! because nothing matched is a guess with a currency symbol attached. The
//! response carries [`PricingSource`] per model so the UI can say which it is;
//! presenting them identically would be the same failure as an OpenAPI document
//! that lies about its own enum casing.

use axum::extract::{Query, State};
use axum::Json;
use chrono::{DateTime, NaiveDate, Utc};
use diesel::prelude::*;
use serde::{Deserialize, Serialize};
use utoipa::{IntoParams, ToSchema};

use crate::auth::CurrentUser;
use crate::error::AppError;
use crate::jobs::analyzer::{apply_rate, resolve_pricing, PricingSource};
use crate::AppState;

/// Query string of `GET /api/v1/usage`.
#[derive(Debug, Deserialize, IntoParams)]
pub struct UsageQuery {
    /// Inclusive local start date, `YYYY-MM-DD`. Defaults to 30 days back.
    pub from: Option<NaiveDate>,
    /// Inclusive local end date, `YYYY-MM-DD`. Defaults to today.
    pub to: Option<NaiveDate>,
}

/// Spend and tokens for one model.
#[derive(Debug, Serialize, ToSchema)]
pub struct ModelUsage {
    /// Model id exactly as recorded on the job.
    pub model: String,
    /// Analysis jobs run on it in the window.
    pub jobs: i64,
    /// Input tokens.
    pub prompt_tokens: i64,
    /// Output tokens.
    pub completion_tokens: i64,
    /// Estimated spend, micro-USD, at current rates.
    pub cost_micro_usd: i64,
    /// Input rate applied, micro-USD per million tokens.
    pub input_rate_micro_usd: i64,
    /// Output rate applied, micro-USD per million tokens.
    pub output_rate_micro_usd: i64,
    /// Whether that rate was configured, compiled in, or a fallback guess.
    pub pricing_source: PricingSource,
}

/// One day's totals, for the trend line.
#[derive(Debug, Serialize, ToSchema)]
pub struct DailyUsage {
    /// `YYYY-MM-DD`, bucketed by the job's creation instant in UTC.
    pub date: String,
    /// Jobs that ran.
    pub jobs: i64,
    /// Input + output tokens.
    pub total_tokens: i64,
    /// Estimated spend, micro-USD.
    pub cost_micro_usd: i64,
}

/// Body of `GET /api/v1/usage`.
#[derive(Debug, Serialize, ToSchema)]
pub struct UsageResponse {
    /// Window start, echoed back.
    pub from: String,
    /// Window end, echoed back.
    pub to: String,
    /// Every analysis job in the window, whatever its outcome.
    pub jobs: i64,
    /// Jobs that failed terminally — spend that bought nothing.
    pub failed_jobs: i64,
    /// Jobs that were a retry of an earlier attempt (`parent_job_id` set).
    pub retried_jobs: i64,
    /// Distinct meals analysed. Lower than `jobs` when retries or corrections ran.
    pub meals: i64,
    /// Input tokens.
    pub prompt_tokens: i64,
    /// Output tokens.
    pub completion_tokens: i64,
    /// Estimated spend across the window, micro-USD, at current rates.
    pub cost_micro_usd: i64,
    /// Mean spend per analysed meal, micro-USD. Zero when nothing was analysed.
    pub cost_per_meal_micro_usd: i64,
    /// True when any model in the window priced off the fallback, so the UI can
    /// caveat the headline number instead of presenting a guess as a total.
    pub has_estimated_pricing: bool,
    /// Per-model breakdown, most expensive first.
    pub by_model: Vec<ModelUsage>,
    /// Per-day series, oldest first.
    pub by_day: Vec<DailyUsage>,
}

/// A job row reduced to the columns this endpoint needs.
struct JobRow {
    model: String,
    prompt_tokens: i64,
    completion_tokens: i64,
    status: String,
    is_retry: bool,
    meal_id: String,
    created_at: DateTime<Utc>,
}

/// `GET /api/v1/usage` — token and spend totals for the signed-in user.
#[utoipa::path(
    get,
    path = "/api/v1/usage",
    tag = "usage",
    params(UsageQuery),
    summary = "Token and cost totals for the agent runs this user has caused",
    description = "Sums `analysis_jobs` for the caller. Cost is recomputed from token \
counts at the rates currently configured, never summed from the stored column, so \
correcting `PICWEIGHT_MODEL_PRICING` also corrects the history. Each model reports \
whether its rate was configured, compiled in, or a fallback guess.",
    responses(
        (status = 200, description = "Usage totals", body = UsageResponse),
        (status = 401, description = "No session", body = crate::error::ErrorBody),
    ),
    security(("session" = []))
)]
pub async fn get_usage(
    State(state): State<AppState>,
    user: CurrentUser,
    Query(params): Query<UsageQuery>,
) -> Result<Json<UsageResponse>, AppError> {
    let to = params.to.unwrap_or_else(|| Utc::now().date_naive());
    let from = params
        .from
        .unwrap_or_else(|| to - chrono::Duration::days(29));
    if from > to {
        return Err(AppError::BadRequest(format!(
            "`from` ({from}) is after `to` ({to})"
        )));
    }

    let rows = load_jobs(&state, &user.id, from, to)?;
    Ok(Json(summarise(&state.config.model_pricing, from, to, rows)))
}

/// Every analysis job this user caused in the window.
///
/// Scoped through `meals.user_id` — `analysis_jobs` has no `user_id` of its own,
/// and every user-scoped query in this codebase filters on ownership (PRD §8).
fn load_jobs(
    state: &AppState,
    user_id: &str,
    from: NaiveDate,
    to: NaiveDate,
) -> Result<Vec<JobRow>, AppError> {
    use crate::schema::{analysis_jobs, meals};

    let mut conn = state.pool.get()?;
    let start = from.and_hms_opt(0, 0, 0).unwrap_or_default();
    // Exclusive upper bound at midnight after `to`, so the whole last day counts.
    let end = (to + chrono::Duration::days(1))
        .and_hms_opt(0, 0, 0)
        .unwrap_or_default();

    /// `(model, prompt, completion, status, parent_job_id, meal_id, created_at)`
    /// — the select tuple, named so the signature stays readable.
    type Selected = (
        String,
        i64,
        i64,
        String,
        Option<String>,
        String,
        chrono::NaiveDateTime,
    );

    let rows: Vec<Selected> = analysis_jobs::table
            .inner_join(meals::table.on(meals::id.eq(analysis_jobs::meal_id)))
            .filter(meals::user_id.eq(user_id))
            .filter(analysis_jobs::created_at.ge(start))
            .filter(analysis_jobs::created_at.lt(end))
            .select((
                analysis_jobs::model,
                analysis_jobs::prompt_tokens,
                analysis_jobs::completion_tokens,
                analysis_jobs::status,
                analysis_jobs::parent_job_id,
                analysis_jobs::meal_id,
                analysis_jobs::created_at,
            ))
            .load(&mut conn)?;

    Ok(rows
        .into_iter()
        .map(
            |(model, prompt_tokens, completion_tokens, status, parent, meal_id, created_at)| JobRow {
                model,
                prompt_tokens,
                completion_tokens,
                status,
                is_retry: parent.is_some(),
                meal_id,
                created_at: created_at.and_utc(),
            },
        )
        .collect())
}

/// Fold job rows into the response. Pure, so the arithmetic is unit-testable
/// without a database.
fn summarise(
    pricing: &[(String, i64, i64)],
    from: NaiveDate,
    to: NaiveDate,
    rows: Vec<JobRow>,
) -> UsageResponse {
    use std::collections::{BTreeMap, HashSet};

    let mut per_model: BTreeMap<String, (i64, i64, i64)> = BTreeMap::new();
    let mut per_day: BTreeMap<String, (i64, i64, i64)> = BTreeMap::new();
    let mut meals: HashSet<String> = HashSet::new();
    let (mut jobs, mut failed, mut retried) = (0i64, 0i64, 0i64);

    for row in &rows {
        jobs += 1;
        if row.status == "failed" {
            failed += 1;
        }
        if row.is_retry {
            retried += 1;
        }
        meals.insert(row.meal_id.clone());

        let model = per_model.entry(row.model.clone()).or_insert((0, 0, 0));
        model.0 += 1;
        model.1 += row.prompt_tokens.max(0);
        model.2 += row.completion_tokens.max(0);

        let (input_rate, output_rate, _) = resolve_pricing(pricing, &row.model);
        let cost = apply_rate(
            input_rate,
            output_rate,
            row.prompt_tokens,
            row.completion_tokens,
        );

        let day = per_day
            .entry(row.created_at.format("%Y-%m-%d").to_string())
            .or_insert((0, 0, 0));
        day.0 += 1;
        day.1 += row.prompt_tokens.max(0) + row.completion_tokens.max(0);
        day.2 += cost;
    }

    let mut by_model: Vec<ModelUsage> = per_model
        .into_iter()
        .map(|(model, (jobs, prompt, completion))| {
            let (input_rate, output_rate, pricing_source) = resolve_pricing(pricing, &model);
            ModelUsage {
                cost_micro_usd: apply_rate(input_rate, output_rate, prompt, completion),
                model,
                jobs,
                prompt_tokens: prompt,
                completion_tokens: completion,
                input_rate_micro_usd: input_rate,
                output_rate_micro_usd: output_rate,
                pricing_source,
            }
        })
        .collect();
    by_model.sort_by(|a, b| {
        b.cost_micro_usd
            .cmp(&a.cost_micro_usd)
            .then_with(|| a.model.cmp(&b.model))
    });

    let prompt_tokens: i64 = by_model.iter().map(|m| m.prompt_tokens).sum();
    let completion_tokens: i64 = by_model.iter().map(|m| m.completion_tokens).sum();
    let cost_micro_usd: i64 = by_model.iter().map(|m| m.cost_micro_usd).sum();
    let meal_count = meals.len() as i64;

    UsageResponse {
        from: from.to_string(),
        to: to.to_string(),
        jobs,
        failed_jobs: failed,
        retried_jobs: retried,
        meals: meal_count,
        prompt_tokens,
        completion_tokens,
        cost_micro_usd,
        cost_per_meal_micro_usd: if meal_count > 0 {
            cost_micro_usd / meal_count
        } else {
            0
        },
        has_estimated_pricing: by_model
            .iter()
            .any(|m| m.pricing_source == PricingSource::Fallback),
        by_model,
        by_day: per_day
            .into_iter()
            .map(|(date, (jobs, total_tokens, cost))| DailyUsage {
                date,
                jobs,
                total_tokens,
                cost_micro_usd: cost,
            })
            .collect(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn job(model: &str, prompt: i64, completion: i64, meal: &str) -> JobRow {
        JobRow {
            model: model.to_string(),
            prompt_tokens: prompt,
            completion_tokens: completion,
            status: "succeeded".into(),
            is_retry: false,
            meal_id: meal.to_string(),
            created_at: "2026-08-01T12:00:00Z".parse().expect("valid instant"),
        }
    }

    fn window() -> (NaiveDate, NaiveDate) {
        (
            NaiveDate::from_ymd_opt(2026, 8, 1).expect("valid"),
            NaiveDate::from_ymd_opt(2026, 8, 1).expect("valid"),
        )
    }

    #[test]
    fn a_configured_rate_beats_the_built_in_table() {
        let configured = vec![("gpt-4.1".to_string(), 1, 2)];
        let (input, output, source) = resolve_pricing(&configured, "gpt-4.1");
        assert_eq!((input, output), (1, 2));
        assert_eq!(source, PricingSource::Configured);
    }

    #[test]
    fn an_unpriced_model_is_reported_as_a_guess_not_as_a_total() {
        let (_, _, source) = resolve_pricing(&[], "gpt-5.4-mini");
        assert_eq!(
            source,
            PricingSource::Fallback,
            "gpt-5.4 is not in the built-in table; saying otherwise would make the \
             dollar figure look sourced when it is not"
        );

        let (from, to) = window();
        let summary = summarise(&[], from, to, vec![job("gpt-5.4-mini", 1_000_000, 0, "m1")]);
        assert!(summary.has_estimated_pricing);
    }

    #[test]
    fn configuring_a_rate_reprices_history_rather_than_leaving_it_stale() {
        let (from, to) = window();
        let rows = || vec![job("gpt-5.4-mini", 1_000_000, 1_000_000, "m1")];

        let guessed = summarise(&[], from, to, rows());
        let priced = summarise(
            &[("gpt-5.4-mini".to_string(), 250_000, 2_000_000)],
            from,
            to,
            rows(),
        );

        // 1M in at 0.25 + 1M out at 2.00 = 2.25 USD.
        assert_eq!(priced.cost_micro_usd, 2_250_000);
        assert_ne!(guessed.cost_micro_usd, priced.cost_micro_usd);
        assert!(!priced.has_estimated_pricing);
    }

    #[test]
    fn longest_prefix_wins_so_mini_is_never_priced_as_the_full_model() {
        let (_, _, _) = resolve_pricing(&[], "gpt-4.1-mini");
        let mini = apply_rate(
            resolve_pricing(&[], "gpt-4.1-mini").0,
            resolve_pricing(&[], "gpt-4.1-mini").1,
            1_000_000,
            1_000_000,
        );
        let full = apply_rate(
            resolve_pricing(&[], "gpt-4.1").0,
            resolve_pricing(&[], "gpt-4.1").1,
            1_000_000,
            1_000_000,
        );
        assert!(mini < full, "mini must not be priced as the full model");
    }

    #[test]
    fn retries_and_failures_are_counted_because_they_cost_money_too() {
        let (from, to) = window();
        let mut failed = job("gpt-4.1", 100, 100, "m1");
        failed.status = "failed".into();
        let mut retry = job("gpt-4.1", 100, 100, "m1");
        retry.is_retry = true;

        let summary = summarise(&[], from, to, vec![failed, retry]);
        assert_eq!(summary.jobs, 2);
        assert_eq!(summary.failed_jobs, 1);
        assert_eq!(summary.retried_jobs, 1);
        // Both jobs belong to the same meal, so per-meal cost divides by one.
        assert_eq!(summary.meals, 1);
        assert_eq!(summary.cost_per_meal_micro_usd, summary.cost_micro_usd);
    }

    #[test]
    fn an_empty_window_divides_by_nothing() {
        let (from, to) = window();
        let summary = summarise(&[], from, to, Vec::new());
        assert_eq!(summary.cost_per_meal_micro_usd, 0);
        assert!(!summary.has_estimated_pricing);
        assert!(summary.by_model.is_empty());
    }

    #[test]
    fn models_are_ranked_by_spend_so_the_expensive_one_reads_first() {
        let (from, to) = window();
        let summary = summarise(
            &[],
            from,
            to,
            vec![
                job("gpt-4.1-nano", 1_000_000, 0, "m1"),
                job("gpt-4.1", 1_000_000, 0, "m2"),
            ],
        );
        assert_eq!(summary.by_model[0].model, "gpt-4.1");
    }
}
