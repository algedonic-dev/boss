//! A refused policy write is loud (review 1a73d5ce, F2; backlog 0028804f).
//!
//! WHY. Until this car a write the policy doors refused left nothing
//! behind: a 409 from the lockout guard, from the service-read guard
//! (`crate::service_read`) or from the refusal of an override on the
//! guard's own reader went back to its caller and nowhere else — no log
//! line, no event, no audit row (`policy_rule_audit` records writes that
//! COMMITTED). Someone trying to take every service off the policy check
//! is exactly the attempt an operator should be able to read afterwards.
//!
//! WHAT IS ANNOUNCED. Every answer of 409 or 503 from one of the five
//! write doors, at the door's own exit ([`crate::http`] wraps each
//! handler), so a refusal added inside a door later is loud without
//! remembering to be — the override door's refusal for a service id, on
//! the sibling car, among them. Each of those answers is given only
//! AFTER the caller's policy authority was judged: a caller with none is
//! answered 403 and is not announced, so a stranger on the port cannot
//! write to the log by being refused.
//!
//! * ONE LOG LINE per refusal, always, at WARN: who asked, at which
//!   door, about which rule or override, the status, and the refusal.
//! * ONE EVENT, `policy.write.refused`, at the FIRST SIGHTING of a
//!   (door, target, caller, status) in this process — never one per
//!   request ("No per-request events", docs/architecture-decisions.md):
//!   a client retrying a refused write in a loop is one fact and a
//!   count on the log line. Past [`MAX_KEYS`] distinct sightings the
//!   first one over is recorded with `overflow: true` and later new ones
//!   are log lines only, so the log cannot be grown without bound.
//!
//! The event carries ids and the refusal's own text. A door reads no
//! credential and none is in it; caller text is clipped.
//!
//! LIMIT, STATED. The event is recorded inline, once, with no retry: if
//! the outbox refuses it (its database is down or full), that is said at
//! ERROR beside the WARN line that already carries everything, and the
//! refusal is still returned. The refusal never waits on the log to be
//! a refusal.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, PoisonError};

use axum::http::StatusCode;
use boss_core::actor::ActorId;
use boss_core::port::EventRecorder;
use boss_core::publisher::EventStamp;
use serde_json::json;

use boss_policy_client::types::User;

/// The kind of the one event, declared in
/// `infra/postgres/schema/20261008003755-a-refused-policy-write-is-on-the-log.sql`
/// and held equal to that row by this crate's
/// `the_refused_policy_write_kind_is_registered`.
///
/// Its NAME is its own on purpose. The gate lint
/// `emitted-kinds-are-declared` resolves a const passed to an emit by
/// its bare name across every crate, so a const called `KIND` here read
/// as another crate's `KIND` and the lint reported that crate's value as
/// the undeclared kind (measured on this car's first suite run).
pub const POLICY_WRITE_REFUSED: &str = "policy.write.refused";
/// The service that states it.
const SOURCE: &str = "policy";
/// How many distinct (door, target, caller, status) one process records.
pub const MAX_KEYS: usize = 256;
/// Longest caller id, role or target kept: each is caller text.
const MAX_ID_CHARS: usize = 96;
/// Longest refusal text kept on the event.
const MAX_WHY_CHARS: usize = 2000;

fn clip(text: &str, chars: usize) -> String {
    text.chars().take(chars).collect()
}

/// One write a caller asked a door for.
#[derive(Debug, Clone)]
pub struct Attempt {
    /// The door, as `METHOD route`.
    pub door: &'static str,
    /// `rule` or `override`.
    pub target_kind: &'static str,
    /// The rule's id, or the override's — for a new override, with the
    /// (user, action, resource) it is about.
    pub target: String,
    by: User,
}

impl Attempt {
    pub fn on_rule(door: &'static str, by: &User, id: &str) -> Self {
        Self {
            door,
            target_kind: "rule",
            target: clip(id, MAX_ID_CHARS),
            by: by.clone(),
        }
    }

    pub fn on_override(door: &'static str, by: &User, id: &str) -> Self {
        Self {
            door,
            target_kind: "override",
            target: clip(id, MAX_ID_CHARS * 3),
            by: by.clone(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct Key {
    door: &'static str,
    target: String,
    caller: String,
    status: u16,
}

/// Where a refused policy write is said.
pub struct Announcer {
    recorder: Option<Arc<dyn EventRecorder>>,
    seen: Mutex<HashMap<Key, u64>>,
}

impl Announcer {
    /// Log lines, and events through `recorder` — the policy service's
    /// outbox.
    pub fn recording(recorder: Arc<dyn EventRecorder>) -> Arc<Self> {
        Arc::new(Self {
            recorder: Some(recorder),
            seen: Mutex::default(),
        })
    }

    /// Log lines only — a router built in a test that reads no event.
    pub fn log_only() -> Arc<Self> {
        Arc::new(Self {
            recorder: None,
            seen: Mutex::default(),
        })
    }

    /// Whether an answer with `status` is a refusal this announces: a
    /// 409 (a guard refused the write, or it conflicts with a row) or a
    /// 503 (the lockout guard could not judge it). Both are given only
    /// after the caller's authority was judged; a 403 or a 422 is not.
    pub fn announces(status: StatusCode) -> bool {
        status == StatusCode::CONFLICT || status == StatusCode::SERVICE_UNAVAILABLE
    }

    /// Count this sighting: how many times this key has now been seen,
    /// and whether it is to be recorded — and, if so, whether as the
    /// first one past the key cap.
    fn sight(&self, key: Key) -> (u64, Option<bool>) {
        let mut seen = self.seen.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some(count) = seen.get_mut(&key) {
            *count += 1;
            return (*count, None);
        }
        let held = seen.len();
        if held > MAX_KEYS {
            return (1, None);
        }
        // The key that fills the last slot past the cap is the overflow
        // sighting; it is kept so it is said once.
        seen.insert(key, 1);
        (1, Some(held == MAX_KEYS))
    }

    /// Say that `attempt` was refused with `status`, because `why`.
    pub async fn announce(&self, attempt: &Attempt, status: StatusCode, why: &str) {
        let caller = clip(&attempt.by.id, MAX_ID_CHARS);
        let role = clip(&attempt.by.role, MAX_ID_CHARS);
        let (count, record) = self.sight(Key {
            door: attempt.door,
            target: attempt.target.clone(),
            caller: caller.clone(),
            status: status.as_u16(),
        });
        tracing::warn!(
            door = attempt.door,
            target_kind = attempt.target_kind,
            target = %attempt.target,
            caller = %caller,
            role = %role,
            status = status.as_u16(),
            count,
            "a policy write was refused and nothing was written: {why}"
        );
        let (Some(recorder), Some(overflow)) = (&self.recorder, record) else {
            return;
        };
        let actor = attempt
            .by
            .ambient_actor()
            .unwrap_or_else(|| ActorId::Automation(SOURCE.to_string()));
        let event = EventStamp::new(SOURCE, actor).event(
            POLICY_WRITE_REFUSED,
            json!({
                "door": attempt.door,
                "target_kind": attempt.target_kind,
                "target": attempt.target,
                "caller": caller,
                "role": role,
                "status": status.as_u16(),
                "refused": clip(why, MAX_WHY_CHARS),
                "overflow": overflow,
            }),
        );
        if let Err(error) = recorder.record(&event).await {
            tracing::error!(
                door = attempt.door,
                target = %attempt.target,
                caller = %caller,
                %error,
                "the refused policy write above reached no log: the outbox did not take its \
                 `{POLICY_WRITE_REFUSED}` event (it is not retried; the line above is the record)"
            );
        }
    }
}

/// `app` with every refused policy write also stated as an event through
/// `recorder`. The doors read the announcer this layers on; the service
/// binary calls it on its mounted router.
pub fn recorded(app: axum::Router, recorder: Arc<dyn EventRecorder>) -> axum::Router {
    app.layer(axum::extract::Extension(Announcer::recording(recorder)))
}

/// An [`EventRecorder`] that keeps what it is handed, for the door tests.
#[cfg(test)]
pub(crate) mod fixtures {
    use std::sync::Mutex;

    use boss_core::event::Event;
    use boss_core::port::EventRecorder;

    #[derive(Default)]
    pub(crate) struct Kept {
        pub(crate) events: Mutex<Vec<Event>>,
        /// When set, every record is refused with this.
        pub(crate) down: Option<String>,
    }

    impl Kept {
        pub(crate) fn events(&self) -> Vec<Event> {
            self.events.lock().expect("poisoned lock").clone()
        }
    }

    #[async_trait::async_trait]
    impl EventRecorder for Kept {
        async fn record(&self, event: &Event) -> Result<(), String> {
            if let Some(why) = &self.down {
                return Err(why.clone());
            }
            self.events
                .lock()
                .expect("poisoned lock")
                .push(event.clone());
            Ok(())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::fixtures::Kept;
    use super::*;

    fn attempt(target: &str, caller: &str) -> Attempt {
        let mut by = User::anonymous();
        by.id = caller.into();
        by.role = "platform-admin".into();
        Attempt::on_rule("DELETE /api/policy/rules/{id}", &by, target)
    }

    /// One event at a key's first sighting, none for its repeats, one
    /// more for a different key — and the event carries who, which door,
    /// which rule, the status and the refusal.
    #[tokio::test]
    async fn a_refusal_is_one_event_at_its_first_sighting() {
        let kept = Arc::new(Kept::default());
        let loud = Announcer::recording(kept.clone());
        let one = attempt("platform-admin:policy-rule:read", "emp-founder");
        for _ in 0..3 {
            loud.announce(&one, StatusCode::CONFLICT, "because").await;
        }
        loud.announce(&one, StatusCode::SERVICE_UNAVAILABLE, "dark")
            .await;
        loud.announce(
            &attempt("platform-admin:policy-rule:read", "emp-two"),
            StatusCode::CONFLICT,
            "because",
        )
        .await;
        let events = kept.events();
        assert_eq!(events.len(), 3, "{events:?}");
        let first = &events[0];
        assert_eq!(first.kind, POLICY_WRITE_REFUSED);
        assert_eq!(first.source, "policy");
        assert_eq!(first.payload["door"], "DELETE /api/policy/rules/{id}");
        assert_eq!(first.payload["target_kind"], "rule");
        assert_eq!(first.payload["target"], "platform-admin:policy-rule:read");
        assert_eq!(first.payload["caller"], "emp-founder");
        assert_eq!(first.payload["role"], "platform-admin");
        assert_eq!(first.payload["status"], 409);
        assert_eq!(first.payload["refused"], "because");
        assert_eq!(first.payload["overflow"], false);
        assert!(first.payload.get("_actor").is_some(), "{first:?}");
        assert_eq!(events[1].payload["status"], 503);
        assert_eq!(events[2].payload["caller"], "emp-two");
    }

    /// Only a 409 or a 503 is a refusal this announces.
    #[test]
    fn only_a_conflict_or_an_unjudgeable_write_is_announced() {
        for (status, announced) in [
            (StatusCode::CONFLICT, true),
            (StatusCode::SERVICE_UNAVAILABLE, true),
            (StatusCode::OK, false),
            (StatusCode::CREATED, false),
            (StatusCode::NO_CONTENT, false),
            (StatusCode::FORBIDDEN, false),
            (StatusCode::NOT_FOUND, false),
            (StatusCode::UNPROCESSABLE_ENTITY, false),
            (StatusCode::INTERNAL_SERVER_ERROR, false),
        ] {
            assert_eq!(Announcer::announces(status), announced, "{status}");
        }
    }

    /// The log cannot be grown without bound: [`MAX_KEYS`] sightings are
    /// recorded, the next one is recorded as the overflow, and nothing
    /// after it — while a key already seen is still only counted.
    #[tokio::test]
    async fn past_the_key_cap_one_overflow_is_recorded_and_then_nothing() {
        let kept = Arc::new(Kept::default());
        let loud = Announcer::recording(kept.clone());
        for n in 0..MAX_KEYS + 20 {
            loud.announce(
                &attempt(&format!("role-{n}:job:read"), "emp-founder"),
                StatusCode::CONFLICT,
                "because",
            )
            .await;
        }
        let events = kept.events();
        assert_eq!(events.len(), MAX_KEYS + 1);
        let overflowed: Vec<bool> = events
            .iter()
            .map(|e| e.payload["overflow"] == true)
            .collect();
        assert_eq!(overflowed.iter().filter(|o| **o).count(), 1);
        assert_eq!(overflowed.last(), Some(&true));
    }

    /// Caller text is clipped, and an outbox that refuses the event does
    /// not fail the announcement.
    #[tokio::test]
    async fn caller_text_is_clipped_and_a_dark_outbox_is_survived() {
        let kept = Arc::new(Kept::default());
        let loud = Announcer::recording(kept.clone());
        let long = "x".repeat(5000);
        loud.announce(&attempt(&long, &long), StatusCode::CONFLICT, &long)
            .await;
        let event = &kept.events()[0];
        assert_eq!(event.payload["target"].as_str().map(str::len), Some(96));
        assert_eq!(event.payload["caller"].as_str().map(str::len), Some(96));
        assert_eq!(event.payload["refused"].as_str().map(str::len), Some(2000));

        let down = Arc::new(Kept {
            down: Some("outbox: connection refused".into()),
            ..Kept::default()
        });
        Announcer::recording(down.clone())
            .announce(&attempt("r", "emp-founder"), StatusCode::CONFLICT, "why")
            .await;
        assert_eq!(down.events().len(), 0);
        Announcer::log_only()
            .announce(&attempt("r", "emp-founder"), StatusCode::CONFLICT, "why")
            .await;
    }
}
