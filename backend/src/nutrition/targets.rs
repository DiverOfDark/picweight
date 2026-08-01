//! Mifflin-St Jeor targets (PRD §6).
//!
//! ```text
//! BMR (male)    = 10·kg + 6.25·cm − 5·age + 5
//! BMR (female)  = 10·kg + 6.25·cm − 5·age − 161
//! TDEE          = BMR × activity_factor        (1.2 sedentary … 1.9 very active)
//! target_kcal   = TDEE + goal_delta            (deficit typically −300…−750)
//! protein_floor = 1.6–2.2 g/kg target weight
//! fat_floor     = 0.8 g/kg
//! carbs         = remainder
//! ```
//!
//! A deficit steeper than ~1% bodyweight/week is **warned about, not silently
//! accepted** — see [`deficit_warning`].

use crate::models::{GoalType, Sex};
use chrono::NaiveDate;
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

/// Energy in one kilogram of body mass, kcal. The conventional 7700 figure.
pub const KCAL_PER_KG: f64 = 7700.0;

/// Protein floor in g per kg of target weight. The low end of the 1.6–2.2
/// range: high enough to protect lean mass, low enough to stay reachable.
pub const PROTEIN_G_PER_KG: f64 = 1.8;

/// Fat floor in g per kg of target weight.
pub const FAT_G_PER_KG: f64 = 0.8;

/// kcal per gram of protein.
pub const KCAL_PER_G_PROTEIN: f64 = 4.0;
/// kcal per gram of fat.
pub const KCAL_PER_G_FAT: f64 = 9.0;
/// kcal per gram of carbohydrate.
pub const KCAL_PER_G_CARB: f64 = 4.0;

/// Floor on the computed daily target, so a badly configured profile cannot
/// produce a dangerous number.
pub const MIN_TARGET_KCAL: f64 = 1200.0;

/// Rate of loss, as a fraction of bodyweight per week, beyond which the user is
/// warned.
pub const AGGRESSIVE_RATE_FRACTION: f64 = 0.01;

/// Sedentary activity factor.
pub const ACTIVITY_SEDENTARY: f64 = 1.2;
/// Very active activity factor.
pub const ACTIVITY_VERY_ACTIVE: f64 = 1.9;

/// Everything the formulas need.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct TargetInputs {
    /// Biological sex — the BMR constant differs by 166 kcal.
    pub sex: Sex,
    /// Age in years at the time of computation.
    pub age_years: f64,
    /// Height in centimetres.
    pub height_cm: f64,
    /// Current weight in kilograms.
    pub weight_kg: f64,
    /// Activity multiplier, 1.2–1.9.
    pub activity_factor: f64,
    /// Direction of the goal.
    pub goal_type: GoalType,
    /// Target weight in kilograms; drives the protein and fat floors.
    pub target_weight_kg: f64,
    /// Intended rate of change in kg per week (always positive; direction comes
    /// from `goal_type`).
    pub rate_kg_per_week: f64,
}

/// The computed daily targets.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct Targets {
    /// Basal metabolic rate, kcal/day.
    pub bmr: f64,
    /// Total daily energy expenditure, kcal/day.
    pub tdee: f64,
    /// Daily energy target, kcal.
    pub kcal: f64,
    /// Daily protein floor, grams.
    pub protein_g: f64,
    /// Daily fat floor, grams.
    pub fat_g: f64,
    /// Daily carbohydrate allowance — the remainder.
    pub carbs_g: f64,
}

/// Mifflin-St Jeor basal metabolic rate.
pub fn bmr(sex: Sex, weight_kg: f64, height_cm: f64, age_years: f64) -> f64 {
    let base = 10.0 * weight_kg + 6.25 * height_cm - 5.0 * age_years;
    match sex {
        Sex::Male => base + 5.0,
        Sex::Female => base - 161.0,
    }
}

/// Total daily energy expenditure.
pub fn tdee(bmr: f64, activity_factor: f64) -> f64 {
    bmr * activity_factor.clamp(ACTIVITY_SEDENTARY, ACTIVITY_VERY_ACTIVE)
}

/// Daily kcal delta implied by the goal and the intended rate.
///
/// Negative for a deficit, positive for a surplus, zero for maintenance.
pub fn goal_delta_kcal(goal_type: GoalType, rate_kg_per_week: f64) -> f64 {
    let daily = rate_kg_per_week.abs() * KCAL_PER_KG / 7.0;
    match goal_type {
        GoalType::Lose => -daily,
        GoalType::Gain => daily,
        GoalType::Maintain => 0.0,
    }
}

/// Compute the full target set.
pub fn compute_targets(input: &TargetInputs) -> Targets {
    let bmr_value = bmr(
        input.sex,
        input.weight_kg,
        input.height_cm,
        input.age_years,
    );
    let tdee_value = tdee(bmr_value, input.activity_factor);
    let kcal =
        (tdee_value + goal_delta_kcal(input.goal_type, input.rate_kg_per_week)).max(MIN_TARGET_KCAL);

    let protein_g = PROTEIN_G_PER_KG * input.target_weight_kg;
    let fat_g = FAT_G_PER_KG * input.target_weight_kg;

    // Carbs are the remainder. Clamped at zero: an aggressive deficit with a
    // high target weight can leave nothing, and a negative allowance is
    // nonsense rather than information.
    let carbs_kcal = kcal - protein_g * KCAL_PER_G_PROTEIN - fat_g * KCAL_PER_G_FAT;
    let carbs_g = (carbs_kcal / KCAL_PER_G_CARB).max(0.0);

    Targets {
        bmr: bmr_value,
        tdee: tdee_value,
        kcal,
        protein_g,
        fat_g,
        carbs_g,
    }
}

/// A rate the formulas will honour but the user should be told about.
///
/// PRD §6 says the onboarding "warns (does not silently accept) a deficit
/// steeper than ~1% bodyweight/week". Structured rather than a bare sentence so
/// the API can surface the numbers alongside the prose and a client can render
/// its own copy — the figures are the argument, not the wording.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct RateWarning {
    /// The requested rate, kg/week, always positive.
    pub rate_kg_per_week: f64,
    /// That rate as a fraction of current bodyweight per week.
    pub bodyweight_fraction: f64,
    /// The threshold it exceeded — [`AGGRESSIVE_RATE_FRACTION`].
    pub threshold_fraction: f64,
    /// A rate that would sit exactly on the threshold, kg/week.
    pub suggested_rate_kg_per_week: f64,
    /// Ready-to-display prose, so a thin client does not have to compose any.
    pub message: String,
}

/// Warn — never silently accept — a rate steeper than ~1% bodyweight/week.
///
/// Returns `None` when the plan is unremarkable. Applies to gaining as well as
/// losing: 1.5kg a week of "bulk" is mostly fat, and saying so is the same job.
pub fn rate_warning(input: &TargetInputs) -> Option<RateWarning> {
    if input.goal_type == GoalType::Maintain || input.weight_kg <= 0.0 {
        return None;
    }
    let rate = input.rate_kg_per_week.abs();
    let fraction = rate / input.weight_kg;
    if !fraction.is_finite() || fraction <= AGGRESSIVE_RATE_FRACTION {
        return None;
    }
    let suggested = input.weight_kg * AGGRESSIVE_RATE_FRACTION;
    let verb = if input.goal_type == GoalType::Lose {
        "losing"
    } else {
        "gaining"
    };
    Some(RateWarning {
        rate_kg_per_week: rate,
        bodyweight_fraction: fraction,
        threshold_fraction: AGGRESSIVE_RATE_FRACTION,
        suggested_rate_kg_per_week: suggested,
        message: format!(
            "{verb} {rate:.2} kg/week is {:.1}% of your bodyweight — above the ~1%/week \
that is generally sustainable. Expect muscle loss and hunger; consider {suggested:.2} \
kg/week instead.",
            fraction * 100.0,
        ),
    })
}

/// The prose half of [`rate_warning`], kept as its own function because most
/// call sites only want the sentence.
pub fn deficit_warning(input: &TargetInputs) -> Option<String> {
    rate_warning(input).map(|warning| warning.message)
}

/// Whole years between a birth date and a reference date.
pub fn age_years(birth_date: NaiveDate, on: NaiveDate) -> f64 {
    let mut years = on.year_diff(birth_date);
    if years < 0 {
        years = 0;
    }
    years as f64
}

/// Small extension so [`age_years`] reads plainly.
trait YearDiff {
    fn year_diff(&self, from: NaiveDate) -> i32;
}

impl YearDiff for NaiveDate {
    fn year_diff(&self, from: NaiveDate) -> i32 {
        use chrono::Datelike;
        let mut years = self.year() - from.year();
        let had_birthday = (self.month(), self.day()) >= (from.month(), from.day());
        if !had_birthday {
            years -= 1;
        }
        years
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Published reference: a 30-year-old man, 180cm, 80kg.
    // 10*80 + 6.25*180 - 5*30 + 5 = 800 + 1125 - 150 + 5 = 1780
    #[test]
    fn male_bmr_matches_the_reference_value() {
        assert!((bmr(Sex::Male, 80.0, 180.0, 30.0) - 1780.0).abs() < 0.001);
    }

    // A 30-year-old woman, 165cm, 60kg.
    // 10*60 + 6.25*165 - 5*30 - 161 = 600 + 1031.25 - 150 - 161 = 1320.25
    #[test]
    fn female_bmr_matches_the_reference_value() {
        assert!((bmr(Sex::Female, 60.0, 165.0, 30.0) - 1320.25).abs() < 0.001);
    }

    /// The published Mifflin-St Jeor table, worked by hand.
    ///
    /// `(sex, kg, cm, years, expected BMR)`. PRD §13 names this specifically:
    /// the formula is the one number in the system that is not allowed to drift,
    /// because everything downstream — targets, remaining budget, the verdict —
    /// is derived from it.
    const REFERENCE_BMR: &[(Sex, f64, f64, f64, f64)] = &[
        // 10·70 + 6.25·175 − 5·25 + 5 = 700 + 1093.75 − 125 + 5
        (Sex::Male, 70.0, 175.0, 25.0, 1673.75),
        // 10·100 + 6.25·190 − 5·50 + 5 = 1000 + 1187.5 − 250 + 5
        (Sex::Male, 100.0, 190.0, 50.0, 1942.5),
        // 10·60 + 6.25·170 − 5·18 + 5 = 600 + 1062.5 − 90 + 5
        (Sex::Male, 60.0, 170.0, 18.0, 1577.5),
        // 10·55 + 6.25·160 − 5·40 − 161 = 550 + 1000 − 200 − 161
        (Sex::Female, 55.0, 160.0, 40.0, 1189.0),
        // 10·70 + 6.25·170 − 5·30 − 161 = 700 + 1062.5 − 150 − 161
        (Sex::Female, 70.0, 170.0, 30.0, 1451.5),
        // 10·45 + 6.25·150 − 5·70 − 161 = 450 + 937.5 − 350 − 161
        (Sex::Female, 45.0, 150.0, 70.0, 876.5),
    ];

    #[test]
    fn bmr_matches_every_published_reference_value() {
        for (sex, kg, cm, years, expected) in REFERENCE_BMR {
            let actual = bmr(*sex, *kg, *cm, *years);
            assert!(
                (actual - expected).abs() < 0.001,
                "{sex:?} {kg}kg {cm}cm {years}y: expected {expected}, got {actual}"
            );
        }
    }

    #[test]
    fn the_sex_constant_is_the_published_166_kcal() {
        // The male and female equations differ only by +5 versus −161.
        let male = bmr(Sex::Male, 80.0, 180.0, 30.0);
        let female = bmr(Sex::Female, 80.0, 180.0, 30.0);
        assert!((male - female - 166.0).abs() < 0.001);
    }

    #[test]
    fn each_published_activity_factor_scales_cleanly() {
        // 1.2 sedentary · 1.375 light · 1.55 moderate · 1.725 heavy · 1.9 athlete.
        let base = 1780.0;
        for (factor, expected) in [
            (1.2, 2136.0),
            (1.375, 2447.5),
            (1.55, 2759.0),
            (1.725, 3070.5),
            (1.9, 3382.0),
        ] {
            assert!(
                (tdee(base, factor) - expected).abs() < 0.001,
                "activity {factor}"
            );
        }
    }

    #[test]
    fn tdee_applies_the_activity_factor() {
        assert!((tdee(1780.0, 1.55) - 2759.0).abs() < 0.001);
    }

    #[test]
    fn tdee_clamps_absurd_activity_factors() {
        assert_eq!(tdee(1000.0, 5.0), 1000.0 * ACTIVITY_VERY_ACTIVE);
        assert_eq!(tdee(1000.0, 0.1), 1000.0 * ACTIVITY_SEDENTARY);
    }

    #[test]
    fn a_half_kilo_per_week_deficit_is_550_kcal_per_day() {
        let delta = goal_delta_kcal(GoalType::Lose, 0.5);
        assert!((delta + 550.0).abs() < 0.001);
        assert_eq!(goal_delta_kcal(GoalType::Maintain, 0.5), 0.0);
        assert!(goal_delta_kcal(GoalType::Gain, 0.5) > 0.0);
    }

    #[test]
    fn macro_split_adds_back_up_to_the_target() {
        let input = TargetInputs {
            sex: Sex::Male,
            age_years: 30.0,
            height_cm: 180.0,
            weight_kg: 80.0,
            activity_factor: 1.55,
            goal_type: GoalType::Lose,
            target_weight_kg: 75.0,
            rate_kg_per_week: 0.5,
        };
        let t = compute_targets(&input);
        let from_macros = t.protein_g * KCAL_PER_G_PROTEIN
            + t.fat_g * KCAL_PER_G_FAT
            + t.carbs_g * KCAL_PER_G_CARB;
        assert!((from_macros - t.kcal).abs() < 0.5, "{from_macros} vs {}", t.kcal);
        assert!((t.protein_g - 1.8 * 75.0).abs() < 0.001);
        assert!((t.fat_g - 0.8 * 75.0).abs() < 0.001);
    }

    #[test]
    fn target_never_falls_below_the_floor() {
        let input = TargetInputs {
            sex: Sex::Female,
            age_years: 60.0,
            height_cm: 150.0,
            weight_kg: 50.0,
            activity_factor: 1.2,
            goal_type: GoalType::Lose,
            target_weight_kg: 45.0,
            rate_kg_per_week: 1.5,
        };
        assert_eq!(compute_targets(&input).kcal, MIN_TARGET_KCAL);
    }

    #[test]
    fn the_full_target_set_matches_a_hand_worked_example() {
        // 30-year-old man, 180cm, 80kg, moderately active, losing 0.5 kg/week
        // toward 75kg.
        //   BMR  = 1780
        //   TDEE = 1780 × 1.55            = 2759
        //   goal = −0.5 × 7700 / 7        = −550
        //   kcal = 2759 − 550             = 2209
        //   protein = 1.8 × 75            = 135 g   (540 kcal)
        //   fat     = 0.8 × 75            = 60 g    (540 kcal)
        //   carbs   = (2209 − 1080) / 4   = 282.25 g
        let t = compute_targets(&TargetInputs {
            sex: Sex::Male,
            age_years: 30.0,
            height_cm: 180.0,
            weight_kg: 80.0,
            activity_factor: 1.55,
            goal_type: GoalType::Lose,
            target_weight_kg: 75.0,
            rate_kg_per_week: 0.5,
        });
        assert!((t.bmr - 1780.0).abs() < 0.001);
        assert!((t.tdee - 2759.0).abs() < 0.001);
        assert!((t.kcal - 2209.0).abs() < 0.001);
        assert!((t.protein_g - 135.0).abs() < 0.001);
        assert!((t.fat_g - 60.0).abs() < 0.001);
        assert!((t.carbs_g - 282.25).abs() < 0.001);
    }

    #[test]
    fn maintenance_targets_the_tdee_exactly() {
        let t = compute_targets(&TargetInputs {
            sex: Sex::Female,
            age_years: 30.0,
            height_cm: 165.0,
            weight_kg: 60.0,
            activity_factor: 1.375,
            goal_type: GoalType::Maintain,
            target_weight_kg: 60.0,
            rate_kg_per_week: 0.0,
        });
        assert!((t.kcal - t.tdee).abs() < 0.001);
    }

    #[test]
    fn a_surplus_raises_the_target_by_the_same_arithmetic() {
        let mut input = TargetInputs {
            sex: Sex::Male,
            age_years: 25.0,
            height_cm: 175.0,
            weight_kg: 70.0,
            activity_factor: 1.55,
            goal_type: GoalType::Gain,
            target_weight_kg: 75.0,
            rate_kg_per_week: 0.25,
        };
        let gaining = compute_targets(&input);
        input.goal_type = GoalType::Maintain;
        let maintaining = compute_targets(&input);
        // 0.25 kg/week × 7700 / 7 = 275 kcal/day.
        assert!((gaining.kcal - maintaining.kcal - 275.0).abs() < 0.001);
    }

    #[test]
    fn carbs_never_go_negative_when_the_floors_eat_the_whole_budget() {
        let t = compute_targets(&TargetInputs {
            sex: Sex::Female,
            age_years: 60.0,
            height_cm: 150.0,
            weight_kg: 50.0,
            activity_factor: 1.2,
            goal_type: GoalType::Lose,
            // A 120kg target weight is nonsense for this profile, but a nonsense
            // input must not produce a negative carbohydrate allowance.
            target_weight_kg: 120.0,
            rate_kg_per_week: 1.5,
        });
        assert_eq!(t.carbs_g, 0.0);
        assert!(t.protein_g > 0.0 && t.fat_g > 0.0);
    }

    #[test]
    fn aggressive_rates_are_warned_about() {
        let mut input = TargetInputs {
            sex: Sex::Male,
            age_years: 30.0,
            height_cm: 180.0,
            weight_kg: 80.0,
            activity_factor: 1.55,
            goal_type: GoalType::Lose,
            target_weight_kg: 75.0,
            rate_kg_per_week: 0.5,
        };
        assert!(deficit_warning(&input).is_none(), "0.5kg on 80kg is 0.625%");
        input.rate_kg_per_week = 1.2;
        assert!(deficit_warning(&input).is_some(), "1.2kg on 80kg is 1.5%");
        input.goal_type = GoalType::Maintain;
        assert!(deficit_warning(&input).is_none());
    }

    #[test]
    fn the_warning_carries_the_numbers_not_just_the_sentence() {
        let input = TargetInputs {
            sex: Sex::Male,
            age_years: 30.0,
            height_cm: 180.0,
            weight_kg: 80.0,
            activity_factor: 1.55,
            goal_type: GoalType::Lose,
            target_weight_kg: 70.0,
            rate_kg_per_week: 1.2,
        };
        let warning = rate_warning(&input).expect("1.5%/week is above the threshold");
        assert_eq!(warning.rate_kg_per_week, 1.2);
        assert!((warning.bodyweight_fraction - 0.015).abs() < 1e-9);
        assert_eq!(warning.threshold_fraction, AGGRESSIVE_RATE_FRACTION);
        assert!((warning.suggested_rate_kg_per_week - 0.8).abs() < 1e-9);
        assert!(warning.message.contains("1.5%"));
        // The target is still computed — warned, not refused.
        assert!(compute_targets(&input).kcal > 0.0);
    }

    #[test]
    fn the_threshold_is_inclusive_so_exactly_one_percent_is_fine() {
        let mut input = TargetInputs {
            sex: Sex::Male,
            age_years: 30.0,
            height_cm: 180.0,
            weight_kg: 80.0,
            activity_factor: 1.55,
            goal_type: GoalType::Lose,
            target_weight_kg: 75.0,
            rate_kg_per_week: 0.8,
        };
        assert!(rate_warning(&input).is_none(), "0.8kg on 80kg is exactly 1%");
        input.rate_kg_per_week = 0.81;
        assert!(rate_warning(&input).is_some());
    }

    #[test]
    fn an_aggressive_bulk_is_warned_about_too() {
        let input = TargetInputs {
            sex: Sex::Male,
            age_years: 25.0,
            height_cm: 175.0,
            weight_kg: 70.0,
            activity_factor: 1.55,
            goal_type: GoalType::Gain,
            target_weight_kg: 80.0,
            rate_kg_per_week: 1.0,
        };
        let warning = rate_warning(&input).expect("1kg on 70kg is 1.43%");
        assert!(warning.message.contains("gaining"));
    }

    #[test]
    fn a_zero_bodyweight_never_divides_by_zero() {
        let input = TargetInputs {
            sex: Sex::Male,
            age_years: 30.0,
            height_cm: 180.0,
            weight_kg: 0.0,
            activity_factor: 1.55,
            goal_type: GoalType::Lose,
            target_weight_kg: 75.0,
            rate_kg_per_week: 1.0,
        };
        assert!(rate_warning(&input).is_none());
    }

    #[test]
    fn age_accounts_for_the_birthday_not_yet_reached() {
        let born = NaiveDate::from_ymd_opt(1990, 12, 31).unwrap();
        assert_eq!(age_years(born, NaiveDate::from_ymd_opt(2026, 1, 1).unwrap()), 35.0);
        assert_eq!(
            age_years(born, NaiveDate::from_ymd_opt(2026, 12, 31).unwrap()),
            36.0
        );
    }
}
