//! Packaged-food lookup. **Barcode only.**
//!
//! PRD §1.3 rules out dish-matching against a nutrition database: for delivery
//! food the database was never going to contain the dish, and once the model is
//! already estimating portion grams — where nearly all the error lives — the
//! marginal error from also estimating macros is small by comparison.
//!
//! A barcode is the exception. It is exact, and it is the one place a real
//! database still earns its keep. `foods` is a cache of those lookups and
//! nothing else; there is no bulk seeding pipeline.

pub mod openfoodfacts;

use crate::db::DbPool;
use crate::error::AppError;
use crate::models::{Food, FoodChangeset, NewFood};
use crate::AppState;
use diesel::prelude::*;
use diesel::SqliteConnection;
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

/// `foods.source` value for Open Food Facts rows.
pub const SOURCE_OPENFOODFACTS: &str = "openfoodfacts";

/// How long a cached product is trusted before it is re-fetched.
pub const CACHE_TTL_DAYS: i64 = 90;

/// Normalized per-100g nutrition for one packaged product.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct FoodFacts {
    /// Provider identifier, e.g. `openfoodfacts`.
    pub source: String,
    /// Provider-side identifier — the barcode, for Open Food Facts.
    pub source_ref: String,
    /// Product name.
    pub name: String,
    /// Brand, when the provider knows one.
    pub brand: Option<String>,
    /// The barcode this record was resolved from.
    pub barcode: Option<String>,
    /// Energy per 100g.
    pub kcal_100g: Option<f64>,
    /// Protein per 100g.
    pub protein_100g: Option<f64>,
    /// Fat per 100g.
    pub fat_100g: Option<f64>,
    /// Carbohydrate per 100g.
    pub carbs_100g: Option<f64>,
}

impl FoodFacts {
    /// True when the record carries enough to be useful — energy at minimum.
    pub fn is_usable(&self) -> bool {
        self.kcal_100g.is_some()
    }
}

/// Resolve a barcode to a `foods` row: cache first, provider on a miss or a
/// stale entry.
pub async fn resolve_barcode(state: &AppState, ean: &str) -> Result<Food, AppError> {
    resolve_barcode_with(&state.pool, &state.http, ean).await
}

/// Resolve a barcode from a bare pool and HTTP client.
///
/// The agent's `lookup_barcode` tool is constructed with exactly these two
/// handles rather than the whole [`AppState`] (a tool must not be able to reach
/// the job queue or the event bus), so the cache logic lives here and
/// [`resolve_barcode`] is the thin `AppState` wrapper over it.
pub async fn resolve_barcode_with(
    pool: &DbPool,
    http: &reqwest::Client,
    ean: &str,
) -> Result<Food, AppError> {
    let ean = validate_ean(ean)?.to_string();

    let cached = {
        let pool = pool.clone();
        let ean = ean.clone();
        tokio::task::spawn_blocking(move || {
            let mut conn = pool.get().map_err(AppError::from)?;
            lookup_cached(&mut conn, &ean)
        })
        .await??
    };

    // A fresh hit never leaves the process. Anything else is kept as the
    // fallback for a provider that is down or has forgotten the product.
    let cached = match cached {
        Some(food) if is_fresh(&food) => return Ok(food),
        other => other,
    };

    match openfoodfacts::fetch(http, &ean).await {
        Ok(Some(facts)) => {
            let pool = pool.clone();
            tokio::task::spawn_blocking(move || {
                let mut conn = pool.get().map_err(AppError::from)?;
                upsert_food(&mut conn, &facts)
            })
            .await?
        }
        // The product is genuinely unknown upstream. A stale cached row still
        // beats a 404 — the product did not stop existing because the entry
        // aged out.
        Ok(None) => cached.ok_or_else(|| {
            AppError::NotFound(format!("no product with barcode {ean} in Open Food Facts"))
        }),
        // Serving a stale row through an outage is the whole point of a cache.
        Err(err) => match cached {
            Some(food) => {
                tracing::warn!(%ean, error = %err, "serving a stale cached food; refresh failed");
                Ok(food)
            }
            None => Err(err),
        },
    }
}

/// Whether a cached row is still inside [`CACHE_TTL_DAYS`].
pub fn is_fresh(food: &Food) -> bool {
    let age = chrono::Utc::now().naive_utc() - food.fetched_at;
    age < chrono::Duration::days(CACHE_TTL_DAYS)
}

/// Read a cached product by barcode.
pub fn lookup_cached(conn: &mut SqliteConnection, ean: &str) -> Result<Option<Food>, AppError> {
    use crate::schema::foods::dsl;
    dsl::foods
        .filter(dsl::barcode.eq(ean))
        .select(Food::as_select())
        .first::<Food>(conn)
        .optional()
        .map_err(AppError::from)
}

/// Insert or refresh a cached product.
///
/// Keyed on `(source, source_ref)`, which the migration makes unique — the
/// barcode index is partial, so it cannot be the upsert key on its own.
pub fn upsert_food(conn: &mut SqliteConnection, facts: &FoodFacts) -> Result<Food, AppError> {
    use crate::schema::foods::dsl;

    let existing = dsl::foods
        .filter(dsl::source.eq(&facts.source))
        .filter(dsl::source_ref.eq(&facts.source_ref))
        .select(Food::as_select())
        .first::<Food>(conn)
        .optional()?;

    if let Some(food) = existing {
        let changeset = FoodChangeset {
            name: Some(facts.name.clone()),
            name_normalized: Some(crate::models::normalize_dish_name(&facts.name)),
            brand: Some(facts.brand.clone()),
            kcal_100g: Some(facts.kcal_100g),
            protein_100g: Some(facts.protein_100g),
            fat_100g: Some(facts.fat_100g),
            carbs_100g: Some(facts.carbs_100g),
            fetched_at: Some(chrono::Utc::now().naive_utc()),
        };
        return diesel::update(dsl::foods.filter(dsl::id.eq(&food.id)))
            .set(&changeset)
            .returning(Food::as_returning())
            .get_result::<Food>(conn)
            .map_err(AppError::from);
    }

    diesel::insert_into(dsl::foods)
        .values(&new_food_row(facts))
        .returning(Food::as_returning())
        .get_result::<Food>(conn)
        .map_err(AppError::from)
}

/// Build the insert row for a fetched product.
pub fn new_food_row(facts: &FoodFacts) -> NewFood {
    NewFood {
        id: uuid::Uuid::new_v4().to_string(),
        source: facts.source.clone(),
        source_ref: facts.source_ref.clone(),
        name: facts.name.clone(),
        name_normalized: crate::models::normalize_dish_name(&facts.name),
        brand: facts.brand.clone(),
        barcode: facts.barcode.clone(),
        kcal_100g: facts.kcal_100g,
        protein_100g: facts.protein_100g,
        fat_100g: facts.fat_100g,
        carbs_100g: facts.carbs_100g,
        fetched_at: chrono::Utc::now().naive_utc(),
    }
}

/// Reject anything that is not a plain EAN/UPC before it reaches the provider.
pub fn validate_ean(ean: &str) -> Result<&str, AppError> {
    let trimmed = ean.trim();
    if trimmed.len() < 8 || trimmed.len() > 14 || !trimmed.bytes().all(|b| b.is_ascii_digit()) {
        return Err(AppError::BadRequest(format!(
            "{ean:?} is not a valid EAN/UPC (8-14 digits)"
        )));
    }
    Ok(trimmed)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn facts(barcode: &str, name: &str, kcal: f64) -> FoodFacts {
        FoodFacts {
            source: SOURCE_OPENFOODFACTS.to_string(),
            source_ref: barcode.to_string(),
            name: name.to_string(),
            brand: Some("Тест".to_string()),
            barcode: Some(barcode.to_string()),
            kcal_100g: Some(kcal),
            protein_100g: Some(3.0),
            fat_100g: Some(1.0),
            carbs_100g: Some(10.0),
        }
    }

    fn test_conn() -> crate::db::DbConnection {
        // A file-backed temp database: `:memory:` gives every pooled connection
        // its own empty schema, which is not what a cache test wants.
        let dir = Box::leak(Box::new(tempfile::tempdir().unwrap()));
        let pool = crate::db::establish_pool(dir.path().join("food.db")).unwrap();
        crate::db::run_migrations(&pool).unwrap();
        pool.get().unwrap()
    }

    #[test]
    fn ean_validation_accepts_real_shapes_and_rejects_junk() {
        assert!(validate_ean("4600682001010").is_ok());
        assert!(validate_ean("  4600682001010 ").is_ok());
        assert!(validate_ean("12345678").is_ok());
        assert!(validate_ean("123").is_err());
        assert!(validate_ean("46006820010AB").is_err());
        assert!(validate_ean("../../etc/passwd").is_err());
    }

    #[test]
    fn a_fetched_product_is_written_through_and_served_from_cache() {
        let mut conn = test_conn();
        assert!(lookup_cached(&mut conn, "4600682001010").unwrap().is_none());

        let stored = upsert_food(&mut conn, &facts("4600682001010", "Молоко", 59.0)).unwrap();
        assert_eq!(stored.kcal_100g, Some(59.0));
        assert_eq!(stored.name_normalized, "молоко");

        let cached = lookup_cached(&mut conn, "4600682001010").unwrap().unwrap();
        assert_eq!(cached.id, stored.id);
        assert!(is_fresh(&cached));
    }

    #[test]
    fn re_fetching_refreshes_the_row_rather_than_duplicating_it() {
        use crate::schema::foods::dsl;
        let mut conn = test_conn();

        let first = upsert_food(&mut conn, &facts("4600682001010", "Молоко", 59.0)).unwrap();
        let second = upsert_food(&mut conn, &facts("4600682001010", "Молоко 3.2%", 61.0)).unwrap();

        assert_eq!(first.id, second.id, "the cache key is (source, source_ref)");
        assert_eq!(second.kcal_100g, Some(61.0));
        assert_eq!(second.name, "Молоко 3.2%");
        assert!(second.fetched_at >= first.fetched_at);

        let count: i64 = dsl::foods.count().get_result(&mut conn).unwrap();
        assert_eq!(count, 1);
    }

    #[test]
    fn two_barcodes_are_two_rows() {
        use crate::schema::foods::dsl;
        let mut conn = test_conn();
        upsert_food(&mut conn, &facts("4600682001010", "Молоко", 59.0)).unwrap();
        upsert_food(&mut conn, &facts("5000112637922", "Cola Zero", 0.3)).unwrap();
        let count: i64 = dsl::foods.count().get_result(&mut conn).unwrap();
        assert_eq!(count, 2);
    }

    #[test]
    fn a_row_older_than_the_ttl_is_stale() {
        let mut conn = test_conn();
        let stored = upsert_food(&mut conn, &facts("4600682001010", "Молоко", 59.0)).unwrap();
        assert!(is_fresh(&stored));

        let aged = Food {
            fetched_at: chrono::Utc::now().naive_utc()
                - chrono::Duration::days(CACHE_TTL_DAYS + 1),
            ..stored
        };
        assert!(!is_fresh(&aged));
    }
}
