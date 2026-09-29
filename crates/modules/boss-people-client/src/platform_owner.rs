//! The platform owner, resolved over the people HTTP API — the adapter
//! behind `boss_core::platform_owner::PlatformOwner` (backlog 3c23662d,
//! design 42277636). The port and the WHY live in boss-core; this file
//! is only the read, and it lives here because the core must not
//! depend on the people service.
//!
//! THE READ. `GET /api/people?role=platform-admin&status=active` — the
//! same filter the dispatcher's notifier and the Q7 owner resolution
//! already use — then `boss_core::platform_owner::first_hire` picks the
//! earliest hire. `BOSS_PLATFORM_OWNER` wins before any read.
//!
//! THE CACHE. Per process, bounded: an answer is reused for
//! [`ReqwestPlatformOwner::TTL`] and then re-read. Two rules the roster
//! cache in `boss_jobs::owner_resolution` taught (2026-08-22): an EMPTY
//! answer is never cached — at bootstrap it is exactly the state about
//! to change — and a re-read that FAILS keeps serving the last answer
//! it had, because a conductor whose people read blips at the moment
//! its cache expires must still file its alarm to the same person it
//! filed the last one to. A cold process with a dark registry gets the
//! refusal; that is the honest answer there.

use std::sync::Mutex;
use std::time::{Duration, Instant};

use async_trait::async_trait;
use boss_core::http_client::{self, HttpClientError};
use boss_core::platform_owner::{self, Holder, PlatformOwner, PlatformOwnerError};
use boss_core::roles::PLATFORM_ADMIN_ROLE;

use crate::People;

/// One roster row as `/api/people` answers it; only the two fields the
/// resolution reads.
#[derive(Debug, serde::Deserialize)]
struct HolderRow {
    id: String,
    #[serde(default)]
    hire_date: Option<chrono::NaiveDate>,
}

/// The pure half of one read: rows → the owner or the refusal.
fn resolve(rows: Vec<HolderRow>) -> Result<String, PlatformOwnerError> {
    let holders: Vec<Holder> = rows
        .into_iter()
        .map(|r| Holder {
            id: r.id,
            hire_date: r.hire_date,
        })
        .collect();
    platform_owner::first_hire(&holders).ok_or(PlatformOwnerError::NoHolder {
        role: PLATFORM_ADMIN_ROLE,
    })
}

/// Who the read signs as: its own automation at the operator tier.
/// The roster answers a caller by grant (backlog cda177ef, 2026-09-27),
/// and one with no `x-boss-user` is nobody — this adapter sent none,
/// for every verb that files a packet and for the dispatcher's alarms.
/// Operator tier is machinery, which the roster admits whatever the
/// role; the role is platform-admin as well, the shape every other
/// automation signs with, so the read passes on the core defaults even
/// where a door asks policy first.
pub const PLATFORM_OWNER_READER: &str = r#"{"id":"automation:platform-owner","role":"platform-admin","access_tier":"operator","territory_account_ids":[],"direct_report_ids":[],"department":"platform"}"#;

/// Production adapter over reqwest, with the per-process cache.
pub struct ReqwestPlatformOwner {
    base_url: String,
    http: boss_core::machine_token::Client,
    /// `BOSS_PLATFORM_OWNER` as read at construction — a process's
    /// environment does not change under it, and reading it once is
    /// what lets a test set it without an env write.
    override_id: Option<String>,
    cache: Mutex<Option<(Instant, String)>>,
    ttl: Duration,
}

impl ReqwestPlatformOwner {
    /// How long one answer is reused before the registry is asked
    /// again. Long enough to keep the roster off a conductor's
    /// per-pass path; short enough that a hire is the owner within a
    /// minute.
    pub const TTL: Duration = Duration::from_secs(60);

    pub fn new(base_url: impl Into<String>) -> Self {
        let (base_url, http) = http_client::base(base_url);
        Self {
            base_url,
            http,
            override_id: platform_owner::env_override(),
            cache: Mutex::new(None),
            ttl: Self::TTL,
        }
    }

    /// The override as if the environment had named it — the seam a
    /// test uses instead of writing the process environment.
    pub fn with_override(mut self, id: Option<String>) -> Self {
        self.override_id = platform_owner::override_from(id);
        self
    }

    /// The query this adapter sends, so a test can pin it and a reader
    /// can find it.
    pub fn url(&self) -> String {
        // The one spelling, shared with the gateway's adapter (3c92c5b8).
        format!("{}{}", self.base_url, platform_owner::roster_path())
    }

    fn cached(&self) -> Option<(Instant, String)> {
        // A poisoned cache lock means a panic elsewhere mid-write; the
        // only state it guards is one optional answer, so recover it
        // rather than turn one panic into a refusal on every later read.
        self.cache.lock().unwrap_or_else(|p| p.into_inner()).clone()
    }

    fn remember(&self, id: &str) {
        *self.cache.lock().unwrap_or_else(|p| p.into_inner()) =
            Some((Instant::now(), id.to_string()));
    }

    async fn read(&self) -> Result<String, PlatformOwnerError> {
        let rows = self
            .signed_rows()
            .await
            .map_err(|e| PlatformOwnerError::Unreachable(e.to_string()))?;
        resolve(rows)
    }

    /// `http_client::get_json`'s ladder, with the request signed as
    /// [`PLATFORM_OWNER_READER`] — the one thing the shared helper
    /// cannot carry.
    async fn signed_rows(&self) -> Result<Vec<HolderRow>, HttpClientError<People>> {
        let resp = self
            .http
            .get(self.url())
            .header("x-boss-user", PLATFORM_OWNER_READER)
            .send()
            .await
            .map_err(|e| HttpClientError::Unreachable(e.to_string()))?;
        if !resp.status().is_success() {
            return Err(HttpClientError::UnexpectedStatus(resp.status().as_u16()));
        }
        resp.json()
            .await
            .map_err(|e| HttpClientError::MalformedBody(e.to_string()))
    }
}

#[async_trait]
impl PlatformOwner for ReqwestPlatformOwner {
    async fn platform_owner(&self) -> Result<String, PlatformOwnerError> {
        if let Some(id) = &self.override_id {
            return Ok(id.clone());
        }
        let last = self.cached();
        if let Some((at, id)) = &last
            && at.elapsed() < self.ttl
        {
            return Ok(id.clone());
        }
        match self.read().await {
            Ok(id) => {
                self.remember(&id);
                Ok(id)
            }
            // Stale beats silent: a registry that stopped answering does
            // not change who the owner was a minute ago.
            Err(PlatformOwnerError::Unreachable(_)) if last.is_some() => {
                Ok(last.map(|(_, id)| id).unwrap_or_default())
            }
            Err(e) => Err(e),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    fn row(id: &str, hire: &str) -> HolderRow {
        HolderRow {
            id: id.into(),
            hire_date: chrono::NaiveDate::parse_from_str(hire, "%Y-%m-%d").ok(),
        }
    }

    /// The wire shape resolves through the shared first-hire rule.
    #[test]
    fn the_first_hire_among_the_rows_is_the_owner() {
        let rows = vec![
            row("emp-second", "2026-09-17"),
            row("emp-first", "2026-09-16"),
        ];
        assert_eq!(resolve(rows).unwrap(), "emp-first");
    }

    /// No holder is the named refusal, not a default.
    #[test]
    fn no_rows_is_a_named_refusal() {
        assert_eq!(
            resolve(vec![]),
            Err(PlatformOwnerError::NoHolder {
                role: PLATFORM_ADMIN_ROLE
            })
        );
    }

    /// A stub people API: answers every GET with `body` and records the
    /// request lines it saw. Serves until dropped.
    async fn stub(body: &'static str) -> (String, Arc<Mutex<Vec<String>>>) {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let seen = Arc::new(Mutex::new(Vec::new()));
        let log = seen.clone();
        tokio::spawn(async move {
            loop {
                let Ok((mut sock, _)) = listener.accept().await else {
                    return;
                };
                let mut buf = vec![0u8; 4096];
                let n = sock.read(&mut buf).await.unwrap_or(0);
                // The whole request head, so a test can read a header
                // as well as the request line it starts with.
                let req = String::from_utf8_lossy(&buf[..n]).to_string();
                log.lock().unwrap().push(req);
                let resp = format!(
                    "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
                    body.len()
                );
                let _ = sock.write_all(resp.as_bytes()).await;
            }
        });
        (format!("http://{addr}"), seen)
    }

    /// The adapter asks the people API for active platform-admins, picks
    /// the first hire, and reuses the answer within the TTL — one read
    /// for two asks.
    #[tokio::test]
    async fn reads_active_platform_admins_once_and_caches_the_first_hire() {
        let (base, seen) = stub(
            r#"[{"id":"emp-later","hire_date":"2026-09-17","role":"platform-admin","status":"active"},
                {"id":"emp-first","hire_date":"2026-09-16","role":"platform-admin","status":"active"}]"#,
        )
        .await;
        let port = ReqwestPlatformOwner::new(&base).with_override(None);
        assert_eq!(port.platform_owner().await.unwrap(), "emp-first");
        assert_eq!(port.platform_owner().await.unwrap(), "emp-first");
        let lines = seen.lock().unwrap().clone();
        assert_eq!(lines.len(), 1, "cached within the TTL: {lines:?}");
        assert!(
            lines[0].starts_with("GET /api/people?role=platform-admin&status=active "),
            "{lines:?}"
        );
    }

    /// THE READ SIGNS (backlog cda177ef). The roster answers a caller
    /// by grant now, and a request with no `x-boss-user` is nobody; the
    /// adapter sent none, so every verb that files a packet and the
    /// dispatcher's alarms would have lost their owner the day the
    /// roster stopped answering strangers. It signs as its own
    /// automation at the operator tier — machinery, the tier the
    /// roster admits whatever the role.
    #[tokio::test]
    async fn the_read_signs_as_machinery_at_the_operator_tier() {
        let (base, seen) = stub(r#"[{"id":"emp-first","hire_date":"2026-09-16"}]"#).await;
        let port = ReqwestPlatformOwner::new(&base).with_override(None);
        assert_eq!(port.platform_owner().await.unwrap(), "emp-first");
        let request = seen.lock().unwrap()[0].clone();
        let header = request
            .lines()
            .find_map(|l| l.strip_prefix("x-boss-user: "))
            .unwrap_or_else(|| panic!("the read carried no x-boss-user: {request}"));
        assert_eq!(header, PLATFORM_OWNER_READER);
        assert!(
            header.contains(r#""access_tier":"operator""#)
                && header.contains(r#""id":"automation:platform-owner""#),
            "{header}"
        );
    }

    /// An empty roster is refused by name and NOT cached: the next ask
    /// reads again, because at bootstrap emptiness is about to change.
    #[tokio::test]
    async fn an_empty_roster_is_refused_and_asked_again() {
        let (base, seen) = stub("[]").await;
        let port = ReqwestPlatformOwner::new(&base).with_override(None);
        assert!(matches!(
            port.platform_owner().await,
            Err(PlatformOwnerError::NoHolder { .. })
        ));
        assert!(matches!(
            port.platform_owner().await,
            Err(PlatformOwnerError::NoHolder { .. })
        ));
        assert_eq!(seen.lock().unwrap().len(), 2);
    }

    /// The environment's override wins without a read: the launcher or
    /// unit that names the owner is believed, and the registry is not
    /// consulted at all.
    #[tokio::test]
    async fn the_override_wins_without_a_read() {
        let (base, seen) = stub("[]").await;
        let port = ReqwestPlatformOwner::new(&base).with_override(Some(" emp-named ".into()));
        assert_eq!(port.platform_owner().await.unwrap(), "emp-named");
        assert!(seen.lock().unwrap().is_empty(), "no read was needed");
    }

    /// A cold process with a dark registry gets the refusal, naming the
    /// transport; a warm one keeps its last answer past the TTL.
    #[tokio::test]
    async fn a_dark_registry_refuses_cold_and_serves_stale_warm() {
        let cold = ReqwestPlatformOwner::new("http://127.0.0.1:1").with_override(None);
        assert!(matches!(
            cold.platform_owner().await,
            Err(PlatformOwnerError::Unreachable(_))
        ));

        let mut warm = ReqwestPlatformOwner::new("http://127.0.0.1:1").with_override(None);
        warm.ttl = Duration::ZERO;
        warm.remember("emp-known");
        assert_eq!(warm.platform_owner().await.unwrap(), "emp-known");
    }
}
