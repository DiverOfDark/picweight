# PRD — picweight

AI-assisted calorie & БЖУ tracker. Photograph what you eat and drink; a server-side estimation agent returns calories and macros; every logged meal immediately tells you what budget you have left today.

Self-hosted, single container, deployed to Kubernetes via Helm. Android app + Vue web app + Rust backend, modelled directly on **phos** (`/var/home/diverofdark/RustroverProjects/phos`).

---

## 1. Context

Manual calorie tracking fails for one reason: **friction per meal**. Search a database, pick a portion, confirm — 40+ seconds per entry, five times a day. People quit inside two weeks. The bet: *photo → structured macros* makes daily logging survivable.

The trade is accuracy. A vision model names a dish reliably; it cannot see grams. Portion estimation is where essentially all the error lives.

**Three facts drive the entire design:**

**(1) The dominant food source is delivery**, not packaged goods or home cooking. Nutrition databases have no entry for "шаурма из Фарша", and they never will. Restaurant food is systematically oilier and larger-portioned than the home-cooked equivalent a naive model imagines. And delivery orders **repeat** — the same handful of dishes, over and over.

**(2) The user will rarely type a comment.** Stated up front, and the design must respect it: the obvious accuracy fix — "just tell the model what it is" — is not available by default.

**(3) БЖУ is determined by the LLM.** No dish-matching against USDA/Open Food Facts. This is the right call and worth stating why: for delivery food the database was never going to contain the dish, and once the model is already estimating portion grams — which is where nearly all the error lives — the marginal error from also estimating per-100g macros is small by comparison. Chasing a "real" macro source would add a hard matching problem and a bulk-seeding pipeline to fix the *smaller* half of the error. Cut it.

Together, (1) and (2) make **recall from the user's own confirmed history the primary accuracy mechanism**. A dish confirmed once is ground truth the user already vetted; every subsequent order should be a near-exact, near-free lookup. The system's job is to reach that state fast and then not poison it.

The free-text comment stays — highest signal *when present* — but it's a bonus path. Two zero-keyboard alternatives carry the real load:
- **Tap a recent dish.** Capture screen surfaces your last ~8 confirmed dishes as chips. One tap sets the name. Faster than typing, more accurate than vision.
- **Delivery-app share sheet.** Share an order into picweight; the dish name arrives as text with no typing at all.

**Intended outcome:** a self-hosted tracker used by the author and a handful of others, that (a) makes logging fast enough to sustain, (b) produces defensible numbers for delivery food without user commentary, and (c) tells you what's left in the tank the moment you log.

---

## 2. Reference implementation — phos

`/var/home/diverofdark/RustroverProjects/phos` is the same author's project with the same three-client shape. **Copy its patterns rather than reinventing them.**

| Concern | phos pattern to inherit | phos file |
|---|---|---|
| Monorepo layout | `backend/` + `frontend/` + `android/` + `helm/` at root | — |
| Backend stack | axum 0.8, diesel 2 (`sqlite`, `r2d2`), `diesel_migrations`, tracing, tower-http | `backend/Cargo.toml` |
| OIDC | `openidconnect` 4 + `jsonwebtoken` 10; separate **web (confidential)** and **mobile (public/native)** clients | `backend/src/auth.rs` |
| OpenAPI | `utoipa` + `utoipa-scalar`, spec exported to `android/openapi.json` | `backend/Cargo.toml` |
| Android API client | **Generated**, not hand-written: `org.openapi.generator` 7.20.0 Gradle plugin → Retrofit | `android/app/build.gradle.kts:6,66` |
| Static serving | API under `/api/`, everything else falls through to `fallback_service` | `backend/src/main.rs` |
| APK distribution | Android SDK build stage → APK copied to `static/phos.apk`, linked from web UI | `Dockerfile` stage 2e, `frontend/src/App.vue:442-849` |
| Docker build | Multi-stage + `cargo-chef`; `cargo test` runs *inside* the build; keystore via BuildKit secret | `Dockerfile` |
| CI | 3 jobs — `build-android`, `docker` (buildx → GHCR, `metadata-action`, gha cache), `release` on `v*` | `.github/workflows/ci.yml` |
| Helm | `Chart.yaml`, `values.yaml`, `templates/{_helpers.tpl,deployment,service,ingress,pvc}.yaml` | `helm/phos/` |
| Deployment | ArgoCD, image pinned to `sha-$ARGOCD_APP_REVISION_SHORT` | `helm/phos/values.yaml` |
| Frontend stack | Vue 3, Vite, Tailwind 4, shadcn-vue / reka-ui, lucide | `frontend/package.json` |
| Dep updates | `renovate.json` at root | — |

**Where picweight deliberately diverges:**
- **No `externalsecret.yaml` template.** phos hardcodes External Secrets Operator into the chart. picweight does not — see §10.
- No ONNX, no ffmpeg, no ML system deps → far smaller runtime image and faster build. Drop every `libav*`, `clang`, `nasm` line from the Dockerfile.
- Resources drop from 2Gi/8Gi to ~256Mi/1Gi. This is an I/O-bound API server, not an inference box.
- PVC ~5Gi, not 50Gi (§7 storage math).
- Adds `rig-core` for the agent loop (§5) — phos has no equivalent.

---

## 3. Users & scope

- **Users:** the author plus family/friends — order of 2–10 accounts.
- **Deployment:** self-hosted Kubernetes (author's homelab), Helm + ArgoCD, single container.
- **Auth:** generic OIDC via discovery. Primary target Zitadel (as phos), issuer/client-id from config so any compliant IdP works. No local password storage.
- **Backups:** out of scope for the app. `rclone` against the PVC covers it — one README line, no in-app feature.
- **Not in scope:** billing, multi-tenancy beyond per-user row isolation, Play Store distribution, iOS, social features, recipe management, meal planning, scheduled notifications, in-app cost budgeting (the OpenAI key carries a provider-side spend limit).

---

## 4. Goals & non-goals

### Goals
| # | Goal | Success signal |
|---|---|---|
| G1 | Log a meal in ≤10s of user attention | Median capture→confirmed under 10s |
| G2 | Defensible numbers for delivery food **without** user commentary | <30% median kcal error on known-weight test meals, no comment supplied |
| G3 | Repeat orders become near-exact | Second occurrence of a confirmed dish within a few % of corrected numbers |
| G4 | A wrong estimate is fixable in one message | Re-analysis with feedback produces corrected values without manual per-item editing |
| G5 | Never lose a logged meal | Zero drops across airplane-mode testing |
| G6 | Targets derived from body data | Computed at onboarding, recomputed on weight change |
| G7 | Immediate actionable feedback per meal | Self-contained notification on analysis completion |
| G8 | One-command deploy, any secrets backend | `helm install` works with sealed-secrets *or* external-secrets, chart hardcodes neither |

### Non-goals
- Medically accurate nutrition. Decision-support, not a clinical instrument.
- Authoritative macro sourcing. БЖУ comes from the LLM by design (§1.3).
- Working without an OpenAI API key.
- Periodic/scheduled reminders. Feedback is event-driven only (§6).
- Retaining original-resolution photographs.

---

## 5. Estimation agent

A **bounded agentic loop** server-side, built on **`rig-core`**.

### Why rig rather than hand-rolling
- `Tool` trait gives typed `Args`/`Output` plus the JSON-schema `ToolDefinition` in one impl — no hand-written schemas drifting from the Rust types.
- **`.multi_turn(n)` is exactly the bound this design needs.** Set `n = 6`; exceeding it surfaces `MaxDepthError`, which is the trigger for the single-shot fallback. The loop cap is a library guarantee, not something to reimplement and get wrong.
- Extractors give typed structured output, and compose *as tools* inside a larger agent.
- Provider-neutral, so the OpenAI dependency is swappable later.
- Multi-turn streaming exposes each tool call and result — feeds `agent_steps` (§8) directly.

*Accepted cost:* a framework dependency whose provider abstraction may lag OpenAI's newest features, plus version churn. Contained by keeping all loop logic behind the `backend/src/agent/` module boundary, so a swap to hand-rolled `async-openai` touches one directory. Evaluate `swiftide` only if rig's tool ergonomics disappoint — it is the heavier, RAG-oriented option and this workload is not RAG.

### Inputs
- The photo (768px, §7).
- **Dish name, if available** — from a tapped recent-dish chip, a share-sheet intent, or (rarely) a typed comment. When present it outranks the agent's visual read; the prompt says so explicitly.
- **User feedback, on re-analysis** — see *Correction by conversation* below.
- Meal time and the user's recent meal history.

### Tools
| Tool | Purpose | Notes |
|---|---|---|
| `recall_similar_meals(query)` | Search this user's **confirmed** meals by normalized dish name | **Primary accuracy mechanism.** Always called first; a confident hit short-circuits the loop. |
| `lookup_barcode(ean)` | Open Food Facts by EAN | Packaged goods only. Exact, and the one place a real database still earns its keep. |
| `web_search(query)` | Published nutrition for chain restaurants | Config-gated, off by default. |

**There is no `lookup_food` dish-matching tool and no USDA seeding.** БЖУ comes from the model (§1.3).

### Loop (2–5 steps typical, `multi_turn(6)` cap, ~25s)

1. **Identify** — dish name, cuisine, visible components, and crucially the **container**: a standard delivery bowl, a 30cm pizza box, a 0.5L cup. With no comment and no coin on the plate, the container is the only reliable scale reference in the frame. The prompt must push hard on this.
2. **Recall** — `recall_similar_meals` first, always.
3. **Estimate** — per-item grams **and** per-item kcal/protein/fat/carbs, directly.
4. **Critique** — one mandatory self-check against a fixed checklist: *Is this restaurant food? Then assume more cooking oil than a home recipe. Did you count the rice under the curry? The dressing? The sauce? Is the portion consistent with the container you identified?* Adjust and re-emit.
5. **Emit** — structured output: per-item name, grams, macros, confidence, plus a one-line reasoning note per item so the user can see *why*.

**Bounds:** `multi_turn(6)`, ~25s wall clock. On `MaxDepthError`, tool failure or timeout, fall back to a single-shot vision call so the user always gets a draft. The OpenAI key carries a provider-side spend limit, so no in-app budgeting — but the backend must handle 429/quota-exhausted **loudly**: the meal goes to `failed` with a visible reason, never a silent stall.

### Correction by conversation — a persisted, resumable agent session

Beyond per-item sliders, the user sends **a free-text adjustment message** and the agent continues:

> "too much rice, it was about half that" · "this was the small portion" · "no sour cream" · "that's a 0.33 can not 0.5"

**The agent session is persisted and resumed, not restarted.** The full message thread — system prompt, the image turn, every tool call and tool result, the agent's own reasoning and its structured answer — is serialized to `agent_sessions.messages` when the job finishes. `POST /api/v1/meals/:id/reanalyze { feedback }` loads that thread, appends the feedback as a new user turn, and continues the same conversation. Output is a new `revision`; prior revisions are retained.

Why continuation rather than a fresh call with a summary:
- The agent keeps **why** it said 780 kcal — which container it identified, what recall returned, what it already ruled out. "Half the rice" is interpreted against its own prior reasoning instead of re-derived from scratch.
- Tool results are already in context. Correcting a dish costs one short turn, not another full loop with repeat tool calls.
- Multi-turn correction actually converges: "half the rice" → "still too much" → the agent has both prior attempts and adjusts, rather than oscillating.
- It's the natural fix for a **recall-poisoned** dish. You don't hunt for the wrong row — you say what's wrong, and the corrected revision supersedes it in recall.
- The feedback text is far richer signal than a gram delta, and it's stored verbatim.
- It costs typing, but only on meals that are actually wrong — a completely different budget from typing on every capture.

**Engineering caveats, to handle rather than discover:**
- **Context grows per revision.** Cap stored history (e.g. 20 turns), and drop the oldest tool-result payloads first — they're the bulkiest and least useful on a correction turn. Log token counts per revision in `analysis_jobs`.
- **A session pins the prompt and model it began with.** After a prompt or model change, continuing an old thread runs it under stale instructions. Store `prompt_version` + `model` on the session; when either differs materially, start a fresh session **seeded with the last confirmed result** instead of continuing, and record which path was taken.
- **Images are re-attached from disk** (the 768px thumbnails, §7) rather than assumed to survive in serialized history — provider message formats vary on image retention.

Manual per-item editing stays for small tweaks: gram sliders with snap points, add/remove items in two taps, and `portion_scale` for "ate 60% of it". Every correction, typed or dragged, is stored in `item_corrections`.

### Batch capture — one meal, many photos

Ordering from a restaurant menu produces several dishes at once. Photographing them one at a time would mean N meals, N analyses and **N notifications** for what is one sitting.

Flow: the capture screen has an **"add another"** affordance. Shoot as many photos as the meal needs, see them in a thumbnail strip, tap **Done**. The result is **one meal, one analysis pass over all images, one notification.**

Protocol — deliberately three calls rather than one multi-file upload, because the offline queue (§M6) needs each photo independently retryable on a flaky connection:

```
POST /api/v1/meals              → { client_uuid, ... }  creates meal in `draft`
POST /api/v1/meals/:id/photos   → one photo + photo_uuid (per-photo idempotency key)
POST /api/v1/meals/:id/complete → transitions to `pending`, enqueues ONE job
```

A single N-file upload would be all-or-nothing: one dropped connection and the whole sitting has to be reshot. Per-photo requests let WorkManager retry exactly the photo that failed.

**Failure mode to handle:** the app is killed between the last photo and `complete`, leaving a `draft` meal that never analyses. A sweeper finalizes `draft` meals idle for >15 minutes, so a batch is never silently lost — it completes with whatever photos arrived.

The agent receives all images in one turn and is told they are **one meal, multiple dishes** — so it enumerates dishes across images rather than treating each as a separate estimate, and de-duplicates when the same dish appears in two shots from different angles. Single-photo capture is just a batch of one; there is no separate code path.

### Drinks
Photograph the bottle/can — label reading is a solved vision problem and far easier than portion estimation. Barcode when convenient (ML Kit, on-device, offline, free, exact). Otherwise a comment.

### Manual entry (no photo)
Lightweight "ate something, roughly this" path: dish name (or recent-dish chip) + rough size, into the agent without an image. Keeps the dataset from developing a systematic hole where unphotographed meals live.

### The flywheel
Corrections feed `recall_similar_meals`. Confirm a dish once and the next order starts from your corrected numbers. Given rare comments, this is the main thing standing between you and permanent guesswork.

### Known limitation
Hidden fat is invisible to a camera. Restaurant pad thai can carry 40g of oil nothing in the photo reveals. Mitigations: the critique step's restaurant-oil prior, a "cooked with" quick-chip row (oil / butter / sugar / sauce), and a per-user `calibration_factor` learned from correction history.

---

## 6. Targets & per-meal feedback

### The number is a formula, not an LLM

```
BMR (male)   = 10·kg + 6.25·cm − 5·age + 5
BMR (female) = 10·kg + 6.25·cm − 5·age − 161
TDEE         = BMR × activity_factor   (1.2 sedentary … 1.9 very active)
target_kcal  = TDEE + goal_delta       (deficit typically −300…−750)
protein_floor = 1.6–2.2 g/kg target weight
fat_floor     = 0.8 g/kg
carbs         = remainder
```

Deterministic, offline, auditable, more reliable than asking a language model to do arithmetic. (Distinct from §1.3: the LLM estimates *what you ate*; it does not compute *what you should eat*.) Onboarding collects age, sex, height, current weight, target weight, activity level, target rate — and warns (does not silently accept) a deficit steeper than ~1% bodyweight/week.

### Feedback is event-driven, never scheduled

**No periodic nudges, no daily cron.** The only trigger is *you just logged something*.

On analysis completion, one push notification + home-screen update:

> **Шаурма с курицей — 780 kcal**
> 1,450 / 2,050 today · **600 left**
> Protein 82/165g — 83g short with 600 kcal to spend. Doable, but it has to be mostly protein.

1. **What was logged** — dish + kcal; doubles as confirmation the job finished.
2. **Where you stand** — consumed / target, remaining kcal.
3. **Macro status** — remaining protein against the floor, the constraint that actually binds.
4. **One-line verdict** — LLM-phrased from the numbers.

Rules engine computes the state (on track / tight / over / protein-unreachable); the LLM supplies only the wording. If the LLM is unavailable you get the templated string — never a hard fail.

**Timing — decided:** one notification, fired when the loop completes (~20–30s after capture). No optimistic pre-notification, no corrected follow-up. *Accepted risk:* the phone may be pocketed by then, so the notification must be **self-contained** — it has to make sense read cold, minutes later. Lead with the dish name.

**Channel — decided:** a real notification on *every* logged meal, ~5/day. *Accepted risk:* that frequency is what turns scheduled nudges into wallpaper. The defence is that each carries genuinely new information — your actual remaining budget, not a reminder. If they get swiped unread, fall back to threshold-only notifications with silent home-screen updates. Revisit after two weeks of real use.

A **batch** (§5) fires exactly one notification for the whole sitting, leading with a summary line ("5 dishes — 2,140 kcal") rather than one notification per photo. A **re-analysis** fires an updated notification for that meal.

Optional, **off by default**: a single end-of-day summary.

---

## 7. Architecture

```
┌─────────────────┐        ┌──────────────────┐
│ Android (Kotlin)│        │ Vue 3 SPA        │
│ Compose, Hilt   │        │ Vite + Tailwind4 │
│ Retrofit (gen'd │        │ shadcn-vue       │
│  from openapi)  │        │ history / stats  │
│ Room + WorkMgr  │        │ APK download link│
│ CameraX, MLKit  │        │                  │
└────────┬────────┘        └────────┬─────────┘
    OIDC native (public)      OIDC web (confidential)
         │  Bearer JWT              │
         └───────────┬──────────────┘
                     ▼
    ┌────────────────────────────────────┐
    │ Rust backend (axum 0.8)            │
    │  /api/*  → utoipa-documented routes│
    │  /*      → fallback_service(static)│
    │  /picweight.apk → bundled APK      │
    │  JWT/JWKS validation               │
    │  ingest → job queue → rig agent    │
    │  OpenAI key lives here only        │
    └─────┬──────────────────┬───────────┘
          │                  │
    ┌─────▼──────┐   ┌───────▼─────────┐
    │ SQLite     │   │ thumbs/ on PVC  │
    │ diesel+r2d2│   │ 768px, ~80KB    │
    │ WAL        │   │ content-addressed│
    └────────────┘   └─────────────────┘
                     │
                     ▼ outbound
      OpenAI API / Open Food Facts / (web search)
```

**Non-negotiable:** the OpenAI key lives only in the backend. Clients never talk to OpenAI.

### Photo storage — thumbnails only

Originals are **not retained**. On ingest the backend writes the upload to a temp file; once analysis completes it stores one derivative and deletes the original.

- **768px long edge, JPEG q75, ~60–100KB**
- Content-addressed: `thumbs/<sha256[0:2]>/<sha256[2:4]>/<sha256>.jpg`

*Why 768 and not 320:* vision models downscale to roughly this range anyway, so a 768px thumbnail is simultaneously the display asset **and** a valid re-analysis input — which matters directly, because §5's correction-by-conversation re-runs the agent against the stored image. A 320px display-only thumbnail would break that feature, not just foreclose future re-runs.

Two years at 5 meals/day ≈ **350MB**. PVC of 5Gi is generous.

### Backend — Rust
- `axum` 0.8 + `tokio`, `tower-http` (cors, fs, trace)
- `diesel` 2 (`sqlite`, `r2d2`) + `diesel_migrations`, WAL + `busy_timeout` — matching phos
- **`rig-core`** for the agent loop, tools and extractors
- `openidconnect` 4 + `jsonwebtoken` 10, cached JWKS
- `utoipa` + `utoipa-scalar`; spec exported to `android/openapi.json`
- `image` for the 768px derivative
- `reqwest` for Open Food Facts (rig owns OpenAI transport)
- Analysis is an **async job**, never inline. Upload returns `202`; a worker runs the loop; result over SSE or poll.
- Env prefix `PICWEIGHT_*`, default port **33100** (phos holds 33000)

### Android — Kotlin
- Compose, Hilt, **Retrofit generated from `android/openapi.json`** via `org.openapi.generator` 7.20.0 — API contract can't drift
- CameraX capture; ML Kit barcode scanning
- **Batch capture** — "add another" on the capture screen, thumbnail strip of the pending batch, **Done** to finalize. Each photo uploads independently through WorkManager (own `photo_uuid`); `complete` fires when the queue for that meal drains. One meal, one analysis, one notification.
- **Recent-dish chips** on the capture screen (last ~8 confirmed dishes, one tap sets the name)
- **Share-sheet intent filter** for `text/plain` so a delivery-app order shares straight in
- **Re-analysis input** on the meal detail screen — one text field, "tell it what's wrong"
- Comment field optional, skippable in one gesture, never blocking
- Room as local source of truth; UI never blocks on network
- WorkManager upload queue, exponential backoff, `NetworkType.CONNECTED`
- AppAuth OIDC + PKCE against the **native/public** client; refresh tokens in EncryptedSharedPreferences
- Home: today's ring (kcal consumed/target), macro bars, meal list, large capture FAB

### Frontend — Vue
Vue 3 + Vite + Tailwind 4 + shadcn-vue/reka-ui + lucide, matching phos. History with thumbnails, weight/macro trends, profile & target editing, agent-reasoning inspector with revision history, re-analysis input, CSV/JSON export, and the **APK download card** — copy phos's `HEAD /picweight.apk` availability probe (`App.vue:442`) including the `content-type` check that detects the SPA fallback serving `index.html`.

---

## 8. Data model (SQLite / diesel)

```sql
users            (id, oidc_sub UNIQUE, oidc_issuer, email, display_name, created_at)
user_profiles    (user_id PK, sex, birth_date, height_cm, activity_factor,
                  goal_type, target_weight_kg, rate_kg_per_week,
                  target_kcal, target_protein_g, target_fat_g, target_carbs_g,
                  calibration_factor, targets_computed_at, timezone)
weight_logs      (id, user_id, logged_at, weight_kg, source)
thumbnails       (id, user_id, sha256 UNIQUE, path, width, height, bytes, created_at)
meals            (id, user_id, client_uuid UNIQUE,
                  dish_name, dish_name_normalized, name_source,
                  user_comment TEXT NULL, revision INTEGER NOT NULL DEFAULT 1,
                  eaten_at, timezone_offset, meal_type, status, portion_scale,
                  created_at, updated_at)
                 -- status:      draft | pending | analyzing | needs_review | confirmed | failed
                 -- name_source: vision | recent_chip | share_intent | comment | manual
meal_photos      (id, meal_id, thumbnail_id, photo_uuid UNIQUE, position, created_at)
                 -- a meal has 1..N photos; batch capture (§5). Single photo = batch of one.
meal_items       (id, meal_id, revision, position, name, barcode NULL,
                  grams, grams_source, kcal, protein_g, fat_g, carbs_g,
                  macro_source, confidence, reasoning_note)
                 -- grams_source: agent | user | barcode | recall
                 -- macro_source: recall | model | barcode | web | user
item_corrections (id, meal_item_id, field, original_value, corrected_value, corrected_at)
foods            (id, source, source_ref, name, name_normalized, brand, barcode,
                  kcal_100g, protein_100g, fat_100g, carbs_100g, fetched_at)
                 -- barcode/Open Food Facts cache ONLY; no dish-level seeding
analysis_jobs    (id, meal_id, revision, parent_job_id NULL, status, attempts, model,
                  user_feedback TEXT NULL, steps, tool_calls,
                  prompt_tokens, completion_tokens, cost_micro_usd,
                  error, created_at, finished_at)
agent_steps      (id, job_id, step_no, tool_name, tool_input, tool_output, latency_ms)
agent_sessions   (id, meal_id UNIQUE, messages TEXT, model, prompt_version,
                  turn_count, created_at, updated_at)
                 -- serialized rig message thread; resumed on reanalyze (§5)
```

Notes:
- `client_uuid` is the **idempotency key**, generated on the phone at capture. A retried upload after a flaky connection can never create a duplicate. Single most important field for offline correctness. `meal_photos.photo_uuid` is the same guarantee one level down, so an individually retried photo in a batch can't be appended twice.
- `revision` on `meals`/`meal_items`/`analysis_jobs` implements correction-by-conversation. Re-analysis writes a new revision; prior revisions retained. `parent_job_id` + `user_feedback` make the chain auditable — you can read exactly what the user said and what changed.
- `agent_sessions.messages` holds the serialized rig thread so a correction **continues** the conversation rather than restarting it. `prompt_version` + `model` decide continue-vs-reseed (§5). `turn_count` drives the history cap.
- `meals.status = draft` exists only during batch capture, between the first photo and `complete`. The sweeper finalizes drafts idle >15 min.
- Index on `(user_id, dish_name_normalized)` makes `recall_similar_meals` fast. **Recall reads only the latest revision of `confirmed` meals** — never drafts, never superseded revisions, or the agent learns from its own hallucinations and from corrections the user already rejected.
- `name_source` measures which input path actually gets used — the only way to find out whether recent-chips and the share sheet were worth building.
- `agent_steps` makes a bad estimate debuggable. Without it the loop is unimprovable. Populated from rig's multi-turn stream events.
- `timezone_offset` per meal — "what did I eat today" is a question about *your* day, not UTC's.
- Every user-scoped query filters `user_id`, enforced at the repository layer.

---

## 9. API

All under `/api/`, all `utoipa`-annotated so `android/openapi.json` regenerates cleanly.

```
GET    /api/v1/me                      → profile + today's targets
PUT    /api/v1/me/profile              → body data; recomputes targets
GET    /api/v1/dishes/recent           → last N confirmed dishes (recent-chips)
POST   /api/v1/meals                   → { client_uuid, eaten_at, dish_name?, comment?,
                                           name_source }  creates meal in `draft`
                                         201 { meal_id, status: "draft" }
POST   /api/v1/meals/:id/photos        → multipart: photo + photo_uuid + position
                                         appends to the batch; independently retryable
POST   /api/v1/meals/:id/complete      → close the batch, enqueue ONE analysis job
                                         202 { meal_id, status: "pending" }
GET    /api/v1/meals/:id               → meal + photos + items + agent reasoning + status
POST   /api/v1/meals/:id/reanalyze     → { feedback } — resume the persisted agent
                                         session, emit a new revision
GET    /api/v1/meals/:id/revisions     → revision history + the feedback that caused each
GET    /api/v1/meals/events            → SSE: completion + day-state payload
PATCH  /api/v1/meals/:id               → confirm / edit items / grams / portion_scale
DELETE /api/v1/meals/:id
GET    /api/v1/meals?from=&to=         → history (local-day bucketed)
GET    /api/v1/days/:date              → totals, targets, remaining, verdict line
POST   /api/v1/barcode/:ean            → resolve to a food record
POST   /api/v1/weights                 → log weight
GET    /api/v1/export                  → full JSON/CSV dump
GET    /healthz
GET    /picweight.apk                  → bundled APK (static)
```

The `/meals/events` completion payload carries the **day state** (consumed, remaining, protein gap, verdict line) so the notification renders from one message with no follow-up round trip. Re-analysis completion emits on the same stream.

---

## 10. Deployment

### Docker — single image, adapted from phos's `Dockerfile`

1. **frontend-builder** — `node:25-slim`, `npm ci` with cache mount, `npm run build`
2. **chef / planner / backend-test / backend-builder** — `rust:1.94` + `cargo-chef`. System deps reduce to `pkg-config libssl-dev libsqlite3-dev` (no clang, ffmpeg, nasm). `cargo test --release --lib` runs **inside the build**, so a red test fails the image.
3. **android-builder** — `eclipse-temurin:17-jdk` + cmdline-tools, `platforms;android-36`, `build-tools;36.0.0`. Signs with the `keystore_password` BuildKit secret when present, unsigned otherwise. Derives `versionName`/`versionCode` from a semver `PICWEIGHT_VERSION`. Copy phos's shell logic verbatim.
4. **runtime** — `debian:trixie-slim` + `libssl3 libsqlite3-0 ca-certificates`. Non-root uid/gid 1000. Backend binary + `frontend/dist` → `./static` + APK → `./static/picweight.apk`. `EXPOSE 33100`, `PICWEIGHT_STATIC_DIR=/app/static`.

### CI — `.github/workflows/ci.yml`, three jobs (phos's structure)
- **build-android** — JDK 17, Gradle cache keyed on wrapper + `*.gradle.kts` + `libs.versions.toml`, `assembleDebug assembleRelease`, upload release APK artifact on tags
- **docker** — buildx, GHCR login, `docker/metadata-action` emitting `sha-<sha>` / `latest` on default branch / semver tags, `keystore_password` secret passthrough, `cache-from/to: type=gha`
- **release** — on `v*`, downloads the APK artifact, cuts a GitHub Release with the APK attached

Plus `renovate.json` at root.

### Helm — `helm/picweight/`, secrets-backend agnostic

**The chart never generates a Secret and never assumes an operator.** phos hardcodes an `ExternalSecret`; picweight instead takes *names of existing Secrets* plus an escape hatch for arbitrary manifests, so the user picks sealed-secrets, External Secrets, SOPS, or a hand-applied Secret at their discretion.

Templates: `Chart.yaml`, `values.yaml`, `templates/{_helpers.tpl,deployment,service,ingress,pvc,additional-objects}.yaml`. **No `externalsecret.yaml`.**

```yaml
# templates/additional-objects.yaml
{{- range .Values.additionalObjects }}
---
{{ tpl (toYaml .) $ }}
{{- end }}
```

`tpl` means entries may use `{{ include "picweight.fullname" $ }}` and other chart helpers, so a user's SealedSecret or ExternalSecret can reference generated names without hardcoding the release name.

```yaml
image:
  repository: ghcr.io/diverofdark/picweight
  tag: "latest"
  pullPolicy: IfNotPresent

picweight:
  port: 33100
  dataPath: /app/data          # SQLite + thumbs/
  rustLog: info

persistence:
  size: 5Gi                    # thumbnails only, not 50Gi
resources:
  requests: { cpu: 100m, memory: 256Mi }
  limits:   { memory: 1Gi }    # no ONNX, no ffmpeg

oidc:
  issuer: https://auth.example.com
  scopes: "openid profile email"
  # Name of an existing Secret with client_id / client_secret / mobile_client_id.
  # Create it however you like — see additionalObjects.
  existingSecret: ""

openai:
  # Name of an existing Secret with an api_key entry. Never a values.yaml literal.
  existingSecret: ""
  apiKeyKey: api_key

# Arbitrary manifests rendered alongside the release, each passed through `tpl`.
# Use for SealedSecret, ExternalSecret, SOPS, NetworkPolicy — anything.
additionalObjects: []
  # - apiVersion: external-secrets.io/v1
  #   kind: ExternalSecret
  #   metadata:
  #     name: '{{ include "picweight.fullname" $ }}-oidc'
  #   spec:
  #     secretStoreRef: { name: openbao-store, kind: ClusterSecretStore }
  #     target: { name: '{{ include "picweight.fullname" $ }}-oidc' }
  #     data:
  #       - secretKey: client_id
  #         remoteRef: { key: zitadel/picweight-credentials, property: client_id }
  #
  # - apiVersion: bitnami.com/v1alpha1
  #   kind: SealedSecret
  #   metadata:
  #     name: '{{ include "picweight.fullname" $ }}-openai'
  #   spec:
  #     encryptedData:
  #       api_key: AgBy3i4OJSWK...
```

The deployment mounts `oidc.existingSecret` and `openai.existingSecret` by name via `secretKeyRef`. Two OIDC clients as in phos: **web** confidential (`client_id` + `client_secret`), **android** public/native (`client_id` only). Probes on `/healthz`. `fsGroup: 1000` so the PVC is writable. Deployed by ArgoCD with `image.tag=sha-$ARGOCD_APP_REVISION_SHORT`.

---

## 11. Milestones

**M0 — Skeleton + deployable shell**
Monorepo scaffold, axum + diesel + migrations, OIDC JWT validation, `/healthz`, `/api/v1/me`, utoipa spec, static fallback serving. Dockerfile (frontend + backend stages), CI docker job, Helm chart incl. `additionalObjects`, first ArgoCD deploy.
*Done when:* `helm install` yields a running pod, secrets come from a SealedSecret supplied via `additionalObjects`, and a Zitadel token authenticates against `/api/v1/me`.

**M1 — Capture → single-shot estimate**
The three-call ingest protocol (`meals` → `photos` → `complete`) with `meal_photos`, **built for batches from day one and exercised with a batch of one** — there is never a single-photo code path to migrate later. Draft sweeper. Job queue → one vision call with structured output → items → `needs_review`. Thumbnail pipeline, original deleted. Android app: OIDC login, CameraX, upload, poll, display, confirm. Android build stage added to the Dockerfile, APK served, download card in the Vue app.
*Done when:* you photograph a delivery order and see plausible items, from an APK you installed from the web UI.

**M2 — The rig agent loop**
`rig-core` agent, `Tool` impls for `recall_similar_meals` and `lookup_barcode`, `multi_turn(6)`, critique step, `agent_steps` populated from the multi-turn stream, `MaxDepthError`/timeout → single-shot fallback, loud quota-failure handling. ML Kit barcode scanning in-app.
*Done when:* ordering the same shawarma twice makes the second estimate fast and consistent with the first.

**M3 — Correction by conversation & the flywheel**
`agent_sessions` persistence and resume, `POST /meals/:id/reanalyze`, history cap + `prompt_version` reseed rule, revision model, revision history UI, gram sliders, `portion_scale`, add/remove items, "cooked with" chips, `item_corrections`, `calibration_factor`, recall restricted to latest-revision confirmed meals.
*Done when:* saying "too much rice, about half that" produces corrected numbers **without re-running the tool calls**, a follow-up "still too much" converges rather than oscillates, and the corrected version — not the original — is what recall returns next time.

**M4 — Capture UX: batch meals & zero-keyboard inputs**
Multi-photo batch UI ("add another", thumbnail strip, Done), cross-image dish enumeration and de-duplication in the agent prompt. Recent-dish chips, share-sheet intent filter, manual no-photo entry. `name_source` instrumented.
*Done when:* a five-dish restaurant order photographed as five shots produces one meal and **one** notification, and you can log a repeat order without touching the keyboard.

**M5 — Targets & per-meal feedback**
Onboarding, Mifflin-St Jeor targets, weight logging, day-state computation, verdict phrasing with templated fallback, notification on analysis completion.
*Done when:* logging lunch immediately tells you what's left for dinner, and nothing ever notifies you on a schedule.

**M6 — Offline hardening**
Room as source of truth, WorkManager queue, idempotent replay via `client_uuid`, pending-state UI.
*Done when:* airplane mode → log three meals → reconnect → all three appear exactly once.

**M7 — Dashboard**
History with thumbnails, weight/macro trends, profile editing, export, agent-reasoning inspector with revision diffs.
*Done when:* you can review a month on a laptop and see why the agent estimated what it did, and what your feedback changed.

---

## 12. Files to create

```
picweight/
├─ Dockerfile                       # adapted from phos: 4 stages, no ML deps
├─ docker-compose.yml
├─ renovate.json
├─ CLAUDE.md / ARCHITECTURE.md
├─ .github/workflows/ci.yml         # build-android | docker | release
├─ helm/picweight/
│  ├─ Chart.yaml, values.yaml
│  └─ templates/{_helpers.tpl,deployment,service,ingress,pvc,additional-objects}.yaml
├─ backend/
│  ├─ Cargo.toml, diesel.toml, build.rs
│  ├─ migrations/
│  └─ src/
│     ├─ main.rs, lib.rs            # /api routes + fallback_service(static)
│     ├─ auth.rs                    # openidconnect + JWKS  (port from phos)
│     ├─ db.rs, models.rs, schema.rs
│     ├─ api/{meals,photos,profile,days,barcode,dishes,weights,export,events}.rs
│     ├─ agent/                     # rig-core; the swap boundary
│     │  ├─ mod.rs                  # agent construction, multi_turn(6)
│     │  ├─ tools.rs                # recall_similar_meals, lookup_barcode, web_search
│     │  ├─ prompts.rs              # identify / critique / multi-image checklists
│     │  ├─ schema.rs               # structured output types
│     │  ├─ session.rs              # serialize/resume thread, history cap, reseed rule
│     │  └─ reanalyze.rs            # feedback-driven continuation
│     ├─ jobs/{analyzer.rs,draft_sweeper.rs}
│     ├─ food/openfoodfacts.rs      # barcode only
│     ├─ nutrition/targets.rs       # Mifflin-St Jeor
│     ├─ feedback/{state,phrasing}.rs
│     └─ storage/thumbs.rs          # 768px content-addressed
├─ frontend/                        # Vue 3 + Vite + Tailwind 4 + shadcn-vue
└─ android/
   ├─ openapi.json                  # generated from utoipa spec
   ├─ app/build.gradle.kts          # openapi-generator 7.20.0 → Retrofit
   └─ picweight-release.keystore
```

---

## 13. Verification

- **Backend:** `cargo test` — Mifflin-St Jeor against published reference values, macro split, local-day bucketing across timezone/DST boundaries. Integration tests on a temp SQLite file with a mocked LLM endpoint covering ingest → loop → confirm, including idempotent replay of the same `client_uuid`. These run **inside the Docker build**, as in phos.
- **Agent harness:** replay fixtures — recorded photo + canned tool responses produce a deterministic result. Assert the `multi_turn(6)` cap holds and that `MaxDepthError` triggers the single-shot fallback rather than surfacing an error to the user.
- **Re-analysis:** given a seeded meal and the feedback "half the rice", assert a new revision is written, rice grams roughly halve, the prior revision is retained, and `recall_similar_meals` subsequently returns the *new* revision. Assert the continuation makes **no** repeat tool calls (the session already holds the results), and that a `prompt_version` mismatch triggers reseed rather than continuation.
- **Batch:** upload three photos under one `client_uuid`, assert one meal, one `analysis_job`, one SSE completion event and one notification. Replay a photo with a duplicate `photo_uuid` and assert it is not appended twice. Abandon a batch without `complete` and assert the sweeper finalizes it with the photos that arrived.
- **Accuracy harness — the one that matters:** order/prepare ~20 meals of *known* weight (kitchen scale), record agent estimate vs truth **with no comment supplied**, since that's the real usage mode. Re-run whenever prompts or model change; keep results in-repo.
- **Flywheel:** confirm a dish, submit it again, assert the second estimate is recall-sourced and within a few percent.
- **Auth:** a token from a second IdP works (proves generic OIDC); user A cannot read user B's meals; both the Android public client and web confidential client authenticate.
- **Android:** instrumented offline-queue test — airplane mode, three captures, reconnect, assert exactly three server-side meals. Verify the generated Retrofit client compiles against a freshly exported `openapi.json` (contract-drift guard).
- **Helm:** `helm template` renders cleanly with `additionalObjects` containing (a) a SealedSecret and (b) an ExternalSecret, proving neither backend is baked in; and with `additionalObjects: []` plus pre-existing Secrets. `helm lint` in CI.
- **Deploy:** a real ArgoCD sync comes up healthy; `/picweight.apk` downloads and installs on a real device.
- **Latency:** `analysis_jobs` logs steps and wall clock. Watch p95 — with a single post-loop notification (§6), the 25s cap *is* the user-facing latency budget. Drift to 40s breaks the feedback promise.

---

## 14. Open questions

1. **Recall poisoning — mitigated, not eliminated.** Correction-by-conversation (§5) is the repair mechanism: a poisoned dish is fixed by saying what's wrong, and recall reads only the latest revision. The residual risk is *noticing*. If you rubber-stamp a bad estimate and never look again, the wrong numbers stay authoritative and quietly shape every future estimate of that dish. Cheap partial defence: show recalled meals differently from fresh ones at confirm time, so "this came from your history" is visible rather than implicit. Worth deciding before M3.

2. **Will the zero-keyboard paths actually get used?** Recent-chips and the share sheet are the answer to "I won't type comments". If `name_source` shows everything still arriving as `vision`, the accuracy ceiling is set by container-scale heuristics alone. Instrumented in M4 precisely so this is measurable — check after two weeks.

3. **Does rig's OpenAI integration expose everything the loop needs** — **multiple** image inputs in one turn (batch capture), tool calling, strict JSON-schema structured output, and a **serializable/restorable message history** (session resume)? All four are load-bearing and the last two are the ones frameworks most often leave underspecified. Verify in a spike during M1, before M2 and M3 depend on them. The `agent/` module boundary keeps a fallback to hand-rolled `async-openai` cheap, but finding out early is much cheaper than finding out late.

4. **When does a batch stop being one meal?** Five dishes photographed at a restaurant are one sitting. But nothing stops a batch from being left open across a lunch and a dinner if `complete` is never tapped, and the 15-minute sweeper is a guess, not a principle. If drafts start finalizing mid-meal or spanning meals in real use, the timeout needs to become smarter (idle-since-last-photo, or a foreground prompt) rather than merely longer.

5. **Does the OIDC mobile flow work against Zitadel's native client on a homelab cert?** phos already solved this; port `auth.rs` rather than rediscovering it, but verify early — auth problems surface late and block all device testing.
