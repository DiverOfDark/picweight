//! System prompts and the fixed critique checklist (PRD §5).
//!
//! [`PROMPT_VERSION`] is stored on every `agent_sessions` row. When it differs
//! from the version a session began under, [`super::session::decide`] reseeds
//! instead of continuing — otherwise a correction turn would run under stale
//! instructions.
//!
//! **Bump [`PROMPT_VERSION`] whenever any string in this module changes
//! materially.** That is the entire mechanism; nothing detects drift for you.

use super::AnalysisContext;

/// Version of the prompt set. Stored on `agent_sessions.prompt_version`.
pub const PROMPT_VERSION: &str = "2026-08-01.2";

/// How far [`super::AnalysisContext::calibration_factor`] must sit from 1.0
/// before it is worth telling the model about.
///
/// Below this the correction is noise and mentioning it would only add a
/// distraction to every prompt.
pub const CALIBRATION_MENTION_THRESHOLD: f64 = 0.02;

/// The mandatory self-check the agent runs before emitting (§5, step 4).
pub const CRITIQUE_CHECKLIST: &str = "\
Before you emit, run this checklist once and adjust:
1. Is this restaurant or delivery food? If so, assume noticeably more cooking \
oil and fat than a home recipe of the same dish. Restaurant portions are also \
systematically larger.
2. Did you count everything under and behind the visible layer — the rice under \
the curry, the noodles under the sauce, the bread under the topping?
3. Did you count the dressing, sauce, mayo, sour cream, oil sheen and cheese? \
These are the single most common source of underestimation.
4. Is the total mass consistent with the container you identified? A standard \
delivery bowl does not hold 900g of food; a 30cm pizza box does not hold 200g.
5. Do the macros multiply out? kcal should be within ~10% of \
4*protein + 4*carbs + 9*fat. If not, one of the four numbers is wrong.
Adjust and re-emit. Do not narrate the checklist in your answer.";

/// System preamble for the full agentic loop.
pub fn system_preamble(web_search_enabled: bool) -> String {
    let web_search_note = if web_search_enabled {
        "\n- web_search(query): published nutrition for chain restaurants. Use \
only when the dish is from an identifiable chain."
    } else {
        ""
    };

    format!(
        "You estimate the calories and macronutrients of a meal from a single \
photograph, for a self-hosted food tracker.

Work in these steps:

1. IDENTIFY. Name the dish, its cuisine, its visible components, and — most \
importantly — the CONTAINER. A standard delivery bowl, a 30cm pizza box, a 0.5L \
cup, a 25cm dinner plate. With no user comment and no coin on the plate, the \
container is your only reliable scale reference. Commit to one and state it.

2. RECALL. Call recall_similar_meals FIRST, always, before estimating anything. \
It searches this user's own previously confirmed meals. A confident hit is \
ground truth the user already vetted — prefer it over your visual read, adjust \
it only for a visibly different portion, and say so in the reasoning note.

3. ESTIMATE. Give per-item edible grams AND per-item kcal, protein, fat and \
carbs directly. Do not emit per-100g figures; emit the numbers for the grams \
you stated.

4. CRITIQUE. {CRITIQUE_CHECKLIST}

5. EMIT. Return the structured object. Every item carries a one-line \
reasoning_note explaining why that number — the user reads it.

Tools:
- recall_similar_meals(query): this user's confirmed history. Always first.
- lookup_barcode(ean): Open Food Facts. Packaged goods and drinks only, and \
only when an EAN is actually available.{web_search_note}

Rules:
- If the user supplied a dish name, it OUTRANKS your visual read. Do not \
second-guess it; use it and estimate the portion.
- If the user supplied a comment, it outranks everything except a barcode.
- Never return an empty item list. A blurry photo still gets your best guess \
with low confidence.
- Confidence is 0.0-1.0 and should be honest. A recalled dish is high; an \
unidentifiable brown stew is low.
- Answer with the structured object only."
    )
}

/// System preamble for the single-shot fallback.
///
/// Used when the loop breaches `max_turns`, a tool fails, or the wall-clock
/// budget expires — the user always gets a draft (§5 bounds). No tools, one
/// model call, same output shape.
pub fn single_shot_preamble() -> String {
    format!(
        "You estimate the calories and macronutrients of a meal from a single \
photograph. You have no tools on this call — answer directly from the image.

Identify the dish and, crucially, the CONTAINER, which is your only scale \
reference. Then give per-item edible grams and per-item kcal, protein, fat and \
carbs.

{CRITIQUE_CHECKLIST}

If the user supplied a dish name or comment, it outranks your visual read. \
Never return an empty item list. Answer with the structured object only."
    )
}

/// The user turn that accompanies the photo on a first analysis.
pub fn analysis_user_turn(ctx: &AnalysisContext) -> String {
    let mut parts = Vec::new();

    match (&ctx.dish_name, &ctx.user_comment) {
        (Some(name), Some(comment)) => {
            parts.push(format!(
                "The user says this is: {name}\nAnd added: {comment}\n\
Both outrank your visual read. Estimate the portion."
            ));
        }
        (Some(name), None) => {
            parts.push(format!(
                "The user says this is: {name}\n\
That name outranks your visual read. Estimate the portion."
            ));
        }
        (None, Some(comment)) => {
            parts.push(format!("The user added: {comment}"));
        }
        (None, None) => {
            parts.push(
                "No dish name and no comment were supplied — this is the common case. \
Identify the dish yourself and lean hard on the container for scale."
                    .to_string(),
            );
        }
    }

    parts.push(format!(
        "Eaten at {} (local time).",
        ctx.eaten_at.format("%Y-%m-%d %H:%M")
    ));

    if !ctx.recent_dishes.is_empty() {
        parts.push(format!(
            "This user's recent confirmed dishes, for context: {}.",
            ctx.recent_dishes.join("; ")
        ));
    }

    if let Some(line) = calibration_line(ctx.calibration_factor) {
        parts.push(line);
    }

    if ctx.image_path.is_none() {
        parts.push(
            "There is NO photograph for this meal — it was logged manually. \
Estimate from the name and any stated size alone, and lower your confidence \
accordingly."
                .to_string(),
        );
    }

    parts.join("\n\n")
}

/// Render the per-user calibration prior, or `None` when it is close enough to
/// 1.0 to be noise.
///
/// PRD §5's answer to hidden fat: a multiplier learned from this user's own
/// correction history. It is handed to the **model** rather than applied to the
/// result arithmetically, on purpose — the model can apply it where it belongs
/// (the oily restaurant dish) and skip it where it obviously does not (a canned
/// drink with a printed label), and the per-item `reasoning_note` then still
/// matches the numbers next to it. **Nothing scales the estimate numerically;
/// this line is the only place the factor is used.**
pub fn calibration_line(calibration_factor: f64) -> Option<String> {
    if !calibration_factor.is_finite()
        || (calibration_factor - 1.0).abs() < CALIBRATION_MENTION_THRESHOLD
    {
        return None;
    }
    let percent = (calibration_factor - 1.0) * 100.0;
    let direction = if percent > 0.0 { "under" } else { "over" };
    Some(format!(
        "Calibration: this user's own corrections say estimates for them have run \
{direction} by about {:.0}%. Lean that way where it plausibly applies — cooking \
oil, sauce and portion size — but do not apply it blindly to a packaged item \
with a printed label.",
        percent.abs()
    ))
}

/// The user turn appended when resuming a session with feedback (§5).
pub fn reanalysis_turn(feedback: &str) -> String {
    format!(
        "The user reviewed your estimate and says:\n\n{feedback}\n\n\
Apply this against your own prior reasoning — you already know which container \
you identified, what recall returned, and what you ruled out. Do NOT re-run \
tools you have already called; their results are in this conversation. Adjust \
only what the feedback implies, keep everything else stable, and re-emit the \
full structured object."
    )
}

/// Seed text used when a session cannot be continued (model or prompt version
/// changed) and a fresh conversation must start from the last confirmed result.
pub fn reseed_context(previous: &super::schema::MealEstimate, feedback: &str) -> String {
    let items = previous
        .items
        .iter()
        .map(|item| {
            format!(
                "- {} — {:.0}g, {:.0} kcal (P{:.0} F{:.0} C{:.0})",
                item.name, item.grams, item.kcal, item.protein_g, item.fat_g, item.carbs_g
            )
        })
        .collect::<Vec<_>>()
        .join("\n");

    format!(
        "This meal was previously estimated as \"{}\"{}:\n{items}\n\n\
That estimate was produced by an earlier prompt or model version, so this is a \
fresh conversation seeded with its result rather than a continuation.\n\n\
The user now says:\n\n{feedback}\n\n\
Apply the feedback to the figures above and emit the full structured object.",
        previous.dish_name,
        previous
            .container
            .as_ref()
            .map(|c| format!(" in {c}"))
            .unwrap_or_default(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn preamble_mentions_the_container_and_recall_order() {
        let preamble = system_preamble(false);
        assert!(preamble.contains("CONTAINER"));
        assert!(preamble.contains("recall_similar_meals FIRST"));
        assert!(!preamble.contains("web_search("));
    }

    #[test]
    fn web_search_is_only_advertised_when_enabled() {
        assert!(system_preamble(true).contains("web_search(query)"));
    }

    #[test]
    fn reanalysis_turn_forbids_repeat_tool_calls() {
        let turn = reanalysis_turn("too much rice, it was about half that");
        assert!(turn.contains("Do NOT re-run tools"));
        assert!(turn.contains("half that"));
    }

    #[test]
    fn a_neutral_calibration_factor_is_not_mentioned() {
        assert!(calibration_line(1.0).is_none());
        assert!(calibration_line(1.01).is_none());
        assert!(calibration_line(f64::NAN).is_none());
    }

    #[test]
    fn a_meaningful_calibration_factor_names_its_direction() {
        let under = calibration_line(1.15).expect("15% is worth mentioning");
        assert!(under.contains("under"), "{under}");
        assert!(under.contains("15%"), "{under}");

        let over = calibration_line(0.9).expect("10% is worth mentioning");
        assert!(over.contains("over"), "{over}");
        assert!(over.contains("10%"), "{over}");
    }

    #[test]
    fn the_user_turn_carries_the_calibration_prior() {
        let ctx = super::super::AnalysisContext {
            user_id: "u1".into(),
            meal_id: "m1".into(),
            job_id: "j1".into(),
            revision: 1,
            dish_name: None,
            user_comment: None,
            image_path: Some(std::path::PathBuf::from("/tmp/thumb.jpg")),
            eaten_at: chrono::NaiveDate::from_ymd_opt(2026, 8, 1)
                .expect("valid date")
                .and_hms_opt(13, 30, 0)
                .expect("valid time"),
            recent_dishes: vec!["шаурма с курицей".into()],
            calibration_factor: 1.12,
        };
        let turn = analysis_user_turn(&ctx);
        assert!(turn.contains("No dish name and no comment"), "{turn}");
        assert!(turn.contains("2026-08-01 13:30"), "{turn}");
        assert!(turn.contains("шаурма с курицей"), "{turn}");
        assert!(turn.contains("Calibration"), "{turn}");
        assert!(!turn.contains("NO photograph"), "{turn}");
    }
}
