-- picweight initial schema (PRD §8).
--
-- Conventions
--   * Every id is a UUID v4 stored as TEXT.
--   * Datetimes are TIMESTAMP (SQLite TEXT under the hood) in UTC and map to
--     `chrono::NaiveDateTime` through diesel's `Timestamp` sql type. Per-meal
--     `timezone_offset` (minutes east of UTC) reconstructs the user's local day.
--   * Grams, kcal and macros are REAL and map to diesel `Double` (f64).
--   * Counters are INTEGER (diesel `Integer` / i32); token and cost counters are
--     INTEGER too but read as `BigInt` (i64) because they accumulate.
--   * String enums are TEXT with a CHECK constraint mirroring the Rust enums in
--     `src/models.rs`.
--   * Every user-scoped table carries `user_id` (or reaches it through exactly
--     one FK hop) so the repository layer can enforce isolation.

PRAGMA foreign_keys = ON;

-- ---------------------------------------------------------------------------
-- users
-- ---------------------------------------------------------------------------
CREATE TABLE users (
    id            TEXT PRIMARY KEY NOT NULL,
    oidc_sub      TEXT NOT NULL UNIQUE,
    oidc_issuer   TEXT NOT NULL,
    email         TEXT,
    display_name  TEXT,
    created_at    TIMESTAMP NOT NULL DEFAULT CURRENT_TIMESTAMP
);

-- Login path: resolve (issuer, sub) -> user on every authenticated request.
CREATE INDEX idx_users_issuer_sub ON users (oidc_issuer, oidc_sub);

-- ---------------------------------------------------------------------------
-- user_profiles — body data + the targets derived from it (Mifflin-St Jeor, §6)
-- ---------------------------------------------------------------------------
CREATE TABLE user_profiles (
    user_id             TEXT PRIMARY KEY NOT NULL
                        REFERENCES users (id) ON DELETE CASCADE,
    sex                 TEXT NOT NULL CHECK (sex IN ('male', 'female')),
    birth_date          DATE NOT NULL,
    height_cm           REAL NOT NULL,
    activity_factor     REAL NOT NULL DEFAULT 1.2,
    goal_type           TEXT NOT NULL DEFAULT 'maintain'
                        CHECK (goal_type IN ('lose', 'maintain', 'gain')),
    target_weight_kg    REAL,
    rate_kg_per_week    REAL,
    target_kcal         REAL,
    target_protein_g    REAL,
    target_fat_g        REAL,
    target_carbs_g      REAL,
    -- Learned from correction history; scales agent estimates (§5 hidden fat).
    calibration_factor  REAL NOT NULL DEFAULT 1.0,
    targets_computed_at TIMESTAMP,
    -- IANA zone name; the per-meal offset is what actually buckets days, this
    -- is the default applied when a client does not send one.
    timezone            TEXT NOT NULL DEFAULT 'UTC',
    created_at          TIMESTAMP NOT NULL DEFAULT CURRENT_TIMESTAMP,
    updated_at          TIMESTAMP NOT NULL DEFAULT CURRENT_TIMESTAMP
);

-- ---------------------------------------------------------------------------
-- weight_logs
-- ---------------------------------------------------------------------------
CREATE TABLE weight_logs (
    id         TEXT PRIMARY KEY NOT NULL,
    user_id    TEXT NOT NULL REFERENCES users (id) ON DELETE CASCADE,
    logged_at  TIMESTAMP NOT NULL,
    weight_kg  REAL NOT NULL,
    source     TEXT NOT NULL DEFAULT 'manual'
               CHECK (source IN ('manual', 'onboarding', 'import', 'scale')),
    created_at TIMESTAMP NOT NULL DEFAULT CURRENT_TIMESTAMP
);

-- "latest weight" (target recompute) and the weight-trend chart both read this.
CREATE INDEX idx_weight_logs_user_logged_at ON weight_logs (user_id, logged_at DESC);

-- ---------------------------------------------------------------------------
-- thumbnails — 768px content-addressed derivatives (§7). Originals are never
-- retained, so this row IS the photo.
-- ---------------------------------------------------------------------------
CREATE TABLE thumbnails (
    id         TEXT PRIMARY KEY NOT NULL,
    user_id    TEXT NOT NULL REFERENCES users (id) ON DELETE CASCADE,
    sha256     TEXT NOT NULL UNIQUE,
    -- Relative to <data_path>/thumbs: "<sha[0:2]>/<sha[2:4]>/<sha>.jpg".
    path       TEXT NOT NULL,
    width      INTEGER NOT NULL,
    height     INTEGER NOT NULL,
    bytes      INTEGER NOT NULL,
    created_at TIMESTAMP NOT NULL DEFAULT CURRENT_TIMESTAMP
);

CREATE INDEX idx_thumbnails_user ON thumbnails (user_id);

-- ---------------------------------------------------------------------------
-- notification_groups — one sitting = one notification (§5, §6).
-- Created lazily by the first photo of a sitting; `expected_size` is the
-- optional client hint that short-circuits the 90s idle debounce.
-- Declared before `meals` so the FK target exists.
-- ---------------------------------------------------------------------------
CREATE TABLE notification_groups (
    group_id      TEXT PRIMARY KEY NOT NULL,
    user_id       TEXT NOT NULL REFERENCES users (id) ON DELETE CASCADE,
    expected_size INTEGER,
    notified_at   TIMESTAMP,
    last_photo_at TIMESTAMP NOT NULL,
    created_at    TIMESTAMP NOT NULL DEFAULT CURRENT_TIMESTAMP
);

-- The settler scans un-notified groups ordered by idle time.
CREATE INDEX idx_notification_groups_pending
    ON notification_groups (notified_at, last_photo_at);
CREATE INDEX idx_notification_groups_user ON notification_groups (user_id);

-- ---------------------------------------------------------------------------
-- meals — one photo = one meal = one agent loop (§5).
-- ---------------------------------------------------------------------------
CREATE TABLE meals (
    id                   TEXT PRIMARY KEY NOT NULL,
    user_id              TEXT NOT NULL REFERENCES users (id) ON DELETE CASCADE,
    -- Idempotency key generated on the phone at capture. A retried upload after
    -- a flaky connection can never create a duplicate (§8 note).
    client_uuid          TEXT NOT NULL UNIQUE,
    thumbnail_id         TEXT REFERENCES thumbnails (id) ON DELETE SET NULL,
    -- Client-generated per sitting; display + notification grouping only.
    group_id             TEXT REFERENCES notification_groups (group_id) ON DELETE SET NULL,
    group_size           INTEGER,
    dish_name            TEXT,
    dish_name_normalized TEXT,
    name_source          TEXT NOT NULL DEFAULT 'vision'
                         CHECK (name_source IN
                                ('vision', 'recent_chip', 'share_intent', 'comment', 'manual')),
    user_comment         TEXT,
    revision             INTEGER NOT NULL DEFAULT 1,
    eaten_at             TIMESTAMP NOT NULL,
    -- Minutes east of UTC at capture time; "my day" is not UTC's day (§8 note).
    timezone_offset      INTEGER NOT NULL DEFAULT 0,
    meal_type            TEXT,
    status               TEXT NOT NULL DEFAULT 'pending'
                         CHECK (status IN
                                ('pending', 'analyzing', 'needs_review', 'confirmed', 'failed')),
    -- "ate 60% of it" — multiplies every item's grams and macros at read time.
    portion_scale        REAL NOT NULL DEFAULT 1.0,
    created_at           TIMESTAMP NOT NULL DEFAULT CURRENT_TIMESTAMP,
    updated_at           TIMESTAMP NOT NULL DEFAULT CURRENT_TIMESTAMP
);

-- The index the PRD names: `recall_similar_meals` lookups.
CREATE INDEX idx_meals_user_dish_normalized ON meals (user_id, dish_name_normalized);
-- Recall reads only *confirmed* meals, newest first — this is the covering shape.
CREATE INDEX idx_meals_recall
    ON meals (user_id, status, dish_name_normalized, eaten_at DESC);
-- Day view / history bucketing.
CREATE INDEX idx_meals_user_eaten_at ON meals (user_id, eaten_at DESC);
-- Recent-dish chips: last N confirmed dishes.
CREATE INDEX idx_meals_user_status_eaten ON meals (user_id, status, eaten_at DESC);
-- Group collapse + settle checks.
CREATE INDEX idx_meals_group ON meals (group_id, created_at);
-- Worker startup re-queue: find meals stuck in pending/analyzing.
CREATE INDEX idx_meals_status_created ON meals (status, created_at);
CREATE INDEX idx_meals_thumbnail ON meals (thumbnail_id);

-- ---------------------------------------------------------------------------
-- meal_items — per revision. A re-analysis writes a new revision; prior
-- revisions are retained so the correction chain stays auditable (§5).
-- ---------------------------------------------------------------------------
CREATE TABLE meal_items (
    id             TEXT PRIMARY KEY NOT NULL,
    meal_id        TEXT NOT NULL REFERENCES meals (id) ON DELETE CASCADE,
    revision       INTEGER NOT NULL DEFAULT 1,
    position       INTEGER NOT NULL DEFAULT 0,
    name           TEXT NOT NULL,
    barcode        TEXT,
    grams          REAL NOT NULL,
    grams_source   TEXT NOT NULL DEFAULT 'agent'
                   CHECK (grams_source IN ('agent', 'user', 'barcode', 'recall')),
    kcal           REAL NOT NULL,
    protein_g      REAL NOT NULL,
    fat_g          REAL NOT NULL,
    carbs_g        REAL NOT NULL,
    macro_source   TEXT NOT NULL DEFAULT 'model'
                   CHECK (macro_source IN ('recall', 'model', 'barcode', 'web', 'user')),
    confidence     REAL,
    -- One line of "why" per item, surfaced in the confirm screen (§5 step 5).
    reasoning_note TEXT,
    created_at     TIMESTAMP NOT NULL DEFAULT CURRENT_TIMESTAMP
);

-- The only read pattern: "items of meal X at revision R, in display order".
CREATE UNIQUE INDEX idx_meal_items_meal_revision_position
    ON meal_items (meal_id, revision, position);
CREATE INDEX idx_meal_items_barcode ON meal_items (barcode);

-- ---------------------------------------------------------------------------
-- item_corrections — every correction, typed or dragged (§5).
-- ---------------------------------------------------------------------------
CREATE TABLE item_corrections (
    id              TEXT PRIMARY KEY NOT NULL,
    meal_item_id    TEXT NOT NULL REFERENCES meal_items (id) ON DELETE CASCADE,
    field           TEXT NOT NULL,
    original_value  TEXT,
    corrected_value TEXT,
    corrected_at    TIMESTAMP NOT NULL DEFAULT CURRENT_TIMESTAMP
);

CREATE INDEX idx_item_corrections_item ON item_corrections (meal_item_id);
-- calibration_factor is learned by scanning corrections over time.
CREATE INDEX idx_item_corrections_corrected_at ON item_corrections (corrected_at);

-- ---------------------------------------------------------------------------
-- foods — barcode / Open Food Facts cache ONLY. No dish-level seeding: БЖУ for
-- prepared dishes comes from the model by design (§1.3).
-- ---------------------------------------------------------------------------
CREATE TABLE foods (
    id              TEXT PRIMARY KEY NOT NULL,
    source          TEXT NOT NULL DEFAULT 'openfoodfacts',
    source_ref      TEXT NOT NULL,
    name            TEXT NOT NULL,
    name_normalized TEXT NOT NULL,
    brand           TEXT,
    barcode         TEXT,
    kcal_100g       REAL,
    protein_100g    REAL,
    fat_100g        REAL,
    carbs_100g      REAL,
    fetched_at      TIMESTAMP NOT NULL DEFAULT CURRENT_TIMESTAMP
);

CREATE UNIQUE INDEX idx_foods_source_ref ON foods (source, source_ref);
-- Partial unique: a barcode resolves to exactly one cached food.
CREATE UNIQUE INDEX idx_foods_barcode ON foods (barcode) WHERE barcode IS NOT NULL;
CREATE INDEX idx_foods_name_normalized ON foods (name_normalized);

-- ---------------------------------------------------------------------------
-- analysis_jobs — one row per agent run. `parent_job_id` + `user_feedback`
-- make the correction chain readable end to end (§8 note).
-- ---------------------------------------------------------------------------
CREATE TABLE analysis_jobs (
    id                TEXT PRIMARY KEY NOT NULL,
    meal_id           TEXT NOT NULL REFERENCES meals (id) ON DELETE CASCADE,
    revision          INTEGER NOT NULL DEFAULT 1,
    parent_job_id     TEXT REFERENCES analysis_jobs (id) ON DELETE SET NULL,
    status            TEXT NOT NULL DEFAULT 'queued'
                      CHECK (status IN ('queued', 'running', 'succeeded', 'failed', 'cancelled')),
    attempts          INTEGER NOT NULL DEFAULT 0,
    model             TEXT NOT NULL,
    user_feedback     TEXT,
    -- Model calls made (rig's `max_turns` unit), not tool calls — see
    -- docs/rig-spike.md "Semantic gotcha".
    steps             INTEGER NOT NULL DEFAULT 0,
    tool_calls        INTEGER NOT NULL DEFAULT 0,
    prompt_tokens     INTEGER NOT NULL DEFAULT 0,
    completion_tokens INTEGER NOT NULL DEFAULT 0,
    cost_micro_usd    INTEGER NOT NULL DEFAULT 0,
    -- Set loudly on 429/quota exhaustion; the meal goes to `failed`, never
    -- silently stalls (§5 bounds).
    error             TEXT,
    -- Not in the PRD's column list, but §13 asks for p95 wall clock and
    -- created_at→finished_at includes queue time. started_at separates them.
    started_at        TIMESTAMP,
    created_at        TIMESTAMP NOT NULL DEFAULT CURRENT_TIMESTAMP,
    finished_at       TIMESTAMP
);

CREATE INDEX idx_analysis_jobs_meal_revision ON analysis_jobs (meal_id, revision DESC);
CREATE INDEX idx_analysis_jobs_status_created ON analysis_jobs (status, created_at);
CREATE INDEX idx_analysis_jobs_parent ON analysis_jobs (parent_job_id);

-- ---------------------------------------------------------------------------
-- agent_steps — one row per tool call, written from rig's `AgentHook`
-- ToolCall/ToolResult events. Without this the loop is unimprovable (§8 note).
-- ---------------------------------------------------------------------------
CREATE TABLE agent_steps (
    id          TEXT PRIMARY KEY NOT NULL,
    job_id      TEXT NOT NULL REFERENCES analysis_jobs (id) ON DELETE CASCADE,
    step_no     INTEGER NOT NULL,
    tool_name   TEXT NOT NULL,
    tool_input  TEXT,
    tool_output TEXT,
    latency_ms  INTEGER,
    created_at  TIMESTAMP NOT NULL DEFAULT CURRENT_TIMESTAMP
);

CREATE UNIQUE INDEX idx_agent_steps_job_step ON agent_steps (job_id, step_no);

-- ---------------------------------------------------------------------------
-- agent_sessions — the serialized rig message thread, resumed (not restarted)
-- on re-analysis. `prompt_version` + `model` decide continue-vs-reseed;
-- `turn_count` drives the history cap (§5).
-- ---------------------------------------------------------------------------
CREATE TABLE agent_sessions (
    id             TEXT PRIMARY KEY NOT NULL,
    meal_id        TEXT NOT NULL UNIQUE REFERENCES meals (id) ON DELETE CASCADE,
    -- JSON array of `rig::completion::Message`.
    messages       TEXT NOT NULL,
    model          TEXT NOT NULL,
    prompt_version TEXT NOT NULL,
    turn_count     INTEGER NOT NULL DEFAULT 0,
    created_at     TIMESTAMP NOT NULL DEFAULT CURRENT_TIMESTAMP,
    updated_at     TIMESTAMP NOT NULL DEFAULT CURRENT_TIMESTAMP
);
