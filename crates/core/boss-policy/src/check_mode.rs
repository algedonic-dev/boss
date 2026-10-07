//! The policy check's own mode: F7 of backlog b8e75382, row D of design
//! b08725c2.
//!
//! WHAT F7 CLOSES. `POST /api/policy/check` answers a SESSION caller only
//! about itself unless it holds Read on `policy-rule` (5a914364 S1). Two
//! arms are still answered about anyone, deny reason included:
//!
//! * [`Arm::Unsigned`] — no `x-boss-user` at all: a caller that reached
//!   the port directly, past the gateway, with no identity.
//! * [`Arm::Service`] — a caller presenting a service's `automation:` id
//!   whose Read on `policy-rule` the bound refuses. Since 2026-09-29 every
//!   `ReqwestPolicyClient` signs as its service, and a refused service is
//!   answered with a warn (hold F1 of review b8e7) because refusing it
//!   hangs every check in the estate on one mutable grant. The same arm
//!   is R1 of that review: an `automation:` id is ASSERTED, so an
//!   employee whose id starts with it skips the read-your-own rule.
//!
//! WHY A MODE AND NOT A REFUSAL (David, 2026-09-29, design b08725c2):
//! "F7 ships behind its own mode key, beside the machine-gate mode and
//! read the same way: off, then report, then enforce, so its rollback is
//! one converged edit, not a deploy". A misfire of F7 denies every
//! signed-in write — the operator's included, and break-glass does not
//! help — so a revert car may not be able to ride. The word lives in a
//! mounted file the service re-reads every few seconds, through the
//! machine gate's own reader ([`ModeSwitch`]):
//!
//! * `off` — the behaviour before the mode existed, and what an absent
//!   file reads as: an unsigned check is not judged; a refused service
//!   is answered with the same warn as before. Nothing is tallied.
//! * `report` — both arms are judged and answered as before; each one
//!   `enforce` would refuse is tallied and warned. The tally is the clean
//!   window the flip is earned on (the design: D enforces once its count
//!   reads zero over 72 hours, every process older than the window), so
//!   it says when it began (`recording_since`: the process's start or
//!   the last mode move) and judges itself once (`clean_since`, `None`
//!   while any row or any overflow stands) — enforce checklist F1 and F2
//!   of review 1c2860f4. Mounted at `report` by
//!   `infra/cluster/manifests/boss.yaml` from the report-prep car of
//!   backlog b8e75382 (checklist F4) until row D's car, and read hourly
//!   by the lapsed-grant alarm (`policy.check.refusals.alarm`, R3).
//! * `enforce` — both arms are refused 403, naming the mode and the file
//!   that is the way back. Still tallied, so a refusal is countable.
//!   Mounted by `infra/cluster/manifests/boss.yaml` since row D's car,
//!   after G1 (47aed706 car 3, checklist F3); the pin
//!   `the_machine_gate_reports_and_refuses_nothing.rs` holds the word to
//!   that one line. It bounds the caller with NO identity. It does not
//!   prove a claimed one: [`Arm::Service`] trusts an asserted
//!   `automation:` id until the machine gate enforces (row C), so a LAN
//!   caller that asserts one at platform-admin is still answered.
//!
//! An unreadable file, or a word that is not a mode, is `report` said at
//! ERROR — never `off`, and never `enforce`.
//!
//! THE TALLY IS THE LIVE HALF; THE LOG IS THE WINDOW (design 21946380,
//! backlog b0787727). The tally restarts with the process, and every
//! train restarts it, so it can never be 72 hours old. Each tally start
//! and mode move, each key's first sighting and the first overflow is
//! therefore also stated on the log (`policy.check.*`, through
//! [`boss_core::gate_evidence::Evidence`]); `gate_evidence::window` reads
//! the window there, and this tally's `not_clean` — which names any fact
//! the log did not take — is the half only a live process can answer.
//!
//! THE WAY BACK, for the car that flips it: set the word to `report` in
//! the mounted file — the converge renders it from boss.yaml; if the
//! converge is itself refused, the ConfigMap over the LAN kube road, the
//! `kubectl -n boss patch configmap boss-machine-gate` spelled beside the
//! ConfigMap in boss.yaml, which passes neither the jobs API nor the
//! forge. Leaving `enforce` is logged at WARN by the switch, and begins
//! the tally again.

use std::collections::HashMap;
use std::net::IpAddr;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use boss_core::gate_evidence::{Evidence, EvidenceHealth, Fact, Gate};
pub use boss_core::machine_gate::Mode;
use boss_core::machine_gate::ModeSwitch;
use boss_core::port::EventRecorder;
use chrono::{DateTime, Utc};
use serde::Serialize;
use serde_json::json;

use boss_policy_client::types::User;

/// The environment variable naming the mode file.
pub const MODE_FILE_ENV: &str = "BOSS_POLICY_CHECK_MODE_FILE";
/// Beside the machine gate's own `mode` key, so the one ConfigMap the
/// machine-token rollout mounts at `/etc/boss/machine-gate/` can carry
/// both words (design b08725c2: "beside the machine-gate mode").
/// `infra/cluster/manifests/boss.yaml` mounts it at `report` (checklist
/// F4 of backlog b8e75382); where no ConfigMap carries it, it is absent,
/// which is `off`.
pub const DEFAULT_MODE_FILE: &str = "/etc/boss/machine-gate/policy-check";
/// The switch's name in everything it says.
pub const SWITCH: &str = "policy check";
/// The tally, answered to a holder of Read on `policy-rule`: it names
/// callers and their addresses.
pub const REFUSALS_PATH: &str = "/api/policy/check/refusals";
/// How many distinct keys one process holds; past it a new key is
/// counted in `overflow`, so a caller spraying identities cannot grow the
/// service's memory without bound.
pub const MAX_TALLY_KEYS: usize = boss_core::gate_evidence::POLICY_TALLY_KEYS;
/// Longest caller id or role kept in a key: both are caller text.
const MAX_ID_CHARS: usize = 96;
/// How often a mounted switch's tally is looked at with no check
/// arriving, so a mode move is stated on the log within seconds of the
/// switch reading it rather than at the next check — `off` most of all,
/// which a quiet port would otherwise never say (design 21946380). The
/// fact carries the move's own instant either way.
const NUDGE: std::time::Duration = std::time::Duration::from_secs(5);
/// The service the policy check runs in, as its facts name it.
const SERVICE: &str = "policy";

/// Which of the two open arms a check came through.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum Arm {
    /// No `x-boss-user`: no identity at all.
    Unsigned,
    /// An `automation:` id the read bound refuses.
    Service,
}

impl Arm {
    pub fn name(self) -> &'static str {
        match self {
            Arm::Unsigned => "unsigned",
            Arm::Service => "service",
        }
    }
}

/// One caller shape the tally counts. The peer is the IP alone — a port
/// is ephemeral, and would make every connection its own key.
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize)]
pub struct RefusalKey {
    pub arm: Arm,
    pub caller: String,
    pub role: String,
    pub peer: String,
}

#[derive(Clone, Debug, Serialize)]
pub struct RefusalRow {
    #[serde(flatten)]
    pub key: RefusalKey,
    pub count: u64,
    /// When this key was first and last counted: an alarm reading a
    /// lapsed service grant (R3 of the 2026-09-29 follow-ups on backlog
    /// b8e75382) judges whether it is lapsing NOW by `last_seen`, since
    /// the count only grows until the tally begins again.
    pub first_seen: DateTime<Utc>,
    pub last_seen: DateTime<Utc>,
}

/// What [`REFUSALS_PATH`] answers. In `report` the rows are what
/// `enforce` WOULD have refused; in `enforce`, what it did.
#[derive(Clone, Debug, Serialize)]
pub struct Refusals {
    pub switch: &'static str,
    pub file: String,
    pub mode: Mode,
    /// Why the mode is not what the file says, when it could not be read
    /// as one.
    pub mode_error: Option<String>,
    pub rows: Vec<RefusalRow>,
    pub overflow: u64,
    /// When this tally began: the process's start, or the switch's last
    /// MODE move, whichever is later (enforce checklist F1, review
    /// 1c2860f4). The tally lives in memory, so a restart empties it;
    /// and a count that spanned a move describes two modes, one of which
    /// may be `off`, which watches nothing. A reader judging "zero for 72
    /// hours" must refuse a tally that began inside the window.
    pub recording_since: DateTime<Utc>,
    /// `recording_since` when the tally is CLEAN — the mode watches (not
    /// `off`), no row, and no overflow — else `None`. The one verdict,
    /// computed here so no reader re-derives it by filtering rows: the
    /// window the enforce flip is earned on is `now - clean_since`.
    pub clean_since: Option<DateTime<Utc>>,
    /// Why the tally is not clean, one reason each, empty when it is.
    /// Overflow > 0 is ALWAYS one (checklist F2): anyone on the port can
    /// fill the keys with forged `automation:` ids, after which a real
    /// refusal is counted there and named nowhere. So is a fact the
    /// recorder is refusing, or one lost that no `facts_lost` on the log
    /// states yet — which no tally restart forgives (design 21946380;
    /// review e4417d48, B2).
    pub not_clean: Vec<String>,
    /// Whether this process's facts reach the log at all.
    pub evidence: EvidenceHealth,
}

impl Refusals {
    fn judge(&mut self) {
        let why = boss_core::gate_evidence::policy_tally_reasons(
            self.mode,
            self.rows.len(),
            self.rows.iter().map(|row| row.count).sum(),
            self.overflow,
            &self.evidence,
        );
        self.clean_since = why.is_empty().then_some(self.recording_since);
        self.not_clean = why;
    }
}

/// When one key was counted, and how often.
struct Seen {
    count: u64,
    first: DateTime<Utc>,
    last: DateTime<Utc>,
}

struct Tally {
    rows: HashMap<RefusalKey, Seen>,
    overflow: u64,
    since: DateTime<Utc>,
}

impl Tally {
    fn starting(since: DateTime<Utc>) -> Self {
        Tally {
            rows: HashMap::new(),
            overflow: 0,
            since,
        }
    }
}

/// The switch, its tally, and where the tally's facts are stated.
pub struct CheckMode {
    switch: Arc<ModeSwitch>,
    tally: Mutex<Tally>,
    evidence: Evidence,
}

impl std::fmt::Debug for CheckMode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CheckMode")
            .field("switch", &self.switch)
            .finish_non_exhaustive()
    }
}

impl CheckMode {
    /// The switch whose file [`MODE_FILE_ENV`] names (else
    /// [`DEFAULT_MODE_FILE`]), read now and re-read on the machine gate's
    /// cadence, stating its facts through `recorder` — the policy
    /// service's outbox. Call it inside the runtime, at boot.
    pub fn mount(recorder: Arc<dyn EventRecorder>) -> Arc<Self> {
        let switch = ModeSwitch::mount(SWITCH, MODE_FILE_ENV, DEFAULT_MODE_FILE);
        let evidence = Evidence::spawn(Gate::PolicyCheck, SERVICE, recorder);
        let mode = Arc::new(Self::over(switch).with_evidence(evidence));
        if let Ok(rt) = tokio::runtime::Handle::try_current() {
            let watched = Arc::downgrade(&mode);
            rt.spawn(async move {
                loop {
                    tokio::time::sleep(NUDGE).await;
                    let Some(mode) = watched.upgrade() else {
                        return;
                    };
                    drop(mode.current());
                }
            });
        }
        mode
    }

    /// This switch stating its facts through `evidence`, beginning with
    /// the mode its tally records in now — a process start is a
    /// `recording_began` (design 21946380 point 2).
    pub fn with_evidence(mut self, evidence: Evidence) -> Self {
        let tally = self.tally.get_mut().unwrap_or_else(PoisonError::into_inner);
        evidence.recording_began(self.switch.mode(), tally.since);
        self.evidence = evidence;
        self
    }

    /// A switch held at `mode` and never re-read — for a router built in
    /// a test, which has no mounted file.
    pub fn fixed(mode: Mode) -> Arc<Self> {
        Arc::new(Self::over(Arc::new(ModeSwitch::new(
            SWITCH,
            "(fixed, no file)",
            (mode, None),
        ))))
    }

    /// A switch whose facts reach no log — a router built in a test.
    /// [`CheckMode::with_evidence`] gives it one.
    pub fn over(switch: Arc<ModeSwitch>) -> Self {
        let evidence = Evidence::none(Gate::PolicyCheck, SERVICE);
        CheckMode {
            switch,
            tally: Mutex::new(Tally::starting(Utc::now())),
            evidence,
        }
    }

    pub fn mode(&self) -> Mode {
        self.switch.mode()
    }

    /// The file that is the way back, as a refusal names it.
    pub fn file(&self) -> String {
        self.switch.path().display().to_string()
    }

    /// The tally, begun again first if the switch's mode moved since it
    /// began, with the reading it was judged against — both taken under
    /// the tally's lock, from ONE read of the switch, so no reader sees a
    /// new mode beside the old tally's start (the D2 race of review
    /// fd151a97, which the machine gate closed the same way). The lock
    /// order is tally, then switch; the switch's own re-read takes only
    /// its lock, so the two cannot deadlock. A tally begun again is a
    /// `recording_began` on the log, carrying the move's own instant.
    fn current(&self) -> (MutexGuard<'_, Tally>, (Mode, Option<String>)) {
        let mut tally = self.tally.lock().unwrap_or_else(PoisonError::into_inner);
        let (reading, since) = self.switch.reading_since();
        if tally.since < since {
            *tally = Tally::starting(since);
            self.evidence.recording_began(reading.0, since);
        }
        (tally, reading)
    }

    /// Count one check `enforce` refuses (or, in `report`, would). A
    /// check decided a microsecond before a mode move is counted in the
    /// new mode's tally, or dropped if the move was to `off`, which
    /// records nothing — the side that never makes a tally read cleaner
    /// than what it watched.
    pub fn record(&self, arm: Arm, caller: &User, peer: Option<IpAddr>) {
        let clip = |s: &str| s.chars().take(MAX_ID_CHARS).collect::<String>();
        let key = RefusalKey {
            arm,
            caller: clip(&caller.id),
            role: clip(&caller.role),
            peer: peer.map_or_else(|| "unknown".to_string(), |ip| ip.to_string()),
        };
        let (mut tally, (mode, _)) = self.current();
        if mode == Mode::Off {
            return;
        }
        let now = Utc::now();
        if let Some(seen) = tally.rows.get_mut(&key) {
            seen.count += 1;
            seen.last = now;
            return;
        }
        let since = tally.since;
        if tally.rows.len() >= MAX_TALLY_KEYS {
            tally.overflow += 1;
            if tally.overflow == 1 {
                self.evidence.emit(
                    Fact::TallyOverflowed,
                    json!({ "mode": mode, "recording_since": since, "scope": "tally" }),
                );
                tracing::warn!(
                    "{SWITCH} tally is full ({MAX_TALLY_KEYS} keys); further new keys are \
                     counted as overflow, and the tally reads NOT clean until it begins again"
                );
            }
            return;
        }
        // A key's first sighting in this tally is one fact on the log —
        // never one per check (design 21946380 point 1): `refused` when
        // `enforce` refused it, `would_refuse` when `report` answered it.
        self.evidence.emit(
            if mode == Mode::Enforce {
                Fact::Refused
            } else {
                Fact::WouldRefuse
            },
            json!({ "mode": mode, "recording_since": since, "key": &key }),
        );
        tally.rows.insert(
            key,
            Seen {
                count: 1,
                first: now,
                last: now,
            },
        );
    }

    /// The tally, sorted by key, with its clean verdict.
    pub fn refusals(&self) -> Refusals {
        let (tally, (mode, mode_error)) = self.current();
        let mut rows: Vec<RefusalRow> = tally
            .rows
            .iter()
            .map(|(key, seen)| RefusalRow {
                key: key.clone(),
                count: seen.count,
                first_seen: seen.first,
                last_seen: seen.last,
            })
            .collect();
        rows.sort_by(|a, b| a.key.cmp(&b.key));
        let mut refusals = Refusals {
            switch: SWITCH,
            file: self.file(),
            mode,
            mode_error,
            rows,
            overflow: tally.overflow,
            recording_since: tally.since,
            clean_since: None,
            not_clean: Vec::new(),
            evidence: self.evidence.health(),
        };
        drop(tally);
        refusals.judge();
        refusals
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_tally_counts_by_caller_and_is_bounded() {
        let mode = CheckMode::fixed(Mode::Report);
        let anon = User::anonymous();
        let svc = User::service("jobs");
        let peer: Option<IpAddr> = "10.20.0.7".parse().ok();
        mode.record(Arm::Unsigned, &anon, peer);
        mode.record(Arm::Unsigned, &anon, peer);
        mode.record(Arm::Service, &svc, None);
        let r = mode.refusals();
        assert_eq!(r.mode, Mode::Report);
        assert_eq!(r.rows.len(), 2, "{r:?}");
        assert_eq!(r.rows[0].key.arm, Arm::Unsigned);
        assert_eq!(r.rows[0].key.peer, "10.20.0.7");
        assert_eq!(r.rows[0].count, 2);
        assert_eq!(r.rows[1].key.caller, "automation:jobs");
        assert_eq!(r.rows[1].key.peer, "unknown");

        for i in 0..(MAX_TALLY_KEYS + 3) {
            let u = User {
                id: format!("automation:spray-{i}"),
                ..svc.clone()
            };
            mode.record(Arm::Service, &u, None);
        }
        let r = mode.refusals();
        assert_eq!(r.rows.len(), MAX_TALLY_KEYS);
        assert_eq!(r.overflow, 5);
    }

    /// Enforce checklist F1 (review 1c2860f4 of car c81280cc): the tally
    /// says when it began, and begins again on a restart (a new
    /// CheckMode) and on EVERY mode move — so `off` turned `report` a
    /// minute ago, or `report` that spent an hour at `off` in between,
    /// never reads as a 72-hour window that was watched. And `off`
    /// records nothing: its silence is no evidence.
    #[test]
    fn the_tally_restarts_on_a_restart_and_on_every_mode_move() {
        let t0 = Utc::now();
        let switch = Arc::new(ModeSwitch::new(SWITCH, "(test)", (Mode::Report, None)));
        let mode = CheckMode::over(Arc::clone(&switch));
        let svc = User::service("jobs");
        mode.record(Arm::Service, &svc, None);
        let r = mode.refusals();
        assert!(
            r.recording_since >= t0 && r.recording_since <= Utc::now(),
            "a restart is a new tally: {}",
            r.recording_since
        );
        assert_eq!(r.rows.len(), 1);
        let row = &r.rows[0];
        assert!(row.first_seen >= t0 && row.last_seen >= row.first_seen);

        let mut since = r.recording_since;
        for next in [Mode::Enforce, Mode::Report, Mode::Off, Mode::Report] {
            mode.record(Arm::Service, &svc, None);
            std::thread::sleep(std::time::Duration::from_millis(2));
            assert!(switch.observe((next, None)));
            let r = mode.refusals();
            assert_eq!(r.mode, next);
            assert!(
                r.recording_since > since,
                "a move to {next:?} begins the tally again"
            );
            assert!(r.rows.is_empty() && r.overflow == 0, "{next:?}: {r:?}");
            since = r.recording_since;
        }

        // `off` records nothing.
        assert!(switch.observe((Mode::Off, None)));
        mode.record(Arm::Service, &svc, None);
        assert!(mode.refusals().rows.is_empty());
    }

    /// The clean verdict is computed HERE, once, so no reader re-derives
    /// it by filtering rows: `off` watched nothing, a row is a check
    /// `enforce` refuses, and overflow > 0 is NEVER clean — a real
    /// refusal that arrives after forged ids filled the tally lands in
    /// overflow, unnamed (enforce checklist F2, review 1c2860f4).
    #[test]
    fn only_an_empty_watched_tally_reads_clean_and_overflow_never_does() {
        let r = CheckMode::fixed(Mode::Report).refusals();
        assert_eq!(r.clean_since, Some(r.recording_since), "{r:?}");
        assert!(r.not_clean.is_empty());

        let r = CheckMode::fixed(Mode::Off).refusals();
        assert_eq!(r.clean_since, None);
        assert!(r.not_clean[0].contains("off"), "{:?}", r.not_clean);

        let mode = CheckMode::fixed(Mode::Enforce);
        mode.record(Arm::Unsigned, &User::anonymous(), None);
        let r = mode.refusals();
        assert_eq!(r.clean_since, None);
        assert!(r.not_clean[0].contains("1 caller"), "{:?}", r.not_clean);

        // A spray fills the keys; the overflow is named on its own.
        let mode = CheckMode::fixed(Mode::Report);
        for i in 0..=MAX_TALLY_KEYS {
            let u = User {
                id: format!("automation:spray-{i}"),
                ..User::service("jobs")
            };
            mode.record(Arm::Service, &u, None);
        }
        let r = mode.refusals();
        assert_eq!(r.overflow, 1);
        assert_eq!(r.clean_since, None);
        assert!(
            r.not_clean.iter().any(|w| w.contains("overflow 1")),
            "{:?}",
            r.not_clean
        );
    }

    fn stated(
        rx: &mut tokio::sync::mpsc::Receiver<boss_core::event::Event>,
    ) -> Vec<(String, serde_json::Value)> {
        std::iter::from_fn(|| rx.try_recv().ok())
            .map(|e| {
                (
                    e.kind.trim_start_matches("policy.check.").to_string(),
                    e.payload,
                )
            })
            .collect()
    }

    /// Design 21946380 points 1 and 2: the tally's start and every mode
    /// move are a `recording_began` carrying the move's own instant; a
    /// key's FIRST sighting is one fact — `would_refuse` in `report`,
    /// `refused` in `enforce` — and the first overflow one more. Never
    /// one per check: the log holds who tried the door, the tally how
    /// often.
    #[test]
    fn a_start_a_mode_move_and_each_new_key_are_one_fact_each() {
        let switch = Arc::new(ModeSwitch::new(SWITCH, "(test)", (Mode::Report, None)));
        let (evidence, mut rx) = Evidence::channel(Gate::PolicyCheck, SERVICE);
        let mode = CheckMode::over(Arc::clone(&switch)).with_evidence(evidence);
        let svc = User::service("jobs");
        for _ in 0..3 {
            mode.record(Arm::Service, &svc, None);
        }
        let facts = stated(&mut rx);
        let kinds: Vec<&str> = facts.iter().map(|(k, _)| k.as_str()).collect();
        assert_eq!(kinds, vec!["recording_began", "would_refuse"]);
        assert_eq!(facts[0].1["mode"], "report");
        assert_eq!(facts[0].1["service"], "policy");
        assert_eq!(facts[1].1["key"]["caller"], "automation:jobs");
        assert_eq!(facts[1].1["key"]["arm"], "service");

        std::thread::sleep(std::time::Duration::from_millis(2));
        assert!(switch.observe((Mode::Enforce, None)));
        mode.record(Arm::Service, &svc, None);
        assert!(switch.observe((Mode::Off, None)));
        let _ = mode.refusals();
        let facts = stated(&mut rx);
        let kinds: Vec<&str> = facts.iter().map(|(k, _)| k.as_str()).collect();
        assert_eq!(
            kinds,
            vec!["recording_began", "refused", "recording_began"],
            "a move begins the tally again, so the key is new in `enforce`"
        );
        assert_eq!(facts[0].1["mode"], "enforce");
        assert_eq!(facts[2].1["mode"], "off");
        assert_eq!(
            facts[2].1["since"],
            serde_json::json!(switch.mode_since().1),
            "the move's own instant, not when it was noticed"
        );

        assert!(switch.observe((Mode::Report, None)));
        for i in 0..(MAX_TALLY_KEYS + 2) {
            let u = User {
                id: format!("automation:spray-{i}"),
                ..User::service("jobs")
            };
            mode.record(Arm::Service, &u, None);
        }
        let facts = stated(&mut rx);
        let overflowed = facts
            .iter()
            .filter(|(k, _)| k == "tally_overflowed")
            .count();
        assert_eq!(overflowed, 1);
        assert_eq!(
            facts.iter().filter(|(k, _)| k == "would_refuse").count(),
            MAX_TALLY_KEYS
        );
    }

    /// A fact the log did not take reads the tally NOT clean until the
    /// tally begins again (design 21946380 point 3): the live half names
    /// what the log half cannot know it is missing.
    #[test]
    fn a_fact_the_log_did_not_take_reads_the_tally_not_clean() {
        let switch = Arc::new(ModeSwitch::new(SWITCH, "(test)", (Mode::Report, None)));
        let (evidence, mut rx) = Evidence::channel(Gate::PolicyCheck, SERVICE);
        let mode = CheckMode::over(Arc::clone(&switch)).with_evidence(evidence.clone());
        // The queue to the log is full, so the would-refuse is lost.
        for _ in 1..boss_core::gate_evidence::QUEUE_DEPTH {
            evidence.emit(Fact::RecordingBegan, json!({}));
        }
        mode.record(Arm::Service, &User::service("jobs"), None);
        let r = mode.refusals();
        assert_eq!(r.clean_since, None);
        assert_eq!(r.evidence.lost, 1);
        assert!(
            r.not_clean.iter().any(|w| w.contains("not on the log")),
            "{:?}",
            r.not_clean
        );
        // Review e4417d48, B2: the recorder catches up, then the mode
        // moves — every move begins the tally again, and the log takes
        // the move's own fact. The loss is still not on the log, so the
        // restart does not forgive it.
        while rx.try_recv().is_ok() {}
        std::thread::sleep(std::time::Duration::from_millis(2));
        assert!(switch.observe((Mode::Enforce, None)));
        let r = mode.refusals();
        assert!(r.rows.is_empty(), "the move began the tally again");
        assert!(
            r.not_clean.iter().any(|w| w.contains("not on the log")),
            "{:?}",
            r.not_clean
        );

        let r = CheckMode::fixed(Mode::Report).refusals();
        assert!(!r.evidence.recorder && r.not_clean.is_empty(), "{r:?}");
    }
}
