//! The estimation agent — a bounded agentic loop on rig 0.41 (PRD §5).
//!
//! **This directory is the swap boundary.** Every rig type stays behind it, so
//! replacing rig with hand-rolled `async-openai` touches only `src/agent/`.
//! Nothing outside this module should name a `rig::` type.
//!
//! ## The loop
//!
//! identify → recall → estimate → critique → emit, bounded by
//! [`MAX_TURNS`] model calls and [`RUN_TIMEOUT`] wall clock.
//!
//! **`max_turns` counts model calls, not tool calls** (docs/rig-spike.md): a run
//! that calls two tools and then answers costs three. Six comfortably fits the
//! five-step loop.
//!
//! On `PromptError::MaxTurnsError`, tool failure or timeout, [`AgentHandle`]
//! falls back to [`AgentHandle::single_shot`] so the user always gets a draft.
//! A 429 / quota-exhausted response is *not* absorbed: it surfaces as
//! [`AppError::RateLimited`] and the meal goes to `failed` with a visible
//! reason (§5 bounds).
//!
//! ## Sessions
//!
//! The full message thread is serialized to `agent_sessions.messages` when a
//! job finishes, and *continued* rather than restarted on re-analysis — see
//! [`session`] and [`reanalyze`]. The photograph is **stripped** from the stored
//! thread and re-attached from disk on every resume, because provider message
//! formats vary on image retention and a base64 payload in the transcript is
//! pure weight (§5 caveats).

pub mod prompts;
pub mod reanalyze;
pub mod schema;
pub mod session;
pub mod tools;

use crate::config::Config;
use crate::db::DbPool;
use crate::error::AppError;
use chrono::NaiveDateTime;
use rig::agent::{
    AgentHook, HookContext, OutputMode, PromptResponse, ToolCall as ToolCallEvent, ToolCallAction,
    ToolResultAction, ToolResultEvent,
};
use rig::client::CompletionClient;
use rig::completion::{Message, PromptError};
use rig::message::{ImageMediaType, UserContent};
use rig::OneOrMany;
use schema::MealEstimate;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// The concrete completion model behind the agent, named through the trait so
/// a provider swap is one line.
pub type ChatModel = <rig::providers::openai::Client as rig::client::CompletionClient>::CompletionModel;

/// The agent type this crate builds.
pub type PicweightAgent = rig::agent::Agent<ChatModel>;

/// Total model-call budget per run — *not* a tool-call budget.
pub const MAX_TURNS: usize = 6;

/// Reduced budget for a correction turn: the tool results are already in the
/// thread, so a continuation should need one call, two at most.
pub const REANALYZE_MAX_TURNS: usize = 3;

/// Model-call budget for the tool-free fallback.
///
/// Two rather than one: rig re-prompts a bounded number of times when a
/// structured response misses a required field, and spending the user's only
/// draft on a single malformed answer would defeat the point of the fallback.
pub const SINGLE_SHOT_MAX_TURNS: usize = 2;

/// Wall-clock budget for one run. §13: drift past this breaks the feedback
/// promise, since this cap *is* the user-facing latency budget.
pub const RUN_TIMEOUT: Duration = Duration::from_secs(25);

/// Wall-clock budget for the fallback call.
///
/// This is spent *after* [`RUN_TIMEOUT`] in the worst case, so a fully degraded
/// analysis can take ~45s. That is the deliberate trade in §5: a late draft
/// beats no draft.
pub const FALLBACK_TIMEOUT: Duration = Duration::from_secs(20);

/// Longest tool payload written to `agent_steps`. The table exists to make a bad
/// estimate debuggable, not to be a second copy of Open Food Facts.
pub const MAX_STEP_PAYLOAD_CHARS: usize = 2_000;

/// Placeholder left where a photograph was removed from a stored thread.
pub const STRIPPED_IMAGE_PLACEHOLDER: &str =
    "[photograph omitted from stored history — re-attached from disk on resume]";

/// Everything one agent run needs to know, assembled by the analysis worker.
#[derive(Debug, Clone)]
pub struct AnalysisContext {
    /// Owning user — scopes `recall_similar_meals`.
    pub user_id: String,
    /// The meal being estimated.
    pub meal_id: String,
    /// The `analysis_jobs` row this run belongs to; `agent_steps` reference it.
    pub job_id: String,
    /// Revision being produced.
    pub revision: i32,
    /// Dish name from a recent-dish chip, share intent, or comment. When
    /// present it outranks the agent's visual read.
    pub dish_name: Option<String>,
    /// The user's free-text comment, when they typed one (rarely).
    pub user_comment: Option<String>,
    /// Absolute path of the 768px thumbnail. `None` for a manual, photo-less
    /// entry. Images are re-attached from disk rather than assumed to survive
    /// in serialized history (§5 caveats).
    pub image_path: Option<PathBuf>,
    /// When the meal was eaten, in the user's local time.
    pub eaten_at: NaiveDateTime,
    /// The user's recent confirmed dish names, for context.
    pub recent_dishes: Vec<String>,
    /// Per-user multiplier learned from correction history (§5 hidden fat).
    pub calibration_factor: f64,
}

/// One tool invocation, ready to become an `agent_steps` row.
///
/// Collected through rig's `AgentHook` (`ToolCall` / `ToolResult` events),
/// which is cleaner than parsing the multi-turn stream and works on the
/// blocking path (docs/rig-spike.md).
#[derive(Debug, Clone)]
pub struct StepRecord {
    /// 1-based ordinal within the run.
    pub step_no: i32,
    /// Tool name as registered.
    pub tool_name: String,
    /// Serialized arguments.
    pub tool_input: Option<String>,
    /// Serialized result, truncated if very large.
    pub tool_output: Option<String>,
    /// Wall time of the tool call.
    pub latency_ms: Option<i64>,
}

/// Token accounting for one run, written to `analysis_jobs`.
#[derive(Debug, Clone, Copy, Default)]
pub struct RunUsage {
    /// Input tokens across every model call in the run.
    pub prompt_tokens: i64,
    /// Output tokens across every model call in the run.
    pub completion_tokens: i64,
    /// Model calls made (rig's `max_turns` unit).
    pub model_calls: i32,
    /// Tool calls made.
    pub tool_calls: i32,
}

/// What one agent run produced.
#[derive(Debug, Clone)]
pub struct AgentOutcome {
    /// The structured estimate.
    pub estimate: MealEstimate,
    /// The full message thread, serialized as JSON for `agent_sessions`.
    pub serialized_messages: Option<String>,
    /// Number of turns in the stored thread, for the history cap.
    pub turn_count: i32,
    /// One entry per tool call.
    pub steps: Vec<StepRecord>,
    /// Token and call counters.
    pub usage: RunUsage,
    /// True when the loop breached its bound and the single-shot fallback ran.
    pub fallback_used: bool,
    /// Model id actually used.
    pub model: String,
    /// Prompt version this run executed under.
    pub prompt_version: String,
}

/// Why a run did not produce a usable outcome.
///
/// The distinction *is* §5's bounds rule: a breached budget or an unusable
/// answer degrades to a single-shot draft, while a refused key or an exhausted
/// quota must reach the user as a failed meal with a visible reason.
enum RunFailure {
    /// Do not fall back — surface this.
    Fatal(AppError),
    /// Fall back to a tool-free call; the string is the reason, for the log.
    Degraded(String),
}

/// Owns the provider client and builds a per-run agent.
///
/// A fresh [`PicweightAgent`] is built per run because the tools are
/// user-scoped: `recall_similar_meals` is constructed with the calling user's
/// id, so one user's agent can never read another's history.
pub struct AgentHandle {
    pool: DbPool,
    http: reqwest::Client,
    client: rig::providers::openai::Client,
    model: String,
    web_search_enabled: bool,
}

impl AgentHandle {
    /// Build the handle from configuration.
    ///
    /// Fails when the OpenAI client cannot be constructed — a bad key is a
    /// startup failure, not a per-meal surprise.
    ///
    /// The base URL comes from [`Config::openai_base_url`] rather than rig's
    /// compiled-in default, so an OpenAI-compatible proxy — or the PRD §13
    /// mock — is a configuration change and not a code change.
    pub fn new(config: &Config, pool: DbPool, http: reqwest::Client) -> Result<Self, AppError> {
        let client = rig::providers::openai::Client::builder()
            .api_key(config.openai_api_key.as_str())
            .base_url(&config.openai_base_url)
            .build()
            .map_err(|err| {
                AppError::Internal(format!("could not build the OpenAI client: {err}"))
            })?;

        Ok(Self {
            pool,
            http,
            client,
            model: config.openai_model.clone(),
            web_search_enabled: config.web_search_enabled,
        })
    }

    /// The model id every run uses. Stored on `analysis_jobs.model` and
    /// `agent_sessions.model`.
    pub fn model(&self) -> &str {
        &self.model
    }

    /// The prompt set version. Stored on `agent_sessions.prompt_version` and
    /// compared on resume.
    pub fn prompt_version(&self) -> &'static str {
        prompts::PROMPT_VERSION
    }

    /// Whether the `web_search` tool is registered.
    pub fn web_search_enabled(&self) -> bool {
        self.web_search_enabled
    }

    /// The pool the tools read through.
    pub fn pool(&self) -> &DbPool {
        &self.pool
    }

    /// The HTTP client the tools use for outbound calls.
    pub fn http(&self) -> &reqwest::Client {
        &self.http
    }

    /// The provider client every run is built from.
    ///
    /// `pub(crate)` on purpose: this is the one rig type that would otherwise
    /// leak past the swap boundary, and nothing outside `src/agent/` should
    /// name it.
    pub(crate) fn client(&self) -> &rig::providers::openai::Client {
        &self.client
    }

    /// Build the full agent for one run: preamble, tools scoped to `ctx.user_id`,
    /// `default_max_turns(MAX_TURNS)`, and the structured output schema.
    pub fn build_agent(&self, ctx: &AnalysisContext) -> Result<PicweightAgent, AppError> {
        // `OutputMode::Auto` is provider-aware and correct here: on OpenAI, whose
        // native structured output composes with tool calls, it resolves to
        // `Native` (schema *guaranteed*); on a provider whose native constraint
        // would suppress tool calls it resolves to `Tool` instead. Pinning either
        // one would break the other, which is exactly the trap `Auto` exists for.
        //
        // Temperature is deliberately left at the provider default: newer OpenAI
        // reasoning models reject any explicit value, and a hard 400 on a model
        // swap is worse than slightly looser sampling.
        let mut builder = rig::agent::AgentBuilder::new(
            self.client().completion_model(self.model.as_str()),
        )
        .name("picweight-estimator")
        .preamble(&prompts::system_preamble(self.web_search_enabled))
        .default_max_turns(MAX_TURNS)
        .output_schema_raw(schema::output_schema())
        .output_mode(OutputMode::Auto)
        .tool(tools::RecallSimilarMeals::new(
            self.pool.clone(),
            ctx.user_id.as_str(),
        ))
        .tool(tools::LookupBarcode::new(
            self.pool.clone(),
            self.http.clone(),
        ));

        // Config-gated and off by default (§5). Not registering it is the real
        // gate — the model never learns the tool exists.
        if self.web_search_enabled {
            builder = builder.tool(tools::WebSearch::new(self.http.clone()));
        }

        Ok(builder.build())
    }

    /// Build the tool-free agent used for the single-shot fallback.
    pub fn build_single_shot_agent(&self) -> Result<PicweightAgent, AppError> {
        Ok(
            rig::agent::AgentBuilder::new(self.client().completion_model(self.model.as_str()))
                .name("picweight-single-shot")
                .preamble(&prompts::single_shot_preamble())
                .default_max_turns(SINGLE_SHOT_MAX_TURNS)
                .output_schema_raw(schema::output_schema())
                // No tools, so the provider's native structured output cannot
                // suppress anything: take the guaranteed-conforming mode.
                .output_mode(OutputMode::Native)
                .build(),
        )
    }

    /// Run the full loop for one photo.
    ///
    /// Falls back to [`Self::single_shot`] on `MaxTurnsError`, tool failure or
    /// [`RUN_TIMEOUT`], setting [`AgentOutcome::fallback_used`]. Propagates
    /// [`AppError::RateLimited`] instead of absorbing it.
    pub async fn analyze(&self, ctx: &AnalysisContext) -> Result<AgentOutcome, AppError> {
        let prompt = self.analysis_prompt(ctx)?;
        match self
            .run_loop(ctx, prompt.clone(), None, MAX_TURNS)
            .await
        {
            Ok(outcome) => Ok(outcome),
            Err(RunFailure::Fatal(err)) => Err(err),
            Err(RunFailure::Degraded(reason)) => self.degrade(ctx, prompt, None, &reason).await,
        }
    }

    /// One tool-free vision call producing the same output shape.
    pub async fn single_shot(&self, ctx: &AnalysisContext) -> Result<AgentOutcome, AppError> {
        let prompt = self.analysis_prompt(ctx)?;
        self.run_single_shot(ctx, prompt, None).await
    }

    /// Continue a persisted thread with the user's feedback (§5).
    ///
    /// `history` comes from [`session::load`] and has already been capped. The
    /// image is re-attached from disk because provider message formats vary on
    /// image retention.
    ///
    /// The tools stay registered: the prompt tells the model its tool results
    /// are already in the conversation, and [`REANALYZE_MAX_TURNS`] leaves room
    /// for at most one round anyway — but a correction like "that's actually
    /// barcode 4600682001010" genuinely needs a lookup, and a tool-free agent
    /// could not serve it.
    pub async fn continue_session(
        &self,
        ctx: &AnalysisContext,
        history: Vec<Message>,
        feedback: &str,
    ) -> Result<AgentOutcome, AppError> {
        let prompt = self.reanalysis_prompt(ctx, feedback)?;
        match self
            .run_loop(
                ctx,
                prompt.clone(),
                Some(history.clone()),
                REANALYZE_MAX_TURNS,
            )
            .await
        {
            Ok(outcome) => Ok(outcome),
            Err(RunFailure::Fatal(err)) => Err(err),
            // The fallback keeps both the feedback and the prior thread: a
            // correction that degraded into a fresh, feedback-free estimate
            // would answer a question the user did not ask.
            Err(RunFailure::Degraded(reason)) => {
                self.degrade(ctx, prompt, Some(history), &reason).await
            }
        }
    }

    /// Start a fresh conversation seeded with the last confirmed result.
    ///
    /// Used when [`session::decide`] returns [`session::ResumeDecision::Reseed`]
    /// because the model or prompt version moved on.
    ///
    /// This is a *new* conversation with no tool results in context, so it gets
    /// the full [`MAX_TURNS`] budget rather than the correction budget.
    pub async fn reseed_session(
        &self,
        ctx: &AnalysisContext,
        previous: &MealEstimate,
        feedback: &str,
    ) -> Result<AgentOutcome, AppError> {
        let prompt = self.user_turn(prompts::reseed_context(previous, feedback), ctx)?;
        match self.run_loop(ctx, prompt.clone(), None, MAX_TURNS).await {
            Ok(outcome) => Ok(outcome),
            Err(RunFailure::Fatal(err)) => Err(err),
            Err(RunFailure::Degraded(reason)) => self.degrade(ctx, prompt, None, &reason).await,
        }
    }

    // -----------------------------------------------------------------------
    // internals
    // -----------------------------------------------------------------------

    /// The opening user turn for a first analysis: instructions plus the photo.
    fn analysis_prompt(&self, ctx: &AnalysisContext) -> Result<Message, AppError> {
        self.user_turn(prompts::analysis_user_turn(ctx), ctx)
    }

    /// The user turn for a correction, with the photo re-attached from disk.
    fn reanalysis_prompt(
        &self,
        ctx: &AnalysisContext,
        feedback: &str,
    ) -> Result<Message, AppError> {
        self.user_turn(prompts::reanalysis_turn(feedback), ctx)
    }

    /// Build a user message from `text` plus the meal's thumbnail, when there is
    /// one (a manual entry has no photo).
    fn user_turn(&self, text: String, ctx: &AnalysisContext) -> Result<Message, AppError> {
        user_turn_with_image(text, ctx.image_path.as_deref())
    }

    /// Drive one bounded run and turn it into an [`AgentOutcome`].
    async fn run_loop(
        &self,
        ctx: &AnalysisContext,
        prompt: Message,
        history: Option<Vec<Message>>,
        max_turns: usize,
    ) -> Result<AgentOutcome, RunFailure> {
        let agent = self.build_agent(ctx).map_err(RunFailure::Fatal)?;
        let recorder = StepRecorder::new();

        let mut runner = agent
            .runner(prompt)
            .max_turns(max_turns)
            .add_hook(recorder.clone());
        if let Some(history) = history.clone() {
            runner = runner.history(history);
        }

        let response = match tokio::time::timeout(RUN_TIMEOUT, runner.run()).await {
            Ok(Ok(response)) => response,
            Ok(Err(err)) => return Err(classify_run_error(&err)),
            Err(_elapsed) => {
                return Err(RunFailure::Degraded(format!(
                    "the loop exceeded its {}s wall-clock budget",
                    RUN_TIMEOUT.as_secs()
                )))
            }
        };

        let estimate = parse_estimate(&response.output).map_err(|err| {
            RunFailure::Degraded(format!("the loop's structured output was unusable: {err}"))
        })?;

        Ok(self.outcome(response, history, &recorder, estimate, false))
    }

    /// Log the reason and produce a draft with one tool-free call (§5 bounds).
    async fn degrade(
        &self,
        ctx: &AnalysisContext,
        prompt: Message,
        history: Option<Vec<Message>>,
        reason: &str,
    ) -> Result<AgentOutcome, AppError> {
        tracing::warn!(
            meal_id = %ctx.meal_id,
            job_id = %ctx.job_id,
            reason = %reason,
            "agent loop degraded to the single-shot fallback"
        );
        let mut outcome = self.run_single_shot(ctx, prompt, history).await?;
        outcome.fallback_used = true;
        Ok(outcome)
    }

    /// One tool-free model call. Errors here are terminal — there is nothing
    /// further to fall back to.
    async fn run_single_shot(
        &self,
        _ctx: &AnalysisContext,
        prompt: Message,
        history: Option<Vec<Message>>,
    ) -> Result<AgentOutcome, AppError> {
        let agent = self.build_single_shot_agent()?;
        let recorder = StepRecorder::new();

        let mut runner = agent
            .runner(prompt)
            .max_turns(SINGLE_SHOT_MAX_TURNS)
            .add_hook(recorder.clone());
        if let Some(history) = history.clone() {
            runner = runner.history(history);
        }

        let response = tokio::time::timeout(FALLBACK_TIMEOUT, runner.run())
            .await
            .map_err(|_| {
                AppError::Upstream(format!(
                    "the single-shot fallback exceeded its {}s budget",
                    FALLBACK_TIMEOUT.as_secs()
                ))
            })?
            .map_err(|err| classify_prompt_error(&err))?;

        let estimate = parse_estimate(&response.output)?;
        Ok(self.outcome(response, history, &recorder, estimate, false))
    }

    /// Assemble the outcome, stitching the prior thread back onto the run's own
    /// new messages.
    ///
    /// rig's `PromptResponse::messages` holds only what *this* run added (the
    /// prompt plus every turn after it), so a continuation has to be
    /// re-concatenated with the history it was handed or the stored session
    /// would shrink to the last correction.
    fn outcome(
        &self,
        response: PromptResponse,
        history: Option<Vec<Message>>,
        recorder: &StepRecorder,
        estimate: MealEstimate,
        fallback_used: bool,
    ) -> AgentOutcome {
        let mut thread = history.unwrap_or_default();
        thread.extend(response.messages.unwrap_or_default());
        let thread = session::cap_history(
            session::strip_images(thread),
            session::MAX_STORED_TURNS,
        );
        let turn_count = thread.len() as i32;

        let serialized_messages = match serde_json::to_string(&thread) {
            Ok(json) => Some(json),
            Err(err) => {
                // Losing the thread costs the next correction its continuation,
                // not this estimate — so warn and carry on rather than failing.
                tracing::warn!(error = %err, "could not serialize the agent session");
                None
            }
        };

        let steps = recorder.take();
        let usage = RunUsage {
            prompt_tokens: response.usage.input_tokens as i64,
            completion_tokens: response.usage.output_tokens as i64,
            model_calls: response.completion_calls.len() as i32,
            tool_calls: steps.len() as i32,
        };

        AgentOutcome {
            estimate,
            serialized_messages,
            turn_count,
            steps,
            usage,
            fallback_used,
            model: self.model.clone(),
            prompt_version: prompts::PROMPT_VERSION.to_string(),
        }
    }
}

impl std::fmt::Debug for AgentHandle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AgentHandle")
            .field("model", &self.model)
            .field("web_search_enabled", &self.web_search_enabled)
            .field("prompt_version", &self.prompt_version())
            .finish()
    }
}

// ---------------------------------------------------------------------------
// step recording
// ---------------------------------------------------------------------------

/// Records every tool call as a [`StepRecord`], via rig's `AgentHook`.
///
/// §8 wanted `agent_steps` populated "from rig's multi-turn stream events"; the
/// hook is the better mechanism (docs/rig-spike.md) — typed payloads, no stream
/// parsing, and it fires on the blocking path too.
#[derive(Clone, Default)]
struct StepRecorder {
    inner: Arc<Mutex<StepRecorderState>>,
}

#[derive(Default)]
struct StepRecorderState {
    /// Start instants keyed by rig's per-call correlation id.
    started: HashMap<String, Instant>,
    steps: Vec<StepRecord>,
}

impl StepRecorder {
    fn new() -> Self {
        Self::default()
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, StepRecorderState> {
        // A poisoned lock here means a hook panicked mid-record. The steps are
        // an audit trail, not the estimate; recovering the inner value keeps the
        // meal alive rather than taking the run down with the audit.
        self.inner.lock().unwrap_or_else(|err| err.into_inner())
    }

    /// Drain the recorded steps.
    fn take(&self) -> Vec<StepRecord> {
        std::mem::take(&mut self.lock().steps)
    }
}

impl AgentHook for StepRecorder {
    async fn on_tool_call(&self, _ctx: &HookContext, event: ToolCallEvent<'_>) -> ToolCallAction {
        self.lock()
            .started
            .insert(event.internal_call_id.to_string(), Instant::now());
        ToolCallAction::Run
    }

    async fn on_tool_result(
        &self,
        _ctx: &HookContext,
        event: ToolResultEvent<'_>,
    ) -> ToolResultAction {
        let mut state = self.lock();
        let latency_ms = state
            .started
            .remove(event.internal_call_id)
            .map(|started| started.elapsed().as_millis() as i64);
        let step_no = state.steps.len() as i32 + 1;
        state.steps.push(StepRecord {
            step_no,
            tool_name: event.tool_name.to_string(),
            tool_input: Some(tools::truncate(event.args, MAX_STEP_PAYLOAD_CHARS)),
            tool_output: Some(tools::truncate(
                &event.presentation.render(),
                MAX_STEP_PAYLOAD_CHARS,
            )),
            latency_ms,
        });
        ToolResultAction::Keep
    }
}

// ---------------------------------------------------------------------------
// message construction
// ---------------------------------------------------------------------------

/// Build a user message from `text` plus, when present, the meal's thumbnail.
///
/// The thumbnail is a ~80KB local file, so it is read inline; going through
/// `spawn_blocking` for a page-cache hit would cost more than it saves.
pub(crate) fn user_turn_with_image(
    text: String,
    image_path: Option<&Path>,
) -> Result<Message, AppError> {
    let mut parts = vec![UserContent::text(text)];

    if let Some(path) = image_path {
        let bytes = std::fs::read(path).map_err(|err| match err.kind() {
            std::io::ErrorKind::NotFound => AppError::NotFound(format!(
                "thumbnail {} is missing from disk",
                path.display()
            )),
            _ => AppError::from(err),
        })?;
        parts.push(UserContent::image_base64(
            base64_encode(&bytes),
            Some(image_media_type(path)),
            // Detail is left to the provider: the thumbnail is already 768px,
            // which is roughly what a vision model downsamples to anyway (§7).
            None,
        ));
    }

    let content = OneOrMany::many(parts)
        .map_err(|err| AppError::Internal(format!("empty user turn: {err}")))?;
    Ok(Message::User { content })
}

/// Media type for a stored thumbnail. Storage always writes JPEG, so that is the
/// default; the extension is honoured in case that ever stops being true.
fn image_media_type(path: &Path) -> ImageMediaType {
    match path
        .extension()
        .and_then(|ext| ext.to_str())
        .map(str::to_ascii_lowercase)
        .as_deref()
    {
        Some("png") => ImageMediaType::PNG,
        Some("webp") => ImageMediaType::WEBP,
        Some("gif") => ImageMediaType::GIF,
        Some("heic") => ImageMediaType::HEIC,
        Some("heif") => ImageMediaType::HEIF,
        _ => ImageMediaType::JPEG,
    }
}

/// Standard base64 alphabet, padded.
const BASE64_ALPHABET: &[u8; 64] =
    b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

/// Encode bytes as padded standard base64.
///
/// Hand-rolled on purpose: this is the only base64 in the project, and a
/// twenty-line function with a test against the RFC 4648 vectors is a better
/// trade than another dependency in the supply chain.
pub(crate) fn base64_encode(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let b0 = chunk[0] as u32;
        let b1 = chunk.get(1).copied().unwrap_or(0) as u32;
        let b2 = chunk.get(2).copied().unwrap_or(0) as u32;
        let packed = (b0 << 16) | (b1 << 8) | b2;

        out.push(BASE64_ALPHABET[(packed >> 18 & 0x3f) as usize] as char);
        out.push(BASE64_ALPHABET[(packed >> 12 & 0x3f) as usize] as char);
        out.push(if chunk.len() > 1 {
            BASE64_ALPHABET[(packed >> 6 & 0x3f) as usize] as char
        } else {
            '='
        });
        out.push(if chunk.len() > 2 {
            BASE64_ALPHABET[(packed & 0x3f) as usize] as char
        } else {
            '='
        });
    }
    out
}

// ---------------------------------------------------------------------------
// output parsing and error classification
// ---------------------------------------------------------------------------

/// Deserialize the run's final answer into a [`MealEstimate`].
///
/// Tries a direct parse first — under `OutputMode::Native` (OpenAI) and
/// `OutputMode::Tool` the text is already clean JSON — then falls back to the
/// first balanced JSON value in the string, so a best-effort mode that wrapped
/// the object in prose or a markdown fence still lands.
pub(crate) fn parse_estimate(output: &str) -> Result<MealEstimate, AppError> {
    let trimmed = output.trim();
    if trimmed.is_empty() {
        return Err(AppError::Upstream(
            "the model returned an empty response".to_string(),
        ));
    }

    let mut estimate: MealEstimate = match serde_json::from_str(trimmed) {
        Ok(estimate) => estimate,
        Err(direct) => {
            let start = trimmed.find('{').ok_or_else(|| {
                AppError::Upstream(format!("the model returned no JSON object: {direct}"))
            })?;
            serde_json::Deserializer::from_str(&trimmed[start..])
                .into_iter::<MealEstimate>()
                .next()
                .unwrap_or(Err(direct))
                .map_err(|err| {
                    AppError::Upstream(format!(
                        "the model's estimate did not match the schema: {err}"
                    ))
                })?
        }
    };

    estimate.sanitize();
    if !estimate.is_usable() {
        return Err(AppError::Upstream(
            "the model returned an estimate with no items".to_string(),
        ));
    }
    Ok(estimate)
}

/// Decide whether a failed run degrades to a draft or fails the meal.
fn classify_run_error(err: &PromptError) -> RunFailure {
    let classified = classify_prompt_error(err);
    // §5: a quota-exhausted or refused key must be *loud*. Everything else — a
    // breached turn budget, a provider hiccup, a tool that blew up — is exactly
    // what the single-shot fallback exists for.
    if matches!(
        classified,
        AppError::RateLimited(_) | AppError::Internal(_)
    ) {
        RunFailure::Fatal(classified)
    } else {
        RunFailure::Degraded(err.to_string())
    }
}

/// Map a rig prompt failure onto the crate error type.
///
/// Quota and authentication failures are separated out because they change what
/// the user is told: a 429 is loud and retryable, a rejected key is loud and
/// permanent, and everything else is a transient upstream problem the analysis
/// worker may retry.
pub(crate) fn classify_prompt_error(err: &PromptError) -> AppError {
    if let Some(status) = err.provider_response_status() {
        let code = status.as_u16();
        if code == 429 {
            return AppError::RateLimited(format!("the OpenAI API refused on quota: {err}"));
        }
        if code == 401 || code == 403 {
            return AppError::Internal(format!(
                "the OpenAI API rejected the configured key ({code}): {err}"
            ));
        }
        // 402 is what a hard-exhausted account returns on some deployments.
        if code == 402 {
            return AppError::RateLimited(format!("the OpenAI account is out of credit: {err}"));
        }
    }

    // Some transports surface the quota refusal only in the body, so the text is
    // a second line of defence rather than the primary check.
    let text = err.to_string().to_ascii_lowercase();
    if text.contains("insufficient_quota")
        || text.contains("rate limit")
        || text.contains("rate_limit")
        || text.contains("too many requests")
    {
        return AppError::RateLimited(format!("the OpenAI API refused on quota: {err}"));
    }
    if text.contains("invalid_api_key") || text.contains("incorrect api key") {
        return AppError::Internal(format!("the OpenAI API rejected the configured key: {err}"));
    }

    AppError::Upstream(format!("the estimation model call failed: {err}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A compile-time proof that the analysis worker can `tokio::spawn` a run.
    ///
    /// Never called — the value is that the `Send` bound is checked at compile
    /// time, here, rather than in `jobs/` where the spawn actually happens.
    #[allow(dead_code)]
    fn analyze_future_is_send(handle: &AgentHandle, ctx: &AnalysisContext) {
        fn assert_send<T: Send>(_: T) {}
        assert_send(handle.analyze(ctx));
        assert_send(handle.single_shot(ctx));
        assert_send(handle.continue_session(ctx, Vec::new(), "half the rice"));
    }

    #[test]
    fn base64_matches_the_rfc_4648_vectors() {
        assert_eq!(base64_encode(b""), "");
        assert_eq!(base64_encode(b"f"), "Zg==");
        assert_eq!(base64_encode(b"fo"), "Zm8=");
        assert_eq!(base64_encode(b"foo"), "Zm9v");
        assert_eq!(base64_encode(b"foob"), "Zm9vYg==");
        assert_eq!(base64_encode(b"fooba"), "Zm9vYmE=");
        assert_eq!(base64_encode(b"foobar"), "Zm9vYmFy");
    }

    #[test]
    fn base64_handles_the_high_bytes_a_jpeg_is_full_of() {
        assert_eq!(base64_encode(&[0xff, 0xd8, 0xff]), "/9j/");
        assert_eq!(base64_encode(&[0x00, 0x00, 0x00]), "AAAA");
    }

    #[test]
    fn a_photoless_turn_is_text_only() {
        let message =
            user_turn_with_image("manual entry".to_string(), None).expect("builds a user turn");
        let Message::User { content } = message else {
            panic!("expected a user message");
        };
        assert_eq!(content.len(), 1);
        assert!(matches!(content.first_ref(), UserContent::Text(_)));
    }

    #[test]
    fn a_turn_with_a_photo_carries_the_image_after_the_text() {
        let dir = tempfile::tempdir().expect("temp dir");
        let path = dir.path().join("thumb.jpg");
        std::fs::write(&path, [0xff, 0xd8, 0xff, 0xdb]).expect("write thumbnail");

        let message = user_turn_with_image("look at this".to_string(), Some(&path))
            .expect("builds a user turn");
        let Message::User { content } = message else {
            panic!("expected a user message");
        };
        assert_eq!(content.len(), 2);
        assert!(matches!(content.first_ref(), UserContent::Text(_)));
        match content.last_ref() {
            UserContent::Image(image) => {
                assert_eq!(image.media_type, Some(ImageMediaType::JPEG));
            }
            other => panic!("expected an image, got {other:?}"),
        }
    }

    #[test]
    fn a_missing_thumbnail_is_a_not_found_rather_than_a_panic() {
        let err = user_turn_with_image("x".to_string(), Some(Path::new("/nope/missing.jpg")))
            .expect_err("a missing thumbnail must not be silently ignored");
        assert!(matches!(err, AppError::NotFound(_)), "{err:?}");
    }

    #[test]
    fn the_media_type_follows_the_extension_and_defaults_to_jpeg() {
        assert_eq!(image_media_type(Path::new("a/b.jpg")), ImageMediaType::JPEG);
        assert_eq!(image_media_type(Path::new("a/b.PNG")), ImageMediaType::PNG);
        assert_eq!(image_media_type(Path::new("a/b")), ImageMediaType::JPEG);
    }

    const CLEAN_ESTIMATE: &str = r#"{
        "dish_name": "шаурма с курицей",
        "container": "delivery foil wrap",
        "items": [{
            "name": "lavash", "grams": 90, "kcal": 250, "protein_g": 8,
            "fat_g": 2, "carbs_g": 50, "confidence": 0.7,
            "reasoning_note": "standard wrap"
        }],
        "overall_confidence": 0.65
    }"#;

    #[test]
    fn a_clean_json_answer_parses() {
        let estimate = parse_estimate(CLEAN_ESTIMATE).expect("parses");
        assert_eq!(estimate.dish_name, "шаурма с курицей");
        assert_eq!(estimate.items.len(), 1);
        assert_eq!(estimate.totals().kcal, 250.0);
    }

    #[test]
    fn a_fenced_or_prefixed_answer_still_parses() {
        let fenced = format!("Here you go:\n```json\n{CLEAN_ESTIMATE}\n```");
        let estimate = parse_estimate(&fenced).expect("the balanced-JSON fallback recovers it");
        assert_eq!(estimate.items.len(), 1);
    }

    #[test]
    fn an_empty_or_itemless_answer_is_rejected_so_the_caller_falls_back() {
        assert!(parse_estimate("   ").is_err());
        assert!(parse_estimate("I could not tell what this is.").is_err());
        assert!(parse_estimate(
            r#"{"dish_name":"mystery","items":[],"overall_confidence":0.1}"#
        )
        .is_err());
    }

    #[test]
    fn parsing_sanitizes_on_the_way_through() {
        let estimate = parse_estimate(
            r#"{"dish_name":"  rice  ","items":[
                {"name":"rice","grams":150,"kcal":200,"protein_g":4,"fat_g":1,
                 "carbs_g":44,"confidence":9.0,"reasoning_note":"bowl"}],
               "overall_confidence":-3}"#,
        )
        .expect("parses");
        assert_eq!(estimate.dish_name, "rice");
        assert_eq!(estimate.items[0].confidence, 1.0);
        assert_eq!(estimate.overall_confidence, 0.0);
    }

    #[test]
    fn a_breached_turn_budget_degrades_rather_than_failing_the_meal() {
        let err = PromptError::MaxTurnsError {
            max_turns: MAX_TURNS,
            chat_history: Box::new(Vec::new()),
            prompt: Box::new(Message::user("photo")),
        };
        assert!(matches!(classify_run_error(&err), RunFailure::Degraded(_)));
    }

    #[test]
    fn a_quota_refusal_is_loud_and_never_degrades() {
        let err = PromptError::CompletionError(rig::completion::CompletionError::ProviderError(
            "429 Too Many Requests: insufficient_quota".to_string(),
        ));
        assert!(matches!(
            classify_prompt_error(&err),
            AppError::RateLimited(_)
        ));
        assert!(matches!(classify_run_error(&err), RunFailure::Fatal(_)));
    }

    #[test]
    fn a_rejected_key_is_loud_and_not_retryable() {
        let err = PromptError::CompletionError(rig::completion::CompletionError::ProviderError(
            "invalid_api_key: Incorrect API key provided".to_string(),
        ));
        let classified = classify_prompt_error(&err);
        assert!(matches!(classified, AppError::Internal(_)), "{classified:?}");
        assert!(!classified.is_retryable());
        assert!(matches!(classify_run_error(&err), RunFailure::Fatal(_)));
    }

    #[test]
    fn an_ordinary_provider_hiccup_is_retryable_and_degrades() {
        let err = PromptError::CompletionError(rig::completion::CompletionError::ProviderError(
            "connection reset by peer".to_string(),
        ));
        let classified = classify_prompt_error(&err);
        assert!(matches!(classified, AppError::Upstream(_)), "{classified:?}");
        assert!(classified.is_retryable());
        assert!(matches!(classify_run_error(&err), RunFailure::Degraded(_)));
    }
}
