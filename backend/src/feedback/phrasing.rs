//! Wording for the per-meal notification (PRD §6).
//!
//! The rules engine in [`super::state`] decides *what is true*; this module
//! decides *how to say it*. The LLM supplies only the verdict line, and
//! [`phrase_verdict`] can never fail — an unavailable model degrades to
//! [`templated_verdict`], never to an error.

use super::state::{DayState, DayStatus};
use crate::AppState;
use std::time::Duration;

/// Chat-completions endpoint against OpenAI's own API root.
///
/// This is a single short completion, not an agent run, so it deliberately does
/// **not** go through [`crate::agent`]: nothing here needs tools, structured
/// output, a turn budget or a persisted session, and routing it through the rig
/// boundary would couple notification copy to the estimation loop's lifetime.
///
/// The call itself uses [`crate::Config::chat_completions_url`], so a proxy or
/// a test mock is picked up here exactly as it is by the agent.
pub const CHAT_COMPLETIONS_URL: &str = "https://api.openai.com/v1/chat/completions";

/// Wall-clock budget for the verdict call.
///
/// Short on purpose. The notification fires the moment analysis completes; a
/// slow model must cost the user the *wording*, never the notification.
pub const VERDICT_TIMEOUT: Duration = Duration::from_secs(4);

/// Output cap. One sentence needs nothing more, and a runaway generation would
/// only be truncated by [`clean_verdict`] anyway.
pub const VERDICT_MAX_TOKENS: u32 = 80;

/// Longest verdict rendered. Anything beyond this is cut at a word boundary.
pub const VERDICT_MAX_CHARS: usize = 180;

/// Instructions for the verdict model.
pub const VERDICT_SYSTEM: &str = "You write the closing line of a calorie-tracking \
notification. You are given a day's numbers, already computed. Reply with ONE \
sentence of at most 140 characters, addressed to the user as \"you\". Be concrete \
and practical about what is left to eat. Never restate the dish name, never \
invent numbers, never contradict the ones you were given, never use markdown, \
emoji or quotation marks. If the day is over budget, be matter-of-fact rather \
than scolding.";

/// Line 1 — what was logged. Leads with the dish name because the notification
/// has to make sense read cold, minutes later.
pub fn notification_headline(dish_name: &str, kcal: f64) -> String {
    format!("{dish_name} — {} kcal", thousands(kcal))
}

/// Line 1 for a settled sitting: "5 dishes — 2,140 kcal".
pub fn group_notification_headline(dish_count: usize, kcal: f64) -> String {
    let noun = if dish_count == 1 { "dish" } else { "dishes" };
    format!("{dish_count} {noun} — {} kcal", thousands(kcal))
}

/// Line 2 — where you stand.
pub fn standing_line(day: &DayState) -> String {
    if day.status == DayStatus::NoTargets {
        return format!("{} kcal today", thousands(day.consumed_kcal));
    }
    if day.remaining_kcal < 0.0 {
        format!(
            "{} / {} today · {} over",
            thousands(day.consumed_kcal),
            thousands(day.target_kcal),
            thousands(-day.remaining_kcal)
        )
    } else {
        format!(
            "{} / {} today · {} left",
            thousands(day.consumed_kcal),
            thousands(day.target_kcal),
            thousands(day.remaining_kcal)
        )
    }
}

/// Line 3 — protein against the floor, the constraint that actually binds.
pub fn macro_line(day: &DayState) -> String {
    if day.target_protein_g <= 0.0 {
        return format!("Protein {}g", thousands(day.consumed_protein_g));
    }
    let short = day.remaining_protein_g.max(0.0);
    if short <= 0.0 {
        return format!(
            "Protein {}/{}g — floor met",
            thousands(day.consumed_protein_g),
            thousands(day.target_protein_g)
        );
    }
    format!(
        "Protein {}/{}g — {}g short with {} kcal to spend",
        thousands(day.consumed_protein_g),
        thousands(day.target_protein_g),
        thousands(short),
        thousands(day.remaining_kcal.max(0.0))
    )
}

/// Line 4 — the deterministic verdict, always available.
///
/// This is the fallback the LLM phrasing degrades to, and it is also what tests
/// assert against, so it must stay stable and non-random.
pub fn templated_verdict(day: &DayState) -> String {
    match day.status {
        DayStatus::NoTargets => {
            "Set up your profile to see what's left in the tank.".to_string()
        }
        DayStatus::Over => format!(
            "You're {} kcal over. Not a disaster — just keep dinner light.",
            thousands(-day.remaining_kcal)
        ),
        DayStatus::ProteinUnreachable => format!(
            "{}g of protein won't fit in {} kcal. Get as close as you can and \
accept the shortfall.",
            thousands(day.remaining_protein_g),
            thousands(day.remaining_kcal.max(0.0))
        ),
        DayStatus::Tight => format!(
            "{} kcal left — that's one small meal, so make it count.",
            thousands(day.remaining_kcal)
        ),
        DayStatus::OnTrack => {
            if day.remaining_protein_g > 0.0 {
                format!(
                    "{} kcal left and {}g of protein to find. Doable, but it has \
to be mostly protein.",
                    thousands(day.remaining_kcal),
                    thousands(day.remaining_protein_g)
                )
            } else {
                format!("{} kcal left. Comfortable.", thousands(day.remaining_kcal))
            }
        }
    }
}

/// One-line verdict, LLM-phrased from the numbers.
///
/// **Never fails.** On any error — model unavailable, quota exhausted, timeout,
/// an empty or unusable completion — it returns [`templated_verdict`].
/// Notification copy is not worth a hard failure (§6).
///
/// The rules engine has already decided *what is true*; the model is only asked
/// to say it well, and is handed the finished figures rather than any raw data.
pub async fn phrase_verdict(state: &AppState, day: &DayState, headline: &str) -> String {
    let fallback = templated_verdict(day);

    // Nothing to phrase: the honest answer is "finish onboarding", and the
    // template already says it better than a model would.
    if day.status == DayStatus::NoTargets {
        return fallback;
    }

    match llm_verdict(state, day, headline).await {
        Ok(Some(verdict)) => verdict,
        Ok(None) => {
            tracing::debug!("verdict model returned nothing usable; using the template");
            fallback
        }
        Err(err) => {
            tracing::debug!(error = %err, "verdict model unavailable; using the template");
            fallback
        }
    }
}

/// One short completion, or `None` when the answer is unusable.
async fn llm_verdict(
    state: &AppState,
    day: &DayState,
    headline: &str,
) -> Result<Option<String>, crate::error::AppError> {
    let request = serde_json::json!({
        "model": state.config.openai_model,
        "messages": [
            { "role": "system", "content": VERDICT_SYSTEM },
            { "role": "user", "content": verdict_prompt(day, headline) },
        ],
        "max_completion_tokens": VERDICT_MAX_TOKENS,
    });

    let response = state
        .http
        .post(state.config.chat_completions_url())
        .bearer_auth(&state.config.openai_api_key)
        .json(&request)
        .timeout(VERDICT_TIMEOUT)
        .send()
        .await?;

    if !response.status().is_success() {
        return Err(crate::error::AppError::Upstream(format!(
            "verdict model returned {}",
            response.status()
        )));
    }

    let body: serde_json::Value = response.json().await?;
    Ok(parse_verdict_response(&body))
}

/// The user turn handed to the verdict model.
///
/// Pure and testable: the model never sees a row, only the figures the rules
/// engine already settled on.
pub fn verdict_prompt(day: &DayState, headline: &str) -> String {
    let status = match day.status {
        DayStatus::OnTrack => "on track",
        DayStatus::Tight => "tight — very little energy left",
        DayStatus::Over => "over the energy target",
        DayStatus::ProteinUnreachable => {
            "the protein floor can no longer be reached within the remaining energy"
        }
        DayStatus::NoTargets => "no targets computed",
    };
    format!(
        "Just logged: {headline}\n\
         Day: {date}\n\
         Energy: {consumed:.0} of {target:.0} kcal consumed, {remaining:.0} kcal remaining\n\
         Protein: {p_consumed:.0} of {p_target:.0} g, {p_remaining:.0} g still owed\n\
         Fat: {f_consumed:.0} of {f_target:.0} g\n\
         Carbs: {c_consumed:.0} of {c_target:.0} g\n\
         Meals logged today: {meals}\n\
         Verdict class: {status}",
        date = day.date,
        consumed = day.consumed_kcal,
        target = day.target_kcal,
        remaining = day.remaining_kcal,
        p_consumed = day.consumed_protein_g,
        p_target = day.target_protein_g,
        p_remaining = day.remaining_protein_g,
        f_consumed = day.consumed_fat_g,
        f_target = day.target_fat_g,
        c_consumed = day.consumed_carbs_g,
        c_target = day.target_carbs_g,
        meals = day.meals_logged,
    )
}

/// Pull the verdict out of a chat-completions body.
pub fn parse_verdict_response(body: &serde_json::Value) -> Option<String> {
    let content = body
        .get("choices")?
        .get(0)?
        .get("message")?
        .get("content")?
        .as_str()?;
    clean_verdict(content)
}

/// Normalize whatever the model said into one printable line.
///
/// Models add quotation marks, leading bullets and second paragraphs no matter
/// how firmly the system prompt forbids them; stripping is cheaper than retrying.
pub fn clean_verdict(raw: &str) -> Option<String> {
    let line = raw
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty())?
        .trim_start_matches(['-', '*', '•'])
        .trim();

    let line = line
        .trim_matches(|c| c == '"' || c == '\'' || c == '«' || c == '»')
        .trim();

    let collapsed = line.split_whitespace().collect::<Vec<_>>().join(" ");
    if collapsed.is_empty() {
        return None;
    }

    Some(truncate_on_word(&collapsed, VERDICT_MAX_CHARS))
}

/// Cut a string to at most `max_chars` characters, preferring a word boundary.
fn truncate_on_word(text: &str, max_chars: usize) -> String {
    if text.chars().count() <= max_chars {
        return text.to_string();
    }
    let cut: String = text.chars().take(max_chars).collect();
    match cut.rfind(' ') {
        // Only back off to the last space when it is not right at the start,
        // or a single very long word would truncate to nothing.
        Some(idx) if idx > max_chars / 2 => format!("{}…", cut[..idx].trim_end()),
        _ => format!("{}…", cut.trim_end()),
    }
}

/// Render a kcal/gram figure with thousands separators and no decimals.
fn thousands(value: f64) -> String {
    let rounded = value.round().abs() as u64;
    let sign = if value.round() < 0.0 { "-" } else { "" };
    let digits = rounded.to_string();
    let mut out = String::with_capacity(digits.len() + digits.len() / 3 + 1);
    for (i, ch) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(ch);
    }
    format!("{sign}{out}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::NaiveDate;

    fn day(status: DayStatus) -> DayState {
        DayState {
            date: NaiveDate::from_ymd_opt(2026, 8, 1).unwrap(),
            consumed_kcal: 1450.0,
            target_kcal: 2050.0,
            remaining_kcal: 600.0,
            consumed_protein_g: 82.0,
            target_protein_g: 165.0,
            remaining_protein_g: 83.0,
            consumed_fat_g: 50.0,
            target_fat_g: 60.0,
            consumed_carbs_g: 120.0,
            target_carbs_g: 200.0,
            meals_logged: 3,
            status,
        }
    }

    #[test]
    fn the_prd_example_renders() {
        // §6: "Шаурма с курицей — 780 kcal / 1,450 / 2,050 today · 600 left /
        //      Protein 82/165g — 83g short with 600 kcal to spend"
        assert_eq!(
            notification_headline("Шаурма с курицей", 780.0),
            "Шаурма с курицей — 780 kcal"
        );
        let d = day(DayStatus::OnTrack);
        assert_eq!(standing_line(&d), "1,450 / 2,050 today · 600 left");
        assert_eq!(
            macro_line(&d),
            "Protein 82/165g — 83g short with 600 kcal to spend"
        );
    }

    #[test]
    fn thousands_separates_correctly() {
        assert_eq!(thousands(0.0), "0");
        assert_eq!(thousands(780.4), "780");
        assert_eq!(thousands(1450.0), "1,450");
        assert_eq!(thousands(21450.0), "21,450");
        assert_eq!(thousands(-600.0), "-600");
    }

    #[test]
    fn group_headline_pluralizes() {
        assert_eq!(
            group_notification_headline(5, 2140.0),
            "5 dishes — 2,140 kcal"
        );
        assert_eq!(group_notification_headline(1, 780.0), "1 dish — 780 kcal");
    }

    #[test]
    fn every_status_produces_a_verdict() {
        for status in [
            DayStatus::OnTrack,
            DayStatus::Tight,
            DayStatus::Over,
            DayStatus::ProteinUnreachable,
            DayStatus::NoTargets,
        ] {
            assert!(!templated_verdict(&day(status)).is_empty());
        }
    }

    #[test]
    fn the_templated_verdict_is_deterministic() {
        // It is the fallback *and* what the tests assert against, so it must
        // never drift between two calls on identical input.
        let d = day(DayStatus::Tight);
        assert_eq!(templated_verdict(&d), templated_verdict(&d));
    }

    #[test]
    fn the_prompt_carries_the_settled_figures_and_the_class() {
        let prompt = verdict_prompt(&day(DayStatus::ProteinUnreachable), "Шаурма — 780 kcal");
        assert!(prompt.contains("Шаурма — 780 kcal"));
        assert!(prompt.contains("1450 of 2050 kcal consumed, 600 kcal remaining"));
        assert!(prompt.contains("82 of 165 g, 83 g still owed"));
        assert!(prompt.contains("protein floor can no longer be reached"));
    }

    #[test]
    fn a_normal_completion_is_extracted() {
        let body = serde_json::json!({
            "choices": [{ "message": { "content": "600 kcal left — make it chicken." } }]
        });
        assert_eq!(
            parse_verdict_response(&body).as_deref(),
            Some("600 kcal left — make it chicken.")
        );
    }

    #[test]
    fn a_malformed_completion_yields_nothing_rather_than_panicking() {
        for body in [
            serde_json::json!({}),
            serde_json::json!({ "choices": [] }),
            serde_json::json!({ "choices": [{}] }),
            serde_json::json!({ "choices": [{ "message": {} }] }),
            serde_json::json!({ "choices": [{ "message": { "content": null } }] }),
            serde_json::json!({ "choices": [{ "message": { "content": "   " } }] }),
            serde_json::json!({ "error": { "message": "insufficient_quota" } }),
        ] {
            assert!(parse_verdict_response(&body).is_none(), "{body}");
        }
    }

    #[test]
    fn model_decoration_is_stripped() {
        assert_eq!(
            clean_verdict("\"600 kcal left.\"").as_deref(),
            Some("600 kcal left.")
        );
        assert_eq!(
            clean_verdict("- 600 kcal left.\n\nAnything else?").as_deref(),
            Some("600 kcal left.")
        );
        assert_eq!(
            clean_verdict("  600   kcal\tleft. ").as_deref(),
            Some("600 kcal left.")
        );
        assert_eq!(clean_verdict("\n\n").as_deref(), None);
    }

    #[test]
    fn a_runaway_generation_is_truncated_on_a_word_boundary() {
        let long = "word ".repeat(200);
        let cleaned = clean_verdict(&long).unwrap();
        assert!(cleaned.chars().count() <= VERDICT_MAX_CHARS + 1);
        assert!(cleaned.ends_with('…'));
        assert!(!cleaned.contains("  "));
    }

    #[test]
    fn a_single_enormous_token_still_truncates() {
        let cleaned = clean_verdict(&"я".repeat(500)).unwrap();
        assert_eq!(cleaned.chars().count(), VERDICT_MAX_CHARS + 1);
    }
}
