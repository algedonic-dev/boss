//! `credential.rotate.self-issued` — the credential broker's issuer for
//! a value with no issuer at all: the estate machine token (design
//! 6805c764, car 3; backlog 2710c8fc). The ops runner's credential
//! (`credential.rotate.ops-runner`, backlog 1e50e66b) is minted the same
//! way into a different Secret, with a different verification (its own
//! door, one host at a time) and delivery; the two are separate handlers.
//!
//! WHAT IT REPLACES. The token every service port's machine gate accepts
//! was to be placed by hand — `openssl rand`, a hand-made Secret, a
//! drop-in on a host (the machine-token activation runbook, deleted with
//! this car). David, 2026-09-25: the value is never placed by hand;
//! the broker mints and rotates it. A machine token is a random value we
//! make ourselves, so there is no issuer to call and no root credential
//! to hold: the value is 32 bytes from the operating system's random
//! source ([`fresh_value`]), generated inside this handler and written
//! only to the Secret store.
//!
//! WHY IT CANNOT BE A PLAIN MINT-AND-SWAP. Every service port accepts the
//! token and every machine caller sends it, and the two read one mounted
//! Secret that kubelet refreshes about a minute apart on each pod. A
//! value swapped in one write would be refused wherever a gate saw it
//! before a caller did, or the other way round. So the gate accepts three
//! slots — `current`, `next`, `previous` (`boss_core::machine_gate`) —
//! callers always send `current`, and a rotation walks the packet's four
//! machine steps through them:
//!
//! - **issue + install** — generate the value and write it to `next` in
//!   every declared Secret (the rule's `secret_namespace`/`secret_name`,
//!   then the same name in each of `also_in_namespaces`), and read it
//!   back equal. Nothing sends `next`; every gate starts accepting it.
//! - **verify** — ask every service port's `GET /api/machine-gate/accepts`,
//!   presenting the new value, until each answers `matched: next`. Then
//!   PROMOTE, one write per Secret: `current` → `previous`, `next` →
//!   `current`, `promoted-at` = now. Callers now send the new value and
//!   every gate still accepts the old one as `previous`. The step
//!   completes when every port answers `matched: current`.
//! - **revoke** — blank `previous`, but only once the gates have shown
//!   for a whole drain window that nothing presents it: every served
//!   port's tally (`GET /api/machine-gate/misses`) in `report` or
//!   `enforce`, no overflow that could hide a `previous` (one noisy
//!   source past its own share is named, not held on — backlog
//!   93bcf490; a `previous` is always keyed, so it ages out like any
//!   row — review ebc7b1cc), begun before the window opened, with no
//!   `previous` match inside it. A caller still sending the old value —
//!   an off-cluster host whose pull has not run — holds the step open and
//!   is named on it. The window is the rule's `drain_minutes`, never
//!   below [`DRAIN_FLOOR_MINUTES`], and pinned to outlast the longest
//!   CronJob period in the manifests so every scheduled caller has run
//!   inside it. A gate in `off` records nothing, so its silence is not
//!   evidence and the revoke waits: no evidence is not a pass.
//!
//! The first mint is the same walk with an empty Secret: `previous`
//! stays blank at promotion and the revoke has nothing to drain.
//!
//! TWO FIRINGS, ONE DECLARATION. The scope step of a `rotate-a-credential`
//! packet fires `broker-rotates-the-machine-token`, and a fifteen-minute
//! clock rule, `broker-advances-the-machine-token-rotation`, fires the
//! same handler with `phase = "advance"` over every open packet about the
//! credential. Each firing does what the state allows — kubelet's refresh
//! and the drain window are minutes and hours, far past the 30-second ack
//! window one delivery may hold — records why it stopped on the step
//! (`<step>_deferred`), and acknowledges. Every pass reads the state
//! afresh, so any firing can resume any other's work.
//!
//! THE SECRET IS THE LEDGER. Beside each slot the handler writes
//! `<slot>.minted-for`, the packet that made that value, in the same
//! merge-patch: a redelivery finds its own staged value and re-uses it,
//! a promotion already written in one Secret is not written twice, and a
//! packet that finds another's value where its own should be refuses.
//! ONE ROTATION AT A TIME: a packet issues only when no other is between
//! install and revoke and none opened earlier is waiting to start —
//! judged before anything else, so a packet that fires mid-way through
//! another's promotion waits for it. A promotion a packet half-applied
//! (mirrors promoted, the primary not) is finished from the data when the
//! slots are exactly what that stop leaves, the packet named in `next`
//! closed on its `abandoned` terminal, and every gate accepts the value —
//! with `credential.verified` (`rolled_forward`) recorded before any
//! write (review fd151a97, D3; review 363facd0, F1, F3). A cancelled or
//! missing packet, and any other disagreement between the copies, is
//! refused for a person.
//!
//! THE ROSTER. Every `boss_ports` row that mounts a gate — the one table
//! every service binds from, less `machine_gate::UNGATED`, the one list
//! the gate-mount pin reads too (the gateway answers 404 on the gate's
//! routes; review 70d449f9, B1) — read at 127.0.0.1 because the
//! dispatcher runs in the pod the services run in. An answer counts only
//! from the gate that names itself as the port's service. At VERIFY a
//! port that refuses the connection has no process, and a process that
//! starts later reads the mounted Secret at boot, so it cannot hold a
//! stale reading; it is named as not served and recorded
//! (`not-served-at-verify`). At the DRAIN a port refusing the connection
//! holds the revoke unless it has stayed down since the PROMOTION: a
//! process that answered took its tally with it (review 70d449f9, B3).
//! The excuse is recorded from the read the promotion rests on — verify's,
//! or a roll-forward's (review 363facd0, F1, F2) — and every later read
//! narrows it: each verify pass, and each drain pass inside the window
//! too. A port on the list that answers any of those reads, or fails in
//! any way that shows a process, is struck (review fd151a97, D1); only a
//! refused connection, or a read never sent, leaves it (F6). THE GAP
//! THAT REMAINS is between samples, which the fifteen-minute clock rule
//! sets: a port that boots, is presented `previous` and crashes again
//! inside one interval is never seen answering, and stays excused. The
//! excuse is narrowed by observation, and nothing observes between
//! ticks. A port that answered and never returns
//! holds the revoke — and every later rotation, which waits behind this
//! one — until it answers again or a build whose `boss_ports` drops it:
//! loud, since `revoke_deferred` names it every fifteen minutes, and
//! safe, since `previous` stays accepted. The
//! dispatcher's own port is the control read: it is this process, so if
//! it does not answer the roster is aimed at the wrong host and nothing
//! counts.
//!
//! A blank slot is how a value is removed: the gate and every caller read
//! a blank slot file as absent (`machine_token::read_slot`), so the
//! Secret keeps the key and the broker needs no verb but `get, patch`.
//!
//! THE VALUE NEVER ENTERS A PACKET, AN EVENT, A LOG LINE OR AN ERROR:
//! slot names, Secret paths, port names, byte lengths and counts only.

use async_trait::async_trait;
use base64::Engine as _;
use boss_core::machine_gate::{Accepts, MAX_KEYS_PER_SOURCE, Misses, Mode, Presented};
use boss_dispatcher::rules::expr::Value;
use boss_dispatcher::rules::handler::{Handler, HandlerError, InvocationContext, arg, arg_string};
use boss_jobs::credentials::RotationPhase;
use chrono::{DateTime, Utc};
use serde_json::{Value as JsonValue, json};
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use super::common::{StepEvent, dispatcher_reader_header, sim_origin_value};
use super::credential_issuer::SecretStore;
use super::credential_rotate_cloudflare_tunnel::{
    ROTATION_KIND, StepView, get_json, is_about_credential, open_rotations, steps_of,
};

/// The handler's registered name — the `handler = "…"` of both rules.
pub const HANDLER: &str = "credential.rotate.self-issued";

/// The one value of the `phase` arg: the clock rule's pass over every
/// open rotation packet about the credential.
pub const ADVANCE_PHASE: &str = "advance";

/// The clock rule, named on every deferred step so its reader knows what
/// acts next, and when.
pub const ADVANCE_RULE: &str = "broker-advances-the-machine-token-rotation";

/// The three slots, in the order the gate matches them.
pub const SLOTS: [&str; 3] = ["current", "next", "previous"];

/// The Secret key recording when `next` was promoted to `current`: the
/// moment the gates began to count `previous`, which the drain window is
/// measured from.
pub const PROMOTED_AT: &str = "promoted-at";

/// The Secret key naming the ports that refused the connection when
/// verify completed, comma-separated: the only ports a drain may find
/// down without holding (review 70d449f9, B3). Cleared by a promotion,
/// and narrowed whenever one of them answers (review fd151a97, D1).
pub const NOT_SERVED_AT_VERIFY: &str = "not-served-at-verify";

/// The `issue` step field saying this packet's `credential.minted` is
/// on the record. The event and the Secret cannot be written as one, so
/// a firing that stops between them leaves this field unwritten, and the
/// redelivery that re-uses the staged value records the mint then
/// (review fd151a97, D4).
pub const MINT_RECORDED: &str = "mint_recorded";

/// The least drain window the rule may declare (design 6805c764, car 3:
/// "the longest CronJob period in the manifests, with a floor of one
/// hour").
pub const DRAIN_FLOOR_MINUTES: i64 = 60;

/// The service this process IS: the roster's control read.
pub const CONTROL_SERVICE: &str = "dispatcher";

/// The key naming the packet that made a slot's value.
pub fn minted_for_key(slot: &str) -> String {
    format!("{slot}.minted-for")
}

/// A fresh token: 32 bytes from the operating system's random source,
/// base64url without padding — 43 characters, sendable as a header value
/// as it stands. An OS that cannot answer is an error, never a weaker
/// generator.
pub fn fresh_value() -> Result<String, String> {
    use rand::TryRng as _;
    let mut bytes = [0u8; 32];
    rand::rngs::SysRng
        .try_fill_bytes(&mut bytes)
        .map_err(|e| format!("the operating system's random source failed: {e}"))?;
    Ok(base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes))
}

// ---------------------------------------------------------------------------
// The Secret, read
// ---------------------------------------------------------------------------

/// One slot: its value and the packet that made it. `Debug` never
/// prints the value.
#[derive(Clone, Default, PartialEq, Eq)]
pub struct Held {
    pub value: Option<String>,
    pub minted_for: Option<String>,
}

impl std::fmt::Debug for Held {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Held")
            .field("bytes", &self.value.as_deref().map(str::len))
            .field("minted_for", &self.minted_for)
            .finish()
    }
}

impl Held {
    fn is_for(&self, job_id: &str) -> bool {
        self.value.is_some() && self.minted_for.as_deref() == Some(job_id)
    }
}

/// One Secret's three slots, its promotion time, the ports excused from
/// the drain, and the version it was read at — all from ONE read, so a
/// slot and its `minted-for` cannot come from two versions (review
/// 70d449f9, N5).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SecretSlots {
    pub current: Held,
    pub next: Held,
    pub previous: Held,
    pub promoted_at: Option<String>,
    pub not_served_at_verify: Vec<String>,
    pub version: String,
}

impl SecretSlots {
    /// The slots out of one whole read; an absent or blank key is absent.
    pub fn from_data(d: &super::credential_issuer::SecretData) -> Self {
        let get = |k: &str| {
            d.data
                .get(k)
                .map(|v| v.trim().to_string())
                .filter(|v| !v.is_empty())
        };
        let held = |slot: &str| Held {
            value: get(slot),
            minted_for: get(&minted_for_key(slot)),
        };
        SecretSlots {
            current: held(SLOTS[0]),
            next: held(SLOTS[1]),
            previous: held(SLOTS[2]),
            promoted_at: get(PROMOTED_AT),
            not_served_at_verify: get(NOT_SERVED_AT_VERIFY)
                .map(|v| {
                    v.split(',')
                        .map(str::trim)
                        .filter(|s| !s.is_empty())
                        .map(str::to_string)
                        .collect()
                })
                .unwrap_or_default(),
            version: d.version.clone(),
        }
    }
}

// ---------------------------------------------------------------------------
// The gates — a port, and the adapter that reads them over HTTP
// ---------------------------------------------------------------------------

/// What one service port answered.
#[derive(Clone, Debug)]
pub enum GateRead<T> {
    Answered(T),
    /// The connection was refused: nothing listens there.
    NotServed,
    /// Anything else — a timeout, a non-2xx, a body not in shape.
    Failed(String),
    /// The request was never sent (no HTTP client, a service the roster
    /// cannot place): no word about the port either way (review
    /// 363facd0, F6).
    Unasked(String),
}

/// The machine gate on every service port, as the rotation reads it.
#[async_trait]
pub trait GateReader: Send + Sync {
    /// Every service the rotation must reach, by `boss_ports` name.
    fn roster(&self) -> Vec<String>;
    /// `GET /api/machine-gate/accepts`, presenting `presented`.
    async fn accepts(&self, service: &str, presented: &str) -> GateRead<Accepts>;
    /// `GET /api/machine-gate/misses`, presenting `presented`.
    async fn misses(&self, service: &str, presented: &str) -> GateRead<Misses>;
}

/// The gates of the pod this process runs in: every `boss_ports` row at
/// 127.0.0.1. Each read presents the value it is asked about through a
/// `machine_token::Client` holding that value alone — the client every
/// machine caller uses, so the request follows no redirect.
pub struct LocalGates {
    services: Vec<(String, String)>,
    timeout: Duration,
}

impl LocalGates {
    /// Every service `boss_ports` declares that MOUNTS a gate, at its
    /// production port. The servers that mount none are one list,
    /// `machine_gate::UNGATED`, read by the gate-mount pin as well: the
    /// gateway answers 404 on the gate's routes, and counted here it held
    /// every rotation at verify for ever (review 70d449f9, B1).
    pub fn from_ports() -> Arc<Self> {
        Self::new(
            boss_ports::all()
                .filter(|s| boss_core::machine_gate::is_gated(s.name))
                .map(|s| (s.name.to_string(), format!("http://127.0.0.1:{}", s.prod)))
                .collect(),
        )
    }

    /// Named base URLs (tests).
    pub fn new(services: Vec<(String, String)>) -> Arc<Self> {
        Arc::new(Self {
            services,
            timeout: Duration::from_secs(4),
        })
    }

    async fn read<T: serde::de::DeserializeOwned>(
        &self,
        service: &str,
        path: &str,
        presented: &str,
    ) -> GateRead<T> {
        let Some((_, base)) = self.services.iter().find(|(n, _)| n == service) else {
            return GateRead::Unasked(format!("{service} is not in the roster"));
        };
        let url = format!("{}{path}", base.trim_end_matches('/'));
        let client = match boss_core::machine_token::Client::build_with_source(
            reqwest::Client::builder()
                .connect_timeout(self.timeout)
                .timeout(self.timeout),
            Arc::new(boss_core::machine_token::Source::fixed(Some(
                presented.to_string(),
            ))),
        ) {
            Ok(c) => c,
            Err(e) => return GateRead::Unasked(format!("http client: {e}")),
        };
        let resp = match client
            .get(&url)
            .header("x-boss-user", dispatcher_reader_header())
            .header("x-sim-origin", sim_origin_value())
            .send()
            .await
        {
            Ok(r) => r,
            Err(e) if connection_refused(&e) => return GateRead::NotServed,
            Err(e) => return GateRead::Failed(format!("GET {url}: {e}")),
        };
        let status = resp.status();
        if !status.is_success() {
            let text = resp.text().await.unwrap_or_default();
            let text: String = text.chars().take(200).collect();
            return GateRead::Failed(format!("GET {url} answered {status}: {text}"));
        }
        match resp.json::<T>().await {
            Ok(v) => GateRead::Answered(v),
            Err(e) => GateRead::Failed(format!("GET {url}: body not in shape: {e}")),
        }
    }
}

/// Did this request fail because nothing listens at the port? Only a
/// refused connection counts — a timeout or a reset is a process that
/// exists and did not answer.
fn connection_refused(e: &reqwest::Error) -> bool {
    let mut source: Option<&dyn std::error::Error> = std::error::Error::source(e);
    while let Some(s) = source {
        if s.downcast_ref::<std::io::Error>()
            .is_some_and(|io| io.kind() == std::io::ErrorKind::ConnectionRefused)
        {
            return true;
        }
        source = s.source();
    }
    false
}

#[async_trait]
impl GateReader for LocalGates {
    fn roster(&self) -> Vec<String> {
        self.services.iter().map(|(n, _)| n.clone()).collect()
    }

    async fn accepts(&self, service: &str, presented: &str) -> GateRead<Accepts> {
        self.read(service, boss_core::machine_gate::ACCEPTS_PATH, presented)
            .await
    }

    async fn misses(&self, service: &str, presented: &str) -> GateRead<Misses> {
        self.read(service, boss_core::machine_gate::MISSES_PATH, presented)
            .await
    }
}

// ---------------------------------------------------------------------------
// Pure judgements — the decisions under test
// ---------------------------------------------------------------------------

/// What the ports said about one presented value.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AcceptsVerdict {
    /// Every served port answers the wanted slot.
    pub ready: bool,
    /// Ports that answered the wanted slot.
    pub agreeing: Vec<String>,
    /// Ports refusing the connection.
    pub not_served: Vec<String>,
    /// `<port>: <what it answered>` for each port that did not agree.
    pub lagging: Vec<String>,
}

/// Every served port must answer `matched: want`; the control port must
/// answer at all.
pub fn judge_accepts(reads: &[(String, GateRead<Accepts>)], want: &str) -> AcceptsVerdict {
    let mut v = AcceptsVerdict {
        ready: false,
        agreeing: Vec::new(),
        not_served: Vec::new(),
        lagging: Vec::new(),
    };
    for (service, read) in reads {
        match read {
            // A port that answers as another service is not this one's
            // gate: a proxy, or a roster aimed wrong, vouches for nothing
            // (review 70d449f9, B1).
            GateRead::Answered(a) if a.service != *service => v
                .lagging
                .push(format!("{service} answers as {}", a.service)),
            GateRead::Answered(a) if a.matched == want => v.agreeing.push(service.clone()),
            GateRead::Answered(a) => v
                .lagging
                .push(format!("{service} answers matched {}", a.matched)),
            GateRead::NotServed => v.not_served.push(service.clone()),
            GateRead::Failed(e) | GateRead::Unasked(e) => v.lagging.push(format!("{service}: {e}")),
        }
    }
    if let Some(miss) = control_missing(reads.iter().map(|(s, r)| (s, r.answered()))) {
        v.lagging.push(miss);
    }
    v.ready = v.lagging.is_empty();
    v
}

impl<T> GateRead<T> {
    fn answered(&self) -> bool {
        matches!(self, GateRead::Answered(_))
    }
}

/// The control read: this process's own port answered, so the roster is
/// aimed at the pod the services run in. `Some(why)` when it is not.
fn control_missing<'a>(answered: impl Iterator<Item = (&'a String, bool)>) -> Option<String> {
    let mut seen = false;
    for (service, ok) in answered {
        if service == CONTROL_SERVICE {
            seen = true;
            if !ok {
                return Some(format!(
                    "{CONTROL_SERVICE} (this process's own port) did not answer: the roster is \
                     aimed at a host this dispatcher does not run on, so no reading counts"
                ));
            }
        }
    }
    (!seen).then(|| {
        format!(
            "the roster has no {CONTROL_SERVICE} port, so nothing proves it is aimed at the pod \
             this dispatcher runs in"
        )
    })
}

/// Has nothing presented `previous` for the whole window? `Ok` names the
/// ports read; `Err` names every reason the answer is not yet.
///
/// `excused` are the ports that refused the connection at verify and on
/// every drain read since (recorded then, [`NOT_SERVED_AT_VERIFY`], and
/// narrowed by [`still_excused`]). Any other port refusing
/// the connection HOLDS: a process that answered at verify and is down
/// now took its tally — and whatever `previous` presentations it held —
/// with it (review 70d449f9, B3).
pub fn judge_drain(
    reads: &[(String, GateRead<Misses>)],
    now: DateTime<Utc>,
    window: chrono::Duration,
    excused: &[String],
) -> Result<String, Vec<String>> {
    let cutoff = now - window;
    let mut held = Vec::new();
    let (mut clean, mut not_served, mut noisy) = (Vec::new(), Vec::new(), Vec::new());
    for (service, read) in reads {
        let m = match read {
            GateRead::Answered(m) if m.service != *service => {
                held.push(format!(
                    "{service} answers as {}, so its tally is not this port's",
                    m.service
                ));
                continue;
            }
            GateRead::Answered(m) => m,
            GateRead::NotServed if excused.contains(service) => {
                not_served.push(service.clone());
                continue;
            }
            GateRead::NotServed => {
                held.push(format!(
                    "{service} refused the connection at the drain, and it is not recorded as \
                     down since the promotion ({NOT_SERVED_AT_VERIFY}): a process there took its \
                     tally, and any previous presentation it held, with it. The revoke holds until \
                     {service} answers again, or a build whose boss_ports drops it"
                ));
                continue;
            }
            GateRead::Failed(e) | GateRead::Unasked(e) => {
                held.push(format!("{service}: its tally could not be read ({e})"));
                continue;
            }
        };
        let before = held.len();
        if m.mode == Mode::Off {
            held.push(format!(
                "{service} is in mode off, where the gate records nothing, so its silence is not \
                 evidence"
            ));
        }
        // Overflow is judged by WHAT overflowed (backlog 93bcf490, from
        // review 1829e95f finding 2). `overflow` is what the gate could
        // charge to no one source — the whole tally full of sources under
        // their own share — so a previous may be among it, unseen: hold,
        // as before. `source_overflow` is one source past its own share,
        // counted by what it presented; the gate classifies a request
        // before it decides whether to key it, so a source whose count
        // holds no `previous` sent none, and its noise cannot hide a
        // caller on the old value. It is named on the revoke rather than
        // holding it — else one LAN caller varying `x-boss-user` holds
        // every revoke for as long as it sends. A gate keys every
        // `previous` by name (review ebc7b1cc, B1), so a `previous` here
        // is only ever read defensively: it holds while it was seen
        // inside the window, as a row does, or when its time is unknown.
        if m.overflow > 0 {
            // Neutral on where the overflow came from: a gate built before
            // per-source shares counted everything here (review ebc7b1cc,
            // N3).
            held.push(format!(
                "{service}'s tally overflowed ({} request(s) not keyed: the whole tally was full, \
                 and nothing names who sent them), so a previous match may be among them",
                m.overflow
            ));
        }
        for o in &m.source_overflow {
            if o.presented == Presented::Previous {
                if o.last_seen.is_some_and(|t| t <= cutoff) {
                    continue;
                }
                held.push(format!(
                    "{service}: {} presented previous {} time(s) past its own share of the tally, \
                     last {}, so its callers are not named: the old value was in use there inside \
                     the window",
                    o.source,
                    o.count,
                    o.last_seen
                        .map_or("at no recorded time".to_string(), |t| t.to_rfc3339())
                ));
            } else {
                noisy.push(format!(
                    "{service} {} ({} {})",
                    o.source,
                    o.count,
                    o.presented.name()
                ));
            }
        }
        if m.recording_since > cutoff {
            held.push(format!(
                "{service}'s tally began recording at {}, inside the window, so it has not watched all of it",
                m.recording_since.to_rfc3339()
            ));
        }
        for row in m
            .rows
            .iter()
            .filter(|r| r.key.presented == Presented::Previous && r.last_seen > cutoff)
        {
            held.push(format!(
                "{service}: {} as {} still presents previous on {} {} ({} time(s), last {})",
                row.key.peer,
                row.key.user,
                row.key.method,
                row.key.route,
                row.count,
                row.last_seen.to_rfc3339()
            ));
        }
        if held.len() == before {
            clean.push(service.clone());
        }
    }
    if let Some(miss) = control_missing(reads.iter().map(|(s, r)| (s, r.answered()))) {
        held.push(miss);
    }
    if held.is_empty() {
        Ok(format!(
            "no port recorded a previous match since {}: {} tall{} read clean ({}){}{}",
            cutoff.to_rfc3339(),
            clean.len(),
            if clean.len() == 1 { "y" } else { "ies" },
            clean.join(", "),
            not_served_text(&not_served),
            noisy_text(&noisy)
        ))
    } else {
        Err(held)
    }
}

/// The excused ports that are STILL down: each one this pass found no
/// process at. An answer, a timeout or a non-2xx is a process at that
/// port (see [`connection_refused`]), which holds a tally from then on
/// and may take it with it if it crashes, so it is excused no longer
/// (review fd151a97, D1). A read never sent ([`GateRead::Unasked`]) and
/// a port this pass did not read are no evidence either way, and strike
/// nothing (review 363facd0, F6). Only ever narrows.
pub fn still_excused<T>(excused: &[String], reads: &[(String, GateRead<T>)]) -> Vec<String> {
    excused
        .iter()
        .filter(|port| {
            reads
                .iter()
                .filter(|(s, _)| s == *port)
                .all(|(_, r)| matches!(r, GateRead::NotServed | GateRead::Unasked(_)))
        })
        .cloned()
        .collect()
}

/// The sources that sent past their own share of a tally without
/// presenting `previous`: they did not hold the revoke, and the revoke's
/// record says who they were (backlog 93bcf490).
fn noisy_text(noisy: &[String]) -> String {
    if noisy.is_empty() {
        String::new()
    } else {
        format!(
            "; past their own share of a tally ({MAX_KEYS_PER_SOURCE} keys), counted and not \
             keyed, none presenting previous: {}",
            noisy.join(", ")
        )
    }
}

fn not_served_text(not_served: &[String]) -> String {
    if not_served.is_empty() {
        String::new()
    } else {
        format!(
            "; not served (connection refused, no process to hold a stale reading): {}",
            not_served.join(", ")
        )
    }
}

// ---------------------------------------------------------------------------
// The declaration — rule-row data
// ---------------------------------------------------------------------------

fn optional_arg<'a>(args: &'a [(String, Value)], name: &str) -> Option<&'a str> {
    match arg(args, name) {
        Some(Value::String(s)) => Some(s.trim()).filter(|s| !s.is_empty()),
        _ => None,
    }
}

struct Declaration<'a> {
    secret_namespace: &'a str,
    secret_name: &'a str,
    /// The same Secret name in each of these namespaces: a pod mounts
    /// only its own namespace's Secrets, so every namespace with a
    /// caller holds a copy the rotation keeps in step.
    mirrors: Vec<&'a str>,
    credential_id: &'a str,
    drain: chrono::Duration,
    phase: Option<&'a str>,
}

impl<'a> Declaration<'a> {
    /// Every refusal here is PERMANENT: the rule row says the same thing
    /// on every redelivery.
    fn parse(args: &'a [(String, Value)]) -> Result<Self, HandlerError> {
        let raw = arg_string(args, "drain_minutes")?;
        let minutes = raw
            .trim()
            .parse::<i64>()
            .ok()
            .filter(|m| *m >= DRAIN_FLOOR_MINUTES)
            .ok_or_else(|| {
                HandlerError::Permanent(format!(
                    "drain_minutes = {raw:?} is not a whole number of minutes at or above the \
                     {DRAIN_FLOOR_MINUTES}-minute floor"
                ))
            })?;
        let secret_namespace = arg_string(args, "secret_namespace")?;
        let mirrors: Vec<&str> = optional_arg(args, "also_in_namespaces")
            .unwrap_or_default()
            .split(',')
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .collect();
        if mirrors.contains(&secret_namespace) {
            return Err(HandlerError::Permanent(format!(
                "also_in_namespaces names {secret_namespace}, the primary Secret's own namespace"
            )));
        }
        Ok(Self {
            secret_namespace,
            secret_name: arg_string(args, "secret_name")?,
            mirrors,
            credential_id: arg_string(args, "credential_id")?,
            drain: chrono::Duration::minutes(minutes),
            phase: optional_arg(args, "phase"),
        })
    }

    /// The primary first, then each mirror.
    fn targets(&self) -> Vec<(&'a str, &'a str)> {
        std::iter::once(self.secret_namespace)
            .chain(self.mirrors.iter().copied())
            .map(|ns| (ns, self.secret_name))
            .collect()
    }

    fn paths(&self) -> String {
        self.targets()
            .iter()
            .map(|(ns, n)| format!("{ns}/{n}"))
            .collect::<Vec<_>>()
            .join(", ")
    }
}

// ---------------------------------------------------------------------------
// The handler
// ---------------------------------------------------------------------------

/// How long one firing waits for the gates before it defers to the
/// clock rule. Bounded well inside the 30-second ack window.
#[derive(Debug, Clone, Copy)]
pub struct GatePoll {
    pub attempts: u32,
    pub interval: Duration,
}

impl Default for GatePoll {
    fn default() -> Self {
        Self {
            attempts: 2,
            interval: Duration::from_secs(5),
        }
    }
}

pub struct CredentialRotateSelfIssued {
    client: boss_core::machine_token::Client,
    jobs_base: String,
    secrets: Arc<dyn SecretStore>,
    gates: Arc<dyn GateReader>,
    poll: GatePoll,
}

/// What the copies say about `current` ([`CredentialRotateSelfIssued::copies`]).
enum Copies {
    Agree,
    /// A promotion stopped part way, and the data says how to finish it;
    /// `by` is the packet whose value it is.
    RollForward {
        by: String,
    },
    /// Refused, naming what disagrees.
    Disagree(String),
}

/// Where the value this pass stages came from.
#[derive(PartialEq, Eq)]
enum Origin {
    /// Minted by this firing, and `credential.minted` recorded.
    Minted,
    /// Minted by a concurrent firing of this packet, which records it.
    Concurrent,
    /// Staged by an earlier firing of this packet — which may have
    /// stopped before recording its mint (review fd151a97, D4).
    Reused,
}

/// Where one pass stopped.
enum Pass {
    /// The phase is done; go on to the next.
    Done,
    /// Recorded on the step as `<step>_deferred`; the clock rule resumes.
    Deferred,
}

impl CredentialRotateSelfIssued {
    pub fn new(
        jobs_base: impl Into<String>,
        secrets: Arc<dyn SecretStore>,
        gates: Arc<dyn GateReader>,
    ) -> Arc<Self> {
        Self::with_poll(jobs_base, secrets, gates, GatePoll::default())
    }

    pub fn with_poll(
        jobs_base: impl Into<String>,
        secrets: Arc<dyn SecretStore>,
        gates: Arc<dyn GateReader>,
        poll: GatePoll,
    ) -> Arc<Self> {
        Arc::new(Self {
            client: super::common::api_client(),
            jobs_base: jobs_base.into(),
            secrets,
            gates,
            poll,
        })
    }

    fn jobs(&self) -> &str {
        self.jobs_base.trim_end_matches('/')
    }

    // ----- the Secret store -----

    /// One GET of the whole Secret (review 70d449f9, N5). A Secret that
    /// does not exist is an error naming the converge that creates it:
    /// every later phase writes it, and the broker cannot `create`.
    async fn read_slots(&self, ns: &str, name: &str) -> Result<SecretSlots, HandlerError> {
        match self
            .secrets
            .read_secret(ns, name)
            .await
            .map_err(HandlerError::Downstream)?
        {
            Some(d) => Ok(SecretSlots::from_data(&d)),
            None => Err(HandlerError::Downstream(format!(
                "Secret {ns}/{name} does not exist; the cluster converge creates it empty from \
                 this rule's declaration (cluster-deploy-lib.sh broker_secrets), and the broker \
                 is deliberately not granted create"
            ))),
        }
    }

    /// Write keys of one Secret only if it is still at `version` — the
    /// version the pass read before deciding to write. A Secret that moved
    /// in between is an error naming `what`, and the next firing reads it
    /// again: a slow firing must not write back a decision a newer one
    /// has overtaken. Every write this handler makes is conditional; the
    /// unconditional one it had is gone (review 363facd0, F7).
    async fn write_at(
        &self,
        ns: &str,
        name: &str,
        entries: &[(&str, &str)],
        version: &str,
        what: &str,
    ) -> Result<(), HandlerError> {
        match self
            .secrets
            .write_keys_at(ns, name, entries, version)
            .await
            .map_err(HandlerError::Downstream)?
        {
            super::credential_issuer::WriteAt::Written => Ok(()),
            super::credential_issuer::WriteAt::Moved => Err(HandlerError::Downstream(format!(
                "Secret {ns}/{name} moved between this firing's read and its write ({what}); \
                 nothing was written, and the next firing reads it again"
            ))),
        }
    }

    // ----- the gates -----

    async fn read_accepts(&self, presented: &str) -> Vec<(String, GateRead<Accepts>)> {
        let roster = self.gates.roster();
        let reads =
            futures::future::join_all(roster.iter().map(|s| self.gates.accepts(s, presented)))
                .await;
        roster.into_iter().zip(reads).collect()
    }

    async fn read_misses(&self, presented: &str) -> Vec<(String, GateRead<Misses>)> {
        let roster = self.gates.roster();
        let reads =
            futures::future::join_all(roster.iter().map(|s| self.gates.misses(s, presented))).await;
        roster.into_iter().zip(reads).collect()
    }

    /// Read until every served port answers `want`, at most
    /// `poll.attempts` times.
    async fn poll_accepts(&self, presented: &str, want: &str) -> AcceptsVerdict {
        let mut verdict = judge_accepts(&self.read_accepts(presented).await, want);
        for _ in 1..self.poll.attempts {
            if verdict.ready {
                break;
            }
            tokio::time::sleep(self.poll.interval).await;
            verdict = judge_accepts(&self.read_accepts(presented).await, want);
        }
        verdict
    }

    // ----- the jobs API -----

    async fn fetch_steps(&self, job_id: &str) -> Result<HashMap<String, StepView>, HandlerError> {
        let url = format!("{}/api/jobs/{job_id}", self.jobs());
        Ok(steps_of(&get_json(&self.client, &url).await?))
    }

    async fn record_phase(
        &self,
        rule: &str,
        d: &Declaration<'_>,
        phase: RotationPhase,
        evidence: JsonValue,
    ) -> Result<(), HandlerError> {
        let url = format!(
            "{}/api/credentials/{}/rotation/{}",
            self.jobs(),
            d.credential_id,
            phase.as_str()
        );
        super::common::post_json(&self.client, &url, &evidence, rule).await
    }

    /// The registry row must exist before anything is minted: every
    /// phase lands through the rotation door, which answers 404 for an
    /// undeclared id, and a value minted with no record is a value
    /// nobody can account for.
    async fn require_registry_row(&self, credential_id: &str) -> Result<(), HandlerError> {
        let url = format!("{}/api/credentials/{credential_id}", self.jobs());
        let resp = self
            .client
            .get(&url)
            .header("x-boss-user", dispatcher_reader_header())
            .header("x-sim-origin", sim_origin_value())
            .send()
            .await
            .map_err(|e| HandlerError::Downstream(format!("GET {url}: {e}")))?;
        match resp.status() {
            s if s.is_success() => Ok(()),
            reqwest::StatusCode::NOT_FOUND => Err(HandlerError::Permanent(format!(
                "credential {credential_id} has no registry row (GET {url} answered 404): its \
                 rotation events would have nowhere to land, so nothing is minted"
            ))),
            s => Err(HandlerError::Downstream(format!("GET {url} returned {s}"))),
        }
    }

    /// Write one step's evidence through the step merge door, and complete
    /// it when `complete`. A completed step is left alone (the redelivery
    /// path); a slug the packet lacks is skipped; an annotation that says
    /// what the step already says is not written.
    async fn put_step(
        &self,
        rule: &str,
        job_id: &str,
        steps: &HashMap<String, StepView>,
        slug: &str,
        evidence: Vec<(String, String)>,
        complete: bool,
    ) -> Result<(), HandlerError> {
        let Some(step) = steps.get(slug) else {
            tracing::warn!(job_id, slug, "rotation packet has no such step; skipping");
            return Ok(());
        };
        if step.status == "completed" {
            return Ok(());
        }
        let mut fields: serde_json::Map<String, JsonValue> =
            evidence.into_iter().map(|(k, v)| (k, json!(v))).collect();
        if complete {
            // A deferral or a refusal written by an earlier pass stops
            // holding when the phase completes, and the step must not
            // go on saying it does (review fd151a97, D3).
            for stale in [format!("{slug}_deferred"), format!("{slug}_refused")] {
                if step.metadata.contains_key(&stale) {
                    fields.insert(stale, json!("cleared: the phase completed"));
                }
            }
            return super::common::complete_step(
                &self.client,
                self.jobs(),
                job_id,
                &step.id,
                fields,
                rule,
            )
            .await;
        }
        if fields.iter().all(|(k, v)| step.metadata.get(k) == Some(v)) {
            return Ok(());
        }
        super::common::write_json(
            &self.client,
            reqwest::Method::PATCH,
            &format!(
                "{}/api/jobs/{job_id}/steps/{}/metadata",
                self.jobs(),
                step.id
            ),
            &JsonValue::Object(fields),
            rule,
        )
        .await
    }

    /// Record why a phase stopped, on its own step, and acknowledge.
    async fn defer(
        &self,
        rule: &str,
        job_id: &str,
        steps: &HashMap<String, StepView>,
        slug: &str,
        why: String,
    ) -> Result<Pass, HandlerError> {
        let note = format!(
            "{why}. The clock rule {ADVANCE_RULE} resumes this every fifteen minutes; no hand is needed."
        );
        tracing::info!(job_id, slug, %note, "machine token rotation deferred");
        self.put_step(
            rule,
            job_id,
            steps,
            slug,
            vec![(format!("{slug}_deferred"), note)],
            false,
        )
        .await?;
        Ok(Pass::Deferred)
    }

    /// `Some(why)` when another rotation of this credential must go
    /// first: one is between install and revoke, or one opened earlier
    /// has its scope done and is waiting to start. One rotation at a
    /// time — two would each stage `next` over the other.
    async fn ahead_of(
        &self,
        d: &Declaration<'_>,
        job_id: &str,
    ) -> Result<Option<String>, HandlerError> {
        let rows = open_rotations(&self.client, self.jobs()).await?;
        let opened = |row: &JsonValue| {
            row.get("opened_at")
                .and_then(JsonValue::as_str)
                .unwrap_or_default()
                .to_string()
        };
        let mine = rows
            .iter()
            .find(|r| r.get("id").and_then(JsonValue::as_str) == Some(job_id))
            .map(opened)
            .unwrap_or_default();
        for row in &rows {
            let Some(id) = row.get("id").and_then(JsonValue::as_str) else {
                continue;
            };
            let steps = steps_of(row);
            if id == job_id || !is_about_credential(row, &steps, d.credential_id) {
                continue;
            }
            let done = |s: &str| steps.get(s).is_some_and(|v| v.status == "completed");
            if done("install") && !done("revoke") {
                return Ok(Some(format!(
                    "rotation {id} of this credential is in flight (installed, not yet revoked); \
                     one rotation runs at a time"
                )));
            }
            if done("scope") && (opened(row), id) < (mine.clone(), job_id) {
                return Ok(Some(format!(
                    "rotation {id} of this credential was opened earlier and goes first"
                )));
            }
        }
        Ok(None)
    }

    // ----- the phases -----

    /// A value `previous` still holds must drain before anything new is
    /// staged over it: blanked when the gates show nothing presents it,
    /// else `Err(reasons)`. `Ok(None)` when there is nothing to drain.
    async fn drain_previous(
        &self,
        d: &Declaration<'_>,
        primary: &SecretSlots,
    ) -> Result<Result<Option<String>, Vec<String>>, HandlerError> {
        let targets = d.targets();
        let mut holding = Vec::new();
        for (ns, name) in &targets {
            if self.read_slots(ns, name).await?.previous.value.is_some() {
                holding.push(format!("{ns}/{name}"));
            }
        }
        if holding.is_empty() {
            return Ok(Ok(None));
        }
        let Some(current) = primary.current.value.as_deref() else {
            return Ok(Err(vec![format!(
                "previous holds a value in {} while current is blank; the gates' tallies are read \
                 with current, so nothing can judge the drain",
                holding.join(", ")
            )]));
        };
        let Some(promoted) = primary
            .promoted_at
            .as_deref()
            .and_then(|s| DateTime::parse_from_rfc3339(s).ok())
            .map(|t| t.with_timezone(&Utc))
        else {
            return Ok(Err(vec![format!(
                "previous holds a value in {} but {}/{} records no readable {PROMOTED_AT}, so the \
                 drain window has no start",
                holding.join(", "),
                d.secret_namespace,
                d.secret_name
            )]));
        };
        // Read the tallies on EVERY pass, the window's too: a port excused
        // for being down at verify that answers now has a tally from here
        // on, and loses the excuse before it can crash and take that tally
        // with it (review fd151a97, D1).
        let reads = self.read_misses(current).await;
        let excused = still_excused(&primary.not_served_at_verify, &reads);
        if excused != primary.not_served_at_verify {
            let wrote = self
                .secrets
                .write_keys_at(
                    d.secret_namespace,
                    d.secret_name,
                    &[(NOT_SERVED_AT_VERIFY, excused.join(",").as_str())],
                    &primary.version,
                )
                .await
                .map_err(HandlerError::Downstream)?;
            if wrote == super::credential_issuer::WriteAt::Moved {
                return Ok(Err(vec![format!(
                    "{}/{} moved while the ports excused at verify were being narrowed to those \
                     still down; the next pass reads it again",
                    d.secret_namespace, d.secret_name
                )]));
            }
        }
        let now = boss_clock_client::wall_now();
        let ends = promoted + d.drain;
        if now < ends {
            return Ok(Err(vec![format!(
                "the drain window runs {} minutes from the promotion at {}, until {}",
                d.drain.num_minutes(),
                promoted.to_rfc3339(),
                ends.to_rfc3339()
            )]));
        }
        let clean = match judge_drain(&reads, now, d.drain, &excused) {
            Ok(clean) => clean,
            Err(held) => return Ok(Err(held)),
        };
        // The primary first: its gates stop accepting the old value, and
        // a mirror's copy is read by callers only, never by a gate. Each
        // copy is blanked only while its current is still the one this
        // drain judged, at the version just read (review 363facd0, F7).
        for (ns, name) in &targets {
            let s = self.read_slots(ns, name).await?;
            if s.current != primary.current {
                return Ok(Err(vec![format!(
                    "{ns}/{name}'s current changed while the drain was judged; the next pass \
                     judges it again"
                )]));
            }
            self.write_at(
                ns,
                name,
                &[(SLOTS[2], ""), (minted_for_key(SLOTS[2]).as_str(), "")],
                &s.version,
                "blanking previous",
            )
            .await?;
        }
        Ok(Ok(Some(format!(
            "previous blanked in {} after a {}-minute drain from the promotion at {} — {clean}. \
             A blank slot is absent to every gate (machine_token::read_slot), so the old value \
             is accepted nowhere once each gate re-reads its mount",
            holding.join(", "),
            d.drain.num_minutes(),
            promoted.to_rfc3339()
        ))))
    }

    /// Do the copies agree on `current` — by value and by the packet that
    /// made it? A promotion half-applied by a packet that was then
    /// abandoned leaves a namespace's callers sending a value only its
    /// own Secret holds; staging a new `next` over that would leave them
    /// sending a value no gate accepts once the gates enforce (review
    /// 70d449f9, N1).
    ///
    /// One disagreement is not ambiguous (review fd151a97, D3): mirrors
    /// are promoted before the primary, so a verify that stopped part way
    /// leaves exactly this, and it is [`Copies::RollForward`]:
    ///
    /// - the primary's `next` holds a value AND names the packet that
    ///   made it, and its `previous` is blank;
    /// - every promoted copy holds that `next` (value and minted-for) as
    ///   its `current`, the primary's `current` as its `previous`, and a
    ///   blank `next`;
    /// - every other copy still equals the primary's `current` and `next`,
    ///   with a blank `previous`.
    ///
    /// Each clause is pinned by its own refusal test (review 363facd0,
    /// F4, F8). Anything else is refused, never guessed at: which copy is
    /// right is then a question for a person.
    async fn copies(
        &self,
        d: &Declaration<'_>,
        primary: &SecretSlots,
    ) -> Result<Copies, HandlerError> {
        let mut differ = Vec::new();
        let mut finishable = primary.next.value.is_some()
            && primary.next.minted_for.is_some()
            && primary.previous.value.is_none();
        for ns in &d.mirrors {
            let copy = self.read_slots(ns, d.secret_name).await?;
            if copy.current == primary.current {
                finishable &= copy.next == primary.next && copy.previous.value.is_none();
                continue;
            }
            finishable &= copy.current == primary.next
                && copy.previous == primary.current
                && copy.next.value.is_none();
            differ.push(format!(
                "{ns}/{} holds current minted for {} ({} bytes)",
                d.secret_name,
                copy.current
                    .minted_for
                    .as_deref()
                    .unwrap_or("nothing recorded"),
                copy.current.value.as_deref().map_or(0, str::len)
            ));
        }
        if differ.is_empty() {
            return Ok(Copies::Agree);
        }
        if finishable {
            return Ok(Copies::RollForward {
                by: primary.next.minted_for.clone().unwrap_or_default(),
            });
        }
        Ok(Copies::Disagree(format!(
            "the copies disagree on current, so nothing is staged: {}/{} holds current minted for \
             {} ({} bytes), but {}. The slots are not exactly what a verify that stopped part way \
             leaves (the primary's next, naming its packet, as every promoted copy's current; the \
             primary's current as their previous; the primary's own previous blank), so the \
             broker cannot finish it from the data; staging over it would leave the disagreeing \
             namespace's callers sending a value no gate holds. Make the copies agree first",
            d.secret_namespace,
            d.secret_name,
            primary
                .current
                .minted_for
                .as_deref()
                .unwrap_or("nothing recorded"),
            primary.current.value.as_deref().map_or(0, str::len),
            differ.join("; ")
        )))
    }

    /// How the packet whose value a roll-forward would promote was
    /// closed: `Ok(how)` only when it closed on its `abandoned` terminal,
    /// the workflow's exit for a rotation that is stuck, whose procedure
    /// is to open a new rotation — which is what is asking now. A packet
    /// CANCELLED was stopped on purpose, perhaps because its value must
    /// not go live; one still open, closed some other way, or absent from
    /// the record says nothing the data can settle. Each of those is
    /// `Err(why)`, refused for a person (review 363facd0, F3). A roll
    /// BACK is as determined by the slots as a roll forward; which one a
    /// cancelled packet wanted is the question the person answers.
    async fn how_closed(
        &self,
        by: &str,
        credential_id: &str,
    ) -> Result<Result<String, String>, HandlerError> {
        let url = format!("{}/api/jobs/{by}", self.jobs());
        let resp = self
            .client
            .get(&url)
            .header("x-boss-user", dispatcher_reader_header())
            .header("x-sim-origin", sim_origin_value())
            .send()
            .await
            .map_err(|e| HandlerError::Downstream(format!("GET {url}: {e}")))?;
        if resp.status() == reqwest::StatusCode::NOT_FOUND {
            return Ok(Err(format!(
                "the value half-promoted names packet {by}, which the record does not hold, so \
                 nothing says whether it should go live"
            )));
        }
        if !resp.status().is_success() {
            return Err(HandlerError::Downstream(format!(
                "GET {url} returned {}",
                resp.status()
            )));
        }
        let job: JsonValue = resp
            .json()
            .await
            .map_err(|e| HandlerError::Downstream(format!("GET {url}: {e}")))?;
        // The id is whatever the Secret's `next.minted-for` says; the packet
        // it names vouches for the value only if it IS a rotation of this
        // credential (review 1718e070, G4).
        let kind = job.get("kind").and_then(JsonValue::as_str).unwrap_or("");
        if kind != ROTATION_KIND || !is_about_credential(&job, &steps_of(&job), credential_id) {
            return Ok(Err(format!(
                "packet {by}, named as the maker of the half-promoted value, is not a rotation of \
                 {credential_id} (kind {kind:?}), so its closing says nothing about that value"
            )));
        }
        let status = job.get("status").and_then(JsonValue::as_str).unwrap_or("");
        let abandoned = job
            .pointer("/metadata/abandoned")
            .is_some_and(|v| v.as_str() == Some("true") || v.as_bool() == Some(true));
        Ok(match (status, abandoned) {
            ("closed", true) => Ok(format!("{by} closed on its abandoned terminal")),
            ("cancelled", _) => Err(format!(
                "packet {by}, whose value is half-promoted, was cancelled: a person stopped it, \
                 perhaps so that value never goes live, so it is neither rolled forward nor back \
                 without a person"
            )),
            (s, _) => Err(format!(
                "packet {by}, whose value is half-promoted, is {s:?} and did not close on its \
                 abandoned terminal, so the data does not settle whether its value should go live"
            )),
        })
    }

    /// Finish a half-applied promotion ([`Copies::RollForward`]) as verify
    /// promotes: read the gates first, and go on only when every served
    /// port accepts the value as `next` (else `Ok(Err(why))`, deferred);
    /// record `credential.verified` with `rolled_forward` BEFORE any
    /// Secret is written, so a firing that stops part way leaves the
    /// value's going-live on the record (review 363facd0, F3); then every
    /// copy still holding the primary's `next` beside the old `current`
    /// is promoted, mirrors first and the primary last, each written only
    /// at the version it was read at so two firings cannot promote twice.
    /// `promoted-at` is NOW: the primary's callers switch only from this
    /// write, so the old value's drain window starts here. The ports that
    /// refused the connection at that read are written as
    /// `not-served-at-verify`, exactly as verify records them, so the
    /// leftover drain neither holds on a port that never ran nor claims
    /// it answered (review 363facd0, F1).
    async fn roll_forward(
        &self,
        rule: &str,
        d: &Declaration<'_>,
        job_id: &str,
        primary: &SecretSlots,
        by: &str,
        closed_as: &str,
    ) -> Result<Result<String, String>, HandlerError> {
        let value = primary.next.value.clone().unwrap_or_default();
        let staged = judge_accepts(&self.read_accepts(&value).await, SLOTS[1]);
        if !staged.ready {
            return Ok(Err(format!(
                "a half-applied promotion of {by}'s value waits to be finished until every gate \
                 accepts it as next: {}",
                staged.lagging.join("; ")
            )));
        }
        self.record_phase(
            rule,
            d,
            RotationPhase::Verified,
            json!({
                "job_id": job_id,
                "method": "api",
                "rolled_forward": true,
                "for_packet": by,
                "closed_as": closed_as,
                "agreeing": staged.agreeing,
                "not_served": staged.not_served,
                "value_length": value.len(),
            }),
        )
        .await?;
        let now = boss_clock_client::wall_now().to_rfc3339();
        let excused = staged.not_served.join(",");
        let (cur_for, next_for, prev_for) = (
            minted_for_key(SLOTS[0]),
            minted_for_key(SLOTS[1]),
            minted_for_key(SLOTS[2]),
        );
        let mut order = d.targets();
        order.rotate_left(1);
        let mut promoted = Vec::new();
        for (ns, name) in order {
            let s = self.read_slots(ns, name).await?;
            if s.current == primary.next {
                continue;
            }
            if s.next != primary.next || s.previous.value.is_some() {
                return Err(HandlerError::Downstream(format!(
                    "Secret {ns}/{name} changed while a half-applied promotion was being finished; \
                     the next firing reads it again"
                )));
            }
            let wrote = self
                .secrets
                .write_keys_at(
                    ns,
                    name,
                    &[
                        (SLOTS[2], s.current.value.as_deref().unwrap_or("")),
                        (
                            prev_for.as_str(),
                            s.current.minted_for.as_deref().unwrap_or(""),
                        ),
                        (SLOTS[0], s.next.value.as_deref().unwrap_or("")),
                        (cur_for.as_str(), by),
                        (SLOTS[1], ""),
                        (next_for.as_str(), ""),
                        (PROMOTED_AT, now.as_str()),
                        (NOT_SERVED_AT_VERIFY, excused.as_str()),
                    ],
                    &s.version,
                )
                .await
                .map_err(HandlerError::Downstream)?;
            if wrote == super::credential_issuer::WriteAt::Moved {
                return Err(HandlerError::Downstream(format!(
                    "Secret {ns}/{name} moved while a half-applied promotion was being finished; \
                     the next firing reads it again"
                )));
            }
            promoted.push(format!("{ns}/{name}"));
        }
        let text = format!(
            "rolled forward: packet {by} promoted its value in some copies and stopped before \
             the primary ({closed_as}), so no verify of its own will finish it. The slots were \
             exactly what that stop leaves and every served port accepted the value as next ({} \
             port(s)), so the promotion was finished from the data in {} at {now}; the old value \
             is now previous everywhere and drains from this moment{}",
            staged.agreeing.len(),
            promoted.join(", "),
            not_served_text(&staged.not_served)
        );
        tracing::warn!(by = %by, secrets = %promoted.join(", "), "machine token: a half-applied promotion rolled forward");
        Ok(Ok(text))
    }

    /// Generate a value and stage it in the primary's `next` ONLY if the
    /// primary is still at the version `primary` was read at — the API
    /// server's resourceVersion precondition. A concurrent firing of this
    /// packet that got there first wins: its value is taken, and this
    /// firing's is dropped unwritten and unrecorded, so one packet stages
    /// one value and the record carries one mint (review 70d449f9, N2).
    /// `credential.minted` lands only after the write took.
    async fn mint(
        &self,
        rule: &str,
        d: &Declaration<'_>,
        job_id: &str,
        steps: &HashMap<String, StepView>,
        primary: &SecretSlots,
    ) -> Result<(String, String, Origin), HandlerError> {
        let value = fresh_value().map_err(HandlerError::Downstream)?;
        let next_for = minted_for_key(SLOTS[1]);
        let wrote = self
            .secrets
            .write_keys_at(
                d.secret_namespace,
                d.secret_name,
                &[(SLOTS[1], value.as_str()), (next_for.as_str(), job_id)],
                &primary.version,
            )
            .await
            .map_err(HandlerError::Downstream)?;
        if wrote == super::credential_issuer::WriteAt::Moved {
            let now = self.read_slots(d.secret_namespace, d.secret_name).await?;
            if now.next.is_for(job_id) {
                return Ok((
                    now.next.value.clone().unwrap_or_default(),
                    "staged by a concurrent firing of this packet, whose write landed first; this \
                     firing's value was dropped unwritten and unrecorded"
                        .to_string(),
                    Origin::Concurrent,
                ));
            }
            return Err(HandlerError::Downstream(format!(
                "Secret {}/{} moved between this firing's read and its write, and next is not \
                 this packet's; nothing is staged, and the next firing reads it again",
                d.secret_namespace, d.secret_name
            )));
        }
        let replaced = match (&primary.next.value, &primary.next.minted_for) {
            (Some(_), who) => format!(
                "; it replaced a value another packet staged and never promoted ({}), which no \
                 caller ever sent",
                who.as_deref().unwrap_or("unrecorded")
            ),
            (None, _) => String::new(),
        };
        self.record_minted(rule, d, job_id, steps, value.len(), None)
            .await?;
        let issued = format!(
            "32 bytes from the operating system's random source, base64url: a {}-character value \
             generated in the dispatcher and written only to the Secrets{replaced}",
            value.len()
        );
        Ok((value, issued, Origin::Minted))
    }

    /// Record `credential.minted` for this packet's value, then say so on
    /// the issue step ([`MINT_RECORDED`]). `late` names why a redelivery
    /// records a mint its first firing made (review fd151a97, D4). A stop
    /// between the event and the step field records the mint twice on
    /// the redelivery, each naming this packet — the lesser error, since
    /// a value no event accounts for cannot be told from one placed by
    /// hand.
    async fn record_minted(
        &self,
        rule: &str,
        d: &Declaration<'_>,
        job_id: &str,
        steps: &HashMap<String, StepView>,
        value_length: usize,
        late: Option<&str>,
    ) -> Result<(), HandlerError> {
        let mut evidence = json!({
            "job_id": job_id,
            "bytes": 32,
            "encoding": "base64url",
            "value_length": value_length,
        });
        if let Some(why) = late {
            evidence["recorded_late"] = json!(true);
            evidence["why"] = json!(why);
        }
        self.record_phase(rule, d, RotationPhase::Minted, evidence)
            .await?;
        self.put_step(
            rule,
            job_id,
            steps,
            "issue",
            vec![(
                MINT_RECORDED.into(),
                format!(
                    "credential.minted recorded for this packet's {value_length}-character value{}",
                    if late.is_some() {
                        ", late, by the firing that re-used it"
                    } else {
                        ""
                    }
                ),
            )],
            false,
        )
        .await
    }

    /// ISSUE + INSTALL: generate the value (or find this packet's, staged
    /// by an earlier firing) and stage it in `next` of every Secret.
    async fn stage(
        &self,
        rule: &str,
        d: &Declaration<'_>,
        job_id: &str,
        steps: &HashMap<String, StepView>,
    ) -> Result<Pass, HandlerError> {
        let mut primary = self.read_slots(d.secret_namespace, d.secret_name).await?;
        // Another rotation in flight goes first — BEFORE the copies are
        // judged: an older packet between its mirrors' promotion and its
        // primary's is a disagreement that packet's own verify finishes,
        // and a younger packet waits for it rather than refusing
        // (review fd151a97, D3).
        if !primary.next.is_for(job_id)
            && let Some(why) = self.ahead_of(d, job_id).await?
        {
            return self.defer(rule, job_id, steps, "issue", why).await;
        }
        let refusal = match self.copies(d, &primary).await? {
            Copies::Agree => None,
            Copies::Disagree(why) => Some(why),
            Copies::RollForward { by } => match self.how_closed(&by, d.credential_id).await? {
                Err(why) => Some(why),
                Ok(closed_as) => {
                    match self
                        .roll_forward(rule, d, job_id, &primary, &by, &closed_as)
                        .await?
                    {
                        Err(why) => return self.defer(rule, job_id, steps, "issue", why).await,
                        Ok(rolled) => {
                            self.put_step(
                                rule,
                                job_id,
                                steps,
                                "issue",
                                vec![("issue_rolled_forward".into(), rolled)],
                                false,
                            )
                            .await?;
                            primary = self.read_slots(d.secret_namespace, d.secret_name).await?;
                            None
                        }
                    }
                }
            },
        };
        if let Some(why) = refusal {
            self.put_step(
                rule,
                job_id,
                steps,
                "issue",
                vec![("issue_refused".into(), why.clone())],
                false,
            )
            .await?;
            return Err(HandlerError::Permanent(why));
        }
        let reused = |held: &Held| {
            (
                held.value.clone().unwrap_or_default(),
                "staged by an earlier firing of this packet (next.minted-for names it); re-used, \
                 not minted twice"
                    .to_string(),
                Origin::Reused,
            )
        };
        let (value, issued, origin) = if primary.next.is_for(job_id) {
            reused(&primary.next)
        } else {
            match self.drain_previous(d, &primary).await? {
                Err(held) => {
                    return self
                        .defer(
                            rule,
                            job_id,
                            steps,
                            "issue",
                            format!(
                                "previous still holds the value an earlier rotation replaced, and \
                                 a new value is not staged over it until it drains: {}",
                                held.join("; ")
                            ),
                        )
                        .await;
                }
                Ok(Some(revoked)) => {
                    self.record_phase(
                        rule,
                        d,
                        RotationPhase::Revoked,
                        json!({"job_id": job_id, "left_by_an_earlier_rotation": true, "revoked": revoked}),
                    )
                    .await?;
                }
                Ok(None) => {}
            }
            self.require_registry_row(d.credential_id).await?;
            // Read again: the drain may have written, and the mint below
            // is conditioned on the version this read returns.
            let primary = self.read_slots(d.secret_namespace, d.secret_name).await?;
            if primary.next.is_for(job_id) {
                reused(&primary.next)
            } else {
                self.mint(rule, d, job_id, steps, &primary).await?
            }
        };
        // A value an earlier firing staged, with no word on the issue step
        // that its mint was recorded: that firing stopped between the
        // Secret's write and the event, so the event is recorded now
        // (review fd151a97, D4).
        // The same for a concurrent firing whose write beat this one's: if
        // it stopped before its event, this firing completes issue and
        // install, and no later firing re-enters stage to notice — so it
        // reads the issue step afresh (the winner's mark may have landed
        // since this pass began) and records the mint the winner lost. A
        // winner still between its write and its event is recorded twice,
        // both naming this packet: the lesser error (review 363facd0, F5).
        let recorded = match origin {
            Origin::Minted => true,
            Origin::Reused => steps
                .get("issue")
                .is_some_and(|s| s.metadata.contains_key(MINT_RECORDED)),
            Origin::Concurrent => self
                .fetch_steps(job_id)
                .await?
                .get("issue")
                .is_some_and(|s| s.metadata.contains_key(MINT_RECORDED)),
        };
        if !recorded {
            self.record_minted(
                rule,
                d,
                job_id,
                steps,
                value.len(),
                Some(if origin == Origin::Reused {
                    "the firing that staged this value stopped before its credential.minted was \
                     recorded; the value was re-used, not minted again"
                } else {
                    "a concurrent firing of this packet staged this value and left no word that \
                     its credential.minted was recorded; this firing's own value was dropped"
                }),
            )
            .await?;
        }
        let next_for = minted_for_key(SLOTS[1]);
        let mut staged = Vec::new();
        for (ns, name) in d.targets() {
            let before = self.read_slots(ns, name).await?;
            self.write_at(
                ns,
                name,
                &[(SLOTS[1], value.as_str()), (next_for.as_str(), job_id)],
                &before.version,
                "staging next",
            )
            .await?;
            let back = self.read_slots(ns, name).await?;
            if back.next.value.as_deref() != Some(value.as_str()) || !back.next.is_for(job_id) {
                return Err(HandlerError::Downstream(format!(
                    "Secret {ns}/{name}: next did not read back as the value just written; nothing \
                     is promoted"
                )));
            }
            staged.push(format!("{ns}/{name} ({} bytes)", value.len()));
        }
        self.record_phase(
            rule,
            d,
            RotationPhase::Installed,
            json!({
                "job_id": job_id,
                "slot": SLOTS[1],
                "secrets": d.paths(),
                "value_length": value.len(),
            }),
        )
        .await?;
        self.put_step(
            rule,
            job_id,
            steps,
            "issue",
            vec![
                ("issued".into(), issued),
                (
                    "issuer".into(),
                    format!(
                        "self-issued by the credential broker ({rule}): no external issuer and no \
                         root credential"
                    ),
                ),
            ],
            true,
        )
        .await?;
        self.put_step(
            rule,
            job_id,
            steps,
            "install",
            vec![(
                "installed".into(),
                format!(
                    "staged in next, read back equal, beside next.minted-for = this packet: {}. \
                     Every gate accepts next; no caller sends it until the promotion",
                    staged.join(", ")
                ),
            )],
            true,
        )
        .await?;
        Ok(Pass::Done)
    }

    /// VERIFY: every gate accepts the staged value as `next`, then
    /// promote, then every gate accepts it as `current`.
    async fn verify(
        &self,
        rule: &str,
        d: &Declaration<'_>,
        job_id: &str,
        steps: &HashMap<String, StepView>,
    ) -> Result<Pass, HandlerError> {
        let primary = self.read_slots(d.secret_namespace, d.secret_name).await?;
        let (value, promoted) = if primary.current.is_for(job_id) {
            (primary.current.value.clone().unwrap_or_default(), true)
        } else if primary.next.is_for(job_id) {
            (primary.next.value.clone().unwrap_or_default(), false)
        } else {
            return Err(HandlerError::Permanent(format!(
                "this packet's value is in neither next nor current of {}/{} (next.minted-for \
                 names {}, current.minted-for names {}); nothing is promoted",
                d.secret_namespace,
                d.secret_name,
                primary.next.minted_for.as_deref().unwrap_or("nothing"),
                primary.current.minted_for.as_deref().unwrap_or("nothing"),
            )));
        };
        if !promoted {
            let staged = self.poll_accepts(&value, SLOTS[1]).await;
            if !staged.ready {
                return self
                    .defer(
                        rule,
                        job_id,
                        steps,
                        "verify",
                        format!(
                            "not every gate accepts the staged value as next yet (kubelet refreshes \
                             a mounted Secret about a minute after a write): {}",
                            staged.lagging.join("; ")
                        ),
                    )
                    .await;
            }
            // Mirrors first, the primary last: the primary is the ledger a
            // later firing reads to know whether promotion happened. The
            // excuse is recorded HERE, from the read this promotion rests
            // on: the ports down at the moment callers begin to switch
            // (review 363facd0, F2). Every later read narrows it.
            let now = boss_clock_client::wall_now().to_rfc3339();
            let excused = staged.not_served.join(",");
            let mut order = d.targets();
            order.rotate_left(1);
            for (ns, name) in order {
                let s = self.read_slots(ns, name).await?;
                if s.current.is_for(job_id) {
                    continue;
                }
                if s.previous.value.is_some() {
                    return Err(HandlerError::Permanent(format!(
                        "Secret {ns}/{name}: previous holds a value, and promoting would drop it \
                         without its drain; nothing more is promoted"
                    )));
                }
                let (cur_for, prev_for) = (minted_for_key(SLOTS[0]), minted_for_key(SLOTS[2]));
                let next_for = minted_for_key(SLOTS[1]);
                self.write_at(
                    ns,
                    name,
                    &[
                        (SLOTS[2], s.current.value.as_deref().unwrap_or("")),
                        (
                            prev_for.as_str(),
                            s.current.minted_for.as_deref().unwrap_or(""),
                        ),
                        (SLOTS[0], value.as_str()),
                        (cur_for.as_str(), job_id),
                        (SLOTS[1], ""),
                        (next_for.as_str(), ""),
                        (PROMOTED_AT, now.as_str()),
                        (NOT_SERVED_AT_VERIFY, excused.as_str()),
                    ],
                    &s.version,
                    "promoting next to current",
                )
                .await?;
            }
        }
        // Every pass after the promotion narrows the excuse: a port that
        // answers any of these reads holds a tally from then on (review
        // 363facd0, F2) — the same rule the drain's reads apply (D1).
        let reads = self.read_accepts(&value).await;
        let now_primary = self.read_slots(d.secret_namespace, d.secret_name).await?;
        let excused = still_excused(&now_primary.not_served_at_verify, &reads);
        if excused != now_primary.not_served_at_verify {
            self.write_at(
                d.secret_namespace,
                d.secret_name,
                &[(NOT_SERVED_AT_VERIFY, excused.join(",").as_str())],
                &now_primary.version,
                "narrowing the ports excused at the promotion",
            )
            .await?;
        }
        let live = judge_accepts(&reads, SLOTS[0]);
        if !live.ready {
            return self
                .defer(
                    rule,
                    job_id,
                    steps,
                    "verify",
                    format!(
                        "promoted in {} (next is now current, and the old current is previous); \
                         waiting for every gate to read the new value as current: {}",
                        d.paths(),
                        live.lagging.join("; ")
                    ),
                )
                .await;
        }
        let verified = format!(
            "every served port answers GET /api/machine-gate/accepts with matched current for the \
             new value: {} port(s) ({}){}",
            live.agreeing.len(),
            live.agreeing.join(", "),
            not_served_text(&live.not_served)
        );
        // The excuse is already in the primary, beside promoted-at, where
        // every drain — this packet's revoke, or a later packet's leftover —
        // reads it: recorded at the promotion and narrowed on every pass
        // since. It is no longer rewritten from this completing read,
        // which forgot any answer given in between (review 363facd0, F2).
        self.record_phase(
            rule,
            d,
            RotationPhase::Verified,
            json!({
                "job_id": job_id,
                "method": "api",
                "agreeing": live.agreeing,
                "not_served": live.not_served,
                "excused": excused,
            }),
        )
        .await?;
        self.put_step(
            rule,
            job_id,
            steps,
            "verify",
            vec![
                ("verified".into(), verified),
                ("method".into(), "api".into()),
            ],
            true,
        )
        .await?;
        Ok(Pass::Done)
    }

    /// REVOKE: blank `previous` once nothing has presented it for the
    /// drain window.
    async fn revoke(
        &self,
        rule: &str,
        d: &Declaration<'_>,
        job_id: &str,
        steps: &HashMap<String, StepView>,
    ) -> Result<Pass, HandlerError> {
        let primary = self.read_slots(d.secret_namespace, d.secret_name).await?;
        if !primary.current.is_for(job_id) {
            return Err(HandlerError::Permanent(format!(
                "current of {}/{} is not this packet's value (current.minted-for names {}); this \
                 packet revokes nothing",
                d.secret_namespace,
                d.secret_name,
                primary.current.minted_for.as_deref().unwrap_or("nothing")
            )));
        }
        let revoked = match self.drain_previous(d, &primary).await? {
            Err(held) => {
                return self
                    .defer(
                        rule,
                        job_id,
                        steps,
                        "revoke",
                        format!(
                            "previous is not blanked yet, because the gates have not shown that \
                             nothing presents it: {}",
                            held.join("; ")
                        ),
                    )
                    .await;
            }
            Ok(Some(revoked)) => revoked,
            Ok(None) => format!(
                "nothing to revoke: previous is blank in {} (a first mint promotes into an empty \
                 current, so no old value exists)",
                d.paths()
            ),
        };
        self.record_phase(
            rule,
            d,
            RotationPhase::Revoked,
            json!({"job_id": job_id, "revoked": revoked}),
        )
        .await?;
        self.put_step(
            rule,
            job_id,
            steps,
            "revoke",
            vec![("revoked".into(), revoked)],
            true,
        )
        .await?;
        Ok(Pass::Done)
    }

    /// Everything the state allows for one packet, in protocol order.
    async fn advance(
        &self,
        rule: &str,
        d: &Declaration<'_>,
        job_id: &str,
    ) -> Result<(), HandlerError> {
        type Phase = &'static str;
        const PHASES: [(Phase, Phase); 3] = [
            ("install", "stage"),
            ("verify", "verify"),
            ("revoke", "revoke"),
        ];
        for (step, phase) in PHASES {
            let steps = self.fetch_steps(job_id).await?;
            let done = |s: &str| steps.get(s).is_some_and(|v| v.status == "completed");
            if !done("scope") {
                return Ok(());
            }
            if done(step) {
                continue;
            }
            let pass = match phase {
                "stage" => self.stage(rule, d, job_id, &steps).await?,
                "verify" => self.verify(rule, d, job_id, &steps).await?,
                _ => self.revoke(rule, d, job_id, &steps).await?,
            };
            if let Pass::Deferred = pass {
                return Ok(());
            }
        }
        Ok(())
    }

    /// The clock door: every open packet about this credential whose
    /// scope is done, oldest first.
    async fn sweep(&self, rule: &str, d: &Declaration<'_>) -> Result<(), HandlerError> {
        let mut rows: Vec<(String, String)> = open_rotations(&self.client, self.jobs())
            .await?
            .iter()
            .filter(|row| {
                let steps = steps_of(row);
                is_about_credential(row, &steps, d.credential_id)
                    && steps.get("scope").is_some_and(|s| s.status == "completed")
            })
            .filter_map(|row| {
                Some((
                    row.get("opened_at")
                        .and_then(JsonValue::as_str)
                        .unwrap_or_default()
                        .to_string(),
                    row.get("id")?.as_str()?.to_string(),
                ))
            })
            .collect();
        rows.sort();
        let mut failures = Vec::new();
        for (_, job_id) in &rows {
            if let Err(e) = self.advance(rule, d, job_id).await {
                failures.push(format!("{job_id}: {e}"));
            }
        }
        if failures.is_empty() {
            Ok(())
        } else {
            Err(HandlerError::Downstream(format!(
                "{} of {} rotation packet(s) failed: {}",
                failures.len(),
                rows.len(),
                failures.join(" | ")
            )))
        }
    }
}

#[async_trait]
impl Handler for CredentialRotateSelfIssued {
    fn name(&self) -> &'static str {
        HANDLER
    }

    async fn invoke(
        &self,
        args: &[(String, Value)],
        ctx: &InvocationContext,
    ) -> Result<(), HandlerError> {
        let d = Declaration::parse(args)?;
        match d.phase {
            Some(ADVANCE_PHASE) => self.sweep(&ctx.rule_name, &d).await,
            Some(other) => Err(HandlerError::Permanent(format!(
                "phase = {other:?} is not one this handler runs: the clock rule passes \
                 \"{ADVANCE_PHASE}\", a rotation packet's scope step passes none"
            ))),
            None if ctx.event_payload.get("job_id").is_none() => {
                Err(HandlerError::Permanent(format!(
                    "{} fired without a packet and without phase = \"{ADVANCE_PHASE}\"; a clock \
                     firing must say it advances",
                    ctx.rule_name
                )))
            }
            None => {
                let ev = StepEvent::from_payload(&ctx.event_payload)?;
                self.advance(&ctx.rule_name, &d, ev.job_id).await
            }
        }
    }
}

#[cfg(test)]
mod tests;
