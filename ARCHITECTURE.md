# picweight Architecture

picweight turns a photograph of a meal into calories and macros, and tells you
what budget is left today. It is a single container: a Rust backend that serves
its own SPA and ships its own APK.

The design rationale lives in [`docs/PRD.md`](docs/PRD.md); this document
describes what was actually built and why the pieces sit where they do.

## Core principles

- **The friction budget is ten seconds.** Everything else — accuracy mechanisms,
  storage layout, notification timing — is subordinate to keeping capture fast
  enough to sustain five times a day.
- **Recall from the user's own confirmed history is the primary accuracy
  mechanism.** The dominant food source is delivery; no nutrition database has
  an entry for "шаурма из Фарша" and never will. A dish the user confirmed once
  is ground truth they already vetted.
- **БЖУ comes from the model, not a database.** Portion grams is where nearly all
  the error lives; once the model is estimating that, the marginal error from
  also estimating per-100g macros is small. Dish-matching would add a hard
  problem to fix the smaller half of the error.
- **The number you should eat is arithmetic.** Mifflin-St Jeor, offline,
  auditable. The LLM estimates what you *ate*; it never computes what you
  *should*.
- **Analysis is never inline.** Upload returns `202`; a worker runs the loop;
  the result arrives over SSE. The phone must never block on a 25-second call.
- **The OpenAI key lives only in the backend.** Clients never talk to OpenAI.

## Shape

```
┌─────────────────┐        ┌──────────────────┐
│ Android (Kotlin)│        │ Vue 3 SPA        │
│ Compose, Hilt   │        │ Vite + Tailwind4 │
│ Retrofit (gen'd │        │ shadcn-vue       │
│  from openapi)  │        │ history / stats  │
│ Room + WorkMgr  │        │ APK download card│
│ CameraX, MLKit  │        │                  │
└────────┬────────┘        └────────┬─────────┘
    OIDC native (public)      OIDC web (confidential)
         │  Bearer JWT              │  session JWT
         └───────────┬──────────────┘
                     ▼
    ┌────────────────────────────────────┐
    │ Rust backend (axum 0.8) :33100     │
    │  /healthz          no auth         │
    │  /api/auth/*       no auth         │
    │  /api/v1/*         require_auth    │
    │  /api/docs         Scalar          │
    │  /api/**           JSON 404        │
    │  /*                ServeDir → SPA  │
    │  /picweight.apk    bundled APK     │
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

## Backend

Rust, axum 0.8 + tokio, diesel 2 (`sqlite`, `r2d2`) with `diesel_migrations`,
WAL and a busy timeout. `utoipa` + `utoipa-scalar` document every route and
export the spec the Android client is generated from.

`build_router` lives in `lib.rs`, not `main.rs`, so the integration suite
exercises the same wiring the binary serves. `main.rs` owns process concerns
only: configuration, logging, the pool, the OIDC handshake, the workers and the
listener.

### Module map

| Path | Role |
|---|---|
| `main.rs` | Process entry; also `picweight-backend openapi <path>`, which runs with no configuration |
| `lib.rs` | `AppState`, `build_router`, the route table |
| `config.rs` | Every `PICWEIGHT_*` variable. Nothing else reads `std::env` |
| `auth.rs` | `openidconnect` discovery, JWKS cache, web + mobile clients, session JWTs |
| `db.rs`, `models.rs`, `schema.rs` | Pool, embedded migrations, diesel rows, the `TEXT`-column enums |
| `error.rs` | `AppError` → the single `ErrorBody` JSON shape every failure uses |
| `api/` | One module per resource group; each user-scoped query filters `user_id` |
| `agent/` | The rig loop. **The swap boundary** — replacing rig touches only this directory |
| `jobs/analyzer.rs` | One job → one agent loop → one persisted revision |
| `jobs/group_settler.rs` | `group_size` hint + 90s idle debounce → exactly one notification per sitting |
| `food/openfoodfacts.rs` | Barcode lookups, cached in `foods`. No dish-level seeding |
| `nutrition/targets.rs` | Mifflin-St Jeor, macro floors, the too-steep-deficit warning |
| `feedback/state.rs` | Rules engine: consumed / remaining / protein gap / on-track \| tight \| over |
| `feedback/phrasing.rs` | The one-line verdict. LLM-phrased, with a templated fallback that never hard-fails |
| `storage/thumbs.rs` | 768px JPEG q75, `thumbs/<sha[0:2]>/<sha[2:4]>/<sha>.jpg` |

### The estimation agent (`agent/`)

Built on the **`rig` 0.41 facade**, not `rig-core`: 0.41 split the run loop out
of the core crate, so `Agent` / `AgentRunner` only exist behind the facade
(see [`docs/rig-spike.md`](docs/rig-spike.md)).

The loop is *identify → recall → estimate → critique → emit*. The critique step
is a fixed checklist — restaurant food carries more oil than the home recipe;
did you count the rice under the curry, the dressing, the sauce; is the portion
consistent with the container you identified? With no comment and no coin on the
plate, the **container** is the only reliable scale reference in the frame.

Bounds are a library guarantee rather than something reimplemented:
`default_max_turns(6)`. Note the unit — `max_turns` counts **model calls**, not
tool calls, so a two-tool run costs three. Exceeding it surfaces
`PromptError::MaxTurnsError`, which is the trigger for the single-shot fallback:
on cap breach, tool failure or timeout the user still gets a draft.

Tools are `recall_similar_meals` (always called first; a confident hit
short-circuits the loop), `lookup_barcode` (Open Food Facts, exact), and
`web_search` (config-gated, off by default). `lookup_barcode` returns a *miss*
rather than an error for an unknown code — a hard tool failure would burn a turn
from the budget. There is deliberately no dish-matching tool.

`AgentHook` fires on `ToolCall` / `ToolResult`, which is what populates
`agent_steps` — a row per step, no stream parsing. Without it a bad estimate is
undebuggable and the loop is unimprovable.

### Correction by conversation

`rig_core::completion::Message` derives `Serialize` + `Deserialize` and
`AgentRunner::history()` accepts a restored thread, so the whole feature is
`serde_json` in both directions. On job completion the message thread is written
to `agent_sessions.messages`; `POST /meals/:id/reanalyze { feedback }` loads it,
appends the feedback as a user turn, and **continues** the same conversation.

Continuation rather than a fresh call with a summary, because the agent keeps
*why* it said 780 kcal — which container it identified, what recall returned,
what it already ruled out. Tool results are already in context, so correcting a
dish costs one short turn rather than another full loop.

Three caveats are handled rather than discovered:

- **Context grows per revision.** Stored history is capped, dropping the oldest
  tool-result payloads first — bulkiest, least useful on a correction turn.
- **A session pins the prompt and model it began with.** `prompt_version` +
  `model` are stored on the session; when either differs materially the job
  starts a *fresh* session seeded with the last confirmed result instead of
  continuing under stale instructions, and records which path it took.
- **Images are re-attached from disk** on every turn — provider message formats
  vary on image retention, so serialized history is not trusted to carry them.
  This is why thumbnails are 768px and not 320px: the display asset *is* the
  re-analysis input.

### Multi-dish sittings

Each photo gets its own independent loop. The client generates a `group_id` at
the start of a sitting and stamps it on every upload; grouping is a display and
notification concern, never an analysis one.

This is the better design, not merely the simpler one: latency is one loop
rather than N because the jobs run concurrently; each dish enters the recall
corpus as its own dish rather than buried in a five-dish sitting; corrections
resume *that dish's* session without touching the others; one timed-out analysis
does not sink the sitting. A solo meal is a group of one — the only case, not a
special case.

`notification_groups` tracks settle state. A group settles when it has no
in-flight jobs and no new photo has arrived for 90s; the optional `group_size`
hint short-circuits the debounce the moment all N are terminal, which makes the
common case instant. The debounce remains the fallback for a photo that never
arrives, so a group can never hang un-notified. A failed member does not hold
the sitting.

*Accepted cost:* the loops cannot see each other, so a shared side dish
photographed twice from two angles is counted twice. The collapsed group view
makes that visible; deleting the duplicate is two taps.

## Data

SQLite, one file, WAL. Schema in [`docs/PRD.md`](docs/PRD.md) §8. The load-bearing
decisions:

- **`meals.client_uuid` is the idempotency key**, generated on the phone at
  capture. A retried upload after a flaky connection can never create a
  duplicate. Single most important field for offline correctness.
- **Recall reads only the latest revision of `confirmed` meals** — never drafts,
  never superseded revisions. Otherwise the agent learns from its own
  hallucinations and from corrections the user already rejected. Index on
  `(user_id, dish_name_normalized)`.
- **`revision` on meals / meal_items / analysis_jobs.** Re-analysis writes a new
  revision; prior ones are retained. `parent_job_id` + `user_feedback` make the
  chain auditable — you can read exactly what the user said and what changed.
- **`timezone_offset` per meal.** "What did I eat today" is a question about
  *your* day, not UTC's.
- **`name_source`** measures which input path actually gets used — the only way
  to find out whether recent-chips and the share sheet were worth building.
- **Every user-scoped query filters `user_id`**, enforced at the repository
  layer. Another user's meal is a 404, not a 403.

### Photo storage

Originals are not retained. On ingest the upload goes to a temp file; once
analysis completes one derivative is stored and the original deleted:
768px long edge, JPEG q75, ~60–100KB, content-addressed at
`thumbs/<sha256[0:2]>/<sha256[2:4]>/<sha256>.jpg`.

768 rather than 320 because vision models downscale to roughly this range
anyway, so the thumbnail is simultaneously the display asset **and** a valid
re-analysis input. A display-only thumbnail would break correction-by-
conversation, not merely foreclose a future feature.

## Clients

**Vue 3 SPA** — Vite, Tailwind 4, shadcn-vue/reka-ui, lucide. History with
thumbnails, weight and macro trends, profile and target editing, an
agent-reasoning inspector with revision history, re-analysis input, CSV/JSON
export, and the APK download card. That card probes `HEAD /picweight.apk` and
checks the `content-type`, because a static fallback that answers with
`index.html` would otherwise look like a successful probe.

**Android** — Compose, Hilt, Retrofit generated from `android/openapi.json`,
CameraX capture, ML Kit barcode scanning (on-device, offline, free, exact).
Room is the local source of truth and the UI never blocks on network; WorkManager
owns the upload queue with exponential backoff. AppAuth + PKCE against the
native/public client, refresh tokens in EncryptedSharedPreferences. Capture-screen
affordances aim at zero keyboard use: recent-dish chips, a `text/plain`
share-sheet filter for delivery apps, "add another" for sittings.

## Deployment

One image, four build stages: the SPA (`node:25-slim`), the backend
(`rust:1.96` + cargo-chef, with `cargo test --release` running **inside** the
build so a red test fails the image), the APK (`eclipse-temurin:17-jdk` +
Android SDK 36), and a `debian:trixie-slim` runtime that needs only `libssl3`
and `ca-certificates` — SQLite is compiled into the binary and there is no ONNX
or ffmpeg to link against.

The Android stage overwrites `android/openapi.json` with the spec exported from
the binary the same image ships, so an APK whose API client disagrees with its
server is structurally impossible.

The Helm chart is deliberately **secrets-backend agnostic** (goal G8): it never
generates a Secret and never assumes an operator. `oidc.existingSecret` and
`openai.existingSecret` take *names* of existing Secrets consumed via
`secretKeyRef`, and `additionalObjects` renders arbitrary manifests through
`tpl` so a user's SealedSecret or ExternalSecret can reference chart-generated
names. CI proves this by rendering the same chart against a SealedSecret, an
ExternalSecret and plain pre-created Secrets, and by asserting no chart template
ever emits a Secret of its own.

Resources are 100m/256Mi requested, 1Gi limit — this is an I/O-bound API server
whose latency is network round-trips, not an inference box. The PVC is 5Gi
against a two-year projection of 350MB.

## Known limitations

- **Hidden fat is invisible to a camera.** Restaurant pad thai can carry 40g of
  oil nothing in the photo reveals. Mitigated by the critique step's
  restaurant-oil prior, a "cooked with" quick-chip row, and a per-user
  `calibration_factor` learned from correction history.
- **Recall poisoning is mitigated, not eliminated.** A poisoned dish is repaired
  by saying what is wrong, and recall reads only the latest revision — but if you
  rubber-stamp a bad estimate and never look again, the wrong numbers stay
  authoritative.
- **Double-counting across a sitting**, as above. Deliberately not fixed with a
  dedup pass, which would reintroduce the cross-photo coupling that
  one-loop-per-photo removed.
