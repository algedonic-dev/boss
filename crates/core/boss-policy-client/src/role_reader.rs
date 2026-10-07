//! HTTP adapter for the registry role port (approved abf9eeae car 2).
//! Three existing authenticated read doors, never an anonymous shortcut:
//! agents with aliases, automation rows, and the active people roster.
//! A complete answer is required; errors differ from an unregistered id.

use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use async_trait::async_trait;
use boss_core::machine_gate::{Mode, ModeSwitch};
use boss_core::machine_token::{self, Source};
use boss_core::role_of_record::{RoleLookupError, RoleOfRecord, RoleRecord};
use serde_json::Value;

use crate::User;
use crate::role_reporting::{ReportMode, ReportModeSource};

pub const ROLE_MODE_FILE_ENV: &str = "BOSS_ACTOR_ROLE_MODE_FILE";
pub const ROLE_MODE_FILE: &str = "/etc/boss/machine-gate/actor-role";

/// Uses the machine gate's file reader and cadence. This precursor never
/// enforces: the later H car must add that behavior after its clean window.
pub struct MountedReportMode {
    switch: Arc<ModeSwitch>,
}

impl MountedReportMode {
    pub fn mount() -> Self {
        Self {
            switch: ModeSwitch::mount("actor role", ROLE_MODE_FILE_ENV, ROLE_MODE_FILE),
        }
    }

    pub fn over(switch: Arc<ModeSwitch>) -> Self {
        Self { switch }
    }
}

impl ReportModeSource for MountedReportMode {
    fn mode(&self) -> ReportMode {
        match self.switch.mode() {
            Mode::Off => ReportMode::Off,
            Mode::Report => ReportMode::Report,
            Mode::Enforce => {
                tracing::warn!(file = %self.switch.path().display(), "actor-role enforce is not implemented by this report-only precursor; report behavior retained");
                ReportMode::Report
            }
        }
    }
}

const MAX_RESPONSE_BYTES: usize = 4 * 1024 * 1024;

pub struct HttpRoleReader {
    jobs: String,
    people: String,
    http: machine_token::Client,
}

/// An owned, complete registry projection. Lookup performs no I/O, so
/// registry services can compare their own callers without reading themselves.
/// Freshness and atomic publication belong to the injected snapshot adapter.
pub struct RegistryRoles {
    rows: Vec<(Value, bool, bool)>,
}

impl RegistryRoles {
    pub fn from_sources(
        agents: Value,
        automations: Value,
        people: Value,
    ) -> Result<Self, RoleLookupError> {
        let mut rows = Vec::new();
        for (body, path, aliases, people) in [
            (agents, "/api/agents", true, false),
            (automations, "/api/agents/automations", false, false),
            (people, "/api/people", false, true),
        ] {
            for row in complete_rows(body, path, people)? {
                // Validate every row before publication, including rows that
                // happen not to match the first caller using this projection.
                matches_actor(&row, "", aliases, people)?;
                record(&row)?;
                rows.push((row, aliases, people));
            }
        }
        Ok(Self { rows })
    }
}

impl RegistryRoles {
    pub fn lookup(&self, actor: &str) -> Result<Option<RoleRecord>, RoleLookupError> {
        let mut found = Vec::new();
        for (row, aliases, people) in &self.rows {
            if matches_actor(row, actor, *aliases, *people)? {
                found.push(record(row)?);
            }
        }
        if found.len() > 1 {
            return Err(RoleLookupError::Ambiguous(format!(
                "{} registry rows match {actor}",
                found.len()
            )));
        }
        Ok(found.pop())
    }
}

#[async_trait]
impl RoleOfRecord for RegistryRoles {
    async fn role_for(&self, actor: &str) -> Result<Option<RoleRecord>, RoleLookupError> {
        self.lookup(actor)
    }
}

/// Monotonic freshness is an explicit dependency; tests never sleep to age data.
pub trait RoleSnapshotClock: Send + Sync {
    fn now(&self) -> Instant;
}

#[async_trait]
pub trait RoleSnapshotSource: Send + Sync {
    async fn fetch_snapshot(&self) -> Result<RegistryRoles, RoleLookupError>;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RoleRefreshOutcome {
    Published,
    Unavailable,
    Superseded,
}

pub struct MonotonicRoleSnapshotClock;

impl RoleSnapshotClock for MonotonicRoleSnapshotClock {
    fn now(&self) -> Instant {
        Instant::now()
    }
}

struct RefreshMarker {
    started_at: Instant,
}

/// An opaque refresh identity, bound to the reader that admitted it.
/// Pointer identity avoids a wrapping generation counter or caller-supplied ids.
pub struct RoleRefreshTicket {
    marker: Arc<RefreshMarker>,
}

#[derive(Clone)]
enum PublishedRoles {
    NeverLoaded,
    Unavailable(RoleLookupError),
    Ready {
        roles: Arc<RegistryRoles>,
        captured_at: Instant,
    },
}

struct SnapshotState {
    refresh: Option<Arc<RefreshMarker>>,
    published: PublishedRoles,
}

/// Service-owned snapshot publication adapter. Request-time lookup never
/// fetches a registry. A failed refresh replaces previous success with unknown;
/// older refresh completions cannot replace the newest admitted refresh.
pub struct SnapshotRoleReader {
    max_age: Duration,
    clock: Arc<dyn RoleSnapshotClock>,
    state: Mutex<SnapshotState>,
}

/// Inventory of the currently usable generation. Ages use the injected
/// monotonic clock; they are not audit timestamps or a durable clean window.
#[derive(Debug, Clone, serde::Serialize)]
#[serde(tag = "state", rename_all = "kebab-case")]
pub enum RoleSnapshotStatus {
    NeverLoaded,
    Unavailable {
        reason: String,
    },
    Ready {
        age_seconds: u64,
        max_age_seconds: u64,
    },
    Expired {
        age_seconds: Option<u64>,
        max_age_seconds: u64,
    },
}

impl SnapshotRoleReader {
    pub fn snapshot_status(&self) -> RoleSnapshotStatus {
        let state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        match &state.published {
            PublishedRoles::NeverLoaded => RoleSnapshotStatus::NeverLoaded,
            PublishedRoles::Unavailable(error) => RoleSnapshotStatus::Unavailable {
                reason: error.to_string(),
            },
            PublishedRoles::Ready { captured_at, .. } => {
                let age = self.clock.now().checked_duration_since(*captured_at);
                let max_age_seconds = self.max_age.as_secs();
                match age {
                    Some(age) if age < self.max_age => RoleSnapshotStatus::Ready {
                        age_seconds: age.as_secs(),
                        max_age_seconds,
                    },
                    _ => RoleSnapshotStatus::Expired {
                        age_seconds: age.map(|age| age.as_secs()),
                        max_age_seconds,
                    },
                }
            }
        }
    }

    pub fn new(max_age: Duration, clock: Arc<dyn RoleSnapshotClock>) -> Self {
        Self {
            max_age,
            clock,
            state: Mutex::new(SnapshotState {
                refresh: None,
                published: PublishedRoles::NeverLoaded,
            }),
        }
    }

    pub fn begin_refresh(&self) -> RoleRefreshTicket {
        let marker = Arc::new(RefreshMarker {
            started_at: self.clock.now(),
        });
        let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        state.refresh = Some(marker.clone());
        RoleRefreshTicket { marker }
    }

    pub fn finish_refresh(
        &self,
        ticket: RoleRefreshTicket,
        result: Result<RegistryRoles, RoleLookupError>,
    ) -> bool {
        let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        if !state
            .refresh
            .as_ref()
            .is_some_and(|current| Arc::ptr_eq(current, &ticket.marker))
        {
            return false;
        }
        state.published = match result {
            Ok(roles) => PublishedRoles::Ready {
                roles: Arc::new(roles),
                captured_at: ticket.marker.started_at,
            },
            Err(error) => PublishedRoles::Unavailable(error),
        };
        state.refresh = None;
        true
    }

    /// Fetch outside the publication lock. A refused publication means a
    /// newer refresh owns the slot, not that the old fetch became current.
    pub async fn refresh_from(&self, source: &dyn RoleSnapshotSource) -> RoleRefreshOutcome {
        let ticket = self.begin_refresh();
        let result = source.fetch_snapshot().await;
        let outcome = match &result {
            Ok(_) => RoleRefreshOutcome::Published,
            Err(error) => {
                tracing::warn!(%error, "complete actor-role snapshot refresh unavailable");
                RoleRefreshOutcome::Unavailable
            }
        };
        if self.finish_refresh(ticket, result) {
            outcome
        } else {
            RoleRefreshOutcome::Superseded
        }
    }

    fn refresh_owner_stopped(&self) {
        let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        state.refresh = None;
        state.published = PublishedRoles::Unavailable(RoleLookupError::Unavailable(
            "actor-role snapshot refresh owner has stopped".into(),
        ));
    }

    /// Run under the service's owned task and shutdown receiver. Off does no
    /// registry work; shutdown cancels even a source that has not answered.
    pub async fn run_refresh_loop(
        self: Arc<Self>,
        source: Arc<dyn RoleSnapshotSource>,
        mode: Arc<dyn ReportModeSource>,
        cadence: Duration,
        mut stop: tokio::sync::watch::Receiver<bool>,
    ) -> Result<(), RoleLookupError> {
        if cadence.is_zero() {
            return Err(RoleLookupError::Unavailable(
                "actor-role refresh cadence must be nonzero".into(),
            ));
        }
        let mut ticks = tokio::time::interval(cadence);
        ticks.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            let tick_ready = tokio::select! {
                biased;
                _ = stop.wait_for(|stopped| *stopped) => false,
                _ = ticks.tick() => true,
            };
            if !tick_ready {
                self.refresh_owner_stopped();
                return Ok(());
            }
            if mode.mode() == ReportMode::Off {
                continue;
            }
            tokio::select! {
                biased;
                _ = stop.wait_for(|stopped| *stopped) => {
                    self.refresh_owner_stopped();
                    return Ok(());
                },
                _ = self.refresh_from(source.as_ref()) => {}
            }
        }
    }
}

impl SnapshotRoleReader {
    /// Read one fresh complete generation without request-time I/O.
    pub fn lookup(&self, actor: &str) -> Result<Option<RoleRecord>, RoleLookupError> {
        let published = self
            .state
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .published
            .clone();
        match published {
            PublishedRoles::NeverLoaded => Err(RoleLookupError::Unavailable(
                "complete role snapshot has never loaded".into(),
            )),
            PublishedRoles::Unavailable(error) => Err(error),
            PublishedRoles::Ready { roles, captured_at } => {
                let fresh = self
                    .clock
                    .now()
                    .checked_duration_since(captured_at)
                    .is_some_and(|age| age < self.max_age);
                if !fresh {
                    return Err(RoleLookupError::Unavailable(
                        "complete role snapshot is expired or has an invalid capture time".into(),
                    ));
                }
                roles.lookup(actor)
            }
        }
    }
}
#[async_trait]
impl RoleOfRecord for SnapshotRoleReader {
    async fn role_for(&self, actor: &str) -> Result<Option<RoleRecord>, RoleLookupError> {
        self.lookup(actor)
    }
}

impl HttpRoleReader {
    pub fn new(jobs: String, people: String, reader: User) -> Result<Self, RoleLookupError> {
        Self::with_source(
            jobs,
            people,
            reader,
            Duration::from_secs(2),
            boss_core::machine_token::shared(),
        )
    }

    pub fn with_source(
        jobs: String,
        people: String,
        reader: User,
        timeout: Duration,
        source: Arc<Source>,
    ) -> Result<Self, RoleLookupError> {
        let signed = serde_json::to_string(&reader)
            .map_err(|e| RoleLookupError::Unavailable(e.to_string()))?;
        let header = reqwest::header::HeaderValue::from_str(&signed)
            .map_err(|e| RoleLookupError::Unavailable(e.to_string()))?;
        let mut headers = reqwest::header::HeaderMap::new();
        headers.insert("x-boss-user", header);
        let http = machine_token::Client::build_with_source(
            reqwest::Client::builder()
                .timeout(timeout)
                .default_headers(headers),
            source,
        )
        .map_err(|e| RoleLookupError::Unavailable(e.to_string()))?;
        Ok(Self {
            jobs: jobs.trim_end_matches('/').into(),
            people: people.trim_end_matches('/').into(),
            http,
        })
    }

    /// Load one complete generation through the existing authenticated doors.
    /// Snapshot publication happens only after all three responses validate.
    pub async fn fetch_snapshot(&self) -> Result<RegistryRoles, RoleLookupError> {
        let (agents, automations, people) = tokio::try_join!(
            self.get(&self.jobs, "/api/agents"),
            self.get(&self.jobs, "/api/agents/automations"),
            self.get(&self.people, "/api/people?status=active"),
        )?;
        RegistryRoles::from_sources(agents, automations, people)
    }

    async fn get(&self, base: &str, path: &str) -> Result<Value, RoleLookupError> {
        let url = format!("{base}{path}");
        let mut response = self
            .http
            .get(&url)
            .send()
            .await
            .map_err(|e| RoleLookupError::Unavailable(format!("GET {path}: {e}")))?;
        if !response.status().is_success() {
            return Err(RoleLookupError::Unavailable(format!(
                "GET {path}: {}",
                response.status()
            )));
        }
        let mut bytes = Vec::new();
        while let Some(chunk) = response
            .chunk()
            .await
            .map_err(|e| RoleLookupError::Unavailable(e.to_string()))?
        {
            if bytes.len().saturating_add(chunk.len()) > MAX_RESPONSE_BYTES {
                return Err(RoleLookupError::Unavailable(format!(
                    "GET {path}: response exceeds {MAX_RESPONSE_BYTES} bytes"
                )));
            }
            bytes.extend_from_slice(&chunk);
        }
        serde_json::from_slice(&bytes)
            .map_err(|e| RoleLookupError::Unavailable(format!("GET {path}: malformed JSON: {e}")))
    }
}

fn complete_rows(
    value: Value,
    path: &str,
    bare_array: bool,
) -> Result<Vec<Value>, RoleLookupError> {
    if bare_array && let Value::Array(rows) = value {
        return Ok(rows);
    }
    let rows = value.get("data").and_then(Value::as_array);
    let total = value.get("total").and_then(Value::as_u64);
    match (rows, total) {
        (Some(rows), Some(total)) if total == rows.len() as u64 => Ok(rows.clone()),
        _ => Err(RoleLookupError::Unavailable(format!(
            "GET {path}: missing or incomplete row list"
        ))),
    }
}

fn matches_actor(
    row: &Value,
    actor: &str,
    aliases: bool,
    people: bool,
) -> Result<bool, RoleLookupError> {
    let id = row
        .get("id")
        .and_then(Value::as_str)
        .filter(|id| !id.trim().is_empty())
        .ok_or_else(|| RoleLookupError::Unavailable("registry row has no id".into()))?;
    if people {
        let status = row
            .get("status")
            .and_then(Value::as_str)
            .ok_or_else(|| RoleLookupError::Unavailable("people row has no status".into()))?;
        if status != "active" {
            return Ok(false);
        }
    }
    let mut matches = id == actor;
    if aliases {
        let aliases = row
            .get("aliases")
            .and_then(Value::as_array)
            .ok_or_else(|| {
                RoleLookupError::Unavailable("agent registry row has no aliases array".into())
            })?;
        for alias in aliases {
            let alias = alias.as_str().ok_or_else(|| {
                RoleLookupError::Unavailable("agent alias is not a string".into())
            })?;
            if alias.trim().is_empty() {
                return Err(RoleLookupError::Unavailable("agent alias is blank".into()));
            }
            matches |= alias == actor;
        }
    }
    if people {
        match row.get("email") {
            None | Some(Value::Null) => {}
            Some(Value::String(email)) if !email.trim().is_empty() => {
                matches |= email.eq_ignore_ascii_case(actor);
            }
            _ => {
                return Err(RoleLookupError::Unavailable(
                    "active people row has a malformed email".into(),
                ));
            }
        }
    }
    Ok(matches)
}

fn record(row: &Value) -> Result<RoleRecord, RoleLookupError> {
    let role = match row.get("role") {
        Some(Value::Null) => None,
        Some(Value::String(role)) => Some(role.clone()),
        _ => {
            return Err(RoleLookupError::Unavailable(
                "registry row has no role field or a malformed role".into(),
            ));
        }
    };
    let actor_id = row
        .get("id")
        .and_then(Value::as_str)
        .ok_or_else(|| RoleLookupError::Unavailable("registry row has no id".into()))?
        .into();
    Ok(RoleRecord { actor_id, role })
}

#[async_trait]
impl RoleOfRecord for HttpRoleReader {
    async fn role_for(&self, actor: &str) -> Result<Option<RoleRecord>, RoleLookupError> {
        // Finite independent GETs. This reader never calls policy itself;
        // registry doors authorize its named service using their own ports.
        self.fetch_snapshot().await?.role_for(actor).await
    }
}

#[async_trait]
impl RoleSnapshotSource for HttpRoleReader {
    async fn fetch_snapshot(&self) -> Result<RegistryRoles, RoleLookupError> {
        HttpRoleReader::fetch_snapshot(self).await
    }
}
