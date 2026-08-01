//! Test harness for the PRD §13 integration suite.
//!
//! Boots the **real** router ([`picweight_backend::build_router`], the same
//! function `main.rs` serves) against:
//!
//! * a temp directory holding the SQLite file, `thumbs/` and a fake `static/`,
//! * a **mock IdP** — so OIDC discovery runs for real rather than being stubbed,
//! * a **mock LLM** speaking OpenAI's Responses and Chat Completions APIs, so
//!   the analysis loop completes deterministically and offline.
//!
//! Everything binds `127.0.0.1:0`, so the suite is safe to run in parallel.

#![allow(dead_code)]

use std::net::SocketAddr;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use axum::extract::State as AxumState;
use axum::routing::{get, post};
use axum::{Json, Router};
use picweight_backend::api::events::{EventBus, MealEvent};
use picweight_backend::config::{Config, OidcConfig};
use picweight_backend::{agent::AgentHandle, auth, db, jobs, AppState};
use serde_json::{json, Value};
use tempfile::TempDir;
use tokio::sync::broadcast;

/// How long a poll-until-terminal loop waits before giving up.
pub const SETTLE_TIMEOUT: Duration = Duration::from_secs(20);

// ---------------------------------------------------------------------------
// Mock LLM
// ---------------------------------------------------------------------------

/// Canned model behaviour, plus counters the assertions read.
#[derive(Default)]
pub struct LlmMock {
    /// `POST /v1/responses` calls — one per model call, so this is the turn
    /// count the §13 "no repeat tool calls" assertions look at.
    pub responses_calls: AtomicUsize,
    /// `POST /v1/chat/completions` calls — the verdict line.
    pub verdict_calls: AtomicUsize,
    /// The estimate returned as the assistant's structured answer.
    estimate: Mutex<Value>,
    /// The verdict sentence.
    verdict: Mutex<String>,
}

impl LlmMock {
    fn new() -> Self {
        LlmMock {
            responses_calls: AtomicUsize::new(0),
            verdict_calls: AtomicUsize::new(0),
            estimate: Mutex::new(default_estimate("Шаурма с курицей")),
            verdict: Mutex::new("Doable, but it has to be mostly protein.".to_string()),
        }
    }

    /// Replace the estimate every subsequent run returns.
    pub fn set_estimate(&self, estimate: Value) {
        match self.estimate.lock() {
            Ok(mut slot) => *slot = estimate,
            Err(poisoned) => *poisoned.into_inner() = estimate,
        }
    }

    fn current_estimate(&self) -> Value {
        match self.estimate.lock() {
            Ok(slot) => slot.clone(),
            Err(poisoned) => poisoned.into_inner().clone(),
        }
    }

    fn current_verdict(&self) -> String {
        match self.verdict.lock() {
            Ok(slot) => slot.clone(),
            Err(poisoned) => poisoned.into_inner().clone(),
        }
    }
}

/// A complete, plausible estimate in the shape of
/// [`picweight_backend::agent::schema::MealEstimate`].
pub fn default_estimate(dish: &str) -> Value {
    json!({
        "dish_name": dish,
        "cuisine": "Middle Eastern",
        "container": "standard delivery wrap, ~25cm",
        "from_recall": false,
        "overall_confidence": 0.62,
        "notes": "Restaurant preparation; assumed generous oil.",
        "items": [
            {
                "name": "Lavash wrap",
                "grams": 110.0,
                "kcal": 300.0,
                "protein_g": 9.0,
                "fat_g": 4.0,
                "carbs_g": 56.0,
                "confidence": 0.7,
                "reasoning_note": "Two sheets of lavash at the diameter shown."
            },
            {
                "name": "Chicken",
                "grams": 150.0,
                "kcal": 300.0,
                "protein_g": 45.0,
                "fat_g": 12.0,
                "carbs_g": 0.0,
                "confidence": 0.6,
                "reasoning_note": "Filling depth against the wrap diameter."
            },
            {
                "name": "Rice",
                "grams": 120.0,
                "kcal": 180.0,
                "protein_g": 3.0,
                "fat_g": 1.0,
                "carbs_g": 39.0,
                "confidence": 0.5,
                "reasoning_note": "Visible under the filling."
            }
        ]
    })
}

/// The OpenAI Responses-API envelope rig deserializes.
fn responses_envelope(text: &str) -> Value {
    json!({
        "id": "resp_test",
        "object": "response",
        "created_at": 0,
        "status": "completed",
        "model": "mock-model",
        "usage": { "input_tokens": 1200, "output_tokens": 240, "total_tokens": 1440 },
        "output": [{
            "type": "message",
            "id": "msg_test",
            "status": "completed",
            "role": "assistant",
            "content": [{ "type": "output_text", "annotations": [], "text": text }]
        }],
        "tools": []
    })
}

async fn mock_responses(AxumState(mock): AxumState<Arc<LlmMock>>) -> Json<Value> {
    mock.responses_calls.fetch_add(1, Ordering::SeqCst);
    let text = mock.current_estimate().to_string();
    Json(responses_envelope(&text))
}

async fn mock_chat_completions(AxumState(mock): AxumState<Arc<LlmMock>>) -> Json<Value> {
    mock.verdict_calls.fetch_add(1, Ordering::SeqCst);
    Json(json!({
        "id": "chatcmpl_test",
        "object": "chat.completion",
        "created": 0,
        "model": "mock-model",
        "choices": [{
            "index": 0,
            "finish_reason": "stop",
            "message": { "role": "assistant", "content": mock.current_verdict() }
        }],
        "usage": { "prompt_tokens": 90, "completion_tokens": 18, "total_tokens": 108 }
    }))
}

// ---------------------------------------------------------------------------
// Mock IdP
// ---------------------------------------------------------------------------

async fn discovery_document(AxumState(issuer): AxumState<String>) -> Json<Value> {
    Json(json!({
        "issuer": issuer,
        "authorization_endpoint": format!("{issuer}/authorize"),
        "token_endpoint": format!("{issuer}/oauth/token"),
        "userinfo_endpoint": format!("{issuer}/userinfo"),
        "jwks_uri": format!("{issuer}/keys"),
        "response_types_supported": ["code"],
        "subject_types_supported": ["public"],
        "id_token_signing_alg_values_supported": ["RS256"],
        "scopes_supported": ["openid", "profile", "email"],
        "grant_types_supported": ["authorization_code", "refresh_token"],
        "token_endpoint_auth_methods_supported": ["client_secret_post", "client_secret_basic"],
        "code_challenge_methods_supported": ["S256"],
        "claims_supported": ["sub", "email", "name"]
    }))
}

/// An empty key set. These tests never verify a provider-minted ID token —
/// they mint picweight's own HS256 session JWT directly, which is the token
/// every `/api/v1` route actually reads.
async fn empty_jwks() -> Json<Value> {
    Json(json!({ "keys": [] }))
}

// ---------------------------------------------------------------------------
// The harness
// ---------------------------------------------------------------------------

/// A running picweight instance with its two mocked upstreams.
pub struct TestApp {
    /// `http://127.0.0.1:<port>` of the picweight server.
    pub base: String,
    /// Shared state, for assertions that go straight to the database.
    pub state: AppState,
    /// The mocked model.
    pub llm: Arc<LlmMock>,
    /// A plain HTTP client with no default auth.
    pub http: reqwest::Client,
    /// Issuer URL of the mock IdP.
    pub issuer: String,
    /// Keeps the temp directory alive for the lifetime of the app.
    _tmp: TempDir,
}

impl TestApp {
    /// Boot everything: mocks, database, workers, router, listener.
    pub async fn start() -> TestApp {
        TestApp::start_with_static_files(&[]).await
    }

    /// Boot with extra files planted in `static/` before the server starts.
    ///
    /// Used by the `/api/v1/client/version` tests: the bundled-APK sidecar is
    /// read once, in `AppState::new`, so it has to exist before the app boots —
    /// exactly as it does in the image, where the Docker build writes it.
    pub async fn start_with_static_files(files: &[(&str, &[u8])]) -> TestApp {
        let tmp = tempfile::tempdir().expect("temp dir");
        let data_path = tmp.path().join("data");
        let static_dir = tmp.path().join("static");
        std::fs::create_dir_all(data_path.join("thumbs")).expect("thumbs dir");
        std::fs::create_dir_all(&static_dir).expect("static dir");
        std::fs::write(static_dir.join("index.html"), SPA_INDEX).expect("index.html");
        for (name, contents) in files {
            std::fs::write(static_dir.join(name), contents).expect("static file");
        }

        // --- mock IdP -----------------------------------------------------
        let idp_listener = tokio::net::TcpListener::bind(local_addr())
            .await
            .expect("bind idp");
        let issuer = format!("http://{}", idp_listener.local_addr().expect("idp addr"));
        let idp = Router::new()
            .route(
                "/.well-known/openid-configuration",
                get(discovery_document).with_state(issuer.clone()),
            )
            .route("/keys", get(empty_jwks));
        tokio::spawn(async move {
            let _ = axum::serve(idp_listener, idp).await;
        });

        // --- mock LLM -----------------------------------------------------
        let llm = Arc::new(LlmMock::new());
        let llm_listener = tokio::net::TcpListener::bind(local_addr())
            .await
            .expect("bind llm");
        let llm_base = format!(
            "http://{}/v1",
            llm_listener.local_addr().expect("llm addr")
        );
        let llm_router = Router::new()
            .route("/v1/responses", post(mock_responses))
            .route("/v1/chat/completions", post(mock_chat_completions))
            .with_state(llm.clone());
        tokio::spawn(async move {
            let _ = axum::serve(llm_listener, llm_router).await;
        });

        // --- configuration ------------------------------------------------
        let config = Config {
            port: 0,
            data_path: data_path.clone(),
            static_dir,
            database_url: data_path
                .join("picweight.db")
                .to_string_lossy()
                .into_owned(),
            oidc: OidcConfig {
                issuer: issuer.clone(),
                client_id: "picweight-web".to_string(),
                client_secret: "web-secret".to_string(),
                mobile_client_id: Some("picweight-android".to_string()),
                extra_audiences: Vec::new(),
                redirect_uri: "http://127.0.0.1:1/api/auth/callback".to_string(),
                scopes: vec!["openid".into(), "profile".into(), "email".into()],
            },
            jwt_secret: "integration-test-secret".to_string(),
            jwt_ttl_secs: 3600,
            openai_api_key: "sk-test".to_string(),
            openai_model: "mock-model".to_string(),
            openai_base_url: llm_base,
            rust_log: "warn".to_string(),
            web_search_enabled: false,
            model_pricing: Vec::new(),
        };

        let pool = db::establish_pool_from_url(&config.database_url).expect("pool");
        db::run_migrations(&pool).expect("migrations");

        let config = Arc::new(config);
        let http = reqwest::Client::builder()
            .timeout(Duration::from_secs(30))
            .build()
            .expect("http client");

        let auth_state = auth::init_oidc(&config.oidc, &config.jwt_secret, config.jwt_ttl_secs)
            .await
            .expect("OIDC discovery against the mock IdP");

        let agent = Arc::new(
            AgentHandle::new(&config, pool.clone(), http.clone()).expect("agent handle"),
        );
        let (queue, receiver) = jobs::JobQueue::new();
        let state = AppState::new(
            pool,
            config,
            auth_state,
            agent,
            EventBus::default(),
            queue,
            http.clone(),
        );

        jobs::spawn_workers(state.clone(), receiver);

        let listener = tokio::net::TcpListener::bind(local_addr())
            .await
            .expect("bind app");
        let base = format!("http://{}", listener.local_addr().expect("app addr"));
        let app = picweight_backend::build_router(state.clone());
        tokio::spawn(async move {
            let _ = axum::serve(listener, app).await;
        });

        TestApp {
            base,
            state,
            llm,
            http,
            issuer,
            _tmp: tmp,
        }
    }

    /// Mint a session JWT for `sub`, creating the user row on first use.
    pub fn token(&self, sub: &str) -> String {
        auth::issue_session_jwt(
            sub,
            sub,
            &format!("{sub}@example.com"),
            &self.issuer,
            3600,
            &self.state.auth.jwt_encoding_key,
        )
        .expect("session token")
    }

    /// Absolute URL for a path.
    pub fn url(&self, path: &str) -> String {
        format!("{}{}", self.base, path)
    }

    /// `GET` as `token`.
    pub async fn get(&self, path: &str, token: &str) -> reqwest::Response {
        self.http
            .get(self.url(path))
            .bearer_auth(token)
            .send()
            .await
            .expect("GET")
    }

    /// `PATCH` a JSON body as `token`.
    pub async fn patch_json(&self, path: &str, token: &str, body: Value) -> reqwest::Response {
        self.http
            .patch(self.url(path))
            .bearer_auth(token)
            .json(&body)
            .send()
            .await
            .expect("PATCH")
    }

    /// `PUT` a JSON body as `token`.
    pub async fn put_json(&self, path: &str, token: &str, body: Value) -> reqwest::Response {
        self.http
            .put(self.url(path))
            .bearer_auth(token)
            .json(&body)
            .send()
            .await
            .expect("PUT")
    }

    /// `POST` a JSON body as `token`.
    pub async fn post_json(&self, path: &str, token: &str, body: Value) -> reqwest::Response {
        self.http
            .post(self.url(path))
            .bearer_auth(token)
            .json(&body)
            .send()
            .await
            .expect("POST")
    }

    /// Upload one meal exactly as the Android client does.
    pub async fn upload_meal(&self, token: &str, upload: MealUpload<'_>) -> reqwest::Response {
        let mut form = reqwest::multipart::Form::new()
            .text("client_uuid", upload.client_uuid.to_string())
            .text("eaten_at", upload.eaten_at.to_string())
            .text("timezone_offset", upload.timezone_offset.to_string());

        if let Some(photo) = upload.photo {
            let part = reqwest::multipart::Part::bytes(photo.to_vec())
                .file_name("meal.jpg")
                .mime_str("image/jpeg")
                .expect("mime");
            form = form.part("photo", part);
        }
        if let Some(name) = upload.dish_name {
            form = form.text("dish_name", name.to_string());
        }
        if let Some(group_id) = upload.group_id {
            form = form.text("group_id", group_id.to_string());
        }
        if let Some(size) = upload.group_size {
            form = form.text("group_size", size.to_string());
        }

        self.http
            .post(self.url("/api/v1/meals"))
            .bearer_auth(token)
            .multipart(form)
            .send()
            .await
            .expect("POST /api/v1/meals")
    }

    /// Poll `GET /api/v1/meals/:id` until the status is terminal.
    ///
    /// Returns the final meal body. Panics — with the last status seen — rather
    /// than hanging, so a broken analyzer fails the test readably.
    pub async fn await_terminal(&self, token: &str, meal_id: &str) -> Value {
        let deadline = Instant::now() + SETTLE_TIMEOUT;
        let mut last = String::from("<never fetched>");
        while Instant::now() < deadline {
            let response = self
                .get(&format!("/api/v1/meals/{meal_id}"), token)
                .await;
            let body: Value = response.json().await.expect("meal body");
            last = body["status"].as_str().unwrap_or("<missing>").to_string();
            if matches!(last.as_str(), "needs_review" | "confirmed" | "failed") {
                return body;
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
        panic!("meal {meal_id} never reached a terminal status (last saw {last:?})");
    }

    /// Subscribe to the event bus before the action under test runs.
    pub fn events(&self) -> broadcast::Receiver<MealEvent> {
        self.state.events.subscribe()
    }

    /// Drain every event already queued on `rx`, without blocking.
    pub fn drain(rx: &mut broadcast::Receiver<MealEvent>) -> Vec<MealEvent> {
        let mut seen = Vec::new();
        while let Ok(event) = rx.try_recv() {
            seen.push(event);
        }
        seen
    }
}

/// The fields of one multipart upload.
pub struct MealUpload<'a> {
    /// Idempotency key, generated on the phone at capture (§8).
    pub client_uuid: &'a str,
    /// RFC 3339 or naive timestamp.
    pub eaten_at: &'a str,
    /// Minutes east of UTC.
    pub timezone_offset: i32,
    /// JPEG bytes, when there is a photo.
    pub photo: Option<&'a [u8]>,
    /// Dish name from a chip, share intent or comment.
    pub dish_name: Option<&'a str>,
    /// Client-generated per sitting.
    pub group_id: Option<&'a str>,
    /// Settle hint, sent with the final shot.
    pub group_size: Option<i32>,
}

impl<'a> MealUpload<'a> {
    /// A photo upload with sane defaults.
    pub fn photo(client_uuid: &'a str, photo: &'a [u8]) -> Self {
        MealUpload {
            client_uuid,
            eaten_at: "2026-08-01T12:30:00+03:00",
            timezone_offset: 180,
            photo: Some(photo),
            dish_name: None,
            group_id: None,
            group_size: None,
        }
    }

    /// A no-photo manual entry (§5).
    pub fn manual(client_uuid: &'a str, dish_name: &'a str) -> Self {
        MealUpload {
            client_uuid,
            eaten_at: "2026-08-01T12:30:00+03:00",
            timezone_offset: 180,
            photo: None,
            dish_name: Some(dish_name),
            group_id: None,
            group_size: None,
        }
    }

    /// Stamp this upload with its sitting.
    pub fn in_group(mut self, group_id: &'a str, group_size: Option<i32>) -> Self {
        self.group_id = Some(group_id);
        self.group_size = group_size;
        self
    }
}

/// A real, decodable JPEG — `storage::thumbs` rejects junk bytes, so the test
/// photo has to survive an actual decode.
pub fn jpeg(width: u32, height: u32, tint: u8) -> Vec<u8> {
    let mut buffer = image::RgbImage::new(width, height);
    for (x, y, pixel) in buffer.enumerate_pixels_mut() {
        *pixel = image::Rgb([(x % 255) as u8, (y % 255) as u8, tint]);
    }
    let mut out = std::io::Cursor::new(Vec::new());
    image::DynamicImage::ImageRgb8(buffer)
        .write_to(&mut out, image::ImageFormat::Jpeg)
        .expect("encode test jpeg");
    out.into_inner()
}

/// A body distinctive enough that the SPA-fallback assertion cannot pass by
/// accident.
const SPA_INDEX: &str = "<!doctype html><title>picweight</title><div id=app></div>";

fn local_addr() -> SocketAddr {
    SocketAddr::from(([127, 0, 0, 1], 0))
}
