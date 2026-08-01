//! The agent's tools (PRD §5).
//!
//! Three, deliberately:
//!
//! * [`RecallSimilarMeals`] — **the primary accuracy mechanism.** Searches this
//!   user's own *confirmed* meals, latest revision only. Always called first.
//! * [`LookupBarcode`] — Open Food Facts by EAN. Packaged goods only, and the
//!   one place a real database still earns its keep.
//! * [`WebSearch`] — published nutrition for chain restaurants. Config-gated,
//!   off by default.
//!
//! There is deliberately **no** dish-matching tool and no USDA seeding: БЖУ for
//! prepared dishes comes from the model (§1.3).
//!
//! Each type implements `rig::tool::Tool`, which derives the provider-facing
//! JSON schema from `Args` so the schema cannot drift from the Rust type.
//!
//! Every tool body is synchronous-diesel or outbound-HTTP work, so the database
//! half runs on [`tokio::task::spawn_blocking`]: a tool that blocked a tokio
//! worker for the 60s SQLite busy timeout would stall every other concurrent
//! analysis loop in the sitting.

use crate::db::DbPool;
use crate::error::AppError;
use crate::food::{self, openfoodfacts};
use crate::models::{normalize_dish_name, MealItem, MealStatus};
use chrono::NaiveDateTime;
use diesel::prelude::*;
use rig::tool::Tool;
use rig::tool::ToolContext;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// Errors a tool can return. Rig normalizes these at the dispatch boundary.
#[derive(Debug, thiserror::Error)]
pub enum ToolError {
    /// The tool's own failure, already classified.
    #[error(transparent)]
    App(#[from] AppError),
    /// The arguments were syntactically valid but unusable.
    #[error("invalid argument: {0}")]
    InvalidArgument(String),
    /// The tool is registered but switched off by configuration.
    ///
    /// Defence in depth: [`super::AgentHandle::build_agent`] does not register a
    /// disabled tool at all, so the model can never see it. This variant exists
    /// so a mis-wired build fails with a sentence rather than a live call to a
    /// service the operator turned off.
    #[error("tool `{0}` is disabled by configuration")]
    Disabled(&'static str),
}

/// Longest tool payload retained verbatim in `agent_steps` and in the
/// model-visible note. Beyond this the text is truncated with an ellipsis.
pub const MAX_NOTE_CHARS: usize = 400;

// ---------------------------------------------------------------------------
// recall_similar_meals
// ---------------------------------------------------------------------------

/// Searches the calling user's confirmed meal history.
///
/// Scoped to one user at construction time — a tool instance can never read
/// another user's meals, which is the isolation guarantee the PRD requires.
#[derive(Clone)]
pub struct RecallSimilarMeals {
    pool: DbPool,
    user_id: String,
}

impl RecallSimilarMeals {
    /// Build the tool for one user.
    pub fn new(pool: DbPool, user_id: impl Into<String>) -> Self {
        Self {
            pool,
            user_id: user_id.into(),
        }
    }

    /// The user this instance is scoped to.
    pub fn user_id(&self) -> &str {
        &self.user_id
    }

    /// The pool this instance reads through.
    pub fn pool(&self) -> &DbPool {
        &self.pool
    }

    /// Run the search directly, outside the agent loop.
    ///
    /// Used by the recall short-circuit and by tests, which need the query to
    /// be callable without a model in the way.
    ///
    /// **Only the latest revision of `confirmed` meals is read** — never
    /// drafts, never superseded revisions, or the agent learns from its own
    /// hallucinations and from corrections the user already rejected (§8).
    pub fn search(&self, query: &str, limit: usize) -> Result<Vec<RecallHit>, AppError> {
        use crate::schema::{meal_items, meals};

        let normalized = normalize_dish_name(query);
        if normalized.is_empty() {
            return Ok(Vec::new());
        }
        let limit = limit.clamp(1, Self::MAX_LIMIT);

        let mut conn = self.pool.get()?;

        // The index on (user_id, dish_name_normalized) makes the scan cheap, and
        // a homelab user has hundreds of confirmed meals, not millions — so the
        // fuzzy half of the match runs in Rust where it can be unit-tested,
        // rather than as an untestable pile of SQL `LIKE`s.
        let candidates: Vec<CandidateRow> = meals::table
            .filter(meals::user_id.eq(self.user_id.as_str()))
            .filter(meals::status.eq(MealStatus::Confirmed.as_str()))
            .filter(meals::dish_name_normalized.is_not_null())
            .order(meals::eaten_at.desc())
            .limit(Self::SCAN_LIMIT)
            .select((
                meals::id,
                meals::dish_name,
                meals::dish_name_normalized,
                meals::eaten_at,
                meals::revision,
            ))
            .load(&mut conn)?;

        let mut scored: Vec<ScoredCandidate> = candidates
            .into_iter()
            .filter_map(|(id, dish_name, dish_normalized, eaten_at, revision)| {
                let dish_normalized = dish_normalized?;
                let score = match_score(&normalized, &dish_normalized);
                if score < MIN_MATCH_SCORE {
                    return None;
                }
                Some(ScoredCandidate {
                    score,
                    meal_id: id,
                    dish_name: dish_name.unwrap_or(dish_normalized),
                    eaten_at,
                    revision,
                })
            })
            .collect();

        // Best match first; ties broken by recency, because the most recent
        // confirmation is the one that reflects the current portion size.
        scored.sort_by(|a, b| {
            b.score
                .partial_cmp(&a.score)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then(b.eaten_at.cmp(&a.eaten_at))
        });
        scored.truncate(limit);

        let mut hits = Vec::with_capacity(scored.len());
        for ScoredCandidate {
            score,
            meal_id,
            dish_name,
            eaten_at,
            revision,
        } in scored
        {
            // `meals.revision` *is* the latest revision, so filtering items on it
            // is what keeps a superseded estimate out of the recall corpus.
            let rows: Vec<MealItem> = meal_items::table
                .filter(meal_items::meal_id.eq(meal_id.as_str()))
                .filter(meal_items::revision.eq(revision))
                .order(meal_items::position.asc())
                .select(MealItem::as_select())
                .load(&mut conn)?;

            if rows.is_empty() {
                // A confirmed meal with no items at its latest revision is not
                // ground truth about anything; skip rather than report zeros.
                continue;
            }

            let mut totals = super::schema::MacroTotals::default();
            let mut total_grams = 0.0;
            let mut items = Vec::with_capacity(rows.len());
            for row in rows.iter().take(Self::MAX_ITEMS_PER_HIT) {
                total_grams += row.grams;
                totals = totals.plus(super::schema::MacroTotals {
                    kcal: row.kcal,
                    protein_g: row.protein_g,
                    fat_g: row.fat_g,
                    carbs_g: row.carbs_g,
                });
                items.push(RecallItem {
                    name: row.name.clone(),
                    grams: row.grams,
                    kcal: row.kcal,
                    protein_g: row.protein_g,
                    fat_g: row.fat_g,
                    carbs_g: row.carbs_g,
                });
            }

            hits.push(RecallHit {
                meal_id,
                dish_name,
                eaten_at: eaten_at.format("%Y-%m-%dT%H:%M:%S").to_string(),
                revision,
                total_grams,
                totals,
                items,
                match_score: score,
            });
        }

        Ok(hits)
    }
}

/// Arguments for `recall_similar_meals`.
#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
pub struct RecallArgs {
    /// The dish name to look for, as identified from the photo or supplied by
    /// the user.
    pub query: String,
    /// Maximum number of matches to return. Defaults to
    /// [`RecallSimilarMeals::DEFAULT_LIMIT`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limit: Option<usize>,
}

/// One previously confirmed meal that matched.
#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
pub struct RecallHit {
    /// `meals.id`, so a caller can trace the source.
    pub meal_id: String,
    /// The dish name as the user confirmed it.
    pub dish_name: String,
    /// When it was eaten, ISO-8601.
    pub eaten_at: String,
    /// Revision the figures come from.
    pub revision: i32,
    /// Total grams across the meal's items.
    pub total_grams: f64,
    /// Confirmed totals for the whole meal.
    pub totals: super::schema::MacroTotals,
    /// The individual items, so the model can adapt a portion rather than
    /// scaling a single opaque number.
    pub items: Vec<RecallItem>,
    /// 0.0–1.0 name-match strength.
    pub match_score: f64,
}

/// One item of a recalled meal.
#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
pub struct RecallItem {
    /// Component name.
    pub name: String,
    /// Confirmed grams.
    pub grams: f64,
    /// Confirmed energy.
    pub kcal: f64,
    /// Confirmed protein.
    pub protein_g: f64,
    /// Confirmed fat.
    pub fat_g: f64,
    /// Confirmed carbohydrate.
    pub carbs_g: f64,
}

/// What the tool hands back to the model.
#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
pub struct RecallOutput {
    /// Matches, best first. Empty when the user has never confirmed this dish.
    pub hits: Vec<RecallHit>,
    /// Advice rendered for the model, e.g. "no confirmed history for this dish".
    pub note: String,
}

/// The columns [`RecallSimilarMeals::search`] scans: id, display name,
/// normalized name, when it was eaten, and its latest revision.
type CandidateRow = (String, Option<String>, Option<String>, NaiveDateTime, i32);

/// A candidate that cleared [`MIN_MATCH_SCORE`], awaiting its items.
struct ScoredCandidate {
    score: f64,
    meal_id: String,
    dish_name: String,
    eaten_at: NaiveDateTime,
    revision: i32,
}

/// Minimum name-match strength before a confirmed meal is offered as recall.
///
/// Tuned so `шаурма с курицей` still matches `шаурма с говядиной` (0.67 — worth
/// showing, the model can reject it) while `pizza margherita` does not match
/// `pizza pepperoni` (0.5).
pub const MIN_MATCH_SCORE: f64 = 0.6;

/// Score two normalized dish names against each other, 0.0–1.0.
///
/// Token-set overlap normalized by the *longer* name, so a short query cannot
/// score highly against a long unrelated dish; plus a floor for the case where
/// every query token appears in the candidate ("pizza" against
/// "pizza margherita quattro formaggi"), which is a genuine recall hit that raw
/// overlap would under-score.
///
/// Both arguments must already be [`normalize_dish_name`]d.
pub fn match_score(query_normalized: &str, candidate_normalized: &str) -> f64 {
    if query_normalized.is_empty() || candidate_normalized.is_empty() {
        return 0.0;
    }
    if query_normalized == candidate_normalized {
        return 1.0;
    }

    let query_tokens = unique_tokens(query_normalized);
    let candidate_tokens = unique_tokens(candidate_normalized);
    if query_tokens.is_empty() || candidate_tokens.is_empty() {
        return 0.0;
    }

    let overlap = query_tokens
        .iter()
        .filter(|token| candidate_tokens.contains(*token))
        .count();
    let denominator = query_tokens.len().max(candidate_tokens.len()) as f64;
    let mut score = overlap as f64 / denominator;

    if overlap == query_tokens.len() || overlap == candidate_tokens.len() {
        score = score.max(0.7);
    }

    score.clamp(0.0, 1.0)
}

/// Whitespace-split tokens with duplicates removed, so a repeated word cannot
/// inflate the overlap count.
fn unique_tokens(normalized: &str) -> Vec<&str> {
    let mut tokens: Vec<&str> = normalized.split(' ').filter(|t| !t.is_empty()).collect();
    tokens.sort_unstable();
    tokens.dedup();
    tokens
}

impl RecallSimilarMeals {
    /// Default number of matches returned when the model does not ask.
    pub const DEFAULT_LIMIT: usize = 3;

    /// Hard cap on what the model may ask for, so one tool call cannot drag the
    /// whole history into the context window.
    pub const MAX_LIMIT: usize = 8;

    /// How many recent confirmed meals are scored per call.
    pub const SCAN_LIMIT: i64 = 400;

    /// Items reported per hit. A dish with more components than this is being
    /// summarized, not reproduced.
    pub const MAX_ITEMS_PER_HIT: usize = 12;
}

impl Tool for RecallSimilarMeals {
    const NAME: &'static str = "recall_similar_meals";
    type Args = RecallArgs;
    type Output = RecallOutput;
    type Error = ToolError;

    fn description(&self) -> String {
        "Search this user's own previously CONFIRMED meals by dish name. Call \
this FIRST, before estimating anything. A confident hit is ground truth the \
user already vetted — prefer it over your visual read and adjust only for a \
visibly different portion. Returns per-item grams and macros so you can scale \
a portion rather than a single number."
            .to_string()
    }

    fn parameters(&self) -> serde_json::Value {
        serde_json::to_value(schemars::schema_for!(RecallArgs)).unwrap_or_else(|_| {
            serde_json::json!({ "type": "object", "properties": {} })
        })
    }

    async fn call(
        &self,
        _context: &mut ToolContext,
        args: Self::Args,
    ) -> Result<Self::Output, Self::Error> {
        let limit = args.limit.unwrap_or(Self::DEFAULT_LIMIT);
        let this = self.clone();
        let query = args.query.clone();

        let hits = tokio::task::spawn_blocking(move || this.search(&query, limit))
            .await
            .map_err(AppError::from)??;

        let note = if hits.is_empty() {
            format!(
                "No confirmed history for {:?}. Estimate from the photo and lean on the \
container for scale.",
                truncate(&args.query, 80)
            )
        } else {
            let best = &hits[0];
            format!(
                "{} confirmed match(es) from this user's own history. Best: {:?} \
(match {:.2}, {:.0} kcal over {:.0}g, eaten {}). These figures are ground truth the \
user already vetted — prefer them over your visual read and adjust only for a \
visibly different portion, saying so in the reasoning note.",
                hits.len(),
                truncate(&best.dish_name, 80),
                best.match_score,
                best.totals.kcal,
                best.total_grams,
                best.eaten_at,
            )
        };

        Ok(RecallOutput { hits, note })
    }
}

// ---------------------------------------------------------------------------
// lookup_barcode
// ---------------------------------------------------------------------------

/// Resolves an EAN through the `foods` cache, then Open Food Facts.
#[derive(Clone)]
pub struct LookupBarcode {
    pool: DbPool,
    http: reqwest::Client,
}

impl LookupBarcode {
    /// Build the tool.
    pub fn new(pool: DbPool, http: reqwest::Client) -> Self {
        Self { pool, http }
    }

    /// The pool this instance reads and writes the cache through.
    pub fn pool(&self) -> &DbPool {
        &self.pool
    }

    /// The HTTP client used for Open Food Facts.
    pub fn http(&self) -> &reqwest::Client {
        &self.http
    }

    /// Read the cache on a blocking thread.
    async fn cached(&self, ean: &str) -> Result<Option<crate::models::Food>, AppError> {
        let pool = self.pool.clone();
        let ean = ean.to_string();
        tokio::task::spawn_blocking(move || {
            let mut conn = pool.get()?;
            food::lookup_cached(&mut conn, &ean)
        })
        .await?
    }

    /// Write a freshly fetched product to the cache on a blocking thread.
    async fn cache(&self, facts: food::FoodFacts) -> Result<crate::models::Food, AppError> {
        let pool = self.pool.clone();
        tokio::task::spawn_blocking(move || {
            let mut conn = pool.get()?;
            food::upsert_food(&mut conn, &facts)
        })
        .await?
    }
}

/// Arguments for `lookup_barcode`.
#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
pub struct BarcodeArgs {
    /// EAN-8 / EAN-13 / UPC digits, no separators.
    pub ean: String,
}

/// Per-100g nutrition for a packaged product.
#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
pub struct BarcodeOutput {
    /// True when the barcode resolved.
    pub found: bool,
    /// Product name, when found.
    pub name: Option<String>,
    /// Brand, when known.
    pub brand: Option<String>,
    /// Energy per 100g.
    pub kcal_100g: Option<f64>,
    /// Protein per 100g.
    pub protein_100g: Option<f64>,
    /// Fat per 100g.
    pub fat_100g: Option<f64>,
    /// Carbohydrate per 100g.
    pub carbs_100g: Option<f64>,
    /// Rendered advice for the model.
    pub note: String,
}

impl BarcodeOutput {
    /// The "no such product" answer. Not an error: an unknown barcode is a
    /// perfectly ordinary outcome the model should route around.
    fn miss(ean: &str) -> Self {
        Self {
            found: false,
            name: None,
            brand: None,
            kcal_100g: None,
            protein_100g: None,
            fat_100g: None,
            carbs_100g: None,
            note: format!(
                "No product found for barcode {ean}. Estimate from the package \
size and appearance instead."
            ),
        }
    }

    /// Render a cached or freshly fetched `foods` row for the model.
    fn from_food(row: crate::models::Food) -> Self {
        let label = match (&row.brand, row.name.as_str()) {
            (Some(brand), name) => format!("{brand} {name}"),
            (None, name) => name.to_string(),
        };
        Self {
            found: true,
            name: Some(row.name),
            brand: row.brand,
            kcal_100g: row.kcal_100g,
            protein_100g: row.protein_100g,
            fat_100g: row.fat_100g,
            carbs_100g: row.carbs_100g,
            note: format!(
                "{label}: exact per-100g nutrition. Multiply by the portion you \
estimate from the package size — these numbers are not per-serving."
            ),
        }
    }
}

/// True when a cached product is still inside its TTL.
fn cache_is_fresh(fetched_at: NaiveDateTime, now: NaiveDateTime) -> bool {
    now.signed_duration_since(fetched_at) < chrono::Duration::days(food::CACHE_TTL_DAYS)
}

impl Tool for LookupBarcode {
    const NAME: &'static str = "lookup_barcode";
    type Args = BarcodeArgs;
    type Output = BarcodeOutput;
    type Error = ToolError;

    fn description(&self) -> String {
        "Look up a packaged product by its barcode (EAN/UPC) in Open Food Facts. \
Use this only for packaged goods and drinks where an actual barcode is \
available. Returns exact per-100g nutrition — multiply by the portion you \
estimate from the package size."
            .to_string()
    }

    fn parameters(&self) -> serde_json::Value {
        serde_json::to_value(schemars::schema_for!(BarcodeArgs)).unwrap_or_else(|_| {
            serde_json::json!({ "type": "object", "properties": {} })
        })
    }

    async fn call(
        &self,
        _context: &mut ToolContext,
        args: Self::Args,
    ) -> Result<Self::Output, Self::Error> {
        // A model-supplied "barcode" is untrusted input; reject anything that is
        // not plain digits before it reaches the provider or the cache key.
        let ean = food::validate_ean(&args.ean)
            .map_err(|e| ToolError::InvalidArgument(e.to_string()))?
            .to_string();

        let cached = self.cached(&ean).await?;
        let now = chrono::Utc::now().naive_utc();
        if let Some(row) = cached.as_ref() {
            if cache_is_fresh(row.fetched_at, now) {
                return Ok(BarcodeOutput::from_food(row.clone()));
            }
        }

        match openfoodfacts::fetch(&self.http, &ean).await {
            Ok(Some(facts)) if facts.is_usable() => {
                Ok(BarcodeOutput::from_food(self.cache(facts).await?))
            }
            // The provider knows the product but published no energy figure —
            // as useless to the model as a miss, and saying so is kinder than
            // handing back a row of nulls.
            Ok(Some(_)) | Ok(None) => Ok(cached
                .map(BarcodeOutput::from_food)
                .unwrap_or_else(|| BarcodeOutput::miss(&ean))),
            // A stale cache entry beats a failed lookup: the model gets real
            // numbers and the loop keeps its turn budget.
            Err(err) => match cached {
                Some(row) => {
                    tracing::warn!(ean = %ean, error = %err, "serving a stale cached barcode");
                    Ok(BarcodeOutput::from_food(row))
                }
                None => Err(ToolError::App(err)),
            },
        }
    }
}

// ---------------------------------------------------------------------------
// web_search
// ---------------------------------------------------------------------------

/// Published nutrition for chain restaurants. Registered only when
/// `PICWEIGHT_WEB_SEARCH_ENABLED` is true.
///
/// Backed by DuckDuckGo's keyless Instant Answer API, so enabling the tool costs
/// no extra credential — deliberate, since the whole feature is off by default
/// and a self-hoster should not have to source a search API key to try it.
#[derive(Clone)]
pub struct WebSearch {
    http: reqwest::Client,
    enabled: bool,
}

impl WebSearch {
    /// Build the tool, enabled.
    pub fn new(http: reqwest::Client) -> Self {
        Self::gated(http, true)
    }

    /// Build the tool with an explicit on/off switch.
    pub fn gated(http: reqwest::Client, enabled: bool) -> Self {
        Self { http, enabled }
    }

    /// The HTTP client used for the search.
    pub fn http(&self) -> &reqwest::Client {
        &self.http
    }

    /// Whether this instance will actually search.
    pub fn enabled(&self) -> bool {
        self.enabled
    }
}

/// Endpoint used for the (keyless) search.
pub const SEARCH_ENDPOINT: &str = "https://api.duckduckgo.com/";

/// Most results handed back to the model.
pub const MAX_SEARCH_RESULTS: usize = 5;

/// Arguments for `web_search`.
#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
pub struct WebSearchArgs {
    /// Free-text query, e.g. "Burger King Whopper nutrition kcal".
    pub query: String,
}

/// Search results, trimmed to what a model can use.
#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
pub struct WebSearchOutput {
    /// Result snippets, best first.
    pub results: Vec<WebSearchResult>,
    /// Rendered advice for the model.
    pub note: String,
}

/// One search result.
#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
pub struct WebSearchResult {
    /// Page title.
    pub title: String,
    /// Source URL.
    pub url: String,
    /// Extracted snippet.
    pub snippet: String,
}

/// Pull usable results out of a DuckDuckGo Instant Answer document.
///
/// The document has two shapes worth reading: a top-level `Abstract*` block, and
/// `RelatedTopics`, whose entries are either a flat `{Text, FirstURL}` or a
/// category `{Name, Topics: [...]}` that has to be walked one level deeper.
pub fn parse_search_results(value: &serde_json::Value, max: usize) -> Vec<WebSearchResult> {
    let mut results = Vec::new();

    let abstract_text = value
        .get("AbstractText")
        .and_then(serde_json::Value::as_str)
        .unwrap_or_default();
    if !abstract_text.is_empty() {
        results.push(WebSearchResult {
            title: value
                .get("Heading")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("Summary")
                .to_string(),
            url: value
                .get("AbstractURL")
                .and_then(serde_json::Value::as_str)
                .unwrap_or_default()
                .to_string(),
            snippet: truncate(abstract_text, MAX_NOTE_CHARS),
        });
    }

    if let Some(topics) = value
        .get("RelatedTopics")
        .and_then(serde_json::Value::as_array)
    {
        for topic in topics {
            if results.len() >= max {
                break;
            }
            let flat = std::slice::from_ref(topic);
            let nested = topic
                .get("Topics")
                .and_then(serde_json::Value::as_array)
                .map(Vec::as_slice)
                .unwrap_or(flat);
            for entry in nested {
                if results.len() >= max {
                    break;
                }
                let (Some(text), Some(url)) = (
                    entry.get("Text").and_then(serde_json::Value::as_str),
                    entry.get("FirstURL").and_then(serde_json::Value::as_str),
                ) else {
                    continue;
                };
                if text.is_empty() {
                    continue;
                }
                let title = text.split(" - ").next().unwrap_or(text);
                results.push(WebSearchResult {
                    title: truncate(title, 120),
                    url: url.to_string(),
                    snippet: truncate(text, MAX_NOTE_CHARS),
                });
            }
        }
    }

    results.truncate(max);
    results
}

impl Tool for WebSearch {
    const NAME: &'static str = "web_search";
    type Args = WebSearchArgs;
    type Output = WebSearchOutput;
    type Error = ToolError;

    fn description(&self) -> String {
        "Search the web for published nutrition figures from an identifiable \
chain restaurant. Use sparingly and only when the dish clearly comes from a \
chain that publishes its numbers."
            .to_string()
    }

    fn parameters(&self) -> serde_json::Value {
        serde_json::to_value(schemars::schema_for!(WebSearchArgs)).unwrap_or_else(|_| {
            serde_json::json!({ "type": "object", "properties": {} })
        })
    }

    async fn call(
        &self,
        _context: &mut ToolContext,
        args: Self::Args,
    ) -> Result<Self::Output, Self::Error> {
        if !self.enabled {
            return Err(ToolError::Disabled(Self::NAME));
        }
        let query = args.query.trim();
        if query.is_empty() {
            return Err(ToolError::InvalidArgument(
                "query must not be empty".to_string(),
            ));
        }

        let response = self
            .http
            .get(SEARCH_ENDPOINT)
            .query(&[
                ("q", query),
                ("format", "json"),
                ("no_html", "1"),
                ("skip_disambig", "1"),
                ("t", "picweight"),
            ])
            .send()
            .await
            .map_err(AppError::from)?
            .error_for_status()
            .map_err(AppError::from)?;

        let body: serde_json::Value = response.json().await.map_err(AppError::from)?;
        let results = parse_search_results(&body, MAX_SEARCH_RESULTS);

        let note = if results.is_empty() {
            "No published figures found. Fall back to your own estimate and keep \
confidence low."
                .to_string()
        } else {
            "Published figures are per the chain's own serving definition — check \
that the serving matches the portion in the photo before using them."
                .to_string()
        };

        Ok(WebSearchOutput { results, note })
    }
}

/// Truncate on a character boundary, appending an ellipsis when anything was cut.
pub(crate) fn truncate(text: &str, max_chars: usize) -> String {
    if text.chars().count() <= max_chars {
        return text.to_string();
    }
    let mut out: String = text.chars().take(max_chars).collect();
    out.push('…');
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_exact_normalized_name_scores_one() {
        assert_eq!(match_score("шаурма с курицей", "шаурма с курицей"), 1.0);
    }

    #[test]
    fn a_longer_confirmed_name_containing_the_query_still_matches() {
        // "pizza" from a vision read against a dish the user confirmed in full.
        let score = match_score("pizza", "pizza margherita quattro formaggi");
        assert!(score >= MIN_MATCH_SCORE, "expected a hit, scored {score}");
    }

    #[test]
    fn a_different_filling_is_offered_but_a_different_dish_is_not() {
        // Same dish, different filling: worth showing, the model can reject it.
        assert!(match_score("шаурма с курицей", "шаурма с говядиной") >= MIN_MATCH_SCORE);
        // Different pizza entirely: not recall, just a coincidence of one word.
        assert!(match_score("pizza margherita", "pizza pepperoni") < MIN_MATCH_SCORE);
    }

    #[test]
    fn an_empty_query_never_matches() {
        assert_eq!(match_score("", "шаурма"), 0.0);
        assert_eq!(match_score("шаурма", ""), 0.0);
    }

    #[test]
    fn repeated_tokens_do_not_inflate_the_score() {
        // Without deduplication "pizza pizza pizza" would overlap three times.
        assert!(match_score("pizza pizza pizza", "pizza margherita") < 1.0);
    }

    #[test]
    fn a_fresh_cache_entry_is_used_and_a_stale_one_is_not() {
        let now = chrono::NaiveDate::from_ymd_opt(2026, 8, 1)
            .expect("valid date")
            .and_hms_opt(12, 0, 0)
            .expect("valid time");
        let fresh = now - chrono::Duration::days(food::CACHE_TTL_DAYS - 1);
        let stale = now - chrono::Duration::days(food::CACHE_TTL_DAYS + 1);
        assert!(cache_is_fresh(fresh, now));
        assert!(!cache_is_fresh(stale, now));
    }

    #[test]
    fn search_results_come_from_both_the_abstract_and_related_topics() {
        let body = serde_json::json!({
            "Heading": "Whopper",
            "AbstractText": "The Whopper is a hamburger.",
            "AbstractURL": "https://example.org/whopper",
            "RelatedTopics": [
                { "Text": "Whopper Jr. - a smaller burger", "FirstURL": "https://example.org/jr" },
                { "Name": "Menu", "Topics": [
                    { "Text": "Double Whopper - two patties", "FirstURL": "https://example.org/double" }
                ]},
                { "Text": "", "FirstURL": "https://example.org/empty" }
            ]
        });
        let results = parse_search_results(&body, MAX_SEARCH_RESULTS);
        assert_eq!(results.len(), 3);
        assert_eq!(results[0].title, "Whopper");
        assert_eq!(results[1].title, "Whopper Jr.");
        assert_eq!(results[2].url, "https://example.org/double");
    }

    #[test]
    fn search_results_respect_the_cap() {
        let topics: Vec<serde_json::Value> = (0..20)
            .map(|i| serde_json::json!({ "Text": format!("t{i}"), "FirstURL": "https://x/" }))
            .collect();
        let body = serde_json::json!({ "RelatedTopics": topics });
        assert_eq!(
            parse_search_results(&body, MAX_SEARCH_RESULTS).len(),
            MAX_SEARCH_RESULTS
        );
    }

    #[test]
    fn truncation_is_character_safe() {
        assert_eq!(truncate("шаурма", 10), "шаурма");
        assert_eq!(truncate("шаурма", 3), "шау…");
    }

    // -- recall against a real database ------------------------------------
    //
    // §8's invariant is the whole point of this tool: "Recall reads only the
    // latest revision of `confirmed` meals — never drafts, never superseded
    // revisions, or the agent learns from its own hallucinations and from
    // corrections the user already rejected." That is not testable against the
    // scorer alone, so these run against a migrated temp SQLite file.

    use crate::feedback::state::fixtures::{at, seed_user, test_pool};
    use crate::models::{GramsSource, MacroSource, NameSource, NewMeal, NewMealItem};
    use diesel::SqliteConnection;

    #[allow(clippy::too_many_arguments)]
    fn seed_named_meal(
        conn: &mut SqliteConnection,
        id: &str,
        user_id: &str,
        dish_name: &str,
        status: MealStatus,
        revision: i32,
        eaten_at: chrono::NaiveDateTime,
    ) {
        diesel::insert_into(crate::schema::meals::table)
            .values(&NewMeal {
                id: id.to_string(),
                user_id: user_id.to_string(),
                client_uuid: format!("client-{id}"),
                thumbnail_id: None,
                group_id: None,
                group_size: None,
                dish_name: Some(dish_name.to_string()),
                dish_name_normalized: Some(normalize_dish_name(dish_name)),
                name_source: NameSource::Vision.as_str().to_string(),
                user_comment: None,
                revision,
                eaten_at,
                timezone_offset: 180,
                meal_type: None,
                status: status.as_str().to_string(),
                portion_scale: 1.0,
                created_at: eaten_at,
                updated_at: eaten_at,
            })
            .execute(conn)
            .expect("seed meal");
    }

    fn seed_named_item(
        conn: &mut SqliteConnection,
        meal_id: &str,
        revision: i32,
        name: &str,
        grams: f64,
        kcal: f64,
    ) {
        use crate::schema::meal_items as mi;
        let position: i64 = mi::table
            .filter(mi::meal_id.eq(meal_id))
            .filter(mi::revision.eq(revision))
            .count()
            .get_result(conn)
            .expect("count items");

        diesel::insert_into(crate::schema::meal_items::table)
            .values(&NewMealItem {
                id: uuid::Uuid::new_v4().to_string(),
                meal_id: meal_id.to_string(),
                revision,
                position: position as i32,
                name: name.to_string(),
                barcode: None,
                grams,
                grams_source: GramsSource::Agent.as_str().to_string(),
                kcal,
                protein_g: 10.0,
                fat_g: 5.0,
                carbs_g: 20.0,
                macro_source: MacroSource::Model.as_str().to_string(),
                confidence: Some(0.7),
                reasoning_note: Some("seeded".into()),
                created_at: at(2026, 1, 1, 0, 0),
            })
            .execute(conn)
            .expect("seed item");
    }

    #[test]
    fn recall_returns_a_confirmed_meal_with_its_items() {
        let pool = test_pool();
        {
            let mut conn = pool.get().expect("connection");
            seed_user(&mut conn, "u1");
            seed_named_meal(
                &mut conn,
                "m1",
                "u1",
                "Шаурма с курицей",
                MealStatus::Confirmed,
                1,
                at(2026, 7, 30, 13, 0),
            );
            seed_named_item(&mut conn, "m1", 1, "лаваш", 90.0, 250.0);
            seed_named_item(&mut conn, "m1", 1, "курица", 120.0, 200.0);
        }

        let hits = RecallSimilarMeals::new(pool, "u1")
            .search("шаурма с курицей", 3)
            .expect("search runs");

        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].meal_id, "m1");
        assert_eq!(hits[0].dish_name, "Шаурма с курицей");
        assert_eq!(hits[0].match_score, 1.0);
        assert_eq!(hits[0].items.len(), 2);
        assert_eq!(hits[0].total_grams, 210.0);
        assert_eq!(hits[0].totals.kcal, 450.0);
        assert_eq!(hits[0].eaten_at, "2026-07-30T13:00:00");
    }

    #[test]
    fn recall_reads_only_the_latest_revision() {
        let pool = test_pool();
        {
            let mut conn = pool.get().expect("connection");
            seed_user(&mut conn, "u1");
            // The user said "too much rice"; revision 2 is what they accepted.
            seed_named_meal(
                &mut conn,
                "m1",
                "u1",
                "карри с рисом",
                MealStatus::Confirmed,
                2,
                at(2026, 7, 30, 13, 0),
            );
            seed_named_item(&mut conn, "m1", 1, "рис", 300.0, 400.0);
            seed_named_item(&mut conn, "m1", 2, "рис", 150.0, 200.0);
        }

        let hits = RecallSimilarMeals::new(pool, "u1")
            .search("карри с рисом", 3)
            .expect("search runs");

        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].revision, 2);
        assert_eq!(hits[0].items.len(), 1, "the superseded revision leaked in");
        assert_eq!(hits[0].items[0].grams, 150.0);
    }

    #[test]
    fn recall_ignores_drafts_and_failures() {
        let pool = test_pool();
        {
            let mut conn = pool.get().expect("connection");
            seed_user(&mut conn, "u1");
            for (id, status) in [
                ("m-pending", MealStatus::Pending),
                ("m-analyzing", MealStatus::Analyzing),
                ("m-review", MealStatus::NeedsReview),
                ("m-failed", MealStatus::Failed),
            ] {
                seed_named_meal(&mut conn, id, "u1", "борщ", status, 1, at(2026, 7, 30, 13, 0));
                seed_named_item(&mut conn, id, 1, "борщ", 400.0, 300.0);
            }
        }

        let hits = RecallSimilarMeals::new(pool, "u1")
            .search("борщ", 5)
            .expect("search runs");
        assert!(hits.is_empty(), "an unconfirmed meal is not ground truth");
    }

    #[test]
    fn recall_never_crosses_users() {
        let pool = test_pool();
        {
            let mut conn = pool.get().expect("connection");
            seed_user(&mut conn, "u1");
            seed_user(&mut conn, "u2");
            seed_named_meal(
                &mut conn,
                "m2",
                "u2",
                "борщ",
                MealStatus::Confirmed,
                1,
                at(2026, 7, 30, 13, 0),
            );
            seed_named_item(&mut conn, "m2", 1, "борщ", 400.0, 300.0);
        }

        let hits = RecallSimilarMeals::new(pool, "u1")
            .search("борщ", 5)
            .expect("search runs");
        assert!(hits.is_empty(), "user isolation was breached");
    }

    #[test]
    fn recall_ranks_by_score_then_recency_and_honours_the_limit() {
        let pool = test_pool();
        {
            let mut conn = pool.get().expect("connection");
            seed_user(&mut conn, "u1");
            for (id, name, day) in [
                ("m1", "шаурма с курицей", 20),
                ("m2", "шаурма с курицей", 28),
                ("m3", "шаурма с говядиной", 29),
            ] {
                seed_named_meal(
                    &mut conn,
                    id,
                    "u1",
                    name,
                    MealStatus::Confirmed,
                    1,
                    at(2026, 7, day, 13, 0),
                );
                seed_named_item(&mut conn, id, 1, "начинка", 200.0, 500.0);
            }
        }

        let recall = RecallSimilarMeals::new(pool, "u1");

        let hits = recall.search("шаурма с курицей", 5).expect("search runs");
        assert_eq!(hits.len(), 3);
        // Exact matches first, newest of them leading; the beef one trails.
        assert_eq!(hits[0].meal_id, "m2");
        assert_eq!(hits[1].meal_id, "m1");
        assert_eq!(hits[2].meal_id, "m3");

        assert_eq!(recall.search("шаурма с курицей", 1).expect("limited").len(), 1);
    }

    #[test]
    fn recall_skips_a_confirmed_meal_with_no_items_at_its_revision() {
        let pool = test_pool();
        {
            let mut conn = pool.get().expect("connection");
            seed_user(&mut conn, "u1");
            seed_named_meal(
                &mut conn,
                "m1",
                "u1",
                "борщ",
                MealStatus::Confirmed,
                3,
                at(2026, 7, 30, 13, 0),
            );
            // Items exist, but only at a revision the meal has moved past.
            seed_named_item(&mut conn, "m1", 1, "борщ", 400.0, 300.0);
        }

        let hits = RecallSimilarMeals::new(pool, "u1")
            .search("борщ", 5)
            .expect("search runs");
        assert!(hits.is_empty(), "reported a meal with no current figures");
    }

    #[test]
    fn an_unknown_dish_recalls_nothing_rather_than_erroring() {
        let pool = test_pool();
        {
            let mut conn = pool.get().expect("connection");
            seed_user(&mut conn, "u1");
        }
        let recall = RecallSimilarMeals::new(pool, "u1");
        assert!(recall.search("паэлья", 3).expect("search runs").is_empty());
        // Punctuation-only input normalizes to nothing; that is a miss, not a scan.
        assert!(recall.search("!!!", 3).expect("search runs").is_empty());
    }

    #[tokio::test]
    async fn the_recall_tool_renders_a_note_for_a_hit_and_for_a_miss() {
        let pool = test_pool();
        {
            let mut conn = pool.get().expect("connection");
            seed_user(&mut conn, "u1");
            seed_named_meal(
                &mut conn,
                "m1",
                "u1",
                "шаурма",
                MealStatus::Confirmed,
                1,
                at(2026, 7, 30, 13, 0),
            );
            seed_named_item(&mut conn, "m1", 1, "начинка", 200.0, 500.0);
        }
        let tool = RecallSimilarMeals::new(pool, "u1");
        let mut context = ToolContext::new();

        let hit = tool
            .call(
                &mut context,
                RecallArgs {
                    query: "шаурма".into(),
                    limit: None,
                },
            )
            .await
            .expect("the tool runs");
        assert_eq!(hit.hits.len(), 1);
        assert!(hit.note.contains("ground truth"), "{}", hit.note);

        let miss = tool
            .call(
                &mut context,
                RecallArgs {
                    query: "паэлья".into(),
                    limit: Some(2),
                },
            )
            .await
            .expect("the tool runs");
        assert!(miss.hits.is_empty());
        assert!(miss.note.contains("No confirmed history"), "{}", miss.note);
    }

    #[tokio::test]
    async fn a_disabled_web_search_refuses_rather_than_calling_out() {
        let tool = WebSearch::gated(reqwest::Client::new(), false);
        let mut context = ToolContext::new();
        let err = tool
            .call(
                &mut context,
                WebSearchArgs {
                    query: "whopper kcal".into(),
                },
            )
            .await
            .expect_err("a disabled tool must not search");
        assert!(matches!(err, ToolError::Disabled("web_search")));
    }
}
