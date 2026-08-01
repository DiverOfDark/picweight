//! `POST /api/v1/barcode/:ean` — resolve a barcode to a food record.
//!
//! ML Kit scans the barcode on-device (offline, free, exact); the backend turns
//! the digits into nutrition. Packaged goods and drinks only — see
//! [`crate::food`] for why there is no dish-level database.

use crate::auth::CurrentUser;
use crate::error::AppError;
use crate::models::Food;
use crate::AppState;
use axum::extract::{Path, State};
use axum::Json;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

/// A cached food record.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct FoodResponse {
    /// `foods.id`.
    pub id: String,
    /// Provider, e.g. `openfoodfacts`.
    pub source: String,
    /// Product name.
    pub name: String,
    /// Brand, when known.
    pub brand: Option<String>,
    /// The barcode this record resolves.
    pub barcode: Option<String>,
    /// Energy per 100g.
    pub kcal_100g: Option<f64>,
    /// Protein per 100g.
    pub protein_100g: Option<f64>,
    /// Fat per 100g.
    pub fat_100g: Option<f64>,
    /// Carbohydrate per 100g.
    pub carbs_100g: Option<f64>,
    /// When the record was last fetched from the provider.
    pub fetched_at: DateTime<Utc>,
}

impl From<Food> for FoodResponse {
    fn from(food: Food) -> Self {
        FoodResponse {
            id: food.id,
            source: food.source,
            name: food.name,
            brand: food.brand,
            barcode: food.barcode,
            kcal_100g: food.kcal_100g,
            protein_100g: food.protein_100g,
            fat_100g: food.fat_100g,
            carbs_100g: food.carbs_100g,
            // SQLite stores naive UTC; the wire format must carry the offset.
            fetched_at: food.fetched_at.and_utc(),
        }
    }
}

/// `POST /api/v1/barcode/{ean}` — resolve an EAN.
#[utoipa::path(
    post,
    path = "/api/v1/barcode/{ean}",
    tag = "barcode",
    summary = "Resolve a barcode to a food record",
    description = "Reads the local `foods` cache first, then Open Food Facts. \
`POST` rather than `GET` because a miss performs an outbound fetch and writes \
the cache.",
    params(("ean" = String, Path, description = "EAN-8/EAN-13/UPC digits")),
    responses(
        (status = 200, description = "The food record", body = FoodResponse),
        (status = 400, description = "Not a valid EAN", body = crate::error::ErrorBody),
        (status = 404, description = "Unknown product", body = crate::error::ErrorBody),
        (status = 503, description = "Open Food Facts unreachable", body = crate::error::ErrorBody),
    ),
    security(("session" = []))
)]
pub async fn resolve_barcode(
    State(state): State<AppState>,
    user: CurrentUser,
    Path(ean): Path<String>,
) -> Result<Json<FoodResponse>, AppError> {
    let _ = user;
    let ean = crate::food::validate_ean(&ean)?.to_string();
    let food = crate::food::resolve_barcode(&state, &ean).await?;
    Ok(Json(food.into()))
}
