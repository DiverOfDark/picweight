//! Structured output types for the estimation agent (PRD §5, step 5).
//!
//! These derive both [`schemars::JsonSchema`] — which becomes the
//! `output_schema_raw` handed to rig, so the JSON schema can never drift from
//! the Rust types — and [`utoipa::ToSchema`], because the same shapes surface in
//! the meal-detail and revision-history responses.

use crate::models::{GramsSource, MacroSource};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

/// What the agent returns for one photo.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, ToSchema)]
pub struct MealEstimate {
    /// The dish as identified (or as supplied by the user, which outranks the
    /// visual read).
    pub dish_name: String,
    /// Cuisine, when it is a useful prior (e.g. "Thai", "Georgian").
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cuisine: Option<String>,
    /// The container identified in the frame — a standard delivery bowl, a 30cm
    /// pizza box, a 0.5L cup. With no comment and no coin on the plate this is
    /// the only reliable scale reference, so the prompt pushes hard on it (§5).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub container: Option<String>,
    /// Whether the estimate came from the user's own confirmed history.
    /// Surfaced at confirm time so "this came from your history" is visible
    /// rather than implicit (PRD §14.1).
    #[serde(default)]
    pub from_recall: bool,
    /// One row per component.
    pub items: Vec<EstimatedItem>,
    /// 0.0–1.0 across the whole estimate.
    pub overall_confidence: f64,
    /// Free-form note about the estimate as a whole.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub notes: Option<String>,
}

impl MealEstimate {
    /// Clamp a model-supplied estimate into the ranges the rest of the app
    /// assumes, and drop items that carry no information.
    ///
    /// Only [`OutputMode::Native`](rig::agent::OutputMode) actually *constrains*
    /// the response; `Tool` and `Prompted` are best-effort, and even under
    /// `Native` the schema constrains shape rather than sanity. Nothing
    /// downstream — `meal_items`, the day rollup, the notification copy — is
    /// prepared for a `NaN` gram figure or a confidence of 7.0, so the numbers
    /// are made safe once, here, at the boundary.
    ///
    /// This never invents data: an estimate that arrives empty stays empty and
    /// fails [`Self::is_usable`], which is what triggers the single-shot
    /// fallback (§5 bounds).
    pub fn sanitize(&mut self) {
        self.dish_name = self.dish_name.trim().to_string();
        self.overall_confidence = clamp_confidence(self.overall_confidence);
        self.cuisine = self.cuisine.take().filter(|s| !s.trim().is_empty());
        self.container = self.container.take().filter(|s| !s.trim().is_empty());
        self.notes = self.notes.take().filter(|s| !s.trim().is_empty());

        for item in &mut self.items {
            item.name = item.name.trim().to_string();
            item.grams = clamp_quantity(item.grams);
            item.kcal = clamp_quantity(item.kcal);
            item.protein_g = clamp_quantity(item.protein_g);
            item.fat_g = clamp_quantity(item.fat_g);
            item.carbs_g = clamp_quantity(item.carbs_g);
            item.confidence = clamp_confidence(item.confidence);
            item.barcode = item
                .barcode
                .take()
                .filter(|b| b.bytes().all(|c| c.is_ascii_digit()) && !b.is_empty());
        }

        // A nameless, weightless, calorie-free row is noise the user would have
        // to delete by hand.
        self.items
            .retain(|item| !item.name.is_empty() && (item.grams > 0.0 || item.kcal > 0.0));
    }

    /// Whether this estimate is worth showing the user.
    ///
    /// §5: "Never return an empty item list. A blurry photo still gets your best
    /// guess with low confidence." An empty list means the model did not answer
    /// the question, so the caller falls back rather than persisting a blank.
    pub fn is_usable(&self) -> bool {
        !self.items.is_empty()
    }

    /// Sum of every item's macros.
    pub fn totals(&self) -> MacroTotals {
        self.items.iter().fold(MacroTotals::default(), |acc, item| {
            MacroTotals {
                kcal: acc.kcal + item.kcal,
                protein_g: acc.protein_g + item.protein_g,
                fat_g: acc.fat_g + item.fat_g,
                carbs_g: acc.carbs_g + item.carbs_g,
            }
        })
    }
}

/// One component of a dish: the rice, the sauce, the chicken.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, ToSchema)]
pub struct EstimatedItem {
    /// Component name.
    pub name: String,
    /// Estimated edible mass in grams. This is where nearly all the error lives.
    pub grams: f64,
    /// Energy for the stated grams (not per 100g).
    pub kcal: f64,
    /// Protein for the stated grams.
    pub protein_g: f64,
    /// Fat for the stated grams.
    pub fat_g: f64,
    /// Carbohydrate for the stated grams.
    pub carbs_g: f64,
    /// 0.0–1.0 for this item alone.
    pub confidence: f64,
    /// One line of "why", shown next to the item so the user can see the
    /// reasoning without opening the inspector (§5, step 5).
    pub reasoning_note: String,
    /// EAN, when the item was resolved from a barcode.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub barcode: Option<String>,
    /// Provenance of `grams`.
    #[serde(default = "default_grams_source")]
    pub grams_source: GramsSource,
    /// Provenance of the macros.
    #[serde(default = "default_macro_source")]
    pub macro_source: MacroSource,
}

fn default_grams_source() -> GramsSource {
    GramsSource::Agent
}

fn default_macro_source() -> MacroSource {
    MacroSource::Model
}

/// Force a confidence into 0.0–1.0, treating a non-finite value as "no opinion".
fn clamp_confidence(value: f64) -> f64 {
    if value.is_finite() {
        value.clamp(0.0, 1.0)
    } else {
        0.5
    }
}

/// Force a mass or macro figure to be finite and non-negative.
fn clamp_quantity(value: f64) -> f64 {
    if value.is_finite() && value > 0.0 {
        value
    } else {
        0.0
    }
}

/// Summed macros, used for meals, days and groups alike.
#[derive(Debug, Clone, Copy, Default, PartialEq, Serialize, Deserialize, JsonSchema, ToSchema)]
pub struct MacroTotals {
    /// Energy, kcal.
    pub kcal: f64,
    /// Protein, grams.
    pub protein_g: f64,
    /// Fat, grams.
    pub fat_g: f64,
    /// Carbohydrate, grams.
    pub carbs_g: f64,
}

impl MacroTotals {
    /// Scale every figure, for `portion_scale` ("ate 60% of it") and for the
    /// per-user `calibration_factor`.
    pub fn scaled(self, factor: f64) -> Self {
        MacroTotals {
            kcal: self.kcal * factor,
            protein_g: self.protein_g * factor,
            fat_g: self.fat_g * factor,
            carbs_g: self.carbs_g * factor,
        }
    }

    /// Add two totals together (group and day rollups).
    pub fn plus(self, other: Self) -> Self {
        MacroTotals {
            kcal: self.kcal + other.kcal,
            protein_g: self.protein_g + other.protein_g,
            fat_g: self.fat_g + other.fat_g,
            carbs_g: self.carbs_g + other.carbs_g,
        }
    }
}

/// JSON schema handed to rig as `output_schema_raw`.
///
/// Generated from [`MealEstimate`] so the two can never disagree.
pub fn output_schema() -> schemars::Schema {
    schemars::schema_for!(MealEstimate)
}

// `GramsSource` / `MacroSource` live in `models` (they are database enums) but
// the agent's structured output needs schemas for them too.
impl JsonSchema for GramsSource {
    fn schema_name() -> std::borrow::Cow<'static, str> {
        "GramsSource".into()
    }

    fn json_schema(_generator: &mut schemars::SchemaGenerator) -> schemars::Schema {
        string_enum_schema(
            GramsSource::ALL.iter().map(|v| v.as_str()),
            "Where the gram figure came from.",
        )
    }
}

impl JsonSchema for MacroSource {
    fn schema_name() -> std::borrow::Cow<'static, str> {
        "MacroSource".into()
    }

    fn json_schema(_generator: &mut schemars::SchemaGenerator) -> schemars::Schema {
        string_enum_schema(
            MacroSource::ALL.iter().map(|v| v.as_str()),
            "Where the macro figures came from.",
        )
    }
}

/// Build a `{"type":"string","enum":[…]}` schema for a fixed string enum.
fn string_enum_schema<'a, I: Iterator<Item = &'a str>>(
    values: I,
    description: &str,
) -> schemars::Schema {
    let variants: Vec<serde_json::Value> =
        values.map(|v| serde_json::Value::String(v.to_string())).collect();
    schemars::json_schema!({
        "type": "string",
        "enum": variants,
        "description": description,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn totals_sum_items() {
        let estimate = MealEstimate {
            dish_name: "шаурма с курицей".into(),
            cuisine: None,
            container: Some("delivery foil wrap".into()),
            from_recall: false,
            items: vec![
                EstimatedItem {
                    name: "lavash".into(),
                    grams: 90.0,
                    kcal: 250.0,
                    protein_g: 8.0,
                    fat_g: 2.0,
                    carbs_g: 50.0,
                    confidence: 0.7,
                    reasoning_note: "standard wrap".into(),
                    barcode: None,
                    grams_source: GramsSource::Agent,
                    macro_source: MacroSource::Model,
                },
                EstimatedItem {
                    name: "chicken".into(),
                    grams: 120.0,
                    kcal: 200.0,
                    protein_g: 30.0,
                    fat_g: 8.0,
                    carbs_g: 0.0,
                    confidence: 0.6,
                    reasoning_note: "visible fill".into(),
                    barcode: None,
                    grams_source: GramsSource::Agent,
                    macro_source: MacroSource::Model,
                },
            ],
            overall_confidence: 0.65,
            notes: None,
        };
        let totals = estimate.totals();
        assert_eq!(totals.kcal, 450.0);
        assert_eq!(totals.protein_g, 38.0);
    }

    #[test]
    fn output_schema_is_an_object() {
        let schema = output_schema();
        let value = serde_json::to_value(&schema).unwrap();
        assert_eq!(value.get("type").and_then(|v| v.as_str()), Some("object"));
    }

    #[test]
    fn output_schema_declares_every_estimate_field() {
        let value = serde_json::to_value(output_schema()).unwrap();
        let properties = value
            .get("properties")
            .and_then(|p| p.as_object())
            .expect("the estimate schema has properties");
        for field in ["dish_name", "items", "overall_confidence", "container"] {
            assert!(properties.contains_key(field), "missing {field}");
        }
    }

    fn junk_item(name: &str, grams: f64, confidence: f64) -> EstimatedItem {
        EstimatedItem {
            name: name.into(),
            grams,
            kcal: 0.0,
            protein_g: 0.0,
            fat_g: 0.0,
            carbs_g: 0.0,
            confidence,
            reasoning_note: String::new(),
            barcode: None,
            grams_source: GramsSource::Agent,
            macro_source: MacroSource::Model,
        }
    }

    #[test]
    fn sanitize_clamps_nonsense_and_drops_empty_rows() {
        let mut estimate = MealEstimate {
            dish_name: "  шаурма  ".into(),
            cuisine: Some("   ".into()),
            container: Some("foil wrap".into()),
            from_recall: false,
            items: vec![
                junk_item("rice", f64::NAN, 7.0),
                junk_item("", 100.0, 0.5),
                junk_item("chicken", 120.0, -1.0),
            ],
            overall_confidence: f64::INFINITY,
            notes: None,
        };
        estimate.sanitize();

        assert_eq!(estimate.dish_name, "шаурма");
        assert_eq!(estimate.cuisine, None);
        assert_eq!(estimate.overall_confidence, 0.5);
        // "rice" had no mass and no energy; "" had no name.
        assert_eq!(estimate.items.len(), 1);
        assert_eq!(estimate.items[0].name, "chicken");
        assert_eq!(estimate.items[0].confidence, 0.0);
        assert!(estimate.is_usable());
    }

    #[test]
    fn an_estimate_with_no_items_is_not_usable() {
        let mut estimate = MealEstimate {
            dish_name: "unidentifiable".into(),
            cuisine: None,
            container: None,
            from_recall: false,
            items: Vec::new(),
            overall_confidence: 0.1,
            notes: None,
        };
        estimate.sanitize();
        assert!(!estimate.is_usable());
    }

    #[test]
    fn an_estimate_round_trips_through_json() {
        let estimate = MealEstimate {
            dish_name: "шаурма с курицей".into(),
            cuisine: Some("Middle Eastern".into()),
            container: Some("delivery foil wrap".into()),
            from_recall: true,
            items: vec![junk_item("lavash", 90.0, 0.7)],
            overall_confidence: 0.65,
            notes: Some("recalled".into()),
        };
        let encoded = serde_json::to_string(&estimate).expect("serializes");
        let decoded: MealEstimate = serde_json::from_str(&encoded).expect("deserializes");
        assert_eq!(decoded.dish_name, estimate.dish_name);
        assert_eq!(decoded.items.len(), 1);
        assert!(decoded.from_recall);
        assert_eq!(decoded.items[0].grams_source, GramsSource::Agent);
    }

    #[test]
    fn a_model_may_omit_the_optional_provenance_fields() {
        // `grams_source` / `macro_source` carry serde defaults precisely so a
        // model that leaves them out still deserializes.
        let decoded: MealEstimate = serde_json::from_str(
            r#"{"dish_name":"rice","items":[{"name":"rice","grams":150,"kcal":200,
                "protein_g":4,"fat_g":1,"carbs_g":44,"confidence":0.6,
                "reasoning_note":"standard bowl"}],"overall_confidence":0.6}"#,
        )
        .expect("defaults fill the omitted fields");
        assert_eq!(decoded.items[0].grams_source, GramsSource::Agent);
        assert_eq!(decoded.items[0].macro_source, MacroSource::Model);
        assert!(!decoded.from_recall);
    }
}
