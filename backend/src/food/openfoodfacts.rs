//! Open Food Facts client (barcode lookups only).
//!
//! The v2 product endpoint returns a large document; only the handful of
//! `nutriments` fields picweight cares about are pulled out. A 404 or a
//! `status: 0` body is *not* an error — it means "no such product", which the
//! caller renders as a normal miss.

use super::{FoodFacts, SOURCE_OPENFOODFACTS};
use crate::error::AppError;
use serde_json::Value;

/// Base URL of the Open Food Facts v2 API.
pub const API_BASE: &str = "https://world.openfoodfacts.org/api/v2";

/// User agent required by the Open Food Facts terms of use.
pub const USER_AGENT: &str = concat!("picweight/", env!("PICWEIGHT_VERSION"), " (self-hosted)");

/// Only the fields we use, so the response stays small.
pub const FIELDS: &str = "code,product_name,brands,nutriments";

/// How long one product lookup may take before it is treated as unavailable.
///
/// Shorter than the shared client's default: a barcode lookup happens inside the
/// agent's 25s wall-clock budget, so a slow provider must not eat it.
pub const FETCH_TIMEOUT_SECS: u64 = 8;

/// Fetch one product by barcode.
///
/// `Ok(None)` means the product is genuinely unknown. Network and 5xx failures
/// surface as [`AppError::Upstream`] so the caller can decide whether to retry.
pub async fn fetch(http: &reqwest::Client, ean: &str) -> Result<Option<FoodFacts>, AppError> {
    let ean = super::validate_ean(ean)?;
    let url = format!("{API_BASE}/product/{ean}");

    let response = http
        .get(&url)
        .query(&[("fields", FIELDS)])
        .header(reqwest::header::USER_AGENT, USER_AGENT)
        .header(reqwest::header::ACCEPT, "application/json")
        .timeout(std::time::Duration::from_secs(FETCH_TIMEOUT_SECS))
        .send()
        .await
        .map_err(|e| AppError::Upstream(format!("Open Food Facts unreachable: {e}")))?;

    let status = response.status();
    // A miss is a 404 with a perfectly well-formed body; it is not a failure.
    if status == reqwest::StatusCode::NOT_FOUND {
        return Ok(None);
    }
    if status == reqwest::StatusCode::TOO_MANY_REQUESTS {
        return Err(AppError::RateLimited(
            "Open Food Facts rate limit reached".to_string(),
        ));
    }
    if !status.is_success() {
        return Err(AppError::Upstream(format!(
            "Open Food Facts returned {status} for {ean}"
        )));
    }

    let body: Value = response
        .json()
        .await
        .map_err(|e| AppError::Upstream(format!("Open Food Facts sent unreadable JSON: {e}")))?;

    // v2 answers a miss with `status: 0` and HTTP 200 about as often as with a
    // 404, so both shapes have to be treated as "no such product".
    if body.get("status").and_then(status_code) == Some(0) {
        return Ok(None);
    }
    let product = match body.get("product") {
        Some(product) => product,
        None => return Ok(None),
    };

    Ok(parse_product(product).map(|mut facts| {
        // The response echoes `code`, but the barcode we asked for is what the
        // cache is keyed on — never let a provider redirect rewrite the key.
        facts.source_ref = ean.to_string();
        facts.barcode = Some(ean.to_string());
        facts
    }))
}

/// Map an Open Food Facts `nutriments` object onto [`FoodFacts`].
///
/// Handles the two energy spellings the API uses (`energy-kcal_100g` and
/// `energy_100g` in kJ) and the `_100g` suffix convention.
///
/// Returns `None` when the product carries no usable name — an anonymous row
/// would be worse than a miss, because it would then be cached.
pub fn parse_product(value: &Value) -> Option<FoodFacts> {
    let code = text(value.get("code")).unwrap_or_default();

    let name = ["product_name", "product_name_en", "generic_name"]
        .iter()
        .find_map(|key| text(value.get(*key)))?;

    // `brands` is a comma-separated list; the first entry is the owning brand.
    let brand = text(value.get("brands")).and_then(|brands| {
        brands
            .split(',')
            .map(str::trim)
            .find(|part| !part.is_empty())
            .map(str::to_string)
    });

    let nutriments = value.get("nutriments");
    let kcal_100g = number(nutriments.and_then(|n| n.get("energy-kcal_100g")))
        .or_else(|| number(nutriments.and_then(|n| n.get("energy-kj_100g"))).map(kj_to_kcal))
        .or_else(|| number(nutriments.and_then(|n| n.get("energy_100g"))).map(kj_to_kcal));

    Some(FoodFacts {
        source: SOURCE_OPENFOODFACTS.to_string(),
        source_ref: if code.is_empty() { name.clone() } else { code.clone() },
        name,
        brand,
        barcode: if code.is_empty() { None } else { Some(code) },
        kcal_100g: kcal_100g.filter(|v| v.is_finite() && *v >= 0.0),
        protein_100g: number(nutriments.and_then(|n| n.get("proteins_100g")))
            .filter(|v| v.is_finite() && *v >= 0.0),
        fat_100g: number(nutriments.and_then(|n| n.get("fat_100g")))
            .filter(|v| v.is_finite() && *v >= 0.0),
        carbs_100g: number(nutriments.and_then(|n| n.get("carbohydrates_100g")))
            .filter(|v| v.is_finite() && *v >= 0.0),
    })
}

/// kJ to kcal, for products that only publish joules.
pub fn kj_to_kcal(kj: f64) -> f64 {
    kj / 4.184
}

/// Read a JSON value as a trimmed non-empty string.
fn text(value: Option<&Value>) -> Option<String> {
    let raw = value?.as_str()?.trim();
    if raw.is_empty() {
        None
    } else {
        Some(raw.to_string())
    }
}

/// Read a JSON value as a number.
///
/// Open Food Facts is crowd-sourced and inconsistent: the same field arrives as
/// `12.5`, `"12.5"` or `""` depending on who typed it in.
fn number(value: Option<&Value>) -> Option<f64> {
    match value? {
        Value::Number(n) => n.as_f64(),
        Value::String(s) => s.trim().replace(',', ".").parse::<f64>().ok(),
        _ => None,
    }
}

/// Read the `status` flag, which arrives as either `0`/`1` or `"0"`/`"1"`.
fn status_code(value: &Value) -> Option<i64> {
    match value {
        Value::Number(n) => n.as_i64(),
        Value::String(s) => s.trim().parse::<i64>().ok(),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn joule_conversion_matches_the_standard_factor() {
        assert!((kj_to_kcal(1000.0) - 239.0057).abs() < 0.01);
    }

    #[test]
    fn a_complete_product_parses() {
        let product = json!({
            "code": "4600682001010",
            "product_name": "Молоко 3.2%",
            "brands": "Домик в деревне, Вимм-Билль-Данн",
            "nutriments": {
                "energy-kcal_100g": 59.0,
                "proteins_100g": 2.9,
                "fat_100g": 3.2,
                "carbohydrates_100g": 4.7
            }
        });
        let facts = parse_product(&product).unwrap();
        assert_eq!(facts.source, SOURCE_OPENFOODFACTS);
        assert_eq!(facts.name, "Молоко 3.2%");
        assert_eq!(facts.brand.as_deref(), Some("Домик в деревне"));
        assert_eq!(facts.barcode.as_deref(), Some("4600682001010"));
        assert_eq!(facts.kcal_100g, Some(59.0));
        assert_eq!(facts.protein_100g, Some(2.9));
        assert!(facts.is_usable());
    }

    #[test]
    fn kilojoule_only_products_are_converted() {
        let product = json!({
            "code": "5000112637922",
            "product_name": "Cola Zero",
            "nutriments": { "energy_100g": 1000.0 }
        });
        let facts = parse_product(&product).unwrap();
        assert!((facts.kcal_100g.unwrap() - 239.0057).abs() < 0.01);
    }

    #[test]
    fn the_kcal_spelling_wins_over_the_joule_one() {
        let product = json!({
            "code": "1",
            "product_name": "Both",
            "nutriments": { "energy-kcal_100g": 100.0, "energy_100g": 1000.0 }
        });
        assert_eq!(parse_product(&product).unwrap().kcal_100g, Some(100.0));
    }

    #[test]
    fn crowd_sourced_string_numbers_are_accepted() {
        let product = json!({
            "code": "2",
            "product_name": "Typed by hand",
            "nutriments": {
                "energy-kcal_100g": "250",
                "proteins_100g": "7,5",
                "fat_100g": "",
                "carbohydrates_100g": null
            }
        });
        let facts = parse_product(&product).unwrap();
        assert_eq!(facts.kcal_100g, Some(250.0));
        assert_eq!(facts.protein_100g, Some(7.5));
        assert_eq!(facts.fat_100g, None);
        assert_eq!(facts.carbs_100g, None);
    }

    #[test]
    fn a_nameless_product_is_a_miss_not_a_cached_blank() {
        let product = json!({ "code": "3", "product_name": "   ", "nutriments": {} });
        assert!(parse_product(&product).is_none());
    }

    #[test]
    fn a_product_without_energy_parses_but_is_not_usable() {
        let product = json!({ "code": "4", "product_name": "Water", "nutriments": {} });
        let facts = parse_product(&product).unwrap();
        assert!(!facts.is_usable());
    }

    #[test]
    fn negative_and_non_finite_figures_are_dropped() {
        let product = json!({
            "code": "5",
            "product_name": "Broken entry",
            "nutriments": { "energy-kcal_100g": -12.0, "proteins_100g": "NaN" }
        });
        let facts = parse_product(&product).unwrap();
        assert_eq!(facts.kcal_100g, None);
        assert_eq!(facts.protein_100g, None);
    }

    #[test]
    fn the_english_name_is_used_when_the_default_is_blank() {
        let product = json!({
            "code": "6",
            "product_name": "",
            "product_name_en": "Oat drink",
            "nutriments": { "energy-kcal_100g": 46.0 }
        });
        assert_eq!(parse_product(&product).unwrap().name, "Oat drink");
    }
}
