//! `GET /api/v1/export` — full JSON or CSV dump.
//!
//! Backups themselves are out of scope for the app (`rclone` against the PVC
//! covers that, PRD §3). This endpoint exists so the *user* can leave with their
//! data, which is a different concern from operational backup.

use crate::api::meals::{build_meal_responses, load_meals_in_local_range, MealResponse};
use crate::api::profile::ProfileResponse;
use crate::api::weights::WeightLogResponse;
use crate::auth::CurrentUser;
use crate::error::AppError;
use crate::feedback::state::local_date_of;
use crate::AppState;
use axum::body::Body;
use axum::extract::{Query, State};
use axum::http::{header, StatusCode};
use axum::response::Response;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use utoipa::{IntoParams, ToSchema};

/// Output format.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "lowercase")]
pub enum ExportFormat {
    /// One JSON document with everything.
    #[default]
    Json,
    /// A flat CSV of meal items — one row per item, which is the shape a
    /// spreadsheet actually wants.
    Csv,
}

/// Query for `GET /api/v1/export`.
#[derive(Debug, Clone, Default, Deserialize, IntoParams)]
pub struct ExportQuery {
    /// `json` (default) or `csv`.
    pub format: Option<ExportFormat>,
}

/// The JSON export document.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct ExportDocument {
    /// Schema version of this document, so an importer can adapt.
    pub export_version: u32,
    /// Backend version that produced it.
    pub app_version: String,
    /// When it was produced, UTC.
    pub exported_at: DateTime<Utc>,
    /// Body data and targets, when onboarding is complete.
    pub profile: Option<ProfileResponse>,
    /// Every weight reading.
    pub weights: Vec<WeightLogResponse>,
    /// Every meal at its current revision.
    pub meals: Vec<MealResponse>,
}

/// Current [`ExportDocument::export_version`].
pub const EXPORT_VERSION: u32 = 1;

/// CSV header for the flat item export.
pub const CSV_HEADER: &str = "eaten_at,local_date,meal_id,group_id,dish_name,status,item_name,\
grams,grams_source,kcal,protein_g,fat_g,carbs_g,macro_source,confidence";

/// `GET /api/v1/export` — everything this user has logged.
#[utoipa::path(
    get,
    path = "/api/v1/export",
    tag = "export",
    summary = "Export all data",
    params(ExportQuery),
    responses(
        (status = 200, description = "The export", body = ExportDocument),
        (status = 401, description = "Not authenticated", body = crate::error::ErrorBody),
    ),
    security(("session" = []))
)]
pub async fn export_data(
    State(state): State<AppState>,
    user: CurrentUser,
    Query(query): Query<ExportQuery>,
) -> Result<Response, AppError> {
    let format = query.format.unwrap_or_default();
    let user_id = user.id;
    let exported_at = Utc::now();

    let document = state
        .interact(move |conn| {
            let profile = crate::api::profile::load_profile_response(conn, &user_id)?;
            let weights = crate::api::weights::load_weights(conn, &user_id, None, None, None)?;
            // No date bounds: this is the whole account.
            let rows = load_meals_in_local_range(conn, &user_id, None, None)?;
            let meals = build_meal_responses(conn, rows)?;
            Ok(ExportDocument {
                export_version: EXPORT_VERSION,
                app_version: crate::VERSION.to_string(),
                exported_at,
                profile,
                weights,
                meals,
            })
        })
        .await?;

    let stamp = exported_at.date_naive();
    let (content_type, filename, body) = match format {
        ExportFormat::Json => (
            "application/json; charset=utf-8",
            format!("picweight-export-{stamp}.json"),
            serde_json::to_vec_pretty(&document)?,
        ),
        ExportFormat::Csv => (
            "text/csv; charset=utf-8",
            format!("picweight-export-{stamp}.csv"),
            to_csv(&document).into_bytes(),
        ),
    };

    Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, content_type)
        .header(
            header::CONTENT_DISPOSITION,
            format!("attachment; filename=\"{filename}\""),
        )
        .body(Body::from(body))
        .map_err(AppError::internal)
}

/// Flatten an export document to one CSV row per meal item.
///
/// A meal with no items still gets a row, with the item columns blank — an
/// export that silently drops failed or unanalysed meals would misrepresent the
/// history it is meant to preserve.
pub fn to_csv(document: &ExportDocument) -> String {
    let mut out = String::with_capacity(CSV_HEADER.len() + document.meals.len() * 160);
    out.push_str(CSV_HEADER);
    out.push('\n');

    for meal in &document.meals {
        // `local_date_of` works in naive UTC — the same instant, just without
        // the offset the wire format carries.
        let local_date = local_date_of(meal.eaten_at.naive_utc(), meal.timezone_offset);
        let prefix = [
            // RFC 3339 rather than `Display`, which would render a
            // `DateTime<Utc>` as "… UTC" and no longer round-trip.
            meal.eaten_at.to_rfc3339(),
            local_date.to_string(),
            meal.id.clone(),
            meal.group_id.clone().unwrap_or_default(),
            meal.dish_name.clone().unwrap_or_default(),
            meal.status.as_str().to_string(),
        ];

        if meal.items.is_empty() {
            write_row(
                &mut out,
                prefix
                    .iter()
                    .map(String::as_str)
                    .chain(["", "", "", "", "", "", "", "", ""]),
            );
            continue;
        }

        for item in &meal.items {
            let tail = [
                item.name.clone(),
                format_number(item.grams),
                item.grams_source.as_str().to_string(),
                format_number(item.kcal),
                format_number(item.protein_g),
                format_number(item.fat_g),
                format_number(item.carbs_g),
                item.macro_source.as_str().to_string(),
                item.confidence.map(format_number).unwrap_or_default(),
            ];
            write_row(
                &mut out,
                prefix
                    .iter()
                    .map(String::as_str)
                    .chain(tail.iter().map(String::as_str)),
            );
        }
    }

    out
}

/// Write one CSV row, quoting where RFC 4180 requires it.
fn write_row<'a, I: Iterator<Item = &'a str>>(out: &mut String, fields: I) {
    let mut first = true;
    for field in fields {
        if !first {
            out.push(',');
        }
        first = false;
        push_csv_field(out, field);
    }
    out.push('\n');
}

/// Append one field, quoting it when it contains a comma, quote or newline.
fn push_csv_field(out: &mut String, value: &str) {
    if value.contains([',', '"', '\n', '\r']) {
        out.push('"');
        for ch in value.chars() {
            if ch == '"' {
                out.push('"');
            }
            out.push(ch);
        }
        out.push('"');
    } else {
        out.push_str(value);
    }
}

/// Render a float without scientific notation or a trailing `.0`.
fn format_number(value: f64) -> String {
    if !value.is_finite() {
        return String::new();
    }
    if (value - value.round()).abs() < 1e-9 {
        format!("{}", value.round() as i64)
    } else {
        format!("{value:.2}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::schema::MacroTotals;
    use crate::api::meals::MealItemResponse;
    use crate::models::{GramsSource, MacroSource, MealStatus, NameSource};
    use chrono::NaiveDate;

    fn document(dish_name: &str) -> ExportDocument {
        let at = NaiveDate::from_ymd_opt(2026, 8, 1)
            .unwrap()
            .and_hms_opt(22, 30, 0)
            .unwrap()
            .and_utc();
        ExportDocument {
            export_version: EXPORT_VERSION,
            app_version: "test".into(),
            exported_at: at,
            profile: None,
            weights: Vec::new(),
            meals: vec![MealResponse {
                id: "m1".into(),
                client_uuid: "c1".into(),
                group_id: None,
                group_size: None,
                dish_name: Some(dish_name.to_string()),
                name_source: NameSource::Vision,
                user_comment: None,
                revision: 1,
                status: MealStatus::Confirmed,
                eaten_at: at,
                // UTC+3: 22:30 UTC on the 1st is already the 2nd locally.
                timezone_offset: 180,
                meal_type: None,
                portion_scale: 1.0,
                thumbnail_url: None,
                items: vec![MealItemResponse {
                    id: "i1".into(),
                    position: 0,
                    name: "rice".into(),
                    barcode: None,
                    grams: 200.0,
                    grams_source: GramsSource::Agent,
                    kcal: 260.5,
                    protein_g: 5.0,
                    fat_g: 1.0,
                    carbs_g: 57.0,
                    macro_source: MacroSource::Model,
                    confidence: Some(0.6),
                    reasoning_note: None,
                }],
                totals: MacroTotals::default(),
                error: None,
                created_at: at,
                updated_at: at,
            }],
        }
    }

    #[test]
    fn csv_starts_with_the_header_and_buckets_by_the_local_day() {
        let csv = to_csv(&document("plov"));
        let mut lines = csv.lines();
        assert_eq!(lines.next(), Some(CSV_HEADER));
        let row = lines.next().unwrap();
        assert!(
            row.starts_with("2026-08-01T22:30:00+00:00,2026-08-02,m1,"),
            "{row}"
        );
        assert!(row.contains(",rice,200,agent,260.50,"), "{row}");
    }

    #[test]
    fn fields_containing_commas_are_quoted() {
        let csv = to_csv(&document("Шаурма, большая"));
        assert!(csv.contains("\"Шаурма, большая\""), "{csv}");
    }

    #[test]
    fn a_meal_without_items_still_exports() {
        let mut doc = document("plov");
        doc.meals[0].items.clear();
        let csv = to_csv(&doc);
        assert_eq!(csv.lines().count(), 2);
        assert!(csv.lines().nth(1).unwrap().ends_with(",,,,,,,,,"));
    }
}
