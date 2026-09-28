//! The tenant publish stamp, written through the jobs API (backlog
//! 42da8bd2, folding 0b032f13).
//!
//! WHY IT IS A DOOR. `boss tenant publish` records itself — one
//! `tenant_publishes` row (the launcher's once-per-database guard reads
//! it) plus one `tenant.published` event on the outbox, in one
//! transaction (backlog 6a8d4972, dbdc4d31). Until this module the verb
//! wrote both straight into BOSS_POSTGRES_URL, so a publish from the
//! operator's seat — `--door`, which holds no database — printed "not
//! stamped" and left neither: measured 2026-09-27, no `tenant.*` event
//! had ever reached the live audit log (control rows present), and the
//! 2026-09-25 publish to prod that filed 42da8bd2 left no row. The
//! events API has no write route, so the only way to put the fact in
//! the record from off the database was a service door. This is it:
//! `POST /api/tenant/publishes`, and the verb stamps through it on
//! every route, so the transaction lives in ONE place (CLAUDE.md §9a) —
//! here, not in the CLI and here.
//!
//! THE ACTOR IS THE CALLER'S, NEVER THE BODY'S. `published_by` is the
//! `x-boss-user` the request was signed with (after the login door's
//! alias resolution), the way every write on this service is credited;
//! a body that names one is refused (`deny_unknown_fields`) rather than
//! silently overridden. The row and the event's `_actor` are one value
//! read from one place ([`published_event`]).
//!
//! Hexagonal like `surface_opens`: types + the pure event + a port, an
//! in-memory adapter for the door's tests, the Pg adapter the binary
//! runs, and the HTTP door.

pub mod http;
#[cfg(feature = "postgres")]
pub mod postgres;

use async_trait::async_trait;
use boss_core::actor::ActorId;
use boss_core::event::Event;
use boss_core::publisher::EventStamp;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

#[cfg(feature = "postgres")]
pub use postgres::PgTenantPublishes;

/// The one event kind a publish leaves: declared in event_kinds
/// (20260919-a-tenant-publish-is-a-fact-in-the-log.sql), which the
/// emitted-kinds-are-declared lint holds against this constant.
pub const TENANT_PUBLISHED: &str = "tenant.published";

/// One recorded publish — the row as `tenant_publishes` holds it, and
/// the door's answer.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Stamp {
    pub tenant_id: String,
    pub published_at: DateTime<Utc>,
    /// The caller the door credited: the actor the publish ran as, or
    /// `automation:tenant-seed` for the launcher, which runs unnamed.
    pub published_by: String,
    /// The publishing binary's build commit — never the tenant
    /// directory's, which a ConfigMap delivers without a .git.
    pub boss_commit: String,
    /// What `--take` named on this run; empty for a plain publish.
    pub took: Vec<String>,
    /// How many doors the plan wrote through.
    pub writes: i32,
}

/// What a caller sends: the stamp less the two columns the door owns —
/// who (the signed caller) and when (the door's own clock).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NewStamp {
    pub tenant_id: String,
    pub boss_commit: String,
    #[serde(default)]
    pub took: Vec<String>,
    pub writes: i32,
}

impl NewStamp {
    /// PURE: the row this request records, credited to `actor` at `at`,
    /// or the reason it is refused.
    pub fn credited(&self, actor: &str, at: DateTime<Utc>) -> Result<Stamp, String> {
        if self.tenant_id.trim().is_empty() {
            return Err("tenant_id is empty: a stamp is a fact about one tenant".into());
        }
        if self.boss_commit.trim().is_empty() {
            return Err("boss_commit is empty: a stamp names the build that published".into());
        }
        if self.writes < 0 {
            return Err(format!(
                "writes is {}: a count cannot be negative",
                self.writes
            ));
        }
        if actor.trim().is_empty() {
            return Err(
                "no caller to credit: the stamp's published_by is the signed caller".into(),
            );
        }
        Ok(Stamp {
            tenant_id: self.tenant_id.clone(),
            published_at: at,
            published_by: actor.to_string(),
            boss_commit: self.boss_commit.clone(),
            took: self.took.clone(),
            writes: self.writes,
        })
    }
}

fn rfc3339(t: &DateTime<Utc>) -> String {
    t.to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
}

/// PURE: the fact a stamp projects — `tenant.published` with the
/// stamp's columns as its payload, signed as the actor the stamp
/// names (one value, read from one place, so `published_by` and
/// `_actor` cannot disagree). The event's own timestamp is minted
/// wall-clock by the stamp builder, as every live record is;
/// `published_at` rides in the payload as the column it is.
pub fn published_event(stamp: &Stamp) -> Event {
    // `ActorId: FromStr<Err = Infallible>`: every spelling parses.
    let actor: ActorId = stamp
        .published_by
        .parse()
        .unwrap_or_else(|never: std::convert::Infallible| match never {});
    EventStamp::new("tenant", actor).event(
        TENANT_PUBLISHED,
        serde_json::json!({
            "tenant_id": stamp.tenant_id,
            "published_at": rfc3339(&stamp.published_at),
            "published_by": stamp.published_by,
            "boss_commit": stamp.boss_commit,
            "took": stamp.took,
            "writes": stamp.writes,
        }),
    )
}

#[derive(Debug, thiserror::Error)]
pub enum TenantPublishesError {
    #[error("storage: {0}")]
    Storage(String),
}

/// The port: append one stamp AND the [`published_event`] it projects,
/// as one atomic write. Never updates, never deletes.
#[async_trait]
pub trait TenantPublishes: Send + Sync {
    async fn record(&self, stamp: &Stamp) -> Result<(), TenantPublishesError>;
}

/// In memory, for the door's tests: the rows and the events beside
/// them, so a test reads what was recorded on both sides.
#[derive(Default)]
pub struct InMemoryTenantPublishes(std::sync::Mutex<Vec<(Stamp, Event)>>);

impl InMemoryTenantPublishes {
    pub fn rows(&self) -> Vec<(Stamp, Event)> {
        self.0.lock().map(|r| r.clone()).unwrap_or_default()
    }
}

#[async_trait]
impl TenantPublishes for InMemoryTenantPublishes {
    async fn record(&self, stamp: &Stamp) -> Result<(), TenantPublishesError> {
        self.0
            .lock()
            .map_err(|e| TenantPublishesError::Storage(e.to_string()))?
            .push((stamp.clone(), published_event(stamp)));
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn at() -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 9, 18, 19, 0, 0).unwrap()
    }

    fn new(took: &[&str]) -> NewStamp {
        NewStamp {
            tenant_id: "acme".into(),
            boss_commit: "478231fb".into(),
            took: took.iter().map(|s| s.to_string()).collect(),
            writes: 12,
        }
    }

    /// The event a publish leaves in the log — the stamp's columns as
    /// the payload, signed by the actor the stamp names, so the
    /// tenant_publishes row can be rebuilt from it (backlog dbdc4d31).
    #[test]
    fn the_published_event_carries_the_stamps_columns() {
        let st = new(&["employees", "agents"])
            .credited("automation:tenant-seed", at())
            .unwrap();
        let e = published_event(&st);
        assert_eq!(e.kind, TENANT_PUBLISHED);
        assert_eq!(e.kind, "tenant.published");
        assert_eq!(e.source, "tenant");
        assert_eq!(e.payload["tenant_id"], "acme");
        assert_eq!(e.payload["published_at"], "2026-09-18T19:00:00Z");
        assert_eq!(e.payload["published_by"], "automation:tenant-seed");
        assert_eq!(e.payload["boss_commit"], "478231fb");
        assert_eq!(
            e.payload["took"],
            serde_json::json!(["employees", "agents"])
        );
        assert_eq!(e.payload["writes"], 12);
        // Provenance: `_actor` is the same value as published_by, read
        // from one place, never two arguments that can disagree.
        assert_eq!(e.payload["_actor"], "automation:tenant-seed");
        assert_eq!(e.payload.as_object().unwrap().len(), 7);
    }

    #[test]
    fn a_request_is_credited_to_the_caller_at_the_doors_clock() {
        let st = new(&[]).credited("agent-claude", at()).unwrap();
        assert_eq!(st.published_by, "agent-claude");
        assert_eq!(st.published_at, at());
        assert_eq!(st.tenant_id, "acme");
        assert_eq!(st.writes, 12);
    }

    #[test]
    fn a_body_naming_its_own_actor_is_refused_not_overridden() {
        let body = serde_json::json!({
            "tenant_id": "acme", "boss_commit": "478231fb", "took": [], "writes": 1,
            "published_by": "emp-david",
        });
        let err = serde_json::from_value::<NewStamp>(body).unwrap_err();
        assert!(err.to_string().contains("published_by"), "{err}");
    }

    #[test]
    fn an_empty_tenant_commit_or_caller_and_a_negative_count_are_refused() {
        let mut n = new(&[]);
        n.tenant_id = " ".into();
        assert!(n.credited("a", at()).unwrap_err().contains("tenant_id"));
        let mut n = new(&[]);
        n.boss_commit = String::new();
        assert!(n.credited("a", at()).unwrap_err().contains("boss_commit"));
        let mut n = new(&[]);
        n.writes = -1;
        assert!(n.credited("a", at()).unwrap_err().contains("negative"));
        assert!(new(&[]).credited("", at()).unwrap_err().contains("caller"));
    }
}
