//! Diesel table definitions.
//!
//! Hand-written to match `migrations/2026-08-01-000000_init/up.sql` exactly.
//! The diesel CLI is not a build dependency of this project; if you install it
//! (`cargo install diesel_cli --no-default-features --features sqlite`) you can
//! regenerate this file with `diesel print-schema > src/schema.rs` from
//! `backend/` — `diesel.toml` already points at this path.
//!
//! Type mapping notes:
//!   * `TEXT` ids -> `Text` (UUID v4 strings).
//!   * `TIMESTAMP` -> `Timestamp` (`chrono::NaiveDateTime`, always UTC).
//!   * `DATE` -> `Date` (`chrono::NaiveDate`).
//!   * `REAL` -> `Double` (`f64`) everywhere; grams and macros deserve the
//!     precision and SQLite stores REAL as an 8-byte float regardless.
//!   * `INTEGER` -> `Integer` (`i32`) for counters, `BigInt` (`i64`) for the
//!     accumulating token/cost columns on `analysis_jobs` and for byte counts.

// @generated-equivalent: `diesel print-schema` (see module docs).

diesel::table! {
    /// One row per authenticated OIDC identity.
    users (id) {
        id -> Text,
        oidc_sub -> Text,
        oidc_issuer -> Text,
        email -> Nullable<Text>,
        display_name -> Nullable<Text>,
        created_at -> Timestamp,
    }
}

diesel::table! {
    /// Body data plus the Mifflin-St Jeor targets derived from it (PRD §6).
    user_profiles (user_id) {
        user_id -> Text,
        sex -> Text,
        birth_date -> Date,
        height_cm -> Double,
        activity_factor -> Double,
        goal_type -> Text,
        target_weight_kg -> Nullable<Double>,
        rate_kg_per_week -> Nullable<Double>,
        target_kcal -> Nullable<Double>,
        target_protein_g -> Nullable<Double>,
        target_fat_g -> Nullable<Double>,
        target_carbs_g -> Nullable<Double>,
        calibration_factor -> Double,
        targets_computed_at -> Nullable<Timestamp>,
        timezone -> Text,
        created_at -> Timestamp,
        updated_at -> Timestamp,
    }
}

diesel::table! {
    /// Weight history; the newest row feeds target recomputation.
    weight_logs (id) {
        id -> Text,
        user_id -> Text,
        logged_at -> Timestamp,
        weight_kg -> Double,
        source -> Text,
        created_at -> Timestamp,
    }
}

diesel::table! {
    /// 768px content-addressed derivative. Originals are never retained (§7).
    thumbnails (id) {
        id -> Text,
        user_id -> Text,
        sha256 -> Text,
        path -> Text,
        width -> Integer,
        height -> Integer,
        bytes -> BigInt,
        created_at -> Timestamp,
    }
}

diesel::table! {
    /// Settle state for one sitting; drives the single grouped notification.
    notification_groups (group_id) {
        group_id -> Text,
        user_id -> Text,
        expected_size -> Nullable<Integer>,
        notified_at -> Nullable<Timestamp>,
        last_photo_at -> Timestamp,
        created_at -> Timestamp,
    }
}

diesel::table! {
    /// One photo = one meal = one agent loop (§5).
    meals (id) {
        id -> Text,
        user_id -> Text,
        client_uuid -> Text,
        thumbnail_id -> Nullable<Text>,
        group_id -> Nullable<Text>,
        group_size -> Nullable<Integer>,
        dish_name -> Nullable<Text>,
        dish_name_normalized -> Nullable<Text>,
        name_source -> Text,
        user_comment -> Nullable<Text>,
        revision -> Integer,
        eaten_at -> Timestamp,
        timezone_offset -> Integer,
        meal_type -> Nullable<Text>,
        status -> Text,
        portion_scale -> Double,
        created_at -> Timestamp,
        updated_at -> Timestamp,
    }
}

diesel::table! {
    /// Per-item estimate, scoped to a meal revision.
    meal_items (id) {
        id -> Text,
        meal_id -> Text,
        revision -> Integer,
        position -> Integer,
        name -> Text,
        barcode -> Nullable<Text>,
        grams -> Double,
        grams_source -> Text,
        kcal -> Double,
        protein_g -> Double,
        fat_g -> Double,
        carbs_g -> Double,
        macro_source -> Text,
        confidence -> Nullable<Double>,
        reasoning_note -> Nullable<Text>,
        created_at -> Timestamp,
    }
}

diesel::table! {
    /// Audit trail of every user correction, typed or dragged.
    item_corrections (id) {
        id -> Text,
        meal_item_id -> Text,
        field -> Text,
        original_value -> Nullable<Text>,
        corrected_value -> Nullable<Text>,
        corrected_at -> Timestamp,
    }
}

diesel::table! {
    /// Barcode / Open Food Facts cache only — no dish-level seeding (§1.3).
    foods (id) {
        id -> Text,
        source -> Text,
        source_ref -> Text,
        name -> Text,
        name_normalized -> Text,
        brand -> Nullable<Text>,
        barcode -> Nullable<Text>,
        kcal_100g -> Nullable<Double>,
        protein_100g -> Nullable<Double>,
        fat_100g -> Nullable<Double>,
        carbs_100g -> Nullable<Double>,
        fetched_at -> Timestamp,
    }
}

diesel::table! {
    /// One row per agent run, including re-analysis continuations.
    analysis_jobs (id) {
        id -> Text,
        meal_id -> Text,
        revision -> Integer,
        parent_job_id -> Nullable<Text>,
        status -> Text,
        attempts -> Integer,
        model -> Text,
        user_feedback -> Nullable<Text>,
        steps -> Integer,
        tool_calls -> Integer,
        prompt_tokens -> BigInt,
        completion_tokens -> BigInt,
        cost_micro_usd -> BigInt,
        error -> Nullable<Text>,
        started_at -> Nullable<Timestamp>,
        created_at -> Timestamp,
        finished_at -> Nullable<Timestamp>,
    }
}

diesel::table! {
    /// One row per tool call, written from rig's `AgentHook` events.
    agent_steps (id) {
        id -> Text,
        job_id -> Text,
        step_no -> Integer,
        tool_name -> Text,
        tool_input -> Nullable<Text>,
        tool_output -> Nullable<Text>,
        latency_ms -> Nullable<BigInt>,
        created_at -> Timestamp,
    }
}

diesel::table! {
    /// Serialized rig message thread, resumed on re-analysis (§5).
    agent_sessions (id) {
        id -> Text,
        meal_id -> Text,
        messages -> Text,
        model -> Text,
        prompt_version -> Text,
        turn_count -> Integer,
        created_at -> Timestamp,
        updated_at -> Timestamp,
    }
}

diesel::joinable!(user_profiles -> users (user_id));
diesel::joinable!(weight_logs -> users (user_id));
diesel::joinable!(thumbnails -> users (user_id));
diesel::joinable!(notification_groups -> users (user_id));
diesel::joinable!(meals -> users (user_id));
diesel::joinable!(meals -> thumbnails (thumbnail_id));
diesel::joinable!(meals -> notification_groups (group_id));
diesel::joinable!(meal_items -> meals (meal_id));
diesel::joinable!(item_corrections -> meal_items (meal_item_id));
diesel::joinable!(analysis_jobs -> meals (meal_id));
diesel::joinable!(agent_steps -> analysis_jobs (job_id));
diesel::joinable!(agent_sessions -> meals (meal_id));

diesel::allow_tables_to_appear_in_same_query!(
    agent_sessions,
    agent_steps,
    analysis_jobs,
    foods,
    item_corrections,
    meal_items,
    meals,
    notification_groups,
    thumbnails,
    user_profiles,
    users,
    weight_logs,
);
