//! End-to-end tests for the picweight backend — PRD §13, "Verification".
//!
//! Every test boots the real router against a temp SQLite file, a mock IdP and
//! a mock LLM endpoint (see [`harness`]). Nothing here stubs a picweight type:
//! requests go over a real TCP socket, through `require_auth`, into the real
//! handlers, and the analysis worker runs the real rig loop against the mock.
//!
//! What §13 asks of the backend, and where it is covered:
//!
//! | §13 clause | Test |
//! |---|---|
//! | ingest → loop → confirm | [`ingest_runs_the_loop_and_the_meal_confirms`] |
//! | idempotent replay of one `client_uuid` | [`replaying_a_client_uuid_never_creates_a_second_meal`] |
//! | 3 photos, 3 loops, **one** notification | [`one_sitting_is_three_loops_and_one_notification`] |
//! | `group_size` hint beats the debounce | same test — the settler fires with 88s of debounce left |
//! | a failed member does not block the group | [`a_failed_member_does_not_block_the_sitting`] |
//! | user A cannot read user B's meals | [`one_users_meal_is_invisible_to_another`] |
//! | re-analysis writes a new revision, keeps the old | [`a_correction_writes_a_new_revision_in_one_turn`] |
//! | the correction costs one turn, not another full loop | same test |
//! | day state after a confirmed meal | [`a_confirmed_meal_lands_in_the_day_view`] |
//! | static fallback vs. a 404 on `/api` | [`an_unknown_page_falls_through_to_the_spa`] |
//! | the APK the client updates itself from | [`the_bundled_apk_is_advertised_without_a_session`] |
//!
//! Mifflin-St Jeor, macro split and local-day/DST bucketing are exercised
//! exhaustively as unit tests in `nutrition::targets` and `feedback::state`.

mod harness;

use harness::{jpeg, MealUpload, TestApp};
use picweight_backend::api::events::MealEventKind;
use serde_json::json;
use std::sync::atomic::Ordering;

// ---------------------------------------------------------------------------
// Wiring
// ---------------------------------------------------------------------------

#[tokio::test]
async fn healthz_needs_no_session() {
    let app = TestApp::start().await;
    let response = app
        .http
        .get(app.url("/healthz"))
        .send()
        .await
        .expect("GET /healthz");
    assert_eq!(response.status(), 200);
}

#[tokio::test]
async fn an_api_route_without_a_session_is_401() {
    let app = TestApp::start().await;
    let response = app
        .http
        .get(app.url("/api/v1/me"))
        .send()
        .await
        .expect("GET /api/v1/me");
    assert_eq!(response.status(), 401, "an anonymous caller must be refused");
}

#[tokio::test]
async fn a_session_token_reaches_the_api() {
    let app = TestApp::start().await;
    let response = app.get("/api/v1/me", &app.token("alice")).await;
    assert_eq!(response.status(), 200);
    let body: serde_json::Value = response.json().await.expect("body");
    assert_eq!(body["user"]["display_name"], "alice");
    // A brand-new user has no profile yet, and that is a 200 with nulls rather
    // than an error — onboarding is a screen, not a precondition.
    assert!(body["profile"].is_null());
    assert!(body["today"].is_object());
}

/// The whole `/api/v1/me` payload, from a real `users` row, must be RFC 3339.
///
/// The fixture-driven walk in `api::timestamp_contract` proves what the DTOs
/// serialise; this proves what the deployed handler *actually writes* after a
/// round trip through SQLite, which is where the naive value came from. A phone
/// that cannot parse this response never issues the `/days` and
/// `/dishes/recent` calls that follow it, and reports itself offline.
#[tokio::test]
async fn the_me_payload_parses_as_rfc3339_end_to_end() {
    let app = TestApp::start().await;
    let response = app.get("/api/v1/me", &app.token("alice")).await;
    assert_eq!(response.status(), 200);
    let body: serde_json::Value = response.json().await.expect("body");

    let created_at = body["user"]["created_at"]
        .as_str()
        .expect("created_at is a JSON string");
    assert!(
        created_at.ends_with('Z'),
        "GET /api/v1/me returned created_at={created_at:?} — no UTC designator, \
so a generated OffsetDateTime client throws before it reads any other field"
    );
    chrono::DateTime::parse_from_rfc3339(created_at)
        .unwrap_or_else(|e| panic!("created_at {created_at:?} is not RFC 3339: {e}"));
}

#[tokio::test]
async fn an_unknown_page_falls_through_to_the_spa() {
    let app = TestApp::start().await;

    let page = app
        .http
        .get(app.url("/history/2026-08-01"))
        .send()
        .await
        .expect("GET a client route");
    assert_eq!(page.status(), 200);
    assert!(
        page.text().await.expect("html").contains("id=app"),
        "a client-side route must be served the SPA shell"
    );

    // But an unknown /api path must 404 with JSON rather than quietly returning
    // HTML — exactly the failure phos's APK-availability probe content-type
    // sniffs around (PRD §7).
    let missing_api = app
        .http
        .get(app.url("/api/v1/does-not-exist"))
        .send()
        .await
        .expect("GET an unknown api route");
    assert_eq!(missing_api.status(), 404);
    assert_eq!(
        missing_api
            .headers()
            .get("content-type")
            .and_then(|v| v.to_str().ok())
            .map(|v| v.starts_with("application/json")),
        Some(true),
        "an API 404 must not be the SPA shell"
    );
    let body: serde_json::Value = missing_api.json().await.expect("error body");
    assert_eq!(body["error"], "not_found");
}

#[tokio::test]
async fn the_openapi_docs_are_mounted() {
    let app = TestApp::start().await;
    let response = app
        .http
        .get(app.url("/api/docs"))
        .send()
        .await
        .expect("GET /api/docs");
    assert_eq!(response.status(), 200);
}

#[tokio::test]
async fn the_auth_routes_stay_reachable_without_a_session() {
    let app = TestApp::start().await;

    // The Android app self-configures from this before it has any token, so it
    // must survive both `require_auth` and the `/api/**` catch-all.
    let config = app
        .http
        .get(app.url("/api/auth/config"))
        .send()
        .await
        .expect("GET /api/auth/config");
    assert_eq!(config.status(), 200);
    let config: serde_json::Value = config.json().await.expect("config body");
    assert_eq!(config["issuer"], app.issuer);

    // `/api/auth/login` redirects to the IdP rather than 401ing.
    let no_redirect = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .expect("client");
    let login = no_redirect
        .get(app.url("/api/auth/login"))
        .send()
        .await
        .expect("GET /api/auth/login");
    assert!(
        login.status().is_redirection(),
        "login must redirect, got {}",
        login.status()
    );
    let location = login
        .headers()
        .get("location")
        .and_then(|v| v.to_str().ok())
        .unwrap_or_default();
    assert!(
        location.starts_with(&app.issuer),
        "login must bounce to the IdP, got {location:?}"
    );
}

// ---------------------------------------------------------------------------
// In-app update — GET /api/v1/client/version
// ---------------------------------------------------------------------------

/// Stand-in for the APK the image bundles. Content is irrelevant; only its
/// length and digest are, and both have to be real for the round trip below.
const FAKE_APK: &[u8] = b"not really an APK, but it hashes and it has a length";

/// Lowercase hex SHA-256, the form `sha256sum` emits and the sidecar carries.
fn sha256_hex(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    hasher
        .finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

/// The sidecar the `android-builder` stage writes, for a given APK.
fn sidecar_for(apk: &[u8], version_name: &str, version_code: i32) -> String {
    format!(
        r#"{{"version_name":"{version_name}","version_code":{version_code},"sha256":"{}","size_bytes":{}}}
"#,
        sha256_hex(apk),
        apk.len()
    )
}

#[tokio::test]
async fn the_bundled_apk_is_advertised_without_a_session() {
    let sidecar = sidecar_for(FAKE_APK, "master+b5fcc95", 12);
    let app = TestApp::start_with_static_files(&[
        ("picweight.apk", FAKE_APK),
        ("picweight.apk.json", sidecar.as_bytes()),
    ])
    .await;

    // No bearer token: an app whose session has expired still has to be able to
    // discover a newer build.
    let response = app
        .http
        .get(app.url("/api/v1/client/version"))
        .send()
        .await
        .expect("GET /api/v1/client/version");
    assert_eq!(response.status(), 200);
    let body: serde_json::Value = response.json().await.expect("version body");

    assert_eq!(body["available"], true);
    assert_eq!(body["version_name"], "master+b5fcc95");
    assert_eq!(body["version_code"], 12);
    assert_eq!(body["sha256"], sha256_hex(FAKE_APK));
    assert_eq!(body["size_bytes"], FAKE_APK.len());
    assert_eq!(body["download_path"], "/picweight.apk");

    // The client's step 1 → step 2: fetch what `download_path` names and check
    // it against the advertised digest. A digest that describes a *different*
    // build would make every install attempt fail as corrupt, so this is the
    // assertion that keeps the sidecar honest.
    let download = app
        .http
        .get(app.url(body["download_path"].as_str().expect("download_path")))
        .send()
        .await
        .expect("GET the APK");
    assert_eq!(download.status(), 200);
    let bytes = download.bytes().await.expect("apk bytes");
    assert_eq!(sha256_hex(&bytes), body["sha256"].as_str().expect("sha256"));
}

#[tokio::test]
async fn a_backend_only_build_reports_no_client_update() {
    // No APK, no sidecar — a `cargo run` on a laptop, or an image built without
    // the android stage. Must read as "up to date", never as a failure: the
    // check runs unprompted on app start, and a non-2xx there is what
    // resurrects the "offline" confusion.
    let app = TestApp::start().await;

    let response = app
        .http
        .get(app.url("/api/v1/client/version"))
        .send()
        .await
        .expect("GET /api/v1/client/version");
    assert_eq!(response.status(), 200);
    let body: serde_json::Value = response.json().await.expect("version body");

    assert_eq!(body["available"], false);
    // The client updates only on `server > BuildConfig.VERSION_CODE`, and the
    // lowest version code any installed build can carry is 1.
    assert_eq!(body["version_code"], 0);
    assert_eq!(body["version_name"], "");
    assert_eq!(body["sha256"], "");
    assert_eq!(body["size_bytes"], 0);
    // Still published, so the client never hardcodes the path.
    assert_eq!(body["download_path"], "/picweight.apk");
}

#[tokio::test]
async fn malformed_apk_metadata_never_reaches_the_client_as_an_error() {
    // A sidecar the build somehow mangled. The server must not 500 and must not
    // panic — it advertises nothing, logs for the operator, and the app carries
    // on working.
    let app = TestApp::start_with_static_files(&[
        ("picweight.apk", FAKE_APK),
        ("picweight.apk.json", b"{\"version_name\": \"1.2.3\", trunc"),
    ])
    .await;

    let response = app
        .http
        .get(app.url("/api/v1/client/version"))
        .send()
        .await
        .expect("GET /api/v1/client/version");
    assert_eq!(
        response.status(),
        200,
        "a misbuilt sidecar is the operator's problem, not a client error"
    );
    let body: serde_json::Value = response.json().await.expect("version body");
    assert_eq!(body["available"], false);
    assert_eq!(body["version_code"], 0);

    // And the rest of the app is unaffected.
    let health = app
        .http
        .get(app.url("/healthz"))
        .send()
        .await
        .expect("GET /healthz");
    assert_eq!(health.status(), 200);
}

// ---------------------------------------------------------------------------
// §13 — ingest → loop → confirm
// ---------------------------------------------------------------------------

#[tokio::test]
async fn ingest_runs_the_loop_and_the_meal_confirms() {
    let app = TestApp::start().await;
    let token = app.token("alice");
    let photo = jpeg(1600, 1200, 40);

    let accepted = app
        .upload_meal(&token, MealUpload::photo("uuid-ingest-1", &photo))
        .await;
    assert_eq!(accepted.status(), 202, "ingest is always asynchronous");
    let accepted: serde_json::Value = accepted.json().await.expect("accepted body");
    assert_eq!(accepted["status"], "pending");
    assert_eq!(accepted["deduplicated"], false);
    let meal_id = accepted["meal_id"].as_str().expect("meal id").to_string();

    let meal = app.await_terminal(&token, &meal_id).await;
    assert_eq!(
        meal["status"], "needs_review",
        "a fresh estimate is a draft, never auto-confirmed"
    );
    assert_eq!(meal["dish_name"], "Шаурма с курицей");
    assert_eq!(meal["revision"], 1);

    let items = meal["items"].as_array().expect("items");
    assert_eq!(items.len(), 3, "every component from the estimate is stored");
    assert!(
        items
            .iter()
            .all(|i| !i["reasoning_note"].as_str().unwrap_or_default().is_empty()),
        "§5 step 5: each item carries a one-line 'why'"
    );
    let kcal = meal["totals"]["kcal"].as_f64().expect("kcal");
    assert!((kcal - 780.0).abs() < 0.5, "totals sum the items: {kcal}");

    // The thumbnail is the display asset *and* the re-analysis input (§7).
    let thumb = app
        .get(&format!("/api/v1/meals/{meal_id}/thumbnail"), &token)
        .await;
    assert_eq!(thumb.status(), 200);
    assert_eq!(
        thumb
            .headers()
            .get("content-type")
            .and_then(|v| v.to_str().ok()),
        Some("image/jpeg")
    );

    // Confirm.
    let confirmed = app
        .patch_json(
            &format!("/api/v1/meals/{meal_id}"),
            &token,
            json!({ "status": "confirmed" }),
        )
        .await;
    assert_eq!(confirmed.status(), 200);
    let confirmed: serde_json::Value = confirmed.json().await.expect("confirmed body");
    assert_eq!(confirmed["status"], "confirmed");

    // One photo is one loop: exactly one model call reached the provider.
    assert_eq!(app.llm.responses_calls.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn a_manual_entry_needs_no_photo() {
    let app = TestApp::start().await;
    let token = app.token("alice");

    let accepted = app
        .upload_meal(&token, MealUpload::manual("uuid-manual-1", "Борщ"))
        .await;
    assert_eq!(accepted.status(), 202);
    let accepted: serde_json::Value = accepted.json().await.expect("body");
    let meal_id = accepted["meal_id"].as_str().expect("meal id").to_string();

    let meal = app.await_terminal(&token, &meal_id).await;
    assert_eq!(meal["status"], "needs_review");
    assert!(meal["thumbnail_id"].is_null());
}

#[tokio::test]
async fn a_meal_with_neither_photo_nor_name_is_rejected() {
    let app = TestApp::start().await;
    let token = app.token("alice");

    let response = app
        .upload_meal(
            &token,
            MealUpload {
                client_uuid: "uuid-empty",
                eaten_at: "2026-08-01T12:30:00+03:00",
                timezone_offset: 180,
                photo: None,
                dish_name: None,
                group_id: None,
                group_size: None,
            },
        )
        .await;
    assert_eq!(response.status(), 400);
}

// ---------------------------------------------------------------------------
// §13 — idempotent replay (§8: "the single most important field for offline
// correctness")
// ---------------------------------------------------------------------------

#[tokio::test]
async fn replaying_a_client_uuid_never_creates_a_second_meal() {
    let app = TestApp::start().await;
    let token = app.token("alice");
    let photo = jpeg(1200, 900, 90);

    let first: serde_json::Value = app
        .upload_meal(&token, MealUpload::photo("uuid-replay", &photo))
        .await
        .json()
        .await
        .expect("first body");
    let meal_id = first["meal_id"].as_str().expect("meal id").to_string();
    assert_eq!(first["deduplicated"], false);

    // The phone retried after a flaky connection. Same key, byte-identical
    // payload, and — the part that actually matters — a *third* replay after
    // the analysis has already finished.
    let second: serde_json::Value = app
        .upload_meal(&token, MealUpload::photo("uuid-replay", &photo))
        .await
        .json()
        .await
        .expect("second body");
    assert_eq!(second["meal_id"], first["meal_id"]);
    assert_eq!(second["deduplicated"], true);

    app.await_terminal(&token, &meal_id).await;

    let third: serde_json::Value = app
        .upload_meal(&token, MealUpload::photo("uuid-replay", &photo))
        .await
        .json()
        .await
        .expect("third body");
    assert_eq!(third["meal_id"], first["meal_id"]);
    assert_eq!(third["deduplicated"], true);

    let history: Vec<serde_json::Value> = app
        .get("/api/v1/meals", &token)
        .await
        .json()
        .await
        .expect("history");
    assert_eq!(history.len(), 1, "three uploads, one meal");

    // A replay must not re-enqueue: only the original upload ran the loop.
    assert_eq!(
        app.llm.responses_calls.load(Ordering::SeqCst),
        1,
        "a replay re-ran the analysis"
    );
}

#[tokio::test]
async fn a_client_uuid_from_another_user_is_refused_without_disclosure() {
    let app = TestApp::start().await;
    let alice = app.token("alice");
    let bob = app.token("bob");
    let photo = jpeg(800, 600, 10);

    let a = app
        .upload_meal(&alice, MealUpload::photo("shared-uuid", &photo))
        .await;
    assert_eq!(a.status(), 202);
    let a: serde_json::Value = a.json().await.expect("alice body");
    let alice_meal = a["meal_id"].as_str().expect("meal id").to_string();

    // `meals.client_uuid` is globally `UNIQUE` (PRD §8), while the idempotency
    // *lookup* is scoped to the owner — so a key another user already holds is
    // a clean 409, never a 500 and never a window into someone else's data.
    // In practice this is unreachable: the key is a client-generated v4 UUID.
    let b = app
        .upload_meal(&bob, MealUpload::photo("shared-uuid", &photo))
        .await;
    assert_eq!(b.status(), 409);
    let body = b.text().await.expect("conflict body");
    assert!(
        !body.contains(&alice_meal),
        "the conflict must not disclose the other user's meal: {body}"
    );

    // Alice's own replay still deduplicates normally.
    let replay: serde_json::Value = app
        .upload_meal(&alice, MealUpload::photo("shared-uuid", &photo))
        .await
        .json()
        .await
        .expect("replay body");
    assert_eq!(replay["meal_id"], a["meal_id"]);
    assert_eq!(replay["deduplicated"], true);
}

// ---------------------------------------------------------------------------
// §13 — auth isolation
// ---------------------------------------------------------------------------

#[tokio::test]
async fn one_users_meal_is_invisible_to_another() {
    let app = TestApp::start().await;
    let alice = app.token("alice");
    let bob = app.token("bob");
    let photo = jpeg(900, 900, 200);

    let created: serde_json::Value = app
        .upload_meal(&alice, MealUpload::photo("uuid-private", &photo))
        .await
        .json()
        .await
        .expect("body");
    let meal_id = created["meal_id"].as_str().expect("meal id").to_string();
    app.await_terminal(&alice, &meal_id).await;

    for path in [
        format!("/api/v1/meals/{meal_id}"),
        format!("/api/v1/meals/{meal_id}/thumbnail"),
        format!("/api/v1/meals/{meal_id}/revisions"),
    ] {
        let response = app.get(&path, &bob).await;
        assert_eq!(
            response.status(),
            404,
            "{path} leaked across users (a 403 would confirm the id exists)"
        );
    }

    let patched = app
        .patch_json(
            &format!("/api/v1/meals/{meal_id}"),
            &bob,
            json!({ "status": "confirmed" }),
        )
        .await;
    assert_eq!(patched.status(), 404);

    let bobs_history: Vec<serde_json::Value> = app
        .get("/api/v1/meals", &bob)
        .await
        .json()
        .await
        .expect("history");
    assert!(bobs_history.is_empty(), "history must be user-scoped");
}

// ---------------------------------------------------------------------------
// §13 — grouped sittings
// ---------------------------------------------------------------------------

#[tokio::test]
async fn one_sitting_is_three_loops_and_one_notification() {
    let app = TestApp::start().await;
    let token = app.token("alice");
    let group_id = "sitting-abc";
    let mut events = app.events();

    let mut meal_ids = Vec::new();
    for (index, tint) in [30u8, 120, 210].into_iter().enumerate() {
        let photo = jpeg(1024, 768, tint);
        // The hint rides with the final shot, as the Android client sends it.
        let size = if index == 2 { Some(3) } else { None };
        let uuid = format!("uuid-group-{index}");
        let body: serde_json::Value = app
            .upload_meal(
                &token,
                MealUpload::photo(&uuid, &photo).in_group(group_id, size),
            )
            .await
            .json()
            .await
            .expect("body");
        meal_ids.push(body["meal_id"].as_str().expect("meal id").to_string());
    }

    for meal_id in &meal_ids {
        let meal = app.await_terminal(&token, meal_id).await;
        assert_eq!(meal["status"], "needs_review");
        assert_eq!(meal["group_id"], group_id);
    }

    // Three photos → three independent loops (§5).
    assert_eq!(
        app.llm.responses_calls.load(Ordering::SeqCst),
        3,
        "each photo must run its own loop"
    );

    // The sitting settles on the hint, not on the 90s debounce: only a few
    // hundred milliseconds have passed.
    let settled = picweight_backend::jobs::group_settler::tick(&app.state)
        .await
        .expect("settler tick");
    assert_eq!(settled, 1, "the group_size hint must short-circuit the debounce");

    // ... and never again.
    let again = picweight_backend::jobs::group_settler::tick(&app.state)
        .await
        .expect("second settler tick");
    assert_eq!(again, 0, "a settled sitting must not notify twice");

    let seen = TestApp::drain(&mut events);
    let notifications: Vec<_> = seen
        .iter()
        .filter(|e| e.kind == MealEventKind::GroupSettled)
        .collect();
    assert_eq!(
        notifications.len(),
        1,
        "N photos must produce exactly one notification (§6); saw {:?}",
        seen.iter().map(|e| e.kind).collect::<Vec<_>>()
    );

    let notification = notifications[0];
    assert_eq!(notification.group_id.as_deref(), Some(group_id));
    let headline = notification.headline.as_deref().unwrap_or_default();
    assert!(
        headline.contains('3'),
        "the sitting leads with a summary line, got {headline:?}"
    );
    assert!(
        notification.day.is_some(),
        "§9: the payload carries the day state so no follow-up request is needed"
    );

    // Per-meal `completed` events must not carry notification copy, or a
    // grouped sitting would fire four notifications instead of one.
    for event in seen.iter().filter(|e| e.kind == MealEventKind::Completed) {
        assert!(
            event.headline.is_none(),
            "a grouped meal must not carry its own notification copy"
        );
    }

    // The sitting reads back as one row, expandable to three.
    let group: serde_json::Value = app
        .get(&format!("/api/v1/groups/{group_id}"), &token)
        .await
        .json()
        .await
        .expect("group body");
    assert_eq!(group["meals"].as_array().expect("meals").len(), 3);
    let total = group["totals"]["kcal"].as_f64().expect("group kcal");
    assert!(
        (total - 2340.0).abs() < 1.0,
        "the sitting sums its members: {total}"
    );
}

#[tokio::test]
async fn a_failed_member_does_not_block_the_sitting() {
    let app = TestApp::start().await;
    let token = app.token("alice");
    let group_id = "sitting-partial";
    let mut events = app.events();

    // The first photo lands normally.
    let ok_photo = jpeg(900, 700, 15);
    let ok: serde_json::Value = app
        .upload_meal(
            &token,
            MealUpload::photo("uuid-partial-0", &ok_photo).in_group(group_id, None),
        )
        .await
        .json()
        .await
        .expect("body");
    let ok_id = ok["meal_id"].as_str().expect("meal id").to_string();
    app.await_terminal(&token, &ok_id).await;

    // The second gets an unusable answer from the model — an empty item list —
    // which exhausts the loop *and* the single-shot fallback, so the meal
    // fails loudly rather than stalling (§5).
    app.llm.set_estimate(json!({
        "dish_name": "",
        "overall_confidence": 0.0,
        "items": []
    }));
    let bad_photo = jpeg(900, 700, 240);
    let bad: serde_json::Value = app
        .upload_meal(
            &token,
            MealUpload::photo("uuid-partial-1", &bad_photo).in_group(group_id, Some(2)),
        )
        .await
        .json()
        .await
        .expect("body");
    let bad_id = bad["meal_id"].as_str().expect("meal id").to_string();
    let failed = app.await_terminal(&token, &bad_id).await;
    assert_eq!(failed["status"], "failed");
    assert!(
        !failed["error"].as_str().unwrap_or_default().is_empty(),
        "a failed meal must say why, never stall silently"
    );

    let settled = picweight_backend::jobs::group_settler::tick(&app.state)
        .await
        .expect("settler tick");
    assert_eq!(settled, 1, "a failed member must not hold the sitting open");

    let seen = TestApp::drain(&mut events);
    assert_eq!(
        seen.iter()
            .filter(|e| e.kind == MealEventKind::GroupSettled)
            .count(),
        1
    );
    assert!(
        seen.iter().any(|e| e.kind == MealEventKind::Failed),
        "the failure is visible on the stream"
    );
}

// ---------------------------------------------------------------------------
// §13 — correction by conversation
// ---------------------------------------------------------------------------

#[tokio::test]
async fn a_correction_writes_a_new_revision_in_one_turn() {
    let app = TestApp::start().await;
    let token = app.token("alice");
    let photo = jpeg(1400, 1050, 60);

    let created: serde_json::Value = app
        .upload_meal(&token, MealUpload::photo("uuid-correct", &photo))
        .await
        .json()
        .await
        .expect("body");
    let meal_id = created["meal_id"].as_str().expect("meal id").to_string();
    let first = app.await_terminal(&token, &meal_id).await;
    assert_eq!(first["revision"], 1);
    let calls_after_first_loop = app.llm.responses_calls.load(Ordering::SeqCst);
    assert_eq!(calls_after_first_loop, 1);

    // "too much rice, it was about half that"
    let mut halved = harness::default_estimate("Шаурма с курицей");
    halved["items"][2]["grams"] = json!(60.0);
    halved["items"][2]["kcal"] = json!(90.0);
    halved["items"][2]["carbs_g"] = json!(19.5);
    app.llm.set_estimate(halved);

    let accepted = app
        .post_json(
            &format!("/api/v1/meals/{meal_id}/reanalyze"),
            &token,
            json!({ "feedback": "too much rice, it was about half that" }),
        )
        .await;
    assert_eq!(accepted.status(), 202);

    let corrected = loop {
        let meal = app.await_terminal(&token, &meal_id).await;
        if meal["revision"].as_i64() == Some(2) {
            break meal;
        }
        tokio::time::sleep(std::time::Duration::from_millis(25)).await;
    };

    let rice = corrected["items"]
        .as_array()
        .expect("items")
        .iter()
        .find(|i| i["name"] == "Rice")
        .expect("the rice row");
    assert_eq!(rice["grams"], 60.0, "the rice roughly halves");

    // The continuation costs one short turn, not another full loop.
    assert_eq!(
        app.llm.responses_calls.load(Ordering::SeqCst) - calls_after_first_loop,
        1,
        "resuming the session must not re-run the whole loop"
    );

    // Prior revisions are retained, and the feedback that caused each is
    // readable — `parent_job_id` + `user_feedback` make the chain auditable.
    let revisions: serde_json::Value = app
        .get(&format!("/api/v1/meals/{meal_id}/revisions"), &token)
        .await
        .json()
        .await
        .expect("revisions");
    let entries = revisions["revisions"].as_array().expect("revision list");
    assert_eq!(entries.len(), 2, "the first revision is retained");
    let second = entries
        .iter()
        .find(|r| r["revision"] == 2)
        .expect("revision 2");
    assert_eq!(
        second["user_feedback"], "too much rice, it was about half that",
        "the feedback is stored verbatim"
    );
}

#[tokio::test]
async fn reanalyzing_a_meal_still_being_analyzed_is_refused() {
    let app = TestApp::start().await;
    let token = app.token("alice");

    let created: serde_json::Value = app
        .upload_meal(&token, MealUpload::manual("uuid-busy", "Плов"))
        .await
        .json()
        .await
        .expect("body");
    let meal_id = created["meal_id"].as_str().expect("meal id").to_string();

    // Race the worker: whichever side wins, the answer must be a clean 202 or
    // a clean 409 — never a 500 and never a silently dropped correction.
    let response = app
        .post_json(
            &format!("/api/v1/meals/{meal_id}/reanalyze"),
            &token,
            json!({ "feedback": "less oil" }),
        )
        .await;
    assert!(
        matches!(response.status().as_u16(), 202 | 409),
        "unexpected status {}",
        response.status()
    );
}

// ---------------------------------------------------------------------------
// §6 — targets and the day view
// ---------------------------------------------------------------------------

#[tokio::test]
async fn a_confirmed_meal_lands_in_the_day_view() {
    let app = TestApp::start().await;
    let token = app.token("alice");

    let profile = app
        .put_json(
            "/api/v1/me/profile",
            &token,
            json!({
                "sex": "male",
                "birth_date": "1990-05-01",
                "height_cm": 180.0,
                "activity_factor": 1.55,
                "goal_type": "lose",
                "target_weight_kg": 75.0,
                "rate_kg_per_week": 0.5,
                "timezone": "Europe/Moscow",
                "current_weight_kg": 85.0
            }),
        )
        .await;
    assert_eq!(profile.status(), 200);
    let profile: serde_json::Value = profile.json().await.expect("profile body");
    let target_kcal = profile["profile"]["target_kcal"].as_f64().expect("target");
    assert!(
        (1500.0..3500.0).contains(&target_kcal),
        "Mifflin-St Jeor produced {target_kcal}"
    );
    assert!(
        profile["warnings"].as_array().expect("warnings").is_empty(),
        "0.5 kg/week on 85 kg is under the 1%/week threshold"
    );

    let photo = jpeg(1000, 750, 77);
    let created: serde_json::Value = app
        .upload_meal(
            &token,
            MealUpload {
                client_uuid: "uuid-day",
                eaten_at: "2026-08-01T12:30:00+03:00",
                timezone_offset: 180,
                photo: Some(&photo),
                dish_name: None,
                group_id: None,
                group_size: None,
            },
        )
        .await
        .json()
        .await
        .expect("body");
    let meal_id = created["meal_id"].as_str().expect("meal id").to_string();
    app.await_terminal(&token, &meal_id).await;
    app.patch_json(
        &format!("/api/v1/meals/{meal_id}"),
        &token,
        json!({ "status": "confirmed" }),
    )
    .await;

    // The meal was eaten at 12:30 local in UTC+3, so it belongs to 1 Aug —
    // "what did I eat today" is a question about *your* day, not UTC's (§8).
    let day: serde_json::Value = app
        .get("/api/v1/days/2026-08-01", &token)
        .await
        .json()
        .await
        .expect("day body");
    let consumed = day["state"]["consumed_kcal"].as_f64().expect("consumed kcal");
    assert!((consumed - 780.0).abs() < 0.5, "consumed {consumed}");
    assert_eq!(day["state"]["meals_logged"], 1);
    assert_eq!(day["meals"].as_array().expect("day meals").len(), 1);
    let remaining = day["state"]["remaining_kcal"].as_f64().expect("remaining");
    assert!(
        (remaining - (target_kcal - 780.0)).abs() < 1.0,
        "remaining {remaining} against a {target_kcal} target"
    );
    assert!(
        !day["verdict"].as_str().unwrap_or_default().is_empty(),
        "the day view always carries a verdict line, templated or phrased"
    );

    // …and nothing at all on the day before.
    let empty: serde_json::Value = app
        .get("/api/v1/days/2026-07-31", &token)
        .await
        .json()
        .await
        .expect("empty day");
    assert_eq!(empty["state"]["meals_logged"], 0);
}

#[tokio::test]
async fn a_confirmed_dish_becomes_a_recent_chip() {
    let app = TestApp::start().await;
    let token = app.token("alice");
    let photo = jpeg(800, 800, 33);

    let created: serde_json::Value = app
        .upload_meal(&token, MealUpload::photo("uuid-chip", &photo))
        .await
        .json()
        .await
        .expect("body");
    let meal_id = created["meal_id"].as_str().expect("meal id").to_string();
    app.await_terminal(&token, &meal_id).await;

    let before: Vec<serde_json::Value> = app
        .get("/api/v1/dishes/recent", &token)
        .await
        .json()
        .await
        .expect("chips");
    assert!(
        before.is_empty(),
        "a draft is not ground truth and must not become a chip"
    );

    app.patch_json(
        &format!("/api/v1/meals/{meal_id}"),
        &token,
        json!({ "status": "confirmed" }),
    )
    .await;

    let after: Vec<serde_json::Value> = app
        .get("/api/v1/dishes/recent", &token)
        .await
        .json()
        .await
        .expect("chips");
    assert_eq!(after.len(), 1);
    assert_eq!(after[0]["dish_name"], "Шаурма с курицей");
}

#[tokio::test]
async fn the_export_carries_the_users_own_meals_only() {
    let app = TestApp::start().await;
    let alice = app.token("alice");
    let bob = app.token("bob");
    let photo = jpeg(700, 700, 55);

    let created: serde_json::Value = app
        .upload_meal(&alice, MealUpload::photo("uuid-export", &photo))
        .await
        .json()
        .await
        .expect("body");
    let meal_id = created["meal_id"].as_str().expect("meal id").to_string();
    app.await_terminal(&alice, &meal_id).await;

    let mine: serde_json::Value = app
        .get("/api/v1/export", &alice)
        .await
        .json()
        .await
        .expect("export");
    assert_eq!(mine["meals"].as_array().expect("meals").len(), 1);

    let theirs: serde_json::Value = app
        .get("/api/v1/export", &bob)
        .await
        .json()
        .await
        .expect("export");
    assert!(theirs["meals"].as_array().expect("meals").is_empty());
}

// ---------------------------------------------------------------------------
// §13 — contract-drift guard
// ---------------------------------------------------------------------------

/// `android/openapi.json` is the Android client's source of truth: the Retrofit
/// interfaces are code-generated from it (PRD §7). A spec change that is not
/// re-exported silently ships a client that no longer matches the server, and
/// the failure surfaces as a runtime 404 in the app rather than a build error.
///
/// Skips rather than fails when the file is absent, so a build stage that copies
/// only `backend/` still runs the suite.
#[test]
fn the_exported_openapi_spec_is_current() {
    use utoipa::OpenApi;

    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("android")
        .join("openapi.json");
    let Ok(on_disk) = std::fs::read_to_string(&path) else {
        eprintln!("skipping: {} is not present", path.display());
        return;
    };

    let current = picweight_backend::api::ApiDoc::openapi()
        .to_pretty_json()
        .expect("the spec serializes");

    assert_eq!(
        on_disk.trim(),
        current.trim(),
        "{} is stale — regenerate it with `cargo run -p picweight-backend -- openapi ../android/openapi.json`",
        path.display()
    );
}
