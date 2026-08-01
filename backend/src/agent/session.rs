//! Persisted, resumable agent sessions (PRD §5).
//!
//! `rig::completion::Message` derives `Serialize + Deserialize`, so persistence
//! is `serde_json` in both directions and `AgentRunner::history()` takes the
//! restored thread straight back (docs/rig-spike.md). That makes
//! correction-by-conversation a few lines rather than a project.
//!
//! Two engineering caveats the PRD asks to *handle*, not discover:
//!
//! * **Context grows per revision.** [`cap_history`] keeps at most
//!   [`MAX_STORED_TURNS`] messages, dropping the oldest tool-result payloads
//!   first — they are the bulkiest and the least useful on a correction turn.
//! * **A session pins the prompt and model it began with.** [`decide`] compares
//!   both and returns [`ResumeDecision::Reseed`] when either moved, so an old
//!   thread is never continued under stale instructions.

use super::STRIPPED_IMAGE_PLACEHOLDER;
use crate::error::AppError;
use crate::models::{AgentSession, NewAgentSession};
use diesel::prelude::*;
use diesel::SqliteConnection;
use rig::completion::Message;
use rig::message::{ToolResultContent, UserContent};
use rig::OneOrMany;

/// Maximum number of messages retained in a stored thread.
pub const MAX_STORED_TURNS: usize = 20;

/// Tool-result payloads older than the last this-many messages are elided down
/// to [`ELIDED_TOOL_RESULT_CHARS`] when a thread is capped.
///
/// The most recent exchanges are what a correction turn actually reasons
/// against; a recall payload from four revisions ago is pure token weight.
pub const RECENT_MESSAGES_KEPT_VERBATIM: usize = 6;

/// How much of an elided tool result survives.
pub const ELIDED_TOOL_RESULT_CHARS: usize = 400;

/// A loaded session with its thread already deserialized.
#[derive(Debug, Clone)]
pub struct StoredSession {
    /// `agent_sessions.id`.
    pub id: String,
    /// The meal this thread belongs to.
    pub meal_id: String,
    /// The deserialized message thread.
    pub messages: Vec<Message>,
    /// Model the session began under.
    pub model: String,
    /// Prompt version the session began under.
    pub prompt_version: String,
    /// Messages in the stored thread, before capping.
    pub turn_count: i32,
}

/// Whether a stored session can be continued or must be reseeded.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResumeDecision {
    /// Continue the existing thread — the cheap, convergent path.
    Continue,
    /// Start fresh, seeded with the last confirmed result, because the model or
    /// prompt version changed materially.
    Reseed,
}

impl ResumeDecision {
    /// Stable machine-readable name, recorded on the `session_resume`
    /// `agent_steps` row so the chosen path is auditable (§5).
    pub fn as_str(self) -> &'static str {
        match self {
            ResumeDecision::Continue => "continue",
            ResumeDecision::Reseed => "reseed",
        }
    }
}

impl std::fmt::Display for ResumeDecision {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Load and deserialize the session for a meal, if one exists.
///
/// A thread that no longer deserializes — because a rig upgrade changed the
/// `Message` wire shape — is treated as *absent* rather than as an error: the
/// correction then reseeds from the last confirmed result, which is exactly the
/// fallback the reseed rule already exists for. Failing the user's correction
/// because of an old blob would be the wrong trade.
pub fn load(conn: &mut SqliteConnection, meal_id: &str) -> Result<Option<StoredSession>, AppError> {
    let Some(row) = load_row(conn, meal_id)? else {
        return Ok(None);
    };

    let messages: Vec<Message> = match serde_json::from_str(&row.messages) {
        Ok(messages) => messages,
        Err(err) => {
            tracing::warn!(
                meal_id = %meal_id,
                error = %err,
                "stored agent session is unreadable; reseeding instead of continuing"
            );
            return Ok(None);
        }
    };

    Ok(Some(StoredSession {
        id: row.id,
        meal_id: row.meal_id,
        messages,
        model: row.model,
        prompt_version: row.prompt_version,
        turn_count: row.turn_count,
    }))
}

/// Load the raw row without deserializing the thread.
pub fn load_row(
    conn: &mut SqliteConnection,
    meal_id: &str,
) -> Result<Option<AgentSession>, AppError> {
    use crate::schema::agent_sessions;

    agent_sessions::table
        .filter(agent_sessions::meal_id.eq(meal_id))
        .select(AgentSession::as_select())
        .first(conn)
        .optional()
        .map_err(AppError::from)
}

/// Insert or replace the session for a meal.
///
/// `meal_id` is `UNIQUE`, so this is an upsert: one live thread per meal, which
/// each new revision extends.
pub fn save(
    conn: &mut SqliteConnection,
    meal_id: &str,
    messages: &[Message],
    model: &str,
    prompt_version: &str,
) -> Result<(), AppError> {
    let capped = cap_history(messages.to_vec(), MAX_STORED_TURNS);
    let turn_count = capped.len() as i32;
    let serialized = serde_json::to_string(&capped)?;
    save_serialized(conn, meal_id, &serialized, turn_count, model, prompt_version)
}

/// Upsert an already-serialized thread.
///
/// The analysis worker holds [`super::AgentOutcome::serialized_messages`], which
/// is capped and encoded once at the end of the run; re-decoding it just to
/// re-encode it would be pure waste, so this is the path it should use.
pub fn save_serialized(
    conn: &mut SqliteConnection,
    meal_id: &str,
    serialized_messages: &str,
    turn_count: i32,
    model: &str,
    prompt_version: &str,
) -> Result<(), AppError> {
    use crate::schema::agent_sessions;

    let now = chrono::Utc::now().naive_utc();
    let updated = diesel::update(agent_sessions::table.filter(agent_sessions::meal_id.eq(meal_id)))
        .set((
            agent_sessions::messages.eq(serialized_messages),
            agent_sessions::model.eq(model),
            agent_sessions::prompt_version.eq(prompt_version),
            agent_sessions::turn_count.eq(turn_count),
            agent_sessions::updated_at.eq(now),
        ))
        .execute(conn)?;

    if updated == 0 {
        let row = new_session_row(
            meal_id,
            serialized_messages.to_string(),
            turn_count,
            model,
            prompt_version,
        );
        diesel::insert_into(agent_sessions::table)
            .values(&row)
            .execute(conn)?;
    }

    Ok(())
}

/// Build the insert row for a new session.
pub fn new_session_row(
    meal_id: &str,
    serialized_messages: String,
    turn_count: i32,
    model: &str,
    prompt_version: &str,
) -> NewAgentSession {
    let now = chrono::Utc::now().naive_utc();
    NewAgentSession {
        id: uuid::Uuid::new_v4().to_string(),
        meal_id: meal_id.to_string(),
        messages: serialized_messages,
        model: model.to_string(),
        prompt_version: prompt_version.to_string(),
        turn_count,
        created_at: now,
        updated_at: now,
    }
}

/// Delete the session for a meal (used when the meal itself is deleted and the
/// cascade is not in play, e.g. a manual reset).
pub fn delete(conn: &mut SqliteConnection, meal_id: &str) -> Result<(), AppError> {
    use crate::schema::agent_sessions;

    diesel::delete(agent_sessions::table.filter(agent_sessions::meal_id.eq(meal_id)))
        .execute(conn)?;
    Ok(())
}

/// Continue-vs-reseed, per PRD §5.
///
/// A session is continued only when both the model and the prompt version still
/// match. Either changing means the stored reasoning was produced under
/// different instructions, and continuing it would silently mix the two.
pub fn decide(session: &StoredSession, model: &str, prompt_version: &str) -> ResumeDecision {
    if session.model == model && session.prompt_version == prompt_version {
        ResumeDecision::Continue
    } else {
        ResumeDecision::Reseed
    }
}

/// Replace every image in a thread with a short text placeholder.
///
/// PRD §5: "Images are re-attached from disk rather than assumed to survive in
/// serialized history — provider message formats vary on image retention." That
/// makes a base64 photograph in the stored thread strictly harmful: it is
/// ~110KB of JSON per session row, it would be re-sent alongside the freshly
/// attached copy on every correction, and it is the one piece of the
/// conversation that is trivially reconstructible from `thumbnails.path`.
///
/// The placeholder keeps the turn structure intact so the assistant's later
/// references to "the photo" still have an antecedent.
pub fn strip_images(messages: Vec<Message>) -> Vec<Message> {
    messages
        .into_iter()
        .map(|message| {
            let Message::User { content } = message else {
                return message;
            };
            let stripped: Vec<UserContent> = content
                .iter()
                .map(|part| match part {
                    UserContent::Image(_) => UserContent::text(STRIPPED_IMAGE_PLACEHOLDER),
                    other => other.clone(),
                })
                .collect();
            match OneOrMany::many(stripped) {
                Ok(content) => Message::User { content },
                // Unreachable: the input was non-empty, and the map is 1:1.
                Err(_) => Message::User { content },
            }
        })
        .collect()
}

/// Trim a thread to at most `max_turns` messages.
///
/// Keeps the oldest message (the image/system turn that anchors the
/// conversation) and the most recent `max_turns - 1`, so a correction turn
/// still sees what it is correcting.
///
/// Two repairs are applied on the way:
///
/// * **Orphan tool results are dropped.** Slicing out the middle of a thread can
///   leave a tool *result* whose matching tool *call* was discarded. Providers
///   reject a conversation containing an answer to a call they cannot see, so
///   any tool-result message that ends up leading the retained tail goes too.
/// * **Old tool-result payloads are elided.** Everything before the last
///   [`RECENT_MESSAGES_KEPT_VERBATIM`] messages has its tool-result text cut to
///   [`ELIDED_TOOL_RESULT_CHARS`]. That is the PRD's "drop the oldest
///   tool-result payloads first": they are the bulkiest content in the thread
///   and the least useful on a correction turn, and shrinking them in place
///   keeps every call/result pair intact.
pub fn cap_history(messages: Vec<Message>, max_turns: usize) -> Vec<Message> {
    if max_turns == 0 {
        return Vec::new();
    }
    if messages.len() <= max_turns {
        return messages;
    }
    let mut iter = messages.into_iter();
    // `messages.len() > max_turns >= 1`, so there is always a first element.
    let first = iter.next();
    let rest: Vec<Message> = iter.collect();
    let keep = max_turns - 1;
    let skip = rest.len() - keep;

    let mut capped = Vec::with_capacity(max_turns);
    capped.extend(first);
    capped.extend(
        rest.into_iter()
            .skip(skip)
            // A retained tail may open with the answer to a tool call that was
            // just discarded; that dangling result is what providers reject.
            .skip_while(is_tool_result_message),
    );

    elide_old_tool_results(&mut capped, RECENT_MESSAGES_KEPT_VERBATIM);
    capped
}

/// True for a user message that carries nothing but tool results.
fn is_tool_result_message(message: &Message) -> bool {
    match message {
        Message::User { content } => content
            .iter()
            .all(|part| matches!(part, UserContent::ToolResult(_))),
        _ => false,
    }
}

/// Shrink tool-result payloads in every message except the last `keep_recent`.
fn elide_old_tool_results(messages: &mut [Message], keep_recent: usize) {
    let cutoff = messages.len().saturating_sub(keep_recent);
    for message in messages.iter_mut().take(cutoff) {
        let Message::User { content } = message else {
            continue;
        };
        for part in content.iter_mut() {
            let UserContent::ToolResult(result) = part else {
                continue;
            };
            for item in result.content.iter_mut() {
                let elided = elide_tool_result_content(item);
                *item = elided;
            }
        }
    }
}

/// Cut one tool-result content block down to [`ELIDED_TOOL_RESULT_CHARS`].
///
/// JSON payloads are rendered to text first — a truncated JSON *value* would no
/// longer parse, and the model reads these as prose anyway.
fn elide_tool_result_content(item: &ToolResultContent) -> ToolResultContent {
    let rendered = match item {
        ToolResultContent::Text(text) => text.text.clone(),
        ToolResultContent::Json { value } => value.to_string(),
        // An image handed back by a tool is not text and cannot be trimmed;
        // leave it exactly as it is.
        ToolResultContent::Image(_) => return item.clone(),
    };
    if rendered.chars().count() <= ELIDED_TOOL_RESULT_CHARS {
        return item.clone();
    }
    let mut short: String = rendered.chars().take(ELIDED_TOOL_RESULT_CHARS).collect();
    short.push_str("… [older tool result truncated to save context]");
    ToolResultContent::text(short)
}

#[cfg(test)]
mod tests {
    use super::*;
    use rig::message::{ImageMediaType, ToolResult as MessageToolResult};

    fn session(model: &str, prompt_version: &str) -> StoredSession {
        StoredSession {
            id: "s1".into(),
            meal_id: "m1".into(),
            messages: Vec::new(),
            model: model.into(),
            prompt_version: prompt_version.into(),
            turn_count: 0,
        }
    }

    fn tool_result(id: &str, body: &str) -> Message {
        Message::User {
            content: OneOrMany::one(UserContent::ToolResult(MessageToolResult {
                id: id.to_string(),
                call_id: None,
                content: OneOrMany::one(ToolResultContent::text(body)),
            })),
        }
    }

    fn tool_result_text(message: &Message) -> Option<String> {
        let Message::User { content } = message else {
            return None;
        };
        content.iter().find_map(|part| match part {
            UserContent::ToolResult(result) => {
                result.content.first_ref().as_text().map(str::to_string)
            }
            _ => None,
        })
    }

    #[test]
    fn identical_model_and_prompt_continue() {
        let s = session("gpt-4.1", "2026-08-01.1");
        assert_eq!(decide(&s, "gpt-4.1", "2026-08-01.1"), ResumeDecision::Continue);
    }

    #[test]
    fn a_prompt_version_bump_forces_a_reseed() {
        let s = session("gpt-4.1", "2026-08-01.1");
        assert_eq!(decide(&s, "gpt-4.1", "2026-09-01.1"), ResumeDecision::Reseed);
    }

    #[test]
    fn a_model_change_forces_a_reseed() {
        let s = session("gpt-4.1", "2026-08-01.1");
        assert_eq!(decide(&s, "gpt-5.2", "2026-08-01.1"), ResumeDecision::Reseed);
    }

    fn text_of(message: &Message) -> String {
        serde_json::to_string(message).unwrap()
    }

    #[test]
    fn cap_history_keeps_the_anchor_and_the_tail() {
        let messages: Vec<Message> = (0..10).map(|i| Message::user(format!("m{i}"))).collect();
        let capped = cap_history(messages, 4);
        assert_eq!(capped.len(), 4);
        // The anchoring first turn survives...
        assert!(text_of(&capped[0]).contains("m0"));
        // ...and so does the most recent turn.
        assert!(text_of(&capped[3]).contains("m9"));
    }

    #[test]
    fn cap_history_is_a_noop_below_the_cap() {
        let messages: Vec<Message> = (0..3).map(|i| Message::user(format!("m{i}"))).collect();
        assert_eq!(cap_history(messages, MAX_STORED_TURNS).len(), 3);
    }

    #[test]
    fn cap_history_drops_a_tool_result_whose_call_was_cut() {
        // The slice boundary lands on a tool result: its assistant tool call is
        // gone, so keeping it would hand the provider an unanswerable message.
        let messages = vec![
            Message::user("photo turn"),
            Message::assistant("calling recall"),
            tool_result("call_1", "recall payload"),
            Message::assistant("done"),
        ];
        let capped = cap_history(messages, 3);
        assert_eq!(capped.len(), 2);
        assert!(text_of(&capped[0]).contains("photo turn"));
        assert!(text_of(&capped[1]).contains("done"));
    }

    #[test]
    fn cap_history_elides_the_oldest_tool_result_payloads_first() {
        let bulky = "x".repeat(ELIDED_TOOL_RESULT_CHARS * 3);
        let mut messages = vec![Message::user("photo turn")];
        // Sacrificial: this is what the cap actually discards.
        messages.push(Message::assistant("superseded chatter"));
        // Old, well before the recent window: must be elided, not dropped.
        messages.push(Message::assistant("call 1"));
        messages.push(tool_result("call_1", &bulky));
        for i in 0..RECENT_MESSAGES_KEPT_VERBATIM {
            messages.push(Message::assistant(format!("filler {i}")));
        }
        // Recent: must survive verbatim.
        messages.push(Message::assistant("call 2"));
        messages.push(tool_result("call_2", &bulky));

        let cap = messages.len() - 1;
        let capped = cap_history(messages, cap);
        assert_eq!(capped.len(), cap);

        let elided = capped
            .iter()
            .find(|m| tool_result_text(m).is_some_and(|t| t.contains("truncated")))
            .expect("the old tool result should have been elided");
        assert!(tool_result_text(elided).unwrap().chars().count() < bulky.chars().count());

        let verbatim = capped
            .last()
            .expect("the most recent tool result should still be present");
        assert_eq!(tool_result_text(verbatim).as_deref(), Some(bulky.as_str()));
    }

    #[test]
    fn strip_images_replaces_the_photo_but_keeps_the_turn() {
        let photo_turn = Message::User {
            content: OneOrMany::many(vec![
                UserContent::text("look at this"),
                UserContent::image_base64("/9j/", Some(ImageMediaType::JPEG), None),
            ])
            .expect("two parts"),
        };
        let thread = vec![photo_turn, Message::assistant("that is шаурма")];

        let stripped = strip_images(thread);
        assert_eq!(stripped.len(), 2);
        let Message::User { content } = &stripped[0] else {
            panic!("the photo turn must stay a user turn");
        };
        assert_eq!(content.len(), 2);
        assert!(content
            .iter()
            .all(|part| matches!(part, UserContent::Text(_))));

        // ...and nothing base64-shaped survives into the stored blob.
        let encoded = serde_json::to_string(&stripped).expect("serializes");
        assert!(!encoded.contains("/9j/"), "{encoded}");
        assert!(encoded.contains("omitted from stored history"), "{encoded}");
    }

    // -- persistence against a real database -------------------------------

    use crate::feedback::state::fixtures::{at, seed_user, test_conn};
    use crate::models::{MealStatus, NameSource, NewMeal};

    fn seed_meal(conn: &mut diesel::SqliteConnection, id: &str, user_id: &str) {
        diesel::insert_into(crate::schema::meals::table)
            .values(&NewMeal {
                id: id.to_string(),
                user_id: user_id.to_string(),
                client_uuid: format!("client-{id}"),
                thumbnail_id: None,
                group_id: None,
                group_size: None,
                dish_name: Some("шаурма".into()),
                dish_name_normalized: Some("шаурма".into()),
                name_source: NameSource::Vision.as_str().to_string(),
                user_comment: None,
                revision: 1,
                eaten_at: at(2026, 7, 30, 13, 0),
                timezone_offset: 180,
                meal_type: None,
                status: MealStatus::NeedsReview.as_str().to_string(),
                portion_scale: 1.0,
                created_at: at(2026, 7, 30, 13, 0),
                updated_at: at(2026, 7, 30, 13, 0),
            })
            .execute(conn)
            .expect("seed meal");
    }

    #[test]
    fn a_session_round_trips_and_upserts_on_the_unique_meal_id() {
        let mut conn = test_conn();
        seed_user(&mut conn, "u1");
        seed_meal(&mut conn, "m1", "u1");

        assert!(load(&mut conn, "m1").expect("load runs").is_none());

        let thread = vec![Message::user("photo turn"), Message::assistant("шаурма")];
        save(&mut conn, "m1", &thread, "gpt-4.1", "2026-08-01.2").expect("saves");

        let stored = load(&mut conn, "m1")
            .expect("load runs")
            .expect("the session exists");
        assert_eq!(stored.meal_id, "m1");
        assert_eq!(stored.model, "gpt-4.1");
        assert_eq!(stored.prompt_version, "2026-08-01.2");
        assert_eq!(stored.turn_count, 2);
        assert_eq!(stored.messages.len(), 2);
        let first_id = stored.id.clone();

        // A second save for the same meal updates in place — one live thread per
        // meal, which each revision extends.
        let extended = vec![
            Message::user("photo turn"),
            Message::assistant("шаурма"),
            Message::user("half the rice"),
            Message::assistant("corrected"),
        ];
        save(&mut conn, "m1", &extended, "gpt-5.2", "2026-09-01.1").expect("saves again");

        let stored = load(&mut conn, "m1")
            .expect("load runs")
            .expect("the session exists");
        assert_eq!(stored.id, first_id, "the upsert created a second row");
        assert_eq!(stored.turn_count, 4);
        assert_eq!(stored.model, "gpt-5.2");
        assert_eq!(decide(&stored, "gpt-4.1", "2026-08-01.2"), ResumeDecision::Reseed);
    }

    #[test]
    fn an_unreadable_thread_reseeds_instead_of_failing_the_correction() {
        use crate::schema::agent_sessions;

        let mut conn = test_conn();
        seed_user(&mut conn, "u1");
        seed_meal(&mut conn, "m1", "u1");
        save(&mut conn, "m1", &[Message::user("x")], "gpt-4.1", "v1").expect("saves");

        // Simulate a wire-format change under an existing row.
        diesel::update(agent_sessions::table.filter(agent_sessions::meal_id.eq("m1")))
            .set(agent_sessions::messages.eq("{ not a message array }"))
            .execute(&mut conn)
            .expect("corrupts the blob");

        assert!(
            load(&mut conn, "m1").expect("load must not error").is_none(),
            "an unreadable thread should read as absent, not as a failure"
        );
        // The raw row is still there for anyone who wants to inspect it.
        assert!(load_row(&mut conn, "m1").expect("load_row runs").is_some());
    }

    #[test]
    fn deleting_a_session_is_idempotent() {
        let mut conn = test_conn();
        seed_user(&mut conn, "u1");
        seed_meal(&mut conn, "m1", "u1");
        save(&mut conn, "m1", &[Message::user("x")], "gpt-4.1", "v1").expect("saves");

        delete(&mut conn, "m1").expect("deletes");
        assert!(load_row(&mut conn, "m1").expect("load_row runs").is_none());
        delete(&mut conn, "m1").expect("deleting nothing is fine");
    }

    #[test]
    fn saving_caps_the_thread_before_it_reaches_the_database() {
        let mut conn = test_conn();
        seed_user(&mut conn, "u1");
        seed_meal(&mut conn, "m1", "u1");

        let long: Vec<Message> = (0..MAX_STORED_TURNS * 2)
            .map(|i| Message::user(format!("m{i}")))
            .collect();
        save(&mut conn, "m1", &long, "gpt-4.1", "v1").expect("saves");

        let stored = load(&mut conn, "m1")
            .expect("load runs")
            .expect("the session exists");
        assert_eq!(stored.turn_count, MAX_STORED_TURNS as i32);
        assert_eq!(stored.messages.len(), MAX_STORED_TURNS);
    }

    #[test]
    fn a_capped_thread_round_trips_through_json() {
        let messages = vec![
            Message::user("photo turn"),
            Message::assistant("call"),
            tool_result("call_1", "payload"),
            Message::assistant(r#"{"dish_name":"шаурма"}"#),
        ];
        let encoded = serde_json::to_string(&messages).expect("messages serialize");
        let decoded: Vec<Message> = serde_json::from_str(&encoded).expect("messages deserialize");
        assert_eq!(decoded.len(), messages.len());
        assert_eq!(text_of(&decoded[3]), text_of(&messages[3]));
    }
}
