//! Report-only role comparison through explicit registry, policy and
//! telemetry dependencies (ddf0773e, approved design abf9eeae, car 2).
//! The asserted decision is returned unchanged. A registry error or an
//! absent role is an unknown comparison, never a made-up registry role.
//! Enforcing the record, granting roles and deleting headers are later cars.
//!
//! THE TALLY IS THE LIVE HALF; THE LOG IS THE WINDOW (backlog e0bdba74,
//! by the mechanism of design 21946380). Row H of design abf9eeae earns
//! `enforce` on 72 hours with zero would-deny for every registered actor
//! and zero unregistered writers. The tally lives in a process every
//! train restarts, so it can never be 72 hours old. Each tally start and
//! mode move, each shape `enforce` would answer differently at its FIRST
//! sighting, and the first overflow is therefore also a fact on the log
//! (`actor_role.*`, through [`boss_core::gate_evidence::Evidence`]) —
//! never one per request: repeats stay counts here. The window is read
//! back from the log by the join every gate's window uses
//! ([`boss_core::gate_window::join_window`]), and
//! [`ReportSnapshot::durable_window`] is true only when that read
//! succeeded.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex, PoisonError};

use async_trait::async_trait;
use boss_core::gate_evidence::{
    Evidence, EvidenceHealth, Fact, Gate, GateEvidenceLog, actor_role_tally_reasons,
    actor_role_would_refuse,
};
use boss_core::gate_window::{JoinedWindow, LiveRead, join_window};
use boss_core::machine_gate::Mode;
use boss_core::role_of_record::RoleOfRecord;
use chrono::{DateTime, Utc};
use serde::Serialize;
use serde_json::json;

use crate::{Action, Decision, PolicyClient, PolicyClientError, Predicate, Resource, User};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum ReportMode {
    Off,
    Report,
}

pub trait ReportModeSource: Send + Sync {
    fn mode(&self) -> ReportMode;

    /// The mode as the log names it, and when it last moved — `None`
    /// for a mode that never moves. A recording tally begins again at
    /// each move and states it (`actor_role.recording_began`), `off`
    /// included: silence alone cannot tell a quiet report from one that
    /// was not watching. This precursor never enforces, so the word
    /// `enforce` is stated as the `report` it behaves as.
    fn reading_since(&self) -> (Mode, Option<DateTime<Utc>>) {
        let mode = match self.mode() {
            ReportMode::Off => Mode::Off,
            ReportMode::Report => Mode::Report,
        };
        (mode, None)
    }
}

impl ReportModeSource for ReportMode {
    fn mode(&self) -> ReportMode {
        *self
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RoleObservation {
    pub actor: String,
    pub asserted_role: String,
    pub recorded_actor: Option<String>,
    pub recorded_role: Option<String>,
    pub action: String,
    pub resource: String,
    pub lookup_status: String,
    pub asserted_allowed: Option<bool>,
    /// A registered role's actual policy answer, absent when unjudgeable.
    pub recorded_allowed: Option<bool>,
    /// True only when a currently admitted operation would be denied.
    pub would_deny: Option<bool>,
    pub would_change_scope: Option<bool>,
}

fn asserted_observation(
    user: &User,
    action: Action,
    resource: &Resource,
    decision: Option<&Decision>,
) -> RoleObservation {
    RoleObservation {
        actor: user.id.clone(),
        asserted_role: user.role.clone(),
        recorded_actor: None,
        recorded_role: None,
        action: action.as_str().into(),
        resource: resource.as_str().into(),
        lookup_status: if decision.is_some() {
            "unregistered"
        } else {
            "asserted-policy-unavailable"
        }
        .into(),
        asserted_allowed: decision.map(Decision::is_allowed),
        recorded_allowed: None,
        would_deny: None,
        would_change_scope: None,
    }
}

fn compare_decisions(observation: &mut RoleObservation, asserted: &Decision, candidate: &Decision) {
    observation.recorded_allowed = Some(candidate.is_allowed());
    observation.would_deny = Some(asserted.is_allowed() && !candidate.is_allowed());
    observation.would_change_scope = Some(match (asserted, candidate) {
        (Decision::Allow { scope: a }, Decision::Allow { scope: b }) => a != b,
        _ => false,
    });
}

/// Observes the policy service's local evaluator without making a
/// recursive HTTP policy request. The candidate has no observer and
/// shares the admission's evaluation instant, not a transactional snapshot.
pub struct LocalReportingObserver<R: crate::port::PolicyRepository> {
    candidate: crate::engine::PolicyEngine<R>,
    roles: Arc<dyn RoleOfRecord>,
    sink: Arc<dyn RoleReportSink>,
    mode: Arc<dyn ReportModeSource>,
    budget: std::time::Duration,
}

impl<R: crate::port::PolicyRepository> LocalReportingObserver<R> {
    pub fn new(
        repo: Arc<R>,
        roles: Arc<dyn RoleOfRecord>,
        sink: Arc<dyn RoleReportSink>,
        mode: Arc<dyn ReportModeSource>,
        budget: std::time::Duration,
    ) -> Self {
        Self {
            candidate: crate::engine::PolicyEngine::new(repo),
            roles,
            sink,
            mode,
            budget,
        }
    }
}

#[async_trait]
impl<R: crate::port::PolicyRepository> crate::engine::PolicyDecisionObserver
    for LocalReportingObserver<R>
{
    async fn observe(
        &self,
        user: &User,
        action: Action,
        resource: Resource,
        at: chrono::DateTime<chrono::Utc>,
        result: &Result<
            (Decision, Option<chrono::DateTime<chrono::Utc>>),
            crate::port::PolicyError,
        >,
    ) {
        if self.mode.mode() == ReportMode::Off {
            return;
        }
        let decision = result.as_ref().ok().map(|(decision, _)| decision);
        let base = asserted_observation(user, action, &resource, decision);
        let Some(decision) = decision else {
            self.sink.record(base);
            return;
        };
        // Repository adapters filter expired overrides at their own read
        // time. Once the asserted override expires they cannot reproduce
        // that earlier view, even with the evaluator's captured instant.
        if result
            .as_ref()
            .ok()
            .and_then(|(_, expiry)| *expiry)
            .is_some_and(|expiry| expiry <= crate::engine::expiry_now())
        {
            self.sink.record(RoleObservation {
                lookup_status: "comparison-expired".into(),
                ..base
            });
            return;
        }
        let comparison = async {
            let mut observation = base.clone();
            match self.roles.role_for(&user.id).await {
                Ok(Some(record)) => {
                    observation.recorded_actor = Some(record.actor_id);
                    observation.recorded_role = record.role.filter(|role| !role.trim().is_empty());
                    if let Some(role) = &observation.recorded_role {
                        observation.lookup_status = "registered".into();
                        let candidate_user = User {
                            role: role.clone(),
                            ..user.clone()
                        };
                        match self
                            .candidate
                            .check_at(&candidate_user, action, resource.clone(), at)
                            .await
                        {
                            Ok((candidate, _)) => {
                                compare_decisions(&mut observation, decision, &candidate)
                            }
                            Err(_) => observation.lookup_status = "policy-unavailable".into(),
                        }
                    } else {
                        observation.lookup_status = "missing-role".into();
                    }
                }
                Ok(None) => {}
                Err(error) => {
                    observation.lookup_status = match error {
                        boss_core::role_of_record::RoleLookupError::Ambiguous(_) => "ambiguous",
                        _ => "unavailable",
                    }
                    .into()
                }
            }
            observation
        };
        let observation = match tokio::time::timeout(self.budget, comparison).await {
            Ok(observation) => observation,
            Err(_) => RoleObservation {
                lookup_status: "comparison-timeout".into(),
                ..base
            },
        };
        self.sink.record(observation);
    }
}

pub trait RoleReportSink: Send + Sync {
    fn record(&self, observation: RoleObservation);
}

impl RoleObservation {
    /// Why `enforce` would answer this observation differently, by the
    /// one rule the joined reader re-judges every live row with.
    pub fn would_refuse(&self) -> Option<&'static str> {
        actor_role_would_refuse(
            &self.lookup_status,
            &self.action,
            self.asserted_allowed,
            self.recorded_allowed,
            self.would_deny,
            self.would_change_scope,
        )
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct ReportRow {
    pub observation: RoleObservation,
    pub count: u64,
    pub first_seen: DateTime<Utc>,
    pub last_seen: DateTime<Utc>,
    /// [`RoleObservation::would_refuse`], stated so a reader need not
    /// re-derive it — and re-derived by the joined reader anyway.
    pub would_refuse: Option<&'static str>,
}

/// How far back a service's own answer reads its window: the 72 hours
/// row H of design abf9eeae asks for.
pub const WINDOW_HOURS: i64 = 72;

#[derive(Debug, Clone, Serialize)]
pub struct ReportSnapshot {
    pub rows: Vec<ReportRow>,
    pub overflow: u64,
    /// The mode this tally records in, and since when: the process's
    /// start or the mode's last move, whichever is later.
    pub mode: Mode,
    pub recording_since: DateTime<Utc>,
    /// Why this process's tally is not clean, one reason each.
    pub not_clean: Vec<String>,
    /// Whether this process's facts reach the log at all.
    pub evidence: EvidenceHealth,
    /// True ONLY when this answer's `window` was read from the log just
    /// now, by a process whose own observations reach that log. False
    /// from [`ReportTally::snapshot`], which reads no log; false when
    /// the log is dark, and `window_error` then says why.
    pub durable_window: bool,
    /// This service's last [`WINDOW_HOURS`] as the log and this process
    /// state them together — every process that watched, joined only
    /// across a clean end. `covers_requested_window` is the verdict for
    /// THIS service; the estate's is the joined reader's
    /// (`/api/events/gate-window?gate=actor-role`).
    pub window: Option<JoinedWindow>,
    pub window_error: Option<String>,
}

struct Counts {
    rows: BTreeMap<String, ReportRow>,
    overflow: u64,
    mode: Mode,
    since: DateTime<Utc>,
}

impl Counts {
    fn starting(mode: Mode, since: DateTime<Utc>) -> Self {
        Counts {
            rows: BTreeMap::new(),
            overflow: 0,
            mode,
            since,
        }
    }
}

/// How often a recording tally is looked at with no request arriving,
/// so a mode move is stated on the log within seconds — `off` most of
/// all, which a quiet service would otherwise never say.
const NUDGE: std::time::Duration = std::time::Duration::from_secs(5);

/// Bounded telemetry, owned by the service and explicitly injected into
/// the comparison. The counts reset with the process; what `enforce`
/// would answer differently does not, because each such shape's first
/// sighting is a fact on the log (module doc).
pub struct ReportTally {
    limit: usize,
    counts: Mutex<Counts>,
    /// Where the mode is read from, for a tally that records.
    mode: Option<Arc<dyn ReportModeSource>>,
    evidence: Evidence,
    log: Option<Arc<dyn GateEvidenceLog>>,
}

impl ReportTally {
    /// A tally whose facts reach no log and whose mode never moves: a
    /// test double, and a binary with no database. Its answer is never
    /// a durable window.
    pub fn new(limit: usize) -> Self {
        Self {
            limit,
            counts: Mutex::new(Counts::starting(Mode::Report, Utc::now())),
            mode: None,
            evidence: Evidence::none(Gate::ActorRole, "unnamed"),
            log: None,
        }
    }

    /// A tally stating its facts through `evidence`, beginning with the
    /// mode it records in now — a process start is a `recording_began`.
    pub fn recording(limit: usize, mode: Arc<dyn ReportModeSource>, evidence: Evidence) -> Self {
        let since = Utc::now();
        let (reading, _) = mode.reading_since();
        evidence.recording_began(reading, since);
        Self {
            limit,
            counts: Mutex::new(Counts::starting(reading, since)),
            mode: Some(mode),
            evidence,
            log: None,
        }
    }

    /// The log this tally's window is read back from.
    pub fn reading(mut self, log: Arc<dyn GateEvidenceLog>) -> Self {
        self.log = Some(log);
        self
    }

    /// Look at the mode every few seconds for as long as the tally
    /// lives, so a move is stated with no request arriving. Inside a
    /// runtime; outside one a move is stated at the next read.
    pub fn watched(self: Arc<Self>) -> Arc<Self> {
        if let Ok(rt) = tokio::runtime::Handle::try_current() {
            let watched = Arc::downgrade(&self);
            rt.spawn(async move {
                loop {
                    tokio::time::sleep(NUDGE).await;
                    let Some(tally) = watched.upgrade() else {
                        return;
                    };
                    drop(tally.current());
                }
            });
        }
        self
    }

    /// The counts, begun again first if the mode moved since they began
    /// — both under the counts' lock, from one read of the mode, so no
    /// reader sees a new mode beside the old tally's start. A tally
    /// begun again is a `recording_began` on the log, carrying the
    /// move's own instant.
    fn current(&self) -> std::sync::MutexGuard<'_, Counts> {
        let mut counts = self.counts.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some(source) = &self.mode {
            let (mode, moved) = source.reading_since();
            let moved = moved.filter(|at| *at > counts.since);
            if moved.is_some() || mode != counts.mode {
                let since = moved.unwrap_or_else(Utc::now);
                *counts = Counts::starting(mode, since);
                self.evidence.recording_began(mode, since);
            }
        }
        counts
    }

    /// This process's tally. It reads no log, so it is never a durable
    /// window; [`ReportTally::durable_snapshot`] is the answer that is.
    pub fn snapshot(&self) -> ReportSnapshot {
        let counts = self.current();
        let rows: Vec<ReportRow> = counts.rows.values().cloned().collect();
        let held: Vec<&ReportRow> = rows.iter().filter(|r| r.would_refuse.is_some()).collect();
        let evidence = self.evidence.health();
        let not_clean = actor_role_tally_reasons(
            counts.mode,
            held.len(),
            held.iter()
                .fold(0_u64, |sum, r| sum.saturating_add(r.count)),
            counts.overflow,
            &evidence,
        );
        ReportSnapshot {
            overflow: counts.overflow,
            mode: counts.mode,
            recording_since: counts.since,
            not_clean,
            evidence,
            durable_window: false,
            window: None,
            window_error: None,
            rows,
        }
    }

    /// The tally with its window read from the log now: this service's
    /// facts over the last [`WINDOW_HOURS`], joined with this process's
    /// live tally exactly as the estate's reader joins every service's.
    /// A log that cannot be read is said, never guessed past. The
    /// window's instants are the record's wall clock, as every gate
    /// fact's are — not the company's simulated business timeline.
    pub async fn durable_snapshot(&self) -> ReportSnapshot {
        let mut snapshot = self.snapshot();
        let Some(log) = &self.log else {
            snapshot.window_error =
                Some("this tally was given no log to read its window from".into());
            return snapshot;
        };
        let service = self.evidence.service().to_string();
        let from = Utc::now() - chrono::Duration::hours(WINDOW_HOURS);
        let facts = match log.facts(Gate::ActorRole, from).await {
            Ok(facts) => facts,
            Err(error) => {
                snapshot.window_error = Some(error);
                return snapshot;
            }
        };
        // After the read: nothing the log or the tally just answered
        // is later than the instant the window is judged at.
        let now = Utc::now();
        // This answer is about this service. Another service's facts
        // are the joined reader's to judge, beside that service's own
        // live tally — never this one's.
        let facts: Vec<_> = facts
            .into_iter()
            .filter(|e| {
                e.source == service
                    || e.payload.get("service").and_then(serde_json::Value::as_str)
                        == Some(service.as_str())
            })
            .collect();
        let live = LiveRead {
            service: service.clone(),
            answer: serde_json::to_value(&snapshot)
                .map(|report| json!({ "service": service, "report": report }))
                .map_err(|e| format!("this tally cannot be encoded: {e}")),
        };
        let mut window = join_window(
            Gate::ActorRole,
            std::slice::from_ref(&service),
            from,
            now,
            Ok(facts),
            vec![live],
        );
        // The verdict and its reasons, not a second copy of the inputs.
        window.observation = None;
        window.live.clear();
        if snapshot.evidence.recorder {
            snapshot.durable_window = true;
        } else {
            snapshot.window_error = Some(
                "this process has no recorder: what it observes is on no log, so no window \
                 over the log is its window"
                    .into(),
            );
        }
        snapshot.window = Some(window);
        snapshot
    }

    /// Count `observation` as overflow, stating the first on the log: a
    /// would-refuse that names no one, so the window is never clean
    /// past it.
    fn overflowed(&self, counts: &mut Counts) {
        counts.overflow = counts.overflow.saturating_add(1);
        if counts.overflow == 1 {
            self.evidence.emit(
                Fact::TallyOverflowed,
                json!({ "mode": counts.mode, "recording_since": counts.since, "scope": "tally" }),
            );
            tracing::warn!(
                service = self.evidence.service(),
                "actor-role tally is past its bounds ({} rows, 4096 bytes a row); what follows \
                 is counted as overflow, and the tally reads NOT clean until it begins again",
                self.limit
            );
        }
    }
}

impl RoleReportSink for ReportTally {
    fn record(&self, observation: RoleObservation) {
        // The row cap also needs a byte cap: a caller-controlled id must
        // not turn 512 rows into unbounded retained memory. An oversized
        // observation is counted as overflow, never shortened into evidence.
        // The same two caps bound the facts: at most `limit` first
        // sightings and one overflow per tally start, each under 4096
        // bytes of caller text.
        let bytes = [
            observation.actor.len(),
            observation.asserted_role.len(),
            observation.recorded_actor.as_ref().map_or(0, String::len),
            observation.recorded_role.as_ref().map_or(0, String::len),
            observation.action.len(),
            observation.resource.len(),
            observation.lookup_status.len(),
        ]
        .into_iter()
        .fold(0_usize, usize::saturating_add);
        // JSON is only a deterministic key here, containing finite strings
        // and scalars. Refuse a failed key as visible overflow, never silence.
        let key = serde_json::to_string(&observation);
        let mut counts = self.current();
        // A comparison made a microsecond before a move to `off` is
        // dropped: off records nothing, and a tally never reads cleaner
        // for it than what it watched.
        if counts.mode == Mode::Off {
            return;
        }
        let (true, Ok(key)) = (bytes <= 4096, key) else {
            self.overflowed(&mut counts);
            return;
        };
        let now = Utc::now();
        if let Some(row) = counts.rows.get_mut(&key) {
            row.count = row.count.saturating_add(1);
            row.last_seen = now;
        } else if counts.rows.len() < self.limit {
            let would_refuse = observation.would_refuse();
            // A shape's first sighting in this tally is one fact on the
            // log, and only a shape `enforce` would answer differently
            // is one: a clean estate states none.
            if let Some(reason) = would_refuse {
                self.evidence.emit(
                    Fact::WouldRefuse,
                    json!({
                        "mode": counts.mode,
                        "recording_since": counts.since,
                        "reason": reason,
                        "key": &observation,
                    }),
                );
            }
            counts.rows.insert(
                key,
                ReportRow {
                    observation,
                    count: 1,
                    first_seen: now,
                    last_seen: now,
                    would_refuse,
                },
            );
        } else {
            self.overflowed(&mut counts);
        }
    }
}

pub struct ReportingPolicyClient {
    inner: Arc<dyn PolicyClient>,
    roles: Arc<dyn RoleOfRecord>,
    sink: Arc<dyn RoleReportSink>,
    mode: Arc<dyn ReportModeSource>,
    budget: std::time::Duration,
}

impl ReportingPolicyClient {
    pub fn new(
        inner: Arc<dyn PolicyClient>,
        roles: Arc<dyn RoleOfRecord>,
        sink: Arc<dyn RoleReportSink>,
        mode: ReportMode,
    ) -> Self {
        Self::with_mode_source(inner, roles, sink, Arc::new(mode))
    }

    pub fn with_mode_source(
        inner: Arc<dyn PolicyClient>,
        roles: Arc<dyn RoleOfRecord>,
        sink: Arc<dyn RoleReportSink>,
        mode: Arc<dyn ReportModeSource>,
    ) -> Self {
        Self {
            inner,
            roles,
            sink,
            mode,
            budget: std::time::Duration::from_millis(100),
        }
    }

    pub fn with_comparison_budget(mut self, budget: std::time::Duration) -> Self {
        self.budget = budget;
        self
    }

    async fn observe(&self, user: &User, action: Action, resource: Resource, decision: &Decision) {
        if self.mode.mode() == ReportMode::Off {
            return;
        }
        if tokio::time::timeout(
            self.budget,
            self.observe_within_budget(user, action, resource.clone(), decision),
        )
        .await
        .is_err()
        {
            self.comparison_timeout(user, action, &resource, Some(decision));
        }
    }

    fn comparison_timeout(
        &self,
        user: &User,
        action: Action,
        resource: &Resource,
        decision: Option<&Decision>,
    ) {
        let mut observation = asserted_observation(user, action, resource, decision);
        observation.lookup_status = "comparison-timeout".into();
        self.sink.record(observation);
    }

    async fn observe_within_budget(
        &self,
        user: &User,
        action: Action,
        resource: Resource,
        decision: &Decision,
    ) {
        let mut observation = RoleObservation {
            actor: user.id.clone(),
            asserted_role: user.role.clone(),
            recorded_actor: None,
            recorded_role: None,
            action: action.as_str().into(),
            resource: resource.as_str().into(),
            lookup_status: "unregistered".into(),
            asserted_allowed: Some(decision.is_allowed()),
            recorded_allowed: None,
            would_deny: None,
            would_change_scope: None,
        };
        match self.roles.role_for(&user.id).await {
            Ok(Some(record)) => {
                observation.recorded_actor = Some(record.actor_id);
                observation.recorded_role = record.role.filter(|r| !r.trim().is_empty());
                if let Some(role) = &observation.recorded_role {
                    observation.lookup_status = "registered".into();
                    // Identity, access tier, session scope and department are
                    // preserved. This is a role comparison, never elevation.
                    let recorded = User {
                        role: role.clone(),
                        ..user.clone()
                    };
                    match self.inner.check(&recorded, action, resource).await {
                        Ok(candidate) => {
                            compare_decisions(&mut observation, decision, &candidate);
                        }
                        Err(error) => {
                            tracing::warn!(actor = %user.id, %error, "role comparison could not ask policy; asserted decision preserved");
                            observation.lookup_status = "policy-unavailable".into();
                        }
                    }
                } else {
                    observation.lookup_status = "missing-role".into();
                }
            }
            Ok(None) => {}
            Err(error) => {
                tracing::warn!(actor = %user.id, %error, "role registry could not answer; asserted decision preserved");
                observation.lookup_status = match error {
                    boss_core::role_of_record::RoleLookupError::Ambiguous(_) => "ambiguous",
                    _ => "unavailable",
                }
                .into();
            }
        }
        self.sink.record(observation);
    }

    fn unavailable(&self, user: &User, action: Action, resource: &Resource) {
        if self.mode.mode() == ReportMode::Report {
            self.sink.record(RoleObservation {
                actor: user.id.clone(),
                asserted_role: user.role.clone(),
                recorded_actor: None,
                recorded_role: None,
                action: action.as_str().into(),
                resource: resource.as_str().into(),
                lookup_status: "asserted-policy-unavailable".into(),
                asserted_allowed: None,
                recorded_allowed: None,
                would_deny: None,
                would_change_scope: None,
            });
        }
    }
}

#[async_trait]
impl PolicyClient for ReportingPolicyClient {
    async fn check(
        &self,
        user: &User,
        action: Action,
        resource: Resource,
    ) -> Result<Decision, PolicyClientError> {
        let decision = match self.inner.check(user, action, resource.clone()).await {
            Ok(decision) => decision,
            Err(error) => {
                self.unavailable(user, action, &resource);
                return Err(error);
            }
        };
        self.observe(user, action, resource, &decision).await;
        Ok(decision)
    }

    async fn scope_predicate(
        &self,
        user: &User,
        resource: Resource,
    ) -> Result<Predicate, PolicyClientError> {
        // The original predicate remains the inner client's, including its
        // error and scoped filtering. The extra read is telemetry only.
        let predicate = match self.inner.scope_predicate(user, resource.clone()).await {
            Ok(predicate) => predicate,
            Err(error) => {
                self.unavailable(user, Action::Read, &resource);
                return Err(error);
            }
        };
        if self.mode.mode() == ReportMode::Report {
            let comparison = async {
                match self.inner.check(user, Action::Read, resource.clone()).await {
                    Ok(decision) => {
                        self.observe_within_budget(user, Action::Read, resource.clone(), &decision)
                            .await
                    }
                    Err(error) => {
                        self.unavailable(user, Action::Read, &resource);
                        tracing::warn!(%error, "role report read comparison unavailable; original predicate preserved");
                    }
                }
            };
            if tokio::time::timeout(self.budget, comparison).await.is_err() {
                self.comparison_timeout(user, Action::Read, &resource, None);
            }
        }
        Ok(predicate)
    }
}
