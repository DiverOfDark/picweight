//! Meal ingest, retrieval, correction and history.
//!
//! Ingest is idempotent on `client_uuid` (PRD §8): the phone generates it at
//! capture, so a retried upload after a flaky connection returns the existing
//! meal rather than creating a second one. This is the single most important
//! field for offline correctness.
//!
//! Upload returns **202**, never a result. Analysis is an async job (§7).

use crate::agent::schema::{EstimatedItem, MacroTotals, MealEstimate};
use crate::api::events::{MealEvent, MealEventKind};
use crate::auth::CurrentUser;
use crate::error::AppError;
use crate::feedback::state::local_date_of;
use crate::jobs::{Job, JobKind};
use crate::models::{
    normalize_dish_name, GramsSource, JobStatus, MacroSource, Meal, MealChangeset, MealItem,
    MealItemChangeset, MealStatus, NameSource, NewAnalysisJob, NewItemCorrection, NewMeal,
    NewMealItem,
};
use crate::schema::{analysis_jobs, meal_items, meals, thumbnails};
use crate::AppState;
use axum::body::Body;
use axum::extract::{Multipart, Path, Query, State};
use axum::http::{header, StatusCode};
use axum::response::Response;
use axum::Json;
use chrono::{DateTime, NaiveDate, NaiveDateTime, Utc};
use diesel::prelude::*;
use diesel::SqliteConnection;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use utoipa::{IntoParams, ToSchema};

/// Largest multipart body accepted, in bytes.
pub const MAX_BODY_BYTES: usize = 32 * 1024 * 1024;

/// Default page size for history queries.
pub const DEFAULT_HISTORY_LIMIT: i64 = 100;

/// Hard cap on a history page, so a client cannot ask for everything at once.
pub const MAX_HISTORY_LIMIT: i64 = 1000;

/// Largest item list a `PATCH` may contain.
pub const MAX_PATCH_ITEMS: usize = 100;

/// Temporary offset applied to `meal_items.position` while a `PATCH` renumbers
/// a list.
///
/// `(meal_id, revision, position)` is `UNIQUE`, so reordering in place would
/// transiently collide (swapping two items is enough). Shifting every existing
/// row out of the final range first makes the renumber safe without deleting
/// rows — which matters, because `item_corrections` cascades off `meal_items`
/// and deleting a row would take its correction history with it.
const POSITION_SHIFT: i32 = 100_000;

/// Widest UTC offset any client can plausibly report, in minutes.
const MAX_TZ_OFFSET_MINUTES: i32 = 14 * 60;

// ---------------------------------------------------------------------------
// Response DTOs
// ---------------------------------------------------------------------------

/// A meal with its items at the current revision.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct MealResponse {
    /// `meals.id`.
    pub id: String,
    /// Client-generated idempotency key.
    pub client_uuid: String,
    /// The sitting this meal belongs to, if any.
    pub group_id: Option<String>,
    /// Client hint for how many photos the sitting will contain.
    pub group_size: Option<i32>,
    /// Dish name as displayed.
    pub dish_name: Option<String>,
    /// Which input path supplied the name — instrumented per §14.2.
    pub name_source: NameSource,
    /// The user's comment, when they typed one.
    pub user_comment: Option<String>,
    /// Current revision.
    pub revision: i32,
    /// Lifecycle status.
    pub status: MealStatus,
    /// When it was eaten (UTC).
    pub eaten_at: DateTime<Utc>,
    /// Minutes east of UTC at capture; buckets the local day.
    pub timezone_offset: i32,
    /// Optional meal-type label ("breakfast", "snack", …).
    pub meal_type: Option<String>,
    /// "Ate 60% of it" multiplier; already applied to `items` and `totals`.
    pub portion_scale: f64,
    /// Relative URL of the 768px thumbnail, or `None` for a manual entry.
    pub thumbnail_url: Option<String>,
    /// Items at `revision`, in display order.
    pub items: Vec<MealItemResponse>,
    /// Sum of `items`.
    pub totals: MacroTotals,
    /// Failure reason when `status` is `failed` — visible, never silent (§5).
    pub error: Option<String>,
    /// Row creation time.
    pub created_at: DateTime<Utc>,
    /// Last modification time.
    pub updated_at: DateTime<Utc>,
}

/// One component of a meal.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct MealItemResponse {
    /// `meal_items.id`.
    pub id: String,
    /// Display order.
    pub position: i32,
    /// Component name.
    pub name: String,
    /// EAN, when resolved from a barcode.
    pub barcode: Option<String>,
    /// Edible mass in grams.
    pub grams: f64,
    /// Provenance of `grams`.
    pub grams_source: GramsSource,
    /// Energy.
    pub kcal: f64,
    /// Protein, grams.
    pub protein_g: f64,
    /// Fat, grams.
    pub fat_g: f64,
    /// Carbohydrate, grams.
    pub carbs_g: f64,
    /// Provenance of the macros.
    pub macro_source: MacroSource,
    /// 0.0–1.0.
    pub confidence: Option<f64>,
    /// One line of "why", shown next to the item.
    pub reasoning_note: Option<String>,
}

/// 202 body returned by ingest and by re-analysis.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct MealAcceptedResponse {
    /// The meal that was created or resumed.
    pub meal_id: String,
    /// Always `pending` for a new upload; the revision being produced for a
    /// re-analysis.
    pub status: MealStatus,
    /// Revision the enqueued job will write.
    pub revision: i32,
    /// True when an existing meal was returned because `client_uuid` had
    /// already been seen — the idempotent-replay path.
    pub deduplicated: bool,
}

/// One entry of a meal's revision history.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct RevisionEntry {
    /// Revision number, 1-based.
    pub revision: i32,
    /// The verbatim feedback that caused this revision, if any.
    pub user_feedback: Option<String>,
    /// Model that produced it.
    pub model: String,
    /// Whether the session was continued or reseeded.
    pub reseeded: bool,
    /// Items at this revision.
    pub items: Vec<MealItemResponse>,
    /// Totals at this revision.
    pub totals: MacroTotals,
    /// Tool calls made producing it. Zero on a continuation is the point (§13).
    pub tool_calls: i32,
    /// When the producing job finished.
    pub created_at: DateTime<Utc>,
}

/// Revision history for one meal.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct RevisionsResponse {
    /// The meal.
    pub meal_id: String,
    /// Newest revision first.
    pub revisions: Vec<RevisionEntry>,
}

// ---------------------------------------------------------------------------
// Request DTOs
// ---------------------------------------------------------------------------

/// The multipart form accepted by `POST /api/v1/meals`.
///
/// Declared for the OpenAPI spec; the handler reads the parts directly because
/// axum's `Multipart` is a stream.
#[derive(Debug, Clone, ToSchema)]
pub struct CreateMealForm {
    /// The photo. Optional — a manual entry has none (§5 "Manual entry").
    #[schema(format = Binary, value_type = Option<String>)]
    pub photo: Option<Vec<u8>>,
    /// Idempotency key generated on the phone at capture. Required.
    pub client_uuid: String,
    /// When the meal was eaten, RFC 3339.
    pub eaten_at: Option<String>,
    /// Minutes east of UTC at capture.
    pub timezone_offset: Option<i32>,
    /// Dish name from a chip, share intent, or comment.
    pub dish_name: Option<String>,
    /// The user's free-text comment.
    pub comment: Option<String>,
    /// Which path supplied `dish_name`.
    pub name_source: Option<NameSource>,
    /// Client-generated sitting id.
    pub group_id: Option<String>,
    /// How many photos the sitting will contain; settles the group instantly
    /// once all N are terminal.
    pub group_size: Option<i32>,
    /// Optional meal-type label.
    pub meal_type: Option<String>,
}

/// Body of `PATCH /api/v1/meals/:id` — confirm, or edit items.
#[derive(Debug, Clone, Default, Deserialize, ToSchema)]
pub struct PatchMealRequest {
    /// New status. Only `confirmed` is meaningful from a client.
    pub status: Option<MealStatus>,
    /// Corrected dish name; also rewrites `dish_name_normalized`.
    pub dish_name: Option<String>,
    /// "Ate 60% of it".
    pub portion_scale: Option<f64>,
    /// Meal-type label.
    pub meal_type: Option<String>,
    /// Full replacement item list. Omit to leave items untouched.
    ///
    /// Every changed field is written to `item_corrections` so the correction
    /// history — and the per-user `calibration_factor` derived from it — stays
    /// complete.
    pub items: Option<Vec<PatchMealItem>>,
}

/// One item in a `PATCH` body.
#[derive(Debug, Clone, Deserialize, ToSchema)]
pub struct PatchMealItem {
    /// Existing `meal_items.id`, or omitted to add a new item.
    pub id: Option<String>,
    /// Component name.
    pub name: String,
    /// Edible mass in grams.
    pub grams: f64,
    /// Energy.
    pub kcal: f64,
    /// Protein, grams.
    pub protein_g: f64,
    /// Fat, grams.
    pub fat_g: f64,
    /// Carbohydrate, grams.
    pub carbs_g: f64,
    /// EAN, when the user scanned one.
    pub barcode: Option<String>,
}

/// Body of `POST /api/v1/meals/:id/reanalyze`.
#[derive(Debug, Clone, Deserialize, ToSchema)]
pub struct ReanalyzeRequest {
    /// What is wrong, in the user's words: "too much rice, it was about half
    /// that". Stored verbatim on `analysis_jobs.user_feedback` — it is far
    /// richer signal than a gram delta.
    pub feedback: String,
}

/// Query for `GET /api/v1/meals`.
#[derive(Debug, Clone, Default, Deserialize, IntoParams)]
pub struct MealHistoryQuery {
    /// Inclusive local start date.
    pub from: Option<NaiveDate>,
    /// Inclusive local end date.
    pub to: Option<NaiveDate>,
    /// Maximum meals to return. Defaults to [`DEFAULT_HISTORY_LIMIT`].
    pub limit: Option<i64>,
    /// Rows to skip, for paging.
    pub offset: Option<i64>,
}

// ---------------------------------------------------------------------------
// Handlers
// ---------------------------------------------------------------------------

/// `POST /api/v1/meals` — ingest a photo (or a manual entry) and enqueue analysis.
#[utoipa::path(
    post,
    path = "/api/v1/meals",
    tag = "meals",
    summary = "Log a meal",
    description = "Multipart ingest. Idempotent on `client_uuid`: replaying the \
same key returns the existing meal with `deduplicated: true` rather than \
creating a duplicate. Returns 202 — analysis is always asynchronous.",
    request_body(content = CreateMealForm, content_type = "multipart/form-data"),
    responses(
        (status = 202, description = "Accepted; analysis enqueued", body = MealAcceptedResponse),
        (status = 400, description = "Missing client_uuid or unreadable image", body = crate::error::ErrorBody),
        (status = 401, description = "Not authenticated", body = crate::error::ErrorBody),
    ),
    security(("session" = []))
)]
pub async fn create_meal(
    State(state): State<AppState>,
    user: CurrentUser,
    multipart: Multipart,
) -> Result<(StatusCode, Json<MealAcceptedResponse>), AppError> {
    let form = read_ingest_parts(multipart).await?;

    let client_uuid = form
        .client_uuid
        .ok_or_else(|| AppError::BadRequest("client_uuid is required".to_string()))?;

    // A meal with neither a photo nor a name gives the agent nothing to work
    // from; §5's manual-entry path always carries at least a dish name.
    if form.photo.is_none() && form.dish_name.is_none() {
        return Err(AppError::BadRequest(
            "either a photo or a dish_name is required".to_string(),
        ));
    }

    let (eaten_at, offset_from_timestamp) = match form.eaten_at.as_deref() {
        Some(raw) => parse_eaten_at(raw)?,
        None => (Utc::now().naive_utc(), None),
    };
    // An explicit field wins; otherwise the offset carried by an RFC 3339
    // timestamp is exactly the information we want, so use it rather than
    // silently bucketing the meal into UTC's day (§8).
    let timezone_offset = form
        .timezone_offset
        .or(offset_from_timestamp)
        .unwrap_or(0);
    if timezone_offset.abs() > MAX_TZ_OFFSET_MINUTES {
        return Err(AppError::BadRequest(format!(
            "timezone_offset {timezone_offset} is outside ±{MAX_TZ_OFFSET_MINUTES} minutes"
        )));
    }

    let name_source = match form.name_source.as_deref() {
        Some(raw) => NameSource::from_str(raw).map_err(|_| {
            AppError::BadRequest(format!(
                "name_source {raw:?} is not one of vision/recent_chip/share_intent/comment/manual"
            ))
        })?,
        // No explicit source: a name that arrived with the upload was typed or
        // tapped, and one that did not will come from the vision pass.
        None if form.dish_name.is_some() => NameSource::Manual,
        None => NameSource::Vision,
    };

    if let Some(size) = form.group_size {
        if !(1..=64).contains(&size) {
            return Err(AppError::BadRequest(
                "group_size must be between 1 and 64".to_string(),
            ));
        }
    }
    if form.group_size.is_some() && form.group_id.is_none() {
        return Err(AppError::BadRequest(
            "group_size is only meaningful together with group_id".to_string(),
        ));
    }

    let now = Utc::now().naive_utc();
    let user_id = user.id.clone();
    let thumbs_root = state.config.thumbs_root();
    let model = state.agent.model().to_string();

    let photo = form.photo;
    let dish_name = form.dish_name;
    let dish_name_normalized = dish_name.as_deref().map(normalize_dish_name);
    let comment = form.comment;
    let group_id = form.group_id;
    let group_size = form.group_size;
    let meal_type = form.meal_type;
    let uuid_for_db = client_uuid.clone();

    // The idempotency check comes first, before any file is written: a replay
    // must not even re-encode the photo.
    {
        let user_id = user_id.clone();
        let uuid_for_db = uuid_for_db.clone();
        if let Some(existing) = state
            .interact(move |conn| find_by_client_uuid(conn, &user_id, &uuid_for_db))
            .await?
        {
            tracing::info!(
                meal_id = %existing.id,
                client_uuid = %client_uuid,
                "idempotent replay; returning the existing meal"
            );
            return Ok((
                StatusCode::ACCEPTED,
                Json(MealAcceptedResponse {
                    meal_id: existing.id,
                    status: MealStatus::from_str(&existing.status)?,
                    revision: existing.revision,
                    deduplicated: true,
                }),
            ));
        }
    }

    // Decode and encode before touching the database. A 768px JPEG encode is
    // CPU-bound, and holding one of the pool's few connections across it would
    // starve the analysis workers exactly when a five-photo sitting lands.
    //
    // The upload only ever exists in memory: it is never written to disk, so
    // "the original is deleted" (§7) is satisfied by dropping `photo` here.
    let stored = match photo {
        Some(bytes) => {
            let root = thumbs_root.clone();
            Some(
                tokio::task::spawn_blocking(move || {
                    crate::storage::thumbs::store_from_bytes(&root, &bytes)
                })
                .await??,
            )
        }
        None => None,
    };

    let ingested = state
        .interact(move |conn| {
            let inserted = conn.transaction::<_, AppError, _>(|conn| {
                // Re-check inside the transaction: the encode above released
                // the connection, so a replay could have landed meanwhile.
                if let Some(existing) = find_by_client_uuid(conn, &user_id, &uuid_for_db)? {
                    return Ok(Ingested::Duplicate {
                        meal_id: existing.id,
                        status: MealStatus::from_str(&existing.status)?,
                        revision: existing.revision,
                    });
                }

                let thumbnail = match stored.as_ref() {
                    Some(thumb) => Some(crate::storage::thumbs::upsert_thumbnail(
                        conn, &user_id, thumb,
                    )?),
                    None => None,
                };

                // The group row is the FK target of `meals.group_id`, so it has
                // to exist first.
                if let Some(gid) = group_id.as_deref() {
                    crate::jobs::group_settler::touch_group(
                        conn, &user_id, gid, group_size, now,
                    )?;
                }

                let meal_id = uuid::Uuid::new_v4().to_string();
                let new_meal = NewMeal {
                    id: meal_id.clone(),
                    user_id: user_id.clone(),
                    client_uuid: uuid_for_db.clone(),
                    thumbnail_id: thumbnail.map(|t| t.id),
                    group_id: group_id.clone(),
                    group_size,
                    dish_name: dish_name.clone(),
                    dish_name_normalized: dish_name_normalized.clone(),
                    name_source: name_source.as_str().to_string(),
                    user_comment: comment.clone(),
                    revision: 1,
                    eaten_at,
                    timezone_offset,
                    meal_type: meal_type.clone(),
                    status: MealStatus::Pending.as_str().to_string(),
                    portion_scale: 1.0,
                    created_at: now,
                    updated_at: now,
                };
                diesel::insert_into(meals::table)
                    .values(&new_meal)
                    .execute(conn)?;

                let job_id = uuid::Uuid::new_v4().to_string();
                diesel::insert_into(analysis_jobs::table)
                    .values(&NewAnalysisJob {
                        id: job_id.clone(),
                        meal_id: meal_id.clone(),
                        revision: 1,
                        parent_job_id: None,
                        status: JobStatus::Queued.as_str().to_string(),
                        attempts: 0,
                        model: model.clone(),
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

                Ok(Ingested::Created {
                    job: Job {
                        job_id,
                        meal_id: meal_id.clone(),
                        user_id: user_id.clone(),
                        revision: 1,
                        kind: JobKind::Analyze,
                    },
                    meal_id,
                    group_id: group_id.clone(),
                    dish_name: dish_name.clone(),
                })
            });

            match inserted {
                Ok(value) => Ok(value),
                // Lost a race with a concurrent replay of the same
                // `client_uuid`: the UNIQUE index caught it. Idempotency has to
                // hold under concurrency too, so resolve to the winner's row
                // rather than surfacing a 409.
                Err(AppError::Conflict(detail)) => {
                    match find_by_client_uuid(conn, &user_id, &uuid_for_db)? {
                        Some(existing) => Ok(Ingested::Duplicate {
                            meal_id: existing.id,
                            status: MealStatus::from_str(&existing.status)?,
                            revision: existing.revision,
                        }),
                        // The key belongs to a different user. Do not say so.
                        None => Err(AppError::Conflict(detail)),
                    }
                }
                Err(e) => Err(e),
            }
        })
        .await?;

    match ingested {
        Ingested::Duplicate {
            meal_id,
            status,
            revision,
        } => {
            tracing::info!(
                meal_id = %meal_id,
                client_uuid = %client_uuid,
                "concurrent replay resolved to the existing meal"
            );
            Ok((
                StatusCode::ACCEPTED,
                Json(MealAcceptedResponse {
                    meal_id,
                    status,
                    revision,
                    deduplicated: true,
                }),
            ))
        }
        Ingested::Created {
            job,
            meal_id,
            group_id,
            dish_name,
        } => {
            state.jobs.enqueue(job)?;

            let mut event = MealEvent::new(&user.id, MealEventKind::Queued)
                .with_meal(&meal_id, 1);
            if let Some(gid) = &group_id {
                event = event.with_group(gid);
            }
            event.status = Some(MealStatus::Pending);
            event.dish_name = dish_name;
            state.events.publish(event);

            Ok((
                StatusCode::ACCEPTED,
                Json(MealAcceptedResponse {
                    meal_id,
                    status: MealStatus::Pending,
                    revision: 1,
                    deduplicated: false,
                }),
            ))
        }
    }
}

/// `GET /api/v1/meals/{id}` — one meal with its items and agent reasoning.
#[utoipa::path(
    get,
    path = "/api/v1/meals/{id}",
    tag = "meals",
    summary = "Get a meal",
    params(("id" = String, Path, description = "Meal id")),
    responses(
        (status = 200, description = "The meal", body = MealResponse),
        (status = 404, description = "No such meal for this user", body = crate::error::ErrorBody),
    ),
    security(("session" = []))
)]
pub async fn get_meal(
    State(state): State<AppState>,
    user: CurrentUser,
    Path(id): Path<String>,
) -> Result<Json<MealResponse>, AppError> {
    let user_id = user.id;
    let meal = state
        .interact(move |conn| {
            let meal = load_owned_meal(conn, &user_id, &id)?;
            let mut responses = build_meal_responses(conn, vec![meal])?;
            responses
                .pop()
                .ok_or_else(|| AppError::NotFound("meal".to_string()))
        })
        .await?;
    Ok(Json(meal))
}

/// `GET /api/v1/meals` — history, bucketed by local day.
#[utoipa::path(
    get,
    path = "/api/v1/meals",
    tag = "meals",
    summary = "List meals",
    params(MealHistoryQuery),
    responses(
        (status = 200, description = "Meals, newest first", body = Vec<MealResponse>),
    ),
    security(("session" = []))
)]
pub async fn list_meals(
    State(state): State<AppState>,
    user: CurrentUser,
    Query(query): Query<MealHistoryQuery>,
) -> Result<Json<Vec<MealResponse>>, AppError> {
    if let (Some(from), Some(to)) = (query.from, query.to) {
        if from > to {
            return Err(AppError::BadRequest(
                "`from` must not be after `to`".to_string(),
            ));
        }
    }
    let limit = query
        .limit
        .unwrap_or(DEFAULT_HISTORY_LIMIT)
        .clamp(1, MAX_HISTORY_LIMIT);
    let offset = query.offset.unwrap_or(0).max(0);
    let user_id = user.id;

    let responses = state
        .interact(move |conn| {
            let page = match (query.from, query.to) {
                // No date filter means no rows are dropped after the query, so
                // the page can be taken in SQL rather than by loading the whole
                // history into memory.
                (None, None) => meals::table
                    .filter(meals::user_id.eq(&user_id))
                    .order(meals::eaten_at.desc())
                    .limit(limit)
                    .offset(offset)
                    .select(Meal::as_select())
                    .load::<Meal>(conn)?,
                (from, to) => load_meals_in_local_range(conn, &user_id, from, to)?
                    .into_iter()
                    .skip(offset as usize)
                    .take(limit as usize)
                    .collect(),
            };
            build_meal_responses(conn, page)
        })
        .await?;
    Ok(Json(responses))
}

/// `PATCH /api/v1/meals/{id}` — confirm or edit.
#[utoipa::path(
    patch,
    path = "/api/v1/meals/{id}",
    tag = "meals",
    summary = "Confirm or edit a meal",
    description = "Confirming a meal is what feeds `recall_similar_meals` — only \
confirmed meals, latest revision, are ever recalled. Every changed field is \
recorded in `item_corrections`.",
    params(("id" = String, Path, description = "Meal id")),
    request_body = PatchMealRequest,
    responses(
        (status = 200, description = "The updated meal", body = MealResponse),
        (status = 404, description = "No such meal for this user", body = crate::error::ErrorBody),
    ),
    security(("session" = []))
)]
pub async fn patch_meal(
    State(state): State<AppState>,
    user: CurrentUser,
    Path(id): Path<String>,
    Json(body): Json<PatchMealRequest>,
) -> Result<Json<MealResponse>, AppError> {
    validate_patch(&body)?;

    let user_id = user.id.clone();
    let meal_id = id.clone();
    let now = Utc::now().naive_utc();

    let updated = state
        .interact(move |conn| {
            conn.transaction::<_, AppError, _>(|conn| {
                let meal = load_owned_meal(conn, &user_id, &meal_id)?;

                let mut changeset = MealChangeset {
                    updated_at: Some(now),
                    ..Default::default()
                };
                if let Some(status) = body.status {
                    changeset.status = Some(status.as_str().to_string());
                }
                if let Some(name) = body.dish_name.as_deref() {
                    let trimmed = name.trim();
                    if trimmed.is_empty() {
                        changeset.dish_name = Some(None);
                        changeset.dish_name_normalized = Some(None);
                    } else {
                        changeset.dish_name = Some(Some(trimmed.to_string()));
                        changeset.dish_name_normalized =
                            Some(Some(normalize_dish_name(trimmed)));
                    }
                }
                if let Some(scale) = body.portion_scale {
                    changeset.portion_scale = Some(scale);
                }
                if let Some(meal_type) = body.meal_type.as_deref() {
                    let trimmed = meal_type.trim();
                    changeset.meal_type = Some(if trimmed.is_empty() {
                        None
                    } else {
                        Some(trimmed.to_string())
                    });
                }
                diesel::update(meals::table.filter(meals::id.eq(&meal.id)))
                    .set(&changeset)
                    .execute(conn)?;

                if let Some(items) = body.items.as_deref() {
                    apply_item_edits(conn, &meal, items, now)?;
                }

                let meal = load_owned_meal(conn, &user_id, &meal_id)?;
                let mut responses = build_meal_responses(conn, vec![meal])?;
                responses
                    .pop()
                    .ok_or_else(|| AppError::NotFound("meal".to_string()))
            })
        })
        .await?;

    // Tell any other device this user has connected that the day moved. This is
    // a refresh signal, not a notification — §6 allows exactly one notification
    // per meal and the analyzer already fired it.
    let mut event = MealEvent::new(&user.id, MealEventKind::Updated)
        .with_meal(&updated.id, updated.revision);
    if let Some(gid) = &updated.group_id {
        event = event.with_group(gid);
    }
    event.status = Some(updated.status);
    event.dish_name = updated.dish_name.clone();
    event.totals = Some(updated.totals);
    state.events.publish(event);

    Ok(Json(updated))
}

/// `DELETE /api/v1/meals/{id}`.
#[utoipa::path(
    delete,
    path = "/api/v1/meals/{id}",
    tag = "meals",
    summary = "Delete a meal",
    params(("id" = String, Path, description = "Meal id")),
    responses(
        (status = 204, description = "Deleted"),
        (status = 404, description = "No such meal for this user", body = crate::error::ErrorBody),
    ),
    security(("session" = []))
)]
pub async fn delete_meal(
    State(state): State<AppState>,
    user: CurrentUser,
    Path(id): Path<String>,
) -> Result<StatusCode, AppError> {
    let user_id = user.id;
    let thumbs_root = state.config.thumbs_root();

    state
        .interact(move |conn| {
            // Scope the load so a delete can never reach another user's row.
            let meal = load_owned_meal(conn, &user_id, &id)?;
            // Items, corrections, jobs, steps and the session all cascade.
            diesel::delete(meals::table.filter(meals::id.eq(&meal.id))).execute(conn)?;
            // Thumbnails are content-addressed and therefore shared, so
            // reclaiming disk needs a reference check rather than an unlink.
            let pruned = crate::storage::thumbs::prune_orphans(conn, &thumbs_root)?;
            if pruned > 0 {
                tracing::debug!(pruned, "pruned orphaned thumbnails after a meal delete");
            }
            Ok(())
        })
        .await?;

    Ok(StatusCode::NO_CONTENT)
}

/// `POST /api/v1/meals/{id}/reanalyze` — resume the persisted agent session.
#[utoipa::path(
    post,
    path = "/api/v1/meals/{id}/reanalyze",
    tag = "meals",
    summary = "Correct a meal by conversation",
    description = "Loads the persisted rig thread, appends the feedback as a new \
user turn and continues the same conversation, emitting a new revision. Prior \
revisions are retained. Returns 202 — the correction runs in the background.",
    params(("id" = String, Path, description = "Meal id")),
    request_body = ReanalyzeRequest,
    responses(
        (status = 202, description = "Accepted; correction enqueued", body = MealAcceptedResponse),
        (status = 400, description = "Empty feedback", body = crate::error::ErrorBody),
        (status = 404, description = "No such meal for this user", body = crate::error::ErrorBody),
        (status = 409, description = "An analysis is already running for this meal", body = crate::error::ErrorBody),
    ),
    security(("session" = []))
)]
pub async fn reanalyze_meal(
    State(state): State<AppState>,
    user: CurrentUser,
    Path(id): Path<String>,
    Json(body): Json<ReanalyzeRequest>,
) -> Result<(StatusCode, Json<MealAcceptedResponse>), AppError> {
    let feedback = body.feedback.trim().to_string();
    if feedback.is_empty() {
        return Err(AppError::BadRequest(
            "feedback must not be empty — say what is wrong with the estimate".to_string(),
        ));
    }
    if feedback.chars().count() > 2000 {
        return Err(AppError::BadRequest(
            "feedback must be 2000 characters or fewer".to_string(),
        ));
    }

    let user_id = user.id.clone();
    let meal_id = id.clone();
    let now = Utc::now().naive_utc();
    let model = state.agent.model().to_string();
    let queued_feedback = feedback.clone();

    let (job, revision, group_id, dish_name) = state
        .interact(move |conn| {
            conn.transaction::<_, AppError, _>(|conn| {
                let meal = load_owned_meal(conn, &user_id, &meal_id)?;
                // A correction that races the initial loop would write two
                // revisions from two different threads. Make the client wait.
                if !meal.is_terminal() {
                    return Err(AppError::Conflict(
                        "this meal is still being analysed; try again once it finishes"
                            .to_string(),
                    ));
                }

                let revision = crate::agent::reanalyze::next_revision(conn, &meal.id)?;
                let parent_job_id: Option<String> = analysis_jobs::table
                    .filter(analysis_jobs::meal_id.eq(&meal.id))
                    .order((
                        analysis_jobs::revision.desc(),
                        analysis_jobs::created_at.desc(),
                    ))
                    .select(analysis_jobs::id)
                    .first::<String>(conn)
                    .optional()?;

                let job_id = uuid::Uuid::new_v4().to_string();
                diesel::insert_into(analysis_jobs::table)
                    .values(&NewAnalysisJob {
                        id: job_id.clone(),
                        meal_id: meal.id.clone(),
                        revision,
                        parent_job_id: parent_job_id.clone(),
                        status: JobStatus::Queued.as_str().to_string(),
                        attempts: 0,
                        model: model.clone(),
                        user_feedback: Some(queued_feedback.clone()),
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

                // Reflect the queued correction immediately so the UI does not
                // show a stale `confirmed` badge, and so `requeue_unfinished`
                // recovers it after a restart.
                diesel::update(meals::table.filter(meals::id.eq(&meal.id)))
                    .set(&MealChangeset {
                        status: Some(MealStatus::Analyzing.as_str().to_string()),
                        updated_at: Some(now),
                        ..Default::default()
                    })
                    .execute(conn)?;

                Ok((
                    Job {
                        job_id,
                        meal_id: meal.id.clone(),
                        user_id: user_id.clone(),
                        revision,
                        kind: JobKind::Reanalyze {
                            feedback: queued_feedback.clone(),
                            parent_job_id,
                        },
                    },
                    revision,
                    meal.group_id.clone(),
                    meal.dish_name.clone(),
                ))
            })
        })
        .await?;

    state.jobs.enqueue(job)?;

    let mut event = MealEvent::new(&user.id, MealEventKind::Queued).with_meal(&id, revision);
    if let Some(gid) = &group_id {
        event = event.with_group(gid);
    }
    event.status = Some(MealStatus::Analyzing);
    event.dish_name = dish_name;
    state.events.publish(event);

    Ok((
        StatusCode::ACCEPTED,
        Json(MealAcceptedResponse {
            meal_id: id,
            status: MealStatus::Analyzing,
            revision,
            deduplicated: false,
        }),
    ))
}

/// `GET /api/v1/meals/{id}/revisions` — revision history and its causes.
#[utoipa::path(
    get,
    path = "/api/v1/meals/{id}/revisions",
    tag = "meals",
    summary = "Revision history",
    params(("id" = String, Path, description = "Meal id")),
    responses(
        (status = 200, description = "Revisions, newest first", body = RevisionsResponse),
        (status = 404, description = "No such meal for this user", body = crate::error::ErrorBody),
    ),
    security(("session" = []))
)]
pub async fn meal_revisions(
    State(state): State<AppState>,
    user: CurrentUser,
    Path(id): Path<String>,
) -> Result<Json<RevisionsResponse>, AppError> {
    let user_id = user.id;
    let response = state
        .interact(move |conn| {
            let meal = load_owned_meal(conn, &user_id, &id)?;

            let jobs = analysis_jobs::table
                .filter(analysis_jobs::meal_id.eq(&meal.id))
                .order((
                    analysis_jobs::revision.desc(),
                    analysis_jobs::created_at.desc(),
                ))
                .select((
                    analysis_jobs::revision,
                    analysis_jobs::user_feedback,
                    analysis_jobs::model,
                    analysis_jobs::tool_calls,
                    analysis_jobs::finished_at,
                    analysis_jobs::created_at,
                ))
                .load::<(i32, Option<String>, String, i32, Option<NaiveDateTime>, NaiveDateTime)>(
                    conn,
                )?;

            let items = meal_items::table
                .filter(meal_items::meal_id.eq(&meal.id))
                .order(meal_items::position.asc())
                .select(MealItem::as_select())
                .load::<MealItem>(conn)?;

            let mut items_by_revision: HashMap<i32, Vec<MealItem>> = HashMap::new();
            for item in items {
                items_by_revision.entry(item.revision).or_default().push(item);
            }

            // Newest first, one entry per revision. The newest job for a
            // revision wins — a retry re-runs the same revision.
            let mut seen: Vec<i32> = Vec::new();
            let mut revisions = Vec::new();
            for (revision, feedback, model, tool_calls, finished_at, created_at) in jobs {
                if seen.contains(&revision) {
                    continue;
                }
                seen.push(revision);
                let rows = items_by_revision.remove(&revision).unwrap_or_default();
                let items = map_items(&rows, meal.portion_scale)?;
                let totals = sum_items(&items);
                revisions.push(RevisionEntry {
                    revision,
                    user_feedback: feedback,
                    model,
                    // Not stored: nothing records continue-vs-reseed. A
                    // continuation reuses the tool results already in the
                    // thread and therefore makes no tool calls (§13), so tool
                    // calls on a correction revision mean a fresh loop.
                    reseeded: revision > 1 && tool_calls > 0,
                    items,
                    totals,
                    tool_calls,
                    // SQLite stores naive UTC; the wire format must carry the offset.
                    created_at: finished_at.unwrap_or(created_at).and_utc(),
                });
            }

            // Revisions whose job row was pruned still have items; do not hide
            // them from the inspector.
            let mut orphans: Vec<i32> = items_by_revision.keys().copied().collect();
            orphans.sort_unstable_by(|a, b| b.cmp(a));
            for revision in orphans {
                let rows = items_by_revision.remove(&revision).unwrap_or_default();
                let items = map_items(&rows, meal.portion_scale)?;
                let totals = sum_items(&items);
                revisions.push(RevisionEntry {
                    revision,
                    user_feedback: None,
                    model: String::new(),
                    reseeded: false,
                    items,
                    totals,
                    tool_calls: 0,
                    created_at: meal.created_at.and_utc(),
                });
            }
            revisions.sort_by_key(|entry| std::cmp::Reverse(entry.revision));

            Ok(RevisionsResponse {
                meal_id: meal.id,
                revisions,
            })
        })
        .await?;
    Ok(Json(response))
}

/// `GET /api/v1/meals/{id}/thumbnail` — the 768px derivative.
#[utoipa::path(
    get,
    path = "/api/v1/meals/{id}/thumbnail",
    tag = "meals",
    summary = "Get a meal's thumbnail",
    params(("id" = String, Path, description = "Meal id")),
    responses(
        (status = 200, description = "JPEG image", content_type = "image/jpeg"),
        (status = 404, description = "No thumbnail for this meal", body = crate::error::ErrorBody),
    ),
    security(("session" = []))
)]
pub async fn get_meal_thumbnail(
    State(state): State<AppState>,
    user: CurrentUser,
    Path(id): Path<String>,
) -> Result<Response, AppError> {
    let user_id = user.id;
    let thumbs_root = state.config.thumbs_root();

    let bytes = state
        .interact(move |conn| {
            let meal = load_owned_meal(conn, &user_id, &id)?;
            let thumbnail_id = meal
                .thumbnail_id
                .ok_or_else(|| AppError::NotFound("this meal has no photo".to_string()))?;
            let rel_path: String = thumbnails::table
                .filter(thumbnails::id.eq(&thumbnail_id))
                .filter(thumbnails::user_id.eq(&meal.user_id))
                .select(thumbnails::path)
                .first::<String>(conn)
                .optional()?
                .ok_or_else(|| AppError::NotFound("thumbnail".to_string()))?;
            crate::storage::thumbs::read(&thumbs_root, &rel_path)
        })
        .await?;

    Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, "image/jpeg")
        // Content-addressed, so the bytes behind a given meal+hash never
        // change. `private` because this is one user's photograph.
        .header(header::CACHE_CONTROL, "private, max-age=31536000, immutable")
        .body(Body::from(bytes))
        .map_err(AppError::internal)
}

// ---------------------------------------------------------------------------
// Shared mapping helpers
// ---------------------------------------------------------------------------

/// Build a [`MealResponse`] from loaded rows.
///
/// `portion_scale` is applied here, once, so no caller has to remember to.
pub fn to_meal_response(
    meal: &crate::models::Meal,
    items: &[crate::models::MealItem],
    error: Option<String>,
) -> Result<MealResponse, AppError> {
    let items = map_items(items, meal.portion_scale)?;
    let totals = sum_items(&items);
    Ok(MealResponse {
        id: meal.id.clone(),
        client_uuid: meal.client_uuid.clone(),
        group_id: meal.group_id.clone(),
        group_size: meal.group_size,
        dish_name: meal.dish_name.clone(),
        name_source: meal.name_source()?,
        user_comment: meal.user_comment.clone(),
        revision: meal.revision,
        status: meal.status()?,
        // SQLite stores naive UTC (see `models::Meal`); the wire format must
        // carry the offset, or an RFC 3339 client cannot parse it at all.
        eaten_at: meal.eaten_at.and_utc(),
        timezone_offset: meal.timezone_offset,
        meal_type: meal.meal_type.clone(),
        portion_scale: meal.portion_scale,
        thumbnail_url: meal.thumbnail_id.as_ref().map(|_| {
            format!("{}/meals/{}/thumbnail", crate::API_PREFIX, meal.id)
        }),
        items,
        totals,
        error,
        created_at: meal.created_at.and_utc(),
        updated_at: meal.updated_at.and_utc(),
    })
}

/// Convert a persisted revision back into the agent's own output shape, for
/// reseeding a session and for the reasoning inspector.
///
/// Deliberately **unscaled**: `portion_scale` is a statement about how much of
/// the dish was eaten and is applied on top of the estimate at read time, so
/// folding it in here would double-count it the next time the agent re-emits.
pub fn to_meal_estimate(
    meal: &crate::models::Meal,
    items: &[crate::models::MealItem],
) -> Result<MealEstimate, AppError> {
    let mut estimated = Vec::with_capacity(items.len());
    let mut confidence_sum = 0.0;
    let mut confidence_count = 0.0;
    let mut from_recall = false;

    for item in items {
        let grams_source = item.grams_source()?;
        let macro_source = item.macro_source()?;
        if grams_source == GramsSource::Recall || macro_source == MacroSource::Recall {
            from_recall = true;
        }
        if let Some(confidence) = item.confidence {
            confidence_sum += confidence;
            confidence_count += 1.0;
        }
        estimated.push(EstimatedItem {
            name: item.name.clone(),
            grams: item.grams,
            kcal: item.kcal,
            protein_g: item.protein_g,
            fat_g: item.fat_g,
            carbs_g: item.carbs_g,
            confidence: item.confidence.unwrap_or(0.0),
            reasoning_note: item.reasoning_note.clone().unwrap_or_default(),
            barcode: item.barcode.clone(),
            grams_source,
            macro_source,
        });
    }

    let overall_confidence = if confidence_count > 0.0 {
        confidence_sum / confidence_count
    } else {
        0.0
    };

    Ok(MealEstimate {
        dish_name: meal.dish_name.clone().unwrap_or_default(),
        cuisine: None,
        container: None,
        from_recall,
        items: estimated,
        overall_confidence,
        notes: meal.user_comment.clone(),
    })
}

/// Sum a list of items.
pub fn sum_items(items: &[MealItemResponse]) -> MacroTotals {
    items.iter().fold(MacroTotals::default(), |acc, item| MacroTotals {
        kcal: acc.kcal + item.kcal,
        protein_g: acc.protein_g + item.protein_g,
        fat_g: acc.fat_g + item.fat_g,
        carbs_g: acc.carbs_g + item.carbs_g,
    })
}

/// Load one meal, scoped to its owner.
///
/// A miss and a cross-user read are the same 404 by design: distinguishing them
/// would leak the existence of another user's meals (see [`AppError::NotFound`]).
pub fn load_owned_meal(
    conn: &mut SqliteConnection,
    user_id: &str,
    meal_id: &str,
) -> Result<Meal, AppError> {
    meals::table
        .filter(meals::user_id.eq(user_id))
        .filter(meals::id.eq(meal_id))
        .select(Meal::as_select())
        .first::<Meal>(conn)
        .optional()?
        .ok_or_else(|| AppError::NotFound("meal".to_string()))
}

/// Load a user's meals whose **local** day falls in `[from, to]`.
///
/// Bucketing uses each meal's own `timezone_offset`, not UTC and not a single
/// user-wide offset (§8): "what did I eat today" is a question about *your* day.
/// The SQL bound is therefore widened by a day on each side and the exact
/// comparison is done in Rust, which is also the only way to get it right for a
/// user who crossed a timezone mid-range.
pub fn load_meals_in_local_range(
    conn: &mut SqliteConnection,
    user_id: &str,
    from: Option<NaiveDate>,
    to: Option<NaiveDate>,
) -> Result<Vec<Meal>, AppError> {
    let mut query = meals::table
        .filter(meals::user_id.eq(user_id))
        .into_boxed();

    // ±1 day covers every real UTC offset (max ±14h) in both directions.
    if let Some(from) = from {
        let lower = (from - chrono::Duration::days(1))
            .and_hms_opt(0, 0, 0)
            .unwrap_or_else(|| from.and_time(chrono::NaiveTime::MIN));
        query = query.filter(meals::eaten_at.ge(lower));
    }
    if let Some(to) = to {
        let upper = (to + chrono::Duration::days(2))
            .and_hms_opt(0, 0, 0)
            .unwrap_or_else(|| to.and_time(chrono::NaiveTime::MIN));
        query = query.filter(meals::eaten_at.lt(upper));
    }

    let rows = query
        .order(meals::eaten_at.desc())
        .select(Meal::as_select())
        .load::<Meal>(conn)?;

    Ok(rows
        .into_iter()
        .filter(|meal| {
            let local = local_date_of(meal.eaten_at, meal.timezone_offset);
            from.is_none_or(|f| local >= f) && to.is_none_or(|t| local <= t)
        })
        .collect())
}

/// Turn loaded meals into responses, batching the item and error lookups.
pub fn build_meal_responses(
    conn: &mut SqliteConnection,
    rows: Vec<Meal>,
) -> Result<Vec<MealResponse>, AppError> {
    if rows.is_empty() {
        return Ok(Vec::new());
    }
    let ids: Vec<String> = rows.iter().map(|meal| meal.id.clone()).collect();

    let items = meal_items::table
        .filter(meal_items::meal_id.eq_any(&ids))
        .order(meal_items::position.asc())
        .select(MealItem::as_select())
        .load::<MealItem>(conn)?;

    let mut items_by_meal: HashMap<String, Vec<MealItem>> = HashMap::new();
    for item in items {
        items_by_meal
            .entry(item.meal_id.clone())
            .or_default()
            .push(item);
    }

    // Only failures need a reason attached; everything else leaves `error` null.
    let failures = analysis_jobs::table
        .filter(analysis_jobs::meal_id.eq_any(&ids))
        .filter(analysis_jobs::error.is_not_null())
        .order(analysis_jobs::created_at.asc())
        .select((
            analysis_jobs::meal_id,
            analysis_jobs::revision,
            analysis_jobs::error,
        ))
        .load::<(String, i32, Option<String>)>(conn)?;
    let mut errors: HashMap<(String, i32), String> = HashMap::new();
    for (meal_id, revision, error) in failures {
        if let Some(error) = error {
            errors.insert((meal_id, revision), error);
        }
    }

    rows.iter()
        .map(|meal| {
            let items: Vec<MealItem> = items_by_meal
                .get(&meal.id)
                .map(|all| {
                    all.iter()
                        .filter(|item| item.revision == meal.revision)
                        .cloned()
                        .collect()
                })
                .unwrap_or_default();
            let error = if meal.status()? == MealStatus::Failed {
                errors.get(&(meal.id.clone(), meal.revision)).cloned()
            } else {
                None
            };
            to_meal_response(meal, &items, error)
        })
        .collect()
}

/// Map item rows to their response shape, applying `portion_scale`.
fn map_items(items: &[MealItem], portion_scale: f64) -> Result<Vec<MealItemResponse>, AppError> {
    let scale = if portion_scale.is_finite() && portion_scale > 0.0 {
        portion_scale
    } else {
        1.0
    };
    items
        .iter()
        .map(|item| {
            Ok(MealItemResponse {
                id: item.id.clone(),
                position: item.position,
                name: item.name.clone(),
                barcode: item.barcode.clone(),
                grams: item.grams * scale,
                grams_source: item.grams_source()?,
                kcal: item.kcal * scale,
                protein_g: item.protein_g * scale,
                fat_g: item.fat_g * scale,
                carbs_g: item.carbs_g * scale,
                macro_source: item.macro_source()?,
                confidence: item.confidence,
                reasoning_note: item.reasoning_note.clone(),
            })
        })
        .collect()
}

/// Find a meal by its idempotency key, scoped to the owner.
fn find_by_client_uuid(
    conn: &mut SqliteConnection,
    user_id: &str,
    client_uuid: &str,
) -> Result<Option<Meal>, AppError> {
    meals::table
        .filter(meals::user_id.eq(user_id))
        .filter(meals::client_uuid.eq(client_uuid))
        .select(Meal::as_select())
        .first::<Meal>(conn)
        .optional()
        .map_err(AppError::from)
}

// ---------------------------------------------------------------------------
// Ingest plumbing
// ---------------------------------------------------------------------------

/// Outcome of the ingest transaction.
enum Ingested {
    /// A fresh meal; the job still has to be enqueued.
    Created {
        job: Job,
        meal_id: String,
        group_id: Option<String>,
        dish_name: Option<String>,
    },
    /// `client_uuid` had already been seen — nothing was created.
    Duplicate {
        meal_id: String,
        status: MealStatus,
        revision: i32,
    },
}

/// The multipart parts, read into memory.
#[derive(Default)]
struct IngestParts {
    photo: Option<Vec<u8>>,
    client_uuid: Option<String>,
    eaten_at: Option<String>,
    timezone_offset: Option<i32>,
    dish_name: Option<String>,
    comment: Option<String>,
    name_source: Option<String>,
    group_id: Option<String>,
    group_size: Option<i32>,
    meal_type: Option<String>,
}

/// Drain the multipart body.
///
/// Unknown fields are ignored rather than rejected: the Android client is
/// generated from this spec and a newer client sending an extra field must not
/// break against an older server.
async fn read_ingest_parts(mut multipart: Multipart) -> Result<IngestParts, AppError> {
    let mut parts = IngestParts::default();
    while let Some(field) = multipart.next_field().await? {
        let name = field.name().unwrap_or_default().to_string();
        match name.as_str() {
            "photo" | "image" | "file" => {
                let bytes = field.bytes().await?;
                if bytes.len() > crate::storage::thumbs::MAX_UPLOAD_BYTES {
                    return Err(AppError::BadRequest(format!(
                        "photo is {} bytes; the limit is {}",
                        bytes.len(),
                        crate::storage::thumbs::MAX_UPLOAD_BYTES
                    )));
                }
                if !bytes.is_empty() {
                    parts.photo = Some(bytes.to_vec());
                }
            }
            "client_uuid" => parts.client_uuid = non_empty(field.text().await?),
            "eaten_at" => parts.eaten_at = non_empty(field.text().await?),
            "timezone_offset" => {
                parts.timezone_offset = parse_int_field("timezone_offset", field.text().await?)?
            }
            "dish_name" => parts.dish_name = non_empty(field.text().await?),
            "comment" | "user_comment" => parts.comment = non_empty(field.text().await?),
            "name_source" => parts.name_source = non_empty(field.text().await?),
            "group_id" => parts.group_id = non_empty(field.text().await?),
            "group_size" => {
                parts.group_size = parse_int_field("group_size", field.text().await?)?
            }
            "meal_type" => parts.meal_type = non_empty(field.text().await?),
            other => {
                tracing::debug!(field = %other, "ignoring unknown multipart field");
            }
        }
    }
    Ok(parts)
}

/// Trim a text field, treating blank as absent.
fn non_empty(value: String) -> Option<String> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_string())
    }
}

/// Parse an integer form field, treating blank as absent.
fn parse_int_field(name: &str, value: String) -> Result<Option<i32>, AppError> {
    match non_empty(value) {
        None => Ok(None),
        Some(raw) => raw
            .parse::<i32>()
            .map(Some)
            .map_err(|e| AppError::BadRequest(format!("{name} must be an integer: {e}"))),
    }
}

/// Parse `eaten_at`, returning the UTC instant and, when the timestamp carried
/// one, the offset it was written in.
fn parse_eaten_at(raw: &str) -> Result<(NaiveDateTime, Option<i32>), AppError> {
    if let Ok(dt) = chrono::DateTime::parse_from_rfc3339(raw) {
        return Ok((dt.naive_utc(), Some(dt.offset().local_minus_utc() / 60)));
    }
    for format in [
        "%Y-%m-%dT%H:%M:%S%.f",
        "%Y-%m-%d %H:%M:%S%.f",
        "%Y-%m-%dT%H:%M",
        "%Y-%m-%d %H:%M",
    ] {
        if let Ok(dt) = NaiveDateTime::parse_from_str(raw, format) {
            return Ok((dt, None));
        }
    }
    Err(AppError::BadRequest(format!(
        "eaten_at {raw:?} is not an RFC 3339 timestamp"
    )))
}

// ---------------------------------------------------------------------------
// PATCH plumbing
// ---------------------------------------------------------------------------

/// Reject a patch body that would produce nonsense before anything is written.
fn validate_patch(body: &PatchMealRequest) -> Result<(), AppError> {
    if let Some(scale) = body.portion_scale {
        if !scale.is_finite() || !(0.01..=10.0).contains(&scale) {
            return Err(AppError::BadRequest(
                "portion_scale must be between 0.01 and 10".to_string(),
            ));
        }
    }
    if let Some(items) = body.items.as_deref() {
        if items.len() > MAX_PATCH_ITEMS {
            return Err(AppError::BadRequest(format!(
                "a meal may not have more than {MAX_PATCH_ITEMS} items"
            )));
        }
        for item in items {
            if item.name.trim().is_empty() {
                return Err(AppError::BadRequest(
                    "every item needs a name".to_string(),
                ));
            }
            for (field, value) in [
                ("grams", item.grams),
                ("kcal", item.kcal),
                ("protein_g", item.protein_g),
                ("fat_g", item.fat_g),
                ("carbs_g", item.carbs_g),
            ] {
                if !value.is_finite() || value < 0.0 {
                    return Err(AppError::BadRequest(format!(
                        "{field} must be a non-negative number"
                    )));
                }
            }
            if let Some(barcode) = item.barcode.as_deref() {
                crate::food::validate_ean(barcode)?;
            }
        }
    }
    Ok(())
}

/// Apply a full item replacement, recording every change in `item_corrections`.
///
/// Rows are updated in place rather than replaced, because `item_corrections`
/// cascades off `meal_items` — deleting and re-inserting an item would silently
/// erase its correction history, which is the record the `calibration_factor`
/// is learned from (§5).
fn apply_item_edits(
    conn: &mut SqliteConnection,
    meal: &Meal,
    incoming: &[PatchMealItem],
    now: NaiveDateTime,
) -> Result<(), AppError> {
    let existing = meal_items::table
        .filter(meal_items::meal_id.eq(&meal.id))
        .filter(meal_items::revision.eq(meal.revision))
        .select(MealItem::as_select())
        .load::<MealItem>(conn)?;
    let by_id: HashMap<&str, &MealItem> =
        existing.iter().map(|item| (item.id.as_str(), item)).collect();

    // Every referenced id must belong to this meal and revision, or the client
    // is editing something it cannot see.
    for item in incoming {
        if let Some(id) = item.id.as_deref() {
            if !by_id.contains_key(id) {
                return Err(AppError::BadRequest(format!(
                    "item {id} does not belong to this meal at revision {}",
                    meal.revision
                )));
            }
        }
    }

    // Move every surviving row out of the final position range first, so the
    // renumber below cannot collide with the UNIQUE index.
    diesel::update(
        meal_items::table
            .filter(meal_items::meal_id.eq(&meal.id))
            .filter(meal_items::revision.eq(meal.revision)),
    )
    .set(meal_items::position.eq(meal_items::position + POSITION_SHIFT))
    .execute(conn)?;

    let mut corrections: Vec<NewItemCorrection> = Vec::new();

    for (position, item) in incoming.iter().enumerate() {
        let position = position as i32;
        match item.id.as_deref() {
            Some(id) => {
                let previous = by_id
                    .get(id)
                    .copied()
                    .ok_or_else(|| AppError::NotFound(format!("meal item {id}")))?;

                let mut changeset = MealItemChangeset {
                    position: Some(position),
                    ..Default::default()
                };

                let name = item.name.trim();
                if name != previous.name {
                    corrections.push(correction(id, "name", &previous.name, name, now));
                    changeset.name = Some(name.to_string());
                }
                if item.barcode.as_deref() != previous.barcode.as_deref() {
                    corrections.push(correction(
                        id,
                        "barcode",
                        previous.barcode.as_deref().unwrap_or(""),
                        item.barcode.as_deref().unwrap_or(""),
                        now,
                    ));
                    changeset.barcode = Some(item.barcode.clone());
                }
                if differs(previous.grams, item.grams) {
                    corrections.push(numeric_correction(
                        id,
                        "grams",
                        previous.grams,
                        item.grams,
                        now,
                    ));
                    changeset.grams = Some(item.grams);
                    // The gram figure is now the user's, not the agent's — this
                    // is what makes recall return corrected numbers (§5).
                    changeset.grams_source = Some(GramsSource::User.as_str().to_string());
                }
                let mut macros_changed = false;
                for (field, before, after, slot) in [
                    ("kcal", previous.kcal, item.kcal, 0usize),
                    ("protein_g", previous.protein_g, item.protein_g, 1),
                    ("fat_g", previous.fat_g, item.fat_g, 2),
                    ("carbs_g", previous.carbs_g, item.carbs_g, 3),
                ] {
                    if differs(before, after) {
                        corrections.push(numeric_correction(id, field, before, after, now));
                        match slot {
                            0 => changeset.kcal = Some(after),
                            1 => changeset.protein_g = Some(after),
                            2 => changeset.fat_g = Some(after),
                            _ => changeset.carbs_g = Some(after),
                        }
                        macros_changed = true;
                    }
                }
                if macros_changed {
                    changeset.macro_source = Some(MacroSource::User.as_str().to_string());
                }

                diesel::update(meal_items::table.filter(meal_items::id.eq(id)))
                    .set(&changeset)
                    .execute(conn)?;
            }
            None => {
                // A newly added item is the user's own number end to end.
                diesel::insert_into(meal_items::table)
                    .values(&NewMealItem {
                        id: uuid::Uuid::new_v4().to_string(),
                        meal_id: meal.id.clone(),
                        revision: meal.revision,
                        position,
                        name: item.name.trim().to_string(),
                        barcode: item.barcode.clone(),
                        grams: item.grams,
                        grams_source: GramsSource::User.as_str().to_string(),
                        kcal: item.kcal,
                        protein_g: item.protein_g,
                        fat_g: item.fat_g,
                        carbs_g: item.carbs_g,
                        macro_source: MacroSource::User.as_str().to_string(),
                        confidence: None,
                        reasoning_note: Some("added by you".to_string()),
                        created_at: now,
                    })
                    .execute(conn)?;
            }
        }
    }

    if !corrections.is_empty() {
        diesel::insert_into(crate::schema::item_corrections::table)
            .values(&corrections)
            .execute(conn)?;
    }

    // Anything still parked above the shift was dropped from the list.
    diesel::delete(
        meal_items::table
            .filter(meal_items::meal_id.eq(&meal.id))
            .filter(meal_items::revision.eq(meal.revision))
            .filter(meal_items::position.ge(POSITION_SHIFT)),
    )
    .execute(conn)?;

    Ok(())
}

/// Two stored figures differ by more than float noise.
fn differs(before: f64, after: f64) -> bool {
    (before - after).abs() > 1e-6
}

/// Build an `item_corrections` row for a text field.
fn correction(
    meal_item_id: &str,
    field: &str,
    original: &str,
    corrected: &str,
    at: NaiveDateTime,
) -> NewItemCorrection {
    NewItemCorrection {
        id: uuid::Uuid::new_v4().to_string(),
        meal_item_id: meal_item_id.to_string(),
        field: field.to_string(),
        original_value: Some(original.to_string()),
        corrected_value: Some(corrected.to_string()),
        corrected_at: at,
    }
}

/// Build an `item_corrections` row for a numeric field.
fn numeric_correction(
    meal_item_id: &str,
    field: &str,
    original: f64,
    corrected: f64,
    at: NaiveDateTime,
) -> NewItemCorrection {
    correction(
        meal_item_id,
        field,
        &format!("{original}"),
        &format!("{corrected}"),
        at,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::feedback::state::fixtures::*;
    use crate::schema::item_corrections;

    #[test]
    fn rfc3339_supplies_the_timezone_offset_when_no_field_does() {
        let (utc, offset) = parse_eaten_at("2026-08-01T20:30:00+03:00").unwrap();
        assert_eq!(utc.to_string(), "2026-08-01 17:30:00");
        assert_eq!(offset, Some(180));
    }

    #[test]
    fn a_naive_timestamp_parses_without_an_offset() {
        let (utc, offset) = parse_eaten_at("2026-08-01T17:30:00").unwrap();
        assert_eq!(utc.to_string(), "2026-08-01 17:30:00");
        assert_eq!(offset, None);
        assert!(parse_eaten_at("last tuesday").is_err());
    }

    #[test]
    fn totals_are_the_sum_of_the_scaled_items() {
        let items = vec![
            MealItemResponse {
                id: "a".into(),
                position: 0,
                name: "rice".into(),
                barcode: None,
                grams: 200.0,
                grams_source: GramsSource::Agent,
                kcal: 260.0,
                protein_g: 5.0,
                fat_g: 1.0,
                carbs_g: 57.0,
                macro_source: MacroSource::Model,
                confidence: Some(0.6),
                reasoning_note: None,
            },
            MealItemResponse {
                id: "b".into(),
                position: 1,
                name: "chicken".into(),
                barcode: None,
                grams: 120.0,
                grams_source: GramsSource::Agent,
                kcal: 200.0,
                protein_g: 30.0,
                fat_g: 8.0,
                carbs_g: 0.0,
                macro_source: MacroSource::Model,
                confidence: Some(0.5),
                reasoning_note: None,
            },
        ];
        let totals = sum_items(&items);
        assert_eq!(totals.kcal, 460.0);
        assert_eq!(totals.protein_g, 35.0);
    }

    #[test]
    fn a_patch_with_absurd_numbers_is_rejected() {
        let mut body = PatchMealRequest {
            portion_scale: Some(0.0),
            ..Default::default()
        };
        assert!(validate_patch(&body).is_err());

        body.portion_scale = Some(0.6);
        assert!(validate_patch(&body).is_ok());

        body.items = Some(vec![PatchMealItem {
            id: None,
            name: "rice".into(),
            grams: -5.0,
            kcal: 100.0,
            protein_g: 1.0,
            fat_g: 1.0,
            carbs_g: 1.0,
            barcode: None,
        }]);
        assert!(validate_patch(&body).is_err());
    }

    #[test]
    fn float_noise_is_not_a_correction() {
        assert!(!differs(200.0, 200.0000001));
        assert!(differs(200.0, 100.0));
    }

    // -----------------------------------------------------------------------
    // Database-backed behaviour. The handlers themselves need an `AppState`
    // (and therefore a live IdP and an agent), so the load-bearing pieces are
    // exercised directly against a migrated temp database.
    // -----------------------------------------------------------------------

    #[test]
    fn the_idempotency_key_is_scoped_to_its_owner() {
        let mut conn = test_conn();
        seed_user(&mut conn, "u1");
        seed_user(&mut conn, "u2");
        seed_meal(
            &mut conn,
            "m1",
            "u1",
            at(2026, 8, 1, 12, 0),
            180,
            MealStatus::Confirmed,
            1,
            1.0,
            None,
        );

        // `seed_meal` writes `client-<id>` as the key.
        let found = find_by_client_uuid(&mut conn, "u1", "client-m1").unwrap();
        assert_eq!(found.map(|m| m.id).as_deref(), Some("m1"));

        // The same key must not resolve for anybody else — a replay from user
        // B can never hand them user A's meal.
        assert!(find_by_client_uuid(&mut conn, "u2", "client-m1")
            .unwrap()
            .is_none());
        assert!(find_by_client_uuid(&mut conn, "u1", "client-nope")
            .unwrap()
            .is_none());
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
            180,
            MealStatus::Confirmed,
            1,
            1.0,
            None,
        );

        assert!(load_owned_meal(&mut conn, "u1", "m1").is_ok());
        let err = load_owned_meal(&mut conn, "u2", "m1").unwrap_err();
        assert_eq!(err.status(), StatusCode::NOT_FOUND);
        // Indistinguishable from a plain miss, by design.
        let miss = load_owned_meal(&mut conn, "u2", "nope").unwrap_err();
        assert_eq!(err.to_string(), miss.to_string());
    }

    #[test]
    fn history_buckets_by_the_meals_own_offset_not_utc() {
        let mut conn = test_conn();
        seed_user(&mut conn, "u1");
        // 22:30 UTC on the 1st is already the 2nd in UTC+3 …
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
        // … and 01:30 UTC on the 2nd is still the 1st in UTC-8.
        seed_meal(
            &mut conn,
            "early",
            "u1",
            at(2026, 8, 2, 1, 30),
            -480,
            MealStatus::Confirmed,
            1,
            1.0,
            None,
        );

        let first = NaiveDate::from_ymd_opt(2026, 8, 1).unwrap();
        let second = NaiveDate::from_ymd_opt(2026, 8, 2).unwrap();

        let day_one = load_meals_in_local_range(&mut conn, "u1", Some(first), Some(first)).unwrap();
        assert_eq!(
            day_one.iter().map(|m| m.id.as_str()).collect::<Vec<_>>(),
            vec!["early"]
        );

        let day_two =
            load_meals_in_local_range(&mut conn, "u1", Some(second), Some(second)).unwrap();
        assert_eq!(
            day_two.iter().map(|m| m.id.as_str()).collect::<Vec<_>>(),
            vec!["late"]
        );
    }

    #[test]
    fn a_response_scales_by_portion_and_reads_only_the_current_revision() {
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
        // Superseded revision — must not be counted.
        seed_item(&mut conn, "m1", 1, 1000.0, 100.0, 10.0, 10.0);
        seed_item(&mut conn, "m1", 2, 800.0, 40.0, 20.0, 60.0);

        let meal = load_owned_meal(&mut conn, "u1", "m1").unwrap();
        let response = build_meal_responses(&mut conn, vec![meal])
            .unwrap()
            .pop()
            .unwrap();

        assert_eq!(response.items.len(), 1);
        assert_eq!(response.totals.kcal, 400.0);
        assert_eq!(response.totals.protein_g, 20.0);
        // No thumbnail row was seeded, so no URL is advertised.
        assert!(response.thumbnail_url.is_none());
        assert!(response.error.is_none());
    }

    #[test]
    fn a_failed_meal_carries_its_reason_rather_than_stalling_silently() {
        let mut conn = test_conn();
        seed_user(&mut conn, "u1");
        seed_meal(
            &mut conn,
            "m1",
            "u1",
            at(2026, 8, 1, 12, 0),
            180,
            MealStatus::Failed,
            1,
            1.0,
            None,
        );
        diesel::insert_into(analysis_jobs::table)
            .values(&NewAnalysisJob {
                id: "j1".into(),
                meal_id: "m1".into(),
                revision: 1,
                parent_job_id: None,
                status: JobStatus::Failed.as_str().to_string(),
                attempts: 3,
                model: "gpt-4.1".into(),
                user_feedback: None,
                steps: 1,
                tool_calls: 0,
                prompt_tokens: 0,
                completion_tokens: 0,
                cost_micro_usd: 0,
                error: Some("quota exhausted".into()),
                started_at: None,
                created_at: at(2026, 8, 1, 12, 1),
                finished_at: Some(at(2026, 8, 1, 12, 2)),
            })
            .execute(&mut conn)
            .unwrap();

        let meal = load_owned_meal(&mut conn, "u1", "m1").unwrap();
        let response = build_meal_responses(&mut conn, vec![meal])
            .unwrap()
            .pop()
            .unwrap();
        assert_eq!(response.status, MealStatus::Failed);
        assert_eq!(response.error.as_deref(), Some("quota exhausted"));
    }

    #[test]
    fn editing_items_records_corrections_and_keeps_the_rows_that_own_them() {
        let mut conn = test_conn();
        seed_user(&mut conn, "u1");
        seed_meal(
            &mut conn,
            "m1",
            "u1",
            at(2026, 8, 1, 12, 0),
            180,
            MealStatus::NeedsReview,
            1,
            1.0,
            None,
        );
        seed_item(&mut conn, "m1", 1, 260.0, 5.0, 1.0, 57.0); // position 0
        seed_item(&mut conn, "m1", 1, 200.0, 30.0, 8.0, 0.0); // position 1

        let meal = load_owned_meal(&mut conn, "u1", "m1").unwrap();
        let existing = meal_items::table
            .filter(meal_items::meal_id.eq("m1"))
            .order(meal_items::position.asc())
            .select(MealItem::as_select())
            .load::<MealItem>(&mut conn)
            .unwrap();
        let rice_id = existing[0].id.clone();

        // Halve the rice, drop the second item, add a third — and swap the
        // order while doing it, which is what stresses the position index.
        let now = at(2026, 8, 1, 13, 0);
        apply_item_edits(
            &mut conn,
            &meal,
            &[
                PatchMealItem {
                    id: None,
                    name: "sauce".into(),
                    grams: 30.0,
                    kcal: 90.0,
                    protein_g: 0.0,
                    fat_g: 9.0,
                    carbs_g: 1.0,
                    barcode: None,
                },
                PatchMealItem {
                    id: Some(rice_id.clone()),
                    name: "rice".into(),
                    grams: 50.0,
                    kcal: 130.0,
                    protein_g: 2.5,
                    fat_g: 0.5,
                    carbs_g: 28.5,
                    barcode: None,
                },
            ],
            now,
        )
        .unwrap();

        let after = meal_items::table
            .filter(meal_items::meal_id.eq("m1"))
            .order(meal_items::position.asc())
            .select(MealItem::as_select())
            .load::<MealItem>(&mut conn)
            .unwrap();
        assert_eq!(after.len(), 2);
        assert_eq!(after[0].name, "sauce");
        assert_eq!(after[1].id, rice_id, "the edited row keeps its identity");
        assert_eq!(after[1].grams, 50.0);
        // The numbers are the user's now, which is what makes recall return
        // corrected values next time (§5).
        assert_eq!(after[1].grams_source, GramsSource::User.as_str());
        assert_eq!(after[1].macro_source, MacroSource::User.as_str());

        let corrections = item_corrections::table
            .filter(item_corrections::meal_item_id.eq(&rice_id))
            .select(item_corrections::field)
            .load::<String>(&mut conn)
            .unwrap();
        // name was unchanged ("item 0" -> "rice" *is* a change, so it is here
        // too); grams and all four macros moved.
        assert!(corrections.contains(&"grams".to_string()), "{corrections:?}");
        assert!(corrections.contains(&"kcal".to_string()), "{corrections:?}");
        assert!(corrections.contains(&"name".to_string()), "{corrections:?}");
    }

    #[test]
    fn a_patch_cannot_reach_an_item_of_another_meal() {
        let mut conn = test_conn();
        seed_user(&mut conn, "u1");
        seed_meal(
            &mut conn,
            "m1",
            "u1",
            at(2026, 8, 1, 12, 0),
            180,
            MealStatus::NeedsReview,
            1,
            1.0,
            None,
        );
        seed_meal(
            &mut conn,
            "m2",
            "u1",
            at(2026, 8, 1, 13, 0),
            180,
            MealStatus::NeedsReview,
            1,
            1.0,
            None,
        );
        seed_item(&mut conn, "m2", 1, 100.0, 1.0, 1.0, 1.0);
        let foreign_id = meal_items::table
            .filter(meal_items::meal_id.eq("m2"))
            .select(meal_items::id)
            .first::<String>(&mut conn)
            .unwrap();

        let meal = load_owned_meal(&mut conn, "u1", "m1").unwrap();
        let err = apply_item_edits(
            &mut conn,
            &meal,
            &[PatchMealItem {
                id: Some(foreign_id),
                name: "stolen".into(),
                grams: 1.0,
                kcal: 1.0,
                protein_g: 0.0,
                fat_g: 0.0,
                carbs_g: 0.0,
                barcode: None,
            }],
            at(2026, 8, 1, 14, 0),
        )
        .unwrap_err();
        assert_eq!(err.status(), StatusCode::BAD_REQUEST);
    }

    #[test]
    fn an_estimate_rebuilt_from_rows_is_unscaled_and_flags_recall() {
        let mut conn = test_conn();
        seed_user(&mut conn, "u1");
        seed_meal(
            &mut conn,
            "m1",
            "u1",
            at(2026, 8, 1, 12, 0),
            180,
            MealStatus::Confirmed,
            1,
            0.5,
            None,
        );
        seed_item(&mut conn, "m1", 1, 800.0, 40.0, 20.0, 60.0);
        diesel::update(meal_items::table.filter(meal_items::meal_id.eq("m1")))
            .set(meal_items::macro_source.eq(MacroSource::Recall.as_str()))
            .execute(&mut conn)
            .unwrap();

        let meal = load_owned_meal(&mut conn, "u1", "m1").unwrap();
        let items = meal_items::table
            .filter(meal_items::meal_id.eq("m1"))
            .select(MealItem::as_select())
            .load::<MealItem>(&mut conn)
            .unwrap();

        let estimate = to_meal_estimate(&meal, &items).unwrap();
        assert!(estimate.from_recall);
        // portion_scale is 0.5 but the estimate is the agent's own figure —
        // scaling here would double-count on the next re-emit.
        assert_eq!(estimate.totals().kcal, 800.0);
    }
}
