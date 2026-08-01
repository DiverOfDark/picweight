//! `GET /api/v1/meals/events` — server-sent events.
//!
//! The completion payload carries the **day state** (consumed, remaining,
//! protein gap, verdict line) so a client renders the notification from one
//! message with no follow-up round trip (PRD §9). Re-analysis completion emits
//! on the same stream.
//!
//! The bus is a `tokio::sync::broadcast` channel: every connected client
//! subscribes and filters on `user_id`. Broadcast (rather than per-user
//! channels) keeps the fan-out trivial at 2–10 accounts, and a slow client that
//! lags simply misses events rather than blocking a worker — meals are durable
//! in SQLite, so the stream is a convenience, never the source of truth.

use crate::agent::schema::MacroTotals;
use crate::auth::CurrentUser;
use crate::error::AppError;
use crate::feedback::DayState;
use crate::models::MealStatus;
use crate::AppState;
use axum::extract::State;
use axum::response::sse::{Event, Sse};
use chrono::NaiveDateTime;
use futures_util::Stream;
use serde::{Deserialize, Serialize};
use std::pin::Pin;
use std::time::Duration;
use tokio::sync::broadcast;
use utoipa::ToSchema;

/// How many events the bus buffers before slow subscribers start lagging.
pub const EVENT_BUFFER: usize = 256;

/// Heartbeat interval, so proxies do not time the stream out.
pub const KEEPALIVE_SECS: u64 = 15;

/// SSE `event:` name every payload carries, so a client can register one
/// listener rather than parsing untyped messages.
pub const EVENT_NAME: &str = "meal";

/// What happened.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum MealEventKind {
    /// The upload was accepted and a job enqueued.
    Queued,
    /// The agent loop started.
    Analyzing,
    /// A draft estimate is ready.
    Completed,
    /// A correction produced a new revision.
    Reanalyzed,
    /// Analysis failed, loudly, with a reason.
    Failed,
    /// A sitting settled; this is the one notification for the whole group.
    GroupSettled,
    /// The user edited or confirmed a meal, so the day state moved.
    ///
    /// Emitted so a second device (the web app while the phone confirms, say)
    /// stays in step. **Clients must not raise a notification for this** — §6
    /// allows exactly one notification per meal and it is fired by `Completed` /
    /// `Reanalyzed` / `GroupSettled`. This event is a refresh signal.
    Updated,
}

/// One event on the stream.
///
/// `Clone` because broadcast hands every subscriber its own copy.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct MealEvent {
    /// Recipient. Subscribers filter on this; it is not serialized to clients.
    #[serde(skip)]
    pub user_id: String,
    /// What happened.
    pub kind: MealEventKind,
    /// The meal, for everything except `GroupSettled`.
    pub meal_id: Option<String>,
    /// The sitting, when the meal belongs to one.
    pub group_id: Option<String>,
    /// Revision these figures describe.
    pub revision: i32,
    /// Meal status after the event.
    pub status: Option<MealStatus>,
    /// Dish name — the notification leads with it (§6).
    pub dish_name: Option<String>,
    /// Totals for the meal, or for the whole sitting on `GroupSettled`.
    pub totals: Option<MacroTotals>,
    /// The day state, so no follow-up request is needed.
    pub day: Option<DayState>,
    /// Ready-to-render notification lines.
    pub headline: Option<String>,
    /// Line 2 + 3 + 4 of the notification, already phrased.
    pub body: Option<String>,
    /// Failure reason, set only on `Failed`.
    pub error: Option<String>,
    /// When the event was produced, UTC.
    pub emitted_at: NaiveDateTime,
}

impl MealEvent {
    /// Minimal constructor; callers fill in the optional fields.
    pub fn new(user_id: impl Into<String>, kind: MealEventKind) -> Self {
        MealEvent {
            user_id: user_id.into(),
            kind,
            meal_id: None,
            group_id: None,
            revision: 1,
            status: None,
            dish_name: None,
            totals: None,
            day: None,
            headline: None,
            body: None,
            error: None,
            emitted_at: chrono::Utc::now().naive_utc(),
        }
    }

    /// Attach the meal this event is about.
    pub fn with_meal(mut self, meal_id: impl Into<String>, revision: i32) -> Self {
        self.meal_id = Some(meal_id.into());
        self.revision = revision;
        self
    }

    /// Attach the sitting this event belongs to.
    pub fn with_group(mut self, group_id: impl Into<String>) -> Self {
        self.group_id = Some(group_id.into());
        self
    }
}

/// Broadcast fan-out for [`MealEvent`]s.
#[derive(Clone)]
pub struct EventBus {
    tx: broadcast::Sender<MealEvent>,
}

impl EventBus {
    /// Create a bus buffering `capacity` events.
    pub fn new(capacity: usize) -> Self {
        let (tx, _rx) = broadcast::channel(capacity);
        EventBus { tx }
    }

    /// Subscribe to every event; callers filter by `user_id`.
    pub fn subscribe(&self) -> broadcast::Receiver<MealEvent> {
        self.tx.subscribe()
    }

    /// Publish an event.
    ///
    /// Deliberately infallible: with no subscribers connected, `send` returns
    /// an error that means "nobody is listening", which is normal — a worker
    /// must not fail a meal because the phone is offline.
    pub fn publish(&self, event: MealEvent) {
        let kind = event.kind;
        if self.tx.send(event).is_err() {
            tracing::trace!(?kind, "no SSE subscribers; event dropped");
        }
    }

    /// Number of currently connected subscribers.
    pub fn subscriber_count(&self) -> usize {
        self.tx.receiver_count()
    }
}

impl Default for EventBus {
    fn default() -> Self {
        EventBus::new(EVENT_BUFFER)
    }
}

impl std::fmt::Debug for EventBus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("EventBus")
            .field("subscribers", &self.subscriber_count())
            .finish()
    }
}

/// Boxed so the handler has a nameable return type.
pub type EventStream = Pin<Box<dyn Stream<Item = Result<Event, std::convert::Infallible>> + Send>>;

/// `GET /api/v1/meals/events` — subscribe to this user's meal events.
#[utoipa::path(
    get,
    path = "/api/v1/meals/events",
    tag = "meals",
    summary = "Stream meal analysis events",
    description = "Server-sent events for this user only. Each `completed`, \
`reanalyzed` and `group_settled` event carries the full day state so a client \
can render the notification without a follow-up request.",
    responses(
        (
            status = 200,
            description = "text/event-stream; each frame is a JSON MealEvent under the `meal` event name",
            body = MealEvent,
            content_type = "text/event-stream",
        ),
        (status = 401, description = "Not authenticated", body = crate::error::ErrorBody),
    ),
    security(("session" = []))
)]
pub async fn meal_events(
    State(state): State<AppState>,
    user: CurrentUser,
) -> Result<Sse<EventStream>, AppError> {
    let receiver = state.events.subscribe();
    let user_id = user.id;
    tracing::debug!(
        user_id = %user_id,
        subscribers = state.events.subscriber_count(),
        "SSE client attached"
    );

    // The keep-alive is folded into the stream rather than applied with
    // `Sse::keep_alive`, because that combinator changes the stream type and
    // this handler's return type is the nameable [`EventStream`]. A `recv`
    // that idles past [`KEEPALIVE_SECS`] emits a comment frame instead, which
    // is exactly what a proxy needs to see.
    let keepalive = Duration::from_secs(KEEPALIVE_SECS);
    let stream = futures_util::stream::unfold(receiver, move |mut receiver| {
        let user_id = user_id.clone();
        async move {
            loop {
                match tokio::time::timeout(keepalive, receiver.recv()).await {
                    // Idle: send a comment so the connection stays warm.
                    Err(_elapsed) => {
                        return Some((Ok(Event::default().comment("keep-alive")), receiver));
                    }
                    Ok(Ok(event)) if event.user_id == user_id => {
                        return Some((Ok(encode(&event)), receiver));
                    }
                    // Another user's event. The bus is a single broadcast
                    // channel; this filter is the whole of the per-user
                    // isolation on the stream.
                    Ok(Ok(_)) => continue,
                    // A slow client missed events. Meals are durable in SQLite,
                    // so the correct response is to carry on from the newest
                    // event, not to tear the stream down.
                    Ok(Err(broadcast::error::RecvError::Lagged(missed))) => {
                        tracing::warn!(user_id = %user_id, missed, "SSE subscriber lagged");
                        continue;
                    }
                    Ok(Err(broadcast::error::RecvError::Closed)) => return None,
                }
            }
        }
    });

    Ok(Sse::new(Box::pin(stream) as EventStream))
}

/// Render one event as an SSE frame.
///
/// Serialization cannot realistically fail — every field is a plain scalar —
/// but a panic here would kill the subscriber's stream, so a failure degrades
/// to a comment frame and is logged.
fn encode(event: &MealEvent) -> Event {
    match Event::default().event(EVENT_NAME).json_data(event) {
        Ok(frame) => frame,
        Err(e) => {
            tracing::error!(error = %e, kind = ?event.kind, "failed to serialize a meal event");
            Event::default().comment("unserializable event")
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn publishing_without_subscribers_is_not_an_error() {
        let bus = EventBus::new(8);
        bus.publish(MealEvent::new("u1", MealEventKind::Queued));
        assert_eq!(bus.subscriber_count(), 0);
    }

    #[tokio::test]
    async fn subscribers_receive_published_events() {
        let bus = EventBus::new(8);
        let mut rx = bus.subscribe();
        bus.publish(MealEvent::new("u1", MealEventKind::Completed).with_meal("m1", 2));
        let event = rx.recv().await.unwrap();
        assert_eq!(event.kind, MealEventKind::Completed);
        assert_eq!(event.meal_id.as_deref(), Some("m1"));
        assert_eq!(event.revision, 2);
    }

    #[test]
    fn user_id_is_routing_metadata_and_never_serialized() {
        let event = MealEvent::new("u1", MealEventKind::Queued);
        let json = serde_json::to_value(&event).unwrap();
        assert!(json.get("user_id").is_none());
    }
}
