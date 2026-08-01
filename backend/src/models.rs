//! Diesel row structs and the string enums stored in `TEXT` columns.
//!
//! Every table gets a `Queryable + Selectable` read struct, a `New*`
//! `Insertable` and — where anything is ever updated — a `*Changeset`
//! `AsChangeset`. Field order matches the column order in `crate::schema` so
//! plain `Queryable` loads stay correct.
//!
//! The `TEXT` enums (`MealStatus`, `NameSource`, `GramsSource`, `MacroSource`,
//! `Sex`, `GoalType`, `JobStatus`, `WeightSource`) are modelled as Rust enums
//! with `as_str()` / `from_str()` and are stored as their snake_case string
//! form — the same strings the migration's `CHECK` constraints enforce.

use crate::error::AppError;
use crate::schema::*;
use chrono::{NaiveDate, NaiveDateTime};
use diesel::prelude::*;
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

// ---------------------------------------------------------------------------
// String enums
// ---------------------------------------------------------------------------

/// Generates `as_str`, `from_str`, `FromStr`, `Display` and `ALL` for a plain
/// C-like enum whose database representation is a fixed string.
macro_rules! text_enum {
    (
        $(#[$meta:meta])*
        $vis:vis enum $name:ident {
            $( $(#[$vmeta:meta])* $variant:ident => $text:literal ),+ $(,)?
        }
    ) => {
        $(#[$meta])*
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, ToSchema)]
        $vis enum $name {
            $( $(#[$vmeta])* #[serde(rename = $text)] $variant ),+
        }

        impl $name {
            /// Every variant, in declaration order.
            pub const ALL: &'static [$name] = &[ $( $name::$variant ),+ ];

            /// The exact string stored in the database.
            pub fn as_str(&self) -> &'static str {
                match self {
                    $( $name::$variant => $text ),+
                }
            }

            /// Parse the database representation.
            ///
            /// Returns [`AppError::Internal`] for an unknown value: the
            /// migration's `CHECK` constraint means an unknown string can only
            /// come from a schema/code mismatch, never from user input.
            ///
            /// This is deliberately an inherent method *as well as* a
            /// [`std::str::FromStr`] impl (below), so call sites do not have to
            /// import the trait or turbofish the target type. Both have
            /// identical semantics — the clippy lint's "can be confused for"
            /// concern does not apply.
            #[allow(clippy::should_implement_trait)]
            pub fn from_str(s: &str) -> Result<Self, AppError> {
                match s {
                    $( $text => Ok($name::$variant), )+
                    other => Err(AppError::Internal(format!(
                        concat!("unknown ", stringify!($name), " value in database: {}"),
                        other
                    ))),
                }
            }
        }

        impl std::str::FromStr for $name {
            type Err = AppError;
            fn from_str(s: &str) -> Result<Self, Self::Err> {
                $name::from_str(s)
            }
        }

        impl std::fmt::Display for $name {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.write_str(self.as_str())
            }
        }

        impl From<$name> for String {
            fn from(v: $name) -> String {
                v.as_str().to_string()
            }
        }
    };
}

text_enum! {
    /// Lifecycle of a meal. `pending`/`analyzing` are in-flight, `needs_review`
    /// means the agent produced a draft the user has not confirmed, `confirmed`
    /// is the only status `recall_similar_meals` reads from, `failed` is loud.
    pub enum MealStatus {
        Pending => "pending",
        Analyzing => "analyzing",
        NeedsReview => "needs_review",
        Confirmed => "confirmed",
        Failed => "failed",
    }
}

text_enum! {
    /// Which input path supplied the dish name. Instrumented in M4 to answer
    /// PRD §14.2 — whether the zero-keyboard paths actually get used.
    pub enum NameSource {
        Vision => "vision",
        RecentChip => "recent_chip",
        ShareIntent => "share_intent",
        Comment => "comment",
        Manual => "manual",
    }
}

text_enum! {
    /// Where an item's gram figure came from.
    pub enum GramsSource {
        Agent => "agent",
        User => "user",
        Barcode => "barcode",
        Recall => "recall",
    }
}

text_enum! {
    /// Where an item's macros came from.
    pub enum MacroSource {
        Recall => "recall",
        Model => "model",
        Barcode => "barcode",
        Web => "web",
        User => "user",
    }
}

text_enum! {
    /// Biological sex, as required by the Mifflin-St Jeor BMR formula (§6).
    pub enum Sex {
        Male => "male",
        Female => "female",
    }
}

text_enum! {
    /// Direction of the calorie goal.
    pub enum GoalType {
        Lose => "lose",
        Maintain => "maintain",
        Gain => "gain",
    }
}

text_enum! {
    /// Lifecycle of one `analysis_jobs` row.
    pub enum JobStatus {
        Queued => "queued",
        Running => "running",
        Succeeded => "succeeded",
        Failed => "failed",
        Cancelled => "cancelled",
    }
}

text_enum! {
    /// How a weight reading was captured.
    pub enum WeightSource {
        Manual => "manual",
        Onboarding => "onboarding",
        Import => "import",
        Scale => "scale",
    }
}

/// Normalize a dish name for the `dish_name_normalized` column and for
/// `recall_similar_meals` matching.
///
/// Lowercases, strips punctuation and diacritic-free ASCII noise, and collapses
/// whitespace. Deliberately Unicode-preserving: the dominant food source is
/// Russian-language delivery ("шаурма из Фарша"), so stripping non-ASCII would
/// destroy the key.
pub fn normalize_dish_name(name: &str) -> String {
    let mut out = String::with_capacity(name.len());
    let mut pending_space = false;
    for ch in name.trim().chars() {
        if ch.is_whitespace() || ch == '_' || ch == '-' {
            pending_space = !out.is_empty();
        } else if ch.is_alphanumeric() {
            if pending_space {
                out.push(' ');
                pending_space = false;
            }
            out.extend(ch.to_lowercase());
        }
        // Everything else (punctuation, symbols) is dropped outright.
    }
    out
}

// ---------------------------------------------------------------------------
// users
// ---------------------------------------------------------------------------

/// A row of `users`.
#[derive(Debug, Clone, Queryable, Selectable, Identifiable, Serialize)]
#[diesel(table_name = users)]
#[diesel(check_for_backend(diesel::sqlite::Sqlite))]
pub struct User {
    pub id: String,
    pub oidc_sub: String,
    pub oidc_issuer: String,
    pub email: Option<String>,
    pub display_name: Option<String>,
    pub created_at: NaiveDateTime,
}

/// Insert shape for `users`.
#[derive(Debug, Clone, Insertable)]
#[diesel(table_name = users)]
pub struct NewUser {
    pub id: String,
    pub oidc_sub: String,
    pub oidc_issuer: String,
    pub email: Option<String>,
    pub display_name: Option<String>,
    pub created_at: NaiveDateTime,
}

/// Update shape for `users` (identity fields refreshed from the ID token).
#[derive(Debug, Clone, Default, AsChangeset)]
#[diesel(table_name = users)]
pub struct UserChangeset {
    pub oidc_issuer: Option<String>,
    pub email: Option<Option<String>>,
    pub display_name: Option<Option<String>>,
}

// ---------------------------------------------------------------------------
// user_profiles
// ---------------------------------------------------------------------------

/// A row of `user_profiles`.
#[derive(Debug, Clone, Queryable, Selectable, Identifiable)]
#[diesel(table_name = user_profiles, primary_key(user_id))]
#[diesel(check_for_backend(diesel::sqlite::Sqlite))]
pub struct UserProfile {
    pub user_id: String,
    pub sex: String,
    pub birth_date: NaiveDate,
    pub height_cm: f64,
    pub activity_factor: f64,
    pub goal_type: String,
    pub target_weight_kg: Option<f64>,
    pub rate_kg_per_week: Option<f64>,
    pub target_kcal: Option<f64>,
    pub target_protein_g: Option<f64>,
    pub target_fat_g: Option<f64>,
    pub target_carbs_g: Option<f64>,
    pub calibration_factor: f64,
    pub targets_computed_at: Option<NaiveDateTime>,
    pub timezone: String,
    pub created_at: NaiveDateTime,
    pub updated_at: NaiveDateTime,
}

impl UserProfile {
    /// Typed accessor for the `sex` column.
    pub fn sex(&self) -> Result<Sex, AppError> {
        Sex::from_str(&self.sex)
    }

    /// Typed accessor for the `goal_type` column.
    pub fn goal_type(&self) -> Result<GoalType, AppError> {
        GoalType::from_str(&self.goal_type)
    }
}

/// Insert shape for `user_profiles`.
#[derive(Debug, Clone, Insertable)]
#[diesel(table_name = user_profiles)]
pub struct NewUserProfile {
    pub user_id: String,
    pub sex: String,
    pub birth_date: NaiveDate,
    pub height_cm: f64,
    pub activity_factor: f64,
    pub goal_type: String,
    pub target_weight_kg: Option<f64>,
    pub rate_kg_per_week: Option<f64>,
    pub target_kcal: Option<f64>,
    pub target_protein_g: Option<f64>,
    pub target_fat_g: Option<f64>,
    pub target_carbs_g: Option<f64>,
    pub calibration_factor: f64,
    pub targets_computed_at: Option<NaiveDateTime>,
    pub timezone: String,
    pub created_at: NaiveDateTime,
    pub updated_at: NaiveDateTime,
}

/// Update shape for `user_profiles`.
#[derive(Debug, Clone, Default, AsChangeset)]
#[diesel(table_name = user_profiles)]
pub struct UserProfileChangeset {
    pub sex: Option<String>,
    pub birth_date: Option<NaiveDate>,
    pub height_cm: Option<f64>,
    pub activity_factor: Option<f64>,
    pub goal_type: Option<String>,
    pub target_weight_kg: Option<Option<f64>>,
    pub rate_kg_per_week: Option<Option<f64>>,
    pub target_kcal: Option<Option<f64>>,
    pub target_protein_g: Option<Option<f64>>,
    pub target_fat_g: Option<Option<f64>>,
    pub target_carbs_g: Option<Option<f64>>,
    pub calibration_factor: Option<f64>,
    pub targets_computed_at: Option<Option<NaiveDateTime>>,
    pub timezone: Option<String>,
    pub updated_at: Option<NaiveDateTime>,
}

// ---------------------------------------------------------------------------
// weight_logs
// ---------------------------------------------------------------------------

/// A row of `weight_logs`.
#[derive(Debug, Clone, Queryable, Selectable, Identifiable)]
#[diesel(table_name = weight_logs)]
#[diesel(check_for_backend(diesel::sqlite::Sqlite))]
pub struct WeightLog {
    pub id: String,
    pub user_id: String,
    pub logged_at: NaiveDateTime,
    pub weight_kg: f64,
    pub source: String,
    pub created_at: NaiveDateTime,
}

/// Insert shape for `weight_logs`.
#[derive(Debug, Clone, Insertable)]
#[diesel(table_name = weight_logs)]
pub struct NewWeightLog {
    pub id: String,
    pub user_id: String,
    pub logged_at: NaiveDateTime,
    pub weight_kg: f64,
    pub source: String,
    pub created_at: NaiveDateTime,
}

// ---------------------------------------------------------------------------
// thumbnails
// ---------------------------------------------------------------------------

/// A row of `thumbnails`.
#[derive(Debug, Clone, Queryable, Selectable, Identifiable)]
#[diesel(table_name = thumbnails)]
#[diesel(check_for_backend(diesel::sqlite::Sqlite))]
pub struct Thumbnail {
    pub id: String,
    pub user_id: String,
    pub sha256: String,
    pub path: String,
    pub width: i32,
    pub height: i32,
    pub bytes: i64,
    pub created_at: NaiveDateTime,
}

/// Insert shape for `thumbnails`.
#[derive(Debug, Clone, Insertable)]
#[diesel(table_name = thumbnails)]
pub struct NewThumbnail {
    pub id: String,
    pub user_id: String,
    pub sha256: String,
    pub path: String,
    pub width: i32,
    pub height: i32,
    pub bytes: i64,
    pub created_at: NaiveDateTime,
}

// ---------------------------------------------------------------------------
// notification_groups
// ---------------------------------------------------------------------------

/// A row of `notification_groups`.
#[derive(Debug, Clone, Queryable, Selectable, Identifiable)]
#[diesel(table_name = notification_groups, primary_key(group_id))]
#[diesel(check_for_backend(diesel::sqlite::Sqlite))]
pub struct NotificationGroup {
    pub group_id: String,
    pub user_id: String,
    pub expected_size: Option<i32>,
    pub notified_at: Option<NaiveDateTime>,
    pub last_photo_at: NaiveDateTime,
    pub created_at: NaiveDateTime,
}

/// Insert shape for `notification_groups`.
#[derive(Debug, Clone, Insertable)]
#[diesel(table_name = notification_groups)]
pub struct NewNotificationGroup {
    pub group_id: String,
    pub user_id: String,
    pub expected_size: Option<i32>,
    pub notified_at: Option<NaiveDateTime>,
    pub last_photo_at: NaiveDateTime,
    pub created_at: NaiveDateTime,
}

/// Update shape for `notification_groups`.
#[derive(Debug, Clone, Default, AsChangeset)]
#[diesel(table_name = notification_groups)]
pub struct NotificationGroupChangeset {
    pub expected_size: Option<Option<i32>>,
    pub notified_at: Option<Option<NaiveDateTime>>,
    pub last_photo_at: Option<NaiveDateTime>,
}

// ---------------------------------------------------------------------------
// meals
// ---------------------------------------------------------------------------

/// A row of `meals`.
#[derive(Debug, Clone, Queryable, Selectable, Identifiable)]
#[diesel(table_name = meals)]
#[diesel(check_for_backend(diesel::sqlite::Sqlite))]
pub struct Meal {
    pub id: String,
    pub user_id: String,
    pub client_uuid: String,
    pub thumbnail_id: Option<String>,
    pub group_id: Option<String>,
    pub group_size: Option<i32>,
    pub dish_name: Option<String>,
    pub dish_name_normalized: Option<String>,
    pub name_source: String,
    pub user_comment: Option<String>,
    pub revision: i32,
    pub eaten_at: NaiveDateTime,
    pub timezone_offset: i32,
    pub meal_type: Option<String>,
    pub status: String,
    pub portion_scale: f64,
    pub created_at: NaiveDateTime,
    pub updated_at: NaiveDateTime,
}

impl Meal {
    /// Typed accessor for the `status` column.
    pub fn status(&self) -> Result<MealStatus, AppError> {
        MealStatus::from_str(&self.status)
    }

    /// Typed accessor for the `name_source` column.
    pub fn name_source(&self) -> Result<NameSource, AppError> {
        NameSource::from_str(&self.name_source)
    }

    /// True when no further analysis job will run for this meal.
    pub fn is_terminal(&self) -> bool {
        matches!(
            self.status.as_str(),
            "needs_review" | "confirmed" | "failed"
        )
    }
}

/// Insert shape for `meals`.
#[derive(Debug, Clone, Insertable)]
#[diesel(table_name = meals)]
pub struct NewMeal {
    pub id: String,
    pub user_id: String,
    pub client_uuid: String,
    pub thumbnail_id: Option<String>,
    pub group_id: Option<String>,
    pub group_size: Option<i32>,
    pub dish_name: Option<String>,
    pub dish_name_normalized: Option<String>,
    pub name_source: String,
    pub user_comment: Option<String>,
    pub revision: i32,
    pub eaten_at: NaiveDateTime,
    pub timezone_offset: i32,
    pub meal_type: Option<String>,
    pub status: String,
    pub portion_scale: f64,
    pub created_at: NaiveDateTime,
    pub updated_at: NaiveDateTime,
}

/// Update shape for `meals`.
#[derive(Debug, Clone, Default, AsChangeset)]
#[diesel(table_name = meals)]
pub struct MealChangeset {
    pub thumbnail_id: Option<Option<String>>,
    pub group_id: Option<Option<String>>,
    pub group_size: Option<Option<i32>>,
    pub dish_name: Option<Option<String>>,
    pub dish_name_normalized: Option<Option<String>>,
    pub name_source: Option<String>,
    pub user_comment: Option<Option<String>>,
    pub revision: Option<i32>,
    pub eaten_at: Option<NaiveDateTime>,
    pub timezone_offset: Option<i32>,
    pub meal_type: Option<Option<String>>,
    pub status: Option<String>,
    pub portion_scale: Option<f64>,
    pub updated_at: Option<NaiveDateTime>,
}

// ---------------------------------------------------------------------------
// meal_items
// ---------------------------------------------------------------------------

/// A row of `meal_items`.
#[derive(Debug, Clone, Queryable, Selectable, Identifiable)]
#[diesel(table_name = meal_items)]
#[diesel(check_for_backend(diesel::sqlite::Sqlite))]
pub struct MealItem {
    pub id: String,
    pub meal_id: String,
    pub revision: i32,
    pub position: i32,
    pub name: String,
    pub barcode: Option<String>,
    pub grams: f64,
    pub grams_source: String,
    pub kcal: f64,
    pub protein_g: f64,
    pub fat_g: f64,
    pub carbs_g: f64,
    pub macro_source: String,
    pub confidence: Option<f64>,
    pub reasoning_note: Option<String>,
    pub created_at: NaiveDateTime,
}

impl MealItem {
    /// Typed accessor for the `grams_source` column.
    pub fn grams_source(&self) -> Result<GramsSource, AppError> {
        GramsSource::from_str(&self.grams_source)
    }

    /// Typed accessor for the `macro_source` column.
    pub fn macro_source(&self) -> Result<MacroSource, AppError> {
        MacroSource::from_str(&self.macro_source)
    }
}

/// Insert shape for `meal_items`.
#[derive(Debug, Clone, Insertable)]
#[diesel(table_name = meal_items)]
pub struct NewMealItem {
    pub id: String,
    pub meal_id: String,
    pub revision: i32,
    pub position: i32,
    pub name: String,
    pub barcode: Option<String>,
    pub grams: f64,
    pub grams_source: String,
    pub kcal: f64,
    pub protein_g: f64,
    pub fat_g: f64,
    pub carbs_g: f64,
    pub macro_source: String,
    pub confidence: Option<f64>,
    pub reasoning_note: Option<String>,
    pub created_at: NaiveDateTime,
}

/// Update shape for `meal_items` (per-item slider edits).
#[derive(Debug, Clone, Default, AsChangeset)]
#[diesel(table_name = meal_items)]
pub struct MealItemChangeset {
    pub position: Option<i32>,
    pub name: Option<String>,
    pub barcode: Option<Option<String>>,
    pub grams: Option<f64>,
    pub grams_source: Option<String>,
    pub kcal: Option<f64>,
    pub protein_g: Option<f64>,
    pub fat_g: Option<f64>,
    pub carbs_g: Option<f64>,
    pub macro_source: Option<String>,
    pub confidence: Option<Option<f64>>,
    pub reasoning_note: Option<Option<String>>,
}

// ---------------------------------------------------------------------------
// item_corrections
// ---------------------------------------------------------------------------

/// A row of `item_corrections`.
#[derive(Debug, Clone, Queryable, Selectable, Identifiable)]
#[diesel(table_name = item_corrections)]
#[diesel(check_for_backend(diesel::sqlite::Sqlite))]
pub struct ItemCorrection {
    pub id: String,
    pub meal_item_id: String,
    pub field: String,
    pub original_value: Option<String>,
    pub corrected_value: Option<String>,
    pub corrected_at: NaiveDateTime,
}

/// Insert shape for `item_corrections`.
#[derive(Debug, Clone, Insertable)]
#[diesel(table_name = item_corrections)]
pub struct NewItemCorrection {
    pub id: String,
    pub meal_item_id: String,
    pub field: String,
    pub original_value: Option<String>,
    pub corrected_value: Option<String>,
    pub corrected_at: NaiveDateTime,
}

// ---------------------------------------------------------------------------
// foods
// ---------------------------------------------------------------------------

/// A row of `foods` — the barcode cache.
#[derive(Debug, Clone, Queryable, Selectable, Identifiable)]
#[diesel(table_name = foods)]
#[diesel(check_for_backend(diesel::sqlite::Sqlite))]
pub struct Food {
    pub id: String,
    pub source: String,
    pub source_ref: String,
    pub name: String,
    pub name_normalized: String,
    pub brand: Option<String>,
    pub barcode: Option<String>,
    pub kcal_100g: Option<f64>,
    pub protein_100g: Option<f64>,
    pub fat_100g: Option<f64>,
    pub carbs_100g: Option<f64>,
    pub fetched_at: NaiveDateTime,
}

/// Insert shape for `foods`.
#[derive(Debug, Clone, Insertable)]
#[diesel(table_name = foods)]
pub struct NewFood {
    pub id: String,
    pub source: String,
    pub source_ref: String,
    pub name: String,
    pub name_normalized: String,
    pub brand: Option<String>,
    pub barcode: Option<String>,
    pub kcal_100g: Option<f64>,
    pub protein_100g: Option<f64>,
    pub fat_100g: Option<f64>,
    pub carbs_100g: Option<f64>,
    pub fetched_at: NaiveDateTime,
}

/// Update shape for `foods` (cache refresh).
#[derive(Debug, Clone, Default, AsChangeset)]
#[diesel(table_name = foods)]
pub struct FoodChangeset {
    pub name: Option<String>,
    pub name_normalized: Option<String>,
    pub brand: Option<Option<String>>,
    pub kcal_100g: Option<Option<f64>>,
    pub protein_100g: Option<Option<f64>>,
    pub fat_100g: Option<Option<f64>>,
    pub carbs_100g: Option<Option<f64>>,
    pub fetched_at: Option<NaiveDateTime>,
}

// ---------------------------------------------------------------------------
// analysis_jobs
// ---------------------------------------------------------------------------

/// A row of `analysis_jobs`.
#[derive(Debug, Clone, Queryable, Selectable, Identifiable)]
#[diesel(table_name = analysis_jobs)]
#[diesel(check_for_backend(diesel::sqlite::Sqlite))]
pub struct AnalysisJob {
    pub id: String,
    pub meal_id: String,
    pub revision: i32,
    pub parent_job_id: Option<String>,
    pub status: String,
    pub attempts: i32,
    pub model: String,
    pub user_feedback: Option<String>,
    pub steps: i32,
    pub tool_calls: i32,
    pub prompt_tokens: i64,
    pub completion_tokens: i64,
    pub cost_micro_usd: i64,
    pub error: Option<String>,
    pub started_at: Option<NaiveDateTime>,
    pub created_at: NaiveDateTime,
    pub finished_at: Option<NaiveDateTime>,
}

impl AnalysisJob {
    /// Typed accessor for the `status` column.
    pub fn status(&self) -> Result<JobStatus, AppError> {
        JobStatus::from_str(&self.status)
    }
}

/// Insert shape for `analysis_jobs`.
#[derive(Debug, Clone, Insertable)]
#[diesel(table_name = analysis_jobs)]
pub struct NewAnalysisJob {
    pub id: String,
    pub meal_id: String,
    pub revision: i32,
    pub parent_job_id: Option<String>,
    pub status: String,
    pub attempts: i32,
    pub model: String,
    pub user_feedback: Option<String>,
    pub steps: i32,
    pub tool_calls: i32,
    pub prompt_tokens: i64,
    pub completion_tokens: i64,
    pub cost_micro_usd: i64,
    pub error: Option<String>,
    pub started_at: Option<NaiveDateTime>,
    pub created_at: NaiveDateTime,
    pub finished_at: Option<NaiveDateTime>,
}

/// Update shape for `analysis_jobs`.
#[derive(Debug, Clone, Default, AsChangeset)]
#[diesel(table_name = analysis_jobs)]
pub struct AnalysisJobChangeset {
    pub status: Option<String>,
    pub attempts: Option<i32>,
    pub model: Option<String>,
    pub steps: Option<i32>,
    pub tool_calls: Option<i32>,
    pub prompt_tokens: Option<i64>,
    pub completion_tokens: Option<i64>,
    pub cost_micro_usd: Option<i64>,
    pub error: Option<Option<String>>,
    pub started_at: Option<Option<NaiveDateTime>>,
    pub finished_at: Option<Option<NaiveDateTime>>,
}

// ---------------------------------------------------------------------------
// agent_steps
// ---------------------------------------------------------------------------

/// A row of `agent_steps`.
#[derive(Debug, Clone, Queryable, Selectable, Identifiable)]
#[diesel(table_name = agent_steps)]
#[diesel(check_for_backend(diesel::sqlite::Sqlite))]
pub struct AgentStep {
    pub id: String,
    pub job_id: String,
    pub step_no: i32,
    pub tool_name: String,
    pub tool_input: Option<String>,
    pub tool_output: Option<String>,
    pub latency_ms: Option<i64>,
    pub created_at: NaiveDateTime,
}

/// Insert shape for `agent_steps`.
#[derive(Debug, Clone, Insertable)]
#[diesel(table_name = agent_steps)]
pub struct NewAgentStep {
    pub id: String,
    pub job_id: String,
    pub step_no: i32,
    pub tool_name: String,
    pub tool_input: Option<String>,
    pub tool_output: Option<String>,
    pub latency_ms: Option<i64>,
    pub created_at: NaiveDateTime,
}

// ---------------------------------------------------------------------------
// agent_sessions
// ---------------------------------------------------------------------------

/// A row of `agent_sessions` — the persisted rig message thread.
#[derive(Debug, Clone, Queryable, Selectable, Identifiable)]
#[diesel(table_name = agent_sessions)]
#[diesel(check_for_backend(diesel::sqlite::Sqlite))]
pub struct AgentSession {
    pub id: String,
    pub meal_id: String,
    pub messages: String,
    pub model: String,
    pub prompt_version: String,
    pub turn_count: i32,
    pub created_at: NaiveDateTime,
    pub updated_at: NaiveDateTime,
}

/// Insert shape for `agent_sessions`.
#[derive(Debug, Clone, Insertable)]
#[diesel(table_name = agent_sessions)]
pub struct NewAgentSession {
    pub id: String,
    pub meal_id: String,
    pub messages: String,
    pub model: String,
    pub prompt_version: String,
    pub turn_count: i32,
    pub created_at: NaiveDateTime,
    pub updated_at: NaiveDateTime,
}

/// Update shape for `agent_sessions`.
#[derive(Debug, Clone, Default, AsChangeset)]
#[diesel(table_name = agent_sessions)]
pub struct AgentSessionChangeset {
    pub messages: Option<String>,
    pub model: Option<String>,
    pub prompt_version: Option<String>,
    pub turn_count: Option<i32>,
    pub updated_at: Option<NaiveDateTime>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn text_enum_roundtrips() {
        for status in MealStatus::ALL {
            assert_eq!(MealStatus::from_str(status.as_str()).unwrap(), *status);
        }
        for source in NameSource::ALL {
            assert_eq!(NameSource::from_str(source.as_str()).unwrap(), *source);
        }
        for source in GramsSource::ALL {
            assert_eq!(GramsSource::from_str(source.as_str()).unwrap(), *source);
        }
        for source in MacroSource::ALL {
            assert_eq!(MacroSource::from_str(source.as_str()).unwrap(), *source);
        }
    }

    #[test]
    fn unknown_text_enum_value_is_an_error() {
        assert!(MealStatus::from_str("teleported").is_err());
    }

    #[test]
    fn normalization_is_unicode_preserving_and_punctuation_free() {
        assert_eq!(
            normalize_dish_name("  Шаурма из Фарша (большая)! "),
            "шаурма из фарша большая"
        );
        assert_eq!(normalize_dish_name("Pad-Thai  w/ Chicken"), "pad thai w chicken");
        assert_eq!(normalize_dish_name("Pad Thai"), normalize_dish_name("pad  thai"));
    }
}
