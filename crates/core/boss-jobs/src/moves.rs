//! THE MOVES RECORD — every packet whose place on the IT map changed,
//! stamped with the audit event that changed it (design e765b3fc, "The
//! Department Map", §3; car M1 on feedback 84cba7e2).
//!
//! WHY THIS EXISTS. The map's motion replays each rail's 24-hour RATE
//! as tokens (`apps/web/src/it/yard/world-motion.ts`), and its own text
//! says "they are not individual events". David, on 84cba7e2: "I think
//! we need to visualize the actual jobs/cars moving as the animation
//! rather than a representative." Nothing recorded an individual move:
//! the regions read answers where packets STAND, and the audit log holds
//! what happened to each packet, but no record said "this packet left
//! the gates for the dock at 03:18:21, because of event X". This module
//! is that record — the data half of the motion; the drawing is car M2.
//!
//! THE RULE: NOTHING MOVES THAT DID NOT MOVE IN THE RECORD. A move is a
//! packet whose place under the ONE placement definition
//! ([`crate::regions::members`], the partition every region's count is
//! pinned to) differs between two readings, AND an audit event in
//! between names that packet or its lineage. The move cites that event
//! — its id, its sequence number, its kind, and its `created_at` as the
//! move's instant — so provenance is checkable row by row. A packet
//! whose place changed with no event naming it (a closed train ageing
//! out of the arrivals window) did not move in the record, and no row is
//! written: the baseline simply takes the new reading.
//!
//! NO SECOND PLACEMENT FUNCTION. The mover places nothing itself. Every
//! region a move names is a region [`crate::regions::members`] put the
//! packet in, so the rates, the piles and the motion cannot disagree
//! (CLAUDE.md §9a). What the mover adds is LINEAGE, the one thing the
//! partition does not read: which packet continues which. See
//! [`moves`].
//!
//! A DERIVED PROJECTION, NOT THE LOG. `yard_moves` is a reading of the
//! audit log, never a second copy of the work: every row cites its cause,
//! and the table is idempotent on `(cause_event_id, packet)`, so a
//! replayed batch writes nothing twice.

use std::collections::{BTreeMap, BTreeSet};

use async_trait::async_trait;
use boss_core::job::Job;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use uuid::Uuid;

use crate::regions::RegionInputs;

// v2 hashes dispatch.rs verbatim, including its original tests. Keep that
// versioned input immutable; the live production prefix is equality-pinned.
#[path = "moves/dispatch_live.rs"]
mod dispatch;
pub use dispatch::{DispatchContext, dispatch_context};

pub type Instant = chrono::DateTime<chrono::Utc>;

/// The metadata keys by which one packet names ANOTHER packet it
/// continues or carries, in the order a tie between two predecessors is
/// broken (after a shared branch, which outranks them all).
///
/// * `agent_run` — a gate-run or a car names the run that built it.
/// * `packet` — a run names the item it was dispatched on.
/// * `train_gate_run` — a train names its train gate.
/// * `boarded_jobs` — a train names the cars aboard it ([`CARRIES`]).
pub const LINK_KEYS: [&str; 4] = ["agent_run", "packet", "train_gate_run", CARRIES];

/// The one link that is a CARRIAGE rather than a hand-off: a train
/// carries the cars it names, and a car that leaves the dock aboard one
/// rides it — it does not become it.
pub const CARRIES: &str = "boarded_jobs";

/// The lineage spelled for two packets that share a branch: a gate-run
/// and the car its green filed.
pub const SAME_BRANCH: &str = "branch";

/// How many packet-naming events one tick may judge. Past it the
/// mover does not diff at all — a batch that large is a gap (a restart
/// of the log, a flood), and a move attributed across it would be a
/// guess — so it takes the reading as a new baseline and says so with a
/// `resync` ([`Tick::truncated`]).
pub const MAX_BATCH: i64 = 5_000;

/// What one packet IS, as the move rows name it.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Facts {
    pub kind: String,
    /// The branch where the packet has one, otherwise its title.
    pub label: String,
    pub branch: Option<String>,
    /// Every packet id this packet's metadata names, with the key that
    /// names it ([`LINK_KEYS`]).
    pub names: Vec<(&'static str, String)>,
    /// The packet has closed (or was cancelled). A closed packet that
    /// leaves the map leaves by its terminal: no train carries it, even
    /// one that still names it in `boarded_jobs` (backlog 47880c86).
    pub closed: bool,
}

impl Facts {
    /// A packet's facts off its row.
    pub fn of(job: &Job) -> Self {
        let branch = job
            .metadata
            .get("branch")
            .and_then(Value::as_str)
            .filter(|b| !b.is_empty())
            .map(str::to_string);
        let names = LINK_KEYS
            .iter()
            .flat_map(|key| {
                let ids: Vec<String> = match job.metadata.get(*key) {
                    Some(Value::String(id)) if !id.is_empty() => vec![id.clone()],
                    Some(Value::Array(ids)) => ids
                        .iter()
                        .filter_map(Value::as_str)
                        .filter(|id| !id.is_empty())
                        .map(str::to_string)
                        .collect(),
                    _ => Vec::new(),
                };
                ids.into_iter().map(move |id| (*key, id))
            })
            .collect();
        Facts {
            kind: job.kind.clone(),
            label: branch.clone().unwrap_or_else(|| job.title.clone()),
            branch,
            names,
            closed: matches!(
                job.status,
                boss_core::job::JobStatus::Closed | boss_core::job::JobStatus::Cancelled
            ),
        }
    }

    fn carried(&self) -> impl Iterator<Item = &str> {
        self.names
            .iter()
            .filter(|(k, _)| *k == CARRIES)
            .map(|(_, id)| id.as_str())
    }
}

/// ONE READING of the map: where the partition placed each packet, what
/// each packet is, and which regions could not be read.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Snapshot {
    pub places: BTreeMap<String, &'static str>,
    pub facts: BTreeMap<String, Facts>,
    /// Exact carriage from the partition, packet -> carrier. An observed
    /// carrier places its cargo even when an unrelated region is unread.
    pub aboard: BTreeMap<String, String>,
    pub dispatch: BTreeMap<String, DispatchContext>,
    /// Regions whose reading failed. While any is unread, a packet that
    /// is in no read region might be in one of these — so it is
    /// [`Where::Unknown`], never "off the map".
    pub unread: Vec<&'static str>,
}

/// Where one reading puts one packet.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Where {
    At(&'static str),
    /// On no region of its own: all packet regions answered, or the
    /// partition placed it aboard an independently observed carrier.
    Off,
    /// On no read region while a packet region or its carrier is unread.
    Unknown,
}

impl Where {
    fn region(self) -> Option<&'static str> {
        match self {
            Where::At(r) => Some(r),
            _ => None,
        }
    }
}

impl Snapshot {
    /// The reading the regions read would give: the partition's own
    /// members ([`crate::regions::members`]), and the facts of every row
    /// the reading holds.
    pub fn of(inputs: &RegionInputs<'_>) -> Self {
        Self::of_with_registry(inputs, &[])
    }

    /// Preserve registration from the same original rows that place the
    /// move. A later parent claim is never substituted for this reading.
    pub fn of_with_registry(
        inputs: &RegionInputs<'_>,
        specs: &[crate::registry::WorkflowSpec],
    ) -> Self {
        let mut places = BTreeMap::new();
        let mut unread = Vec::new();
        let lookups = crate::regions::Lookups::of(inputs);
        for (region, ids) in crate::regions::members(inputs) {
            match ids {
                Some(ids) => places.extend(ids.into_iter().map(|id| (id, region))),
                None if lookups.packet_read(region).is_some() => unread.push(region),
                None => {}
            }
        }
        let rows = inputs
            .open_trains
            .iter()
            .chain(inputs.closed_trains)
            .chain(inputs.cars)
            .chain(inputs.inbound.unwrap_or_default())
            .chain(inputs.agent_runs.unwrap_or_default())
            .chain(inputs.publish_packets.unwrap_or_default())
            .map(|(j, _)| j)
            .chain(inputs.gate_runs);
        let facts: BTreeMap<_, _> = rows.map(|j| (j.id.to_string(), Facts::of(j))).collect();
        let aboard = lookups.carriage();
        let parents: Vec<_> = inputs
            .open_trains
            .iter()
            .chain(inputs.closed_trains)
            .chain(inputs.cars)
            .chain(inputs.inbound.unwrap_or_default())
            .chain(inputs.agent_runs.unwrap_or_default())
            .chain(inputs.publish_packets.unwrap_or_default())
            .chain(inputs.ops_requests.unwrap_or_default())
            .collect();
        let dispatch = inputs
            .agent_runs
            .unwrap_or_default()
            .iter()
            .filter_map(|(run, steps)| {
                let parent_id = run.metadata.get("packet")?.as_str()?;
                let (parent, parent_steps) = parents
                    .iter()
                    .find(|(p, _)| p.id.to_string() == parent_id)?;
                let active = specs.iter().find(|s| s.kind == parent.kind);
                dispatch_context(run, steps, parent, parent_steps, active)
                    .map(|context| (run.id.to_string(), context))
            })
            .collect();
        Snapshot {
            places,
            facts,
            aboard,
            dispatch,
            unread,
        }
    }

    pub fn place(&self, packet: &str) -> Where {
        match self.places.get(packet) {
            Some(r) => Where::At(r),
            None if self.aboard.contains_key(packet) => {
                // The partition's exact carrier must itself be observed;
                // an unread carrier never turns cargo into a guessed exit.
                if self
                    .aboard
                    .get(packet)
                    .is_some_and(|carrier| self.places.contains_key(carrier))
                {
                    Where::Off
                } else {
                    Where::Unknown
                }
            }
            None if self.unread.is_empty() => Where::Off,
            None => Where::Unknown,
        }
    }
}

/// One audit event that names a packet: the evidence a move cites.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Cause {
    /// `audit_log.id` — the log's own order.
    pub seq: i64,
    pub event_id: Uuid,
    /// `audit_log.created_at`: the wall instant it was written.
    pub at: Instant,
    pub kind: String,
    /// The packet it names (`payload.job_id`, or `payload.id` on a job
    /// event).
    pub packet: String,
}

/// ONE MOVE: a packet whose place changed, and the event that changed
/// it. `from` / `to` are `None` off the map — a packet filed onto it, or
/// leaving it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Move {
    /// The causing event's `created_at` (wall clock). The record's move
    /// is instantaneous; a surface that draws it over seconds says so.
    pub at: Instant,
    pub packet: String,
    pub kind: String,
    pub label: String,
    pub from: Option<String>,
    pub to: Option<String>,
    /// Whether a DERIVED route supports the move this took — a protocol
    /// crossing or a declared hand-off ([`crate::routes`], car R2), on-
    /// and off-ramps included. Stamped by the mover when it records the
    /// move ([`judged`]); `None` where the routes could not be derived
    /// then, which is unjudged — never "drawn". The regions read judges
    /// every move in its window again against the routes derived now,
    /// so a row stamped under an older derivation is never trusted over
    /// the current one.
    pub declared: Option<bool>,
    pub cause_event_id: Uuid,
    pub cause_seq: i64,
    pub cause_kind: String,
    /// THE HAND-OFF: the packet this one continues under a new identity
    /// — the gate-run whose green filed this car — present only when a
    /// lineage key links the two ([`LINK_KEYS`], or a shared branch).
    /// A surface draws one continuous stroke only when this is set.
    pub handoff_from: Option<String>,
    /// The key that links this move to another packet: the hand-off's,
    /// or [`CARRIES`] when the packet rode a train (or is the train).
    pub lineage: Option<String>,
    /// For a train: the cars aboard it.
    pub aboard: Vec<String>,
    /// For an EXIT: the terminal its packet closed on — the slug of its
    /// completed `outcome` step ([`terminal_of`]) — so an exit is
    /// counted and judged by the off-ramp it took, not only the region
    /// it left (backlog e23005c0). `None` on a move between regions, and
    /// on an exit whose packet the mover could not read back.
    #[serde(default)]
    pub terminal: Option<String>,
    /// Original registered dispatch context. Never reconstructed from a
    /// later claim or guessed for old rows that did not retain it.
    #[serde(default)]
    pub dispatch: Option<DispatchContext>,
}

impl Move {
    /// THE REGION THIS MOVE TAKES ITS PACKET OUT OF — what a region's
    /// "out" counts, so each region conserves what it holds (backlog
    /// 47880c86). That is `from`, except on a CARRIER'S OWN ROW: a train
    /// filed onto the map in the batch that made it up is drawn from where
    /// its cars stood ([`moves`] — it was made up there, and the dot rides
    /// the dock -> gates line its protocol declares), but the train itself
    /// never stood there. Its cars did, and each car's own row carries it
    /// out; counted as the train's too, the dock lost one packet more than
    /// it held. Such a row is the one that names cargo (`aboard`) and was
    /// linked by [`CARRIES`] — a train already on the map moves by its own
    /// place, with no lineage.
    pub fn leaves(&self) -> Option<&str> {
        let made_up_here = self.lineage.as_deref() == Some(CARRIES) && !self.aboard.is_empty();
        self.from.as_deref().filter(|_| !made_up_here)
    }
}

/// THE TERMINAL A PACKET CLOSED ON: the slug of its completed step that
/// the protocol it runs under (`spec`, its pinned version) declares a
/// terminal, or `None` while it has not closed on one. Read off the
/// protocol, not the step kind: an `outcome` step can be a marker that
/// is no terminal (page-audit's `styled`). Two completed terminals —
/// which the close never leaves — resolve to the first by sort order,
/// the rule `WorkflowSpec::completed_terminal_outcome` owns.
pub fn terminal_of(
    spec: &crate::registry::WorkflowSpec,
    steps: &[boss_core::job::Step],
) -> Option<String> {
    steps
        .iter()
        .filter(|s| s.status == boss_core::job::StepStatus::Completed)
        .filter_map(|s| {
            let slug = s.spec_slug.as_deref()?;
            spec.steps
                .iter()
                .any(|ss| ss.title == slug && ss.terminal.is_some())
                .then_some((s.sort_order, slug))
        })
        .min_by_key(|(order, _)| *order)
        .map(|(_, slug)| slug.to_string())
}

/// What the mover reads back about one packet a move names: its kind,
/// and the terminal it closed on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Described {
    pub kind: String,
    pub terminal: Option<String>,
}

/// The packets among `moves` the diff could not fully name: an exit
/// with no terminal yet, or a row whose packet the reading held no facts
/// for (an ops-request, which the partition places by its station
/// alone). The mover reads each back once ([`described`]).
pub fn described_ids(moves: &[Move]) -> Vec<String> {
    let mut ids: Vec<String> = moves
        .iter()
        .filter(|m| m.kind.is_empty() || (m.to.is_none() && m.terminal.is_none()))
        .map(|m| m.packet.clone())
        .collect();
    ids.sort();
    ids.dedup();
    ids
}

/// NAME EACH MOVE FROM ITS PACKET'S OWN ROW: the kind where the reading
/// held none, and on an exit the terminal it closed on. Pure over what
/// was read, and the record it reads is final — a closed packet does not
/// reopen (`job_outcome::REOPEN_HINT`), so a replay reads the same
/// terminal and writes the same row. A packet not read keeps what the
/// diff gave it.
pub fn described(moves: Vec<Move>, read: &BTreeMap<String, Described>) -> Vec<Move> {
    moves
        .into_iter()
        .map(|m| match read.get(&m.packet) {
            Some(d) => Move {
                kind: if m.kind.is_empty() {
                    d.kind.clone()
                } else {
                    m.kind.clone()
                },
                terminal: if m.to.is_none() {
                    m.terminal.clone().or_else(|| d.terminal.clone())
                } else {
                    None
                },
                ..m
            },
            None => m,
        })
        .collect()
}

/// A move as the record holds it, with its place in the record's order.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Recorded {
    pub seq: i64,
    #[serde(flatten)]
    pub r#move: Move,
    /// WHICH MOVER WROTE THE ROW — [`mover_version`] at the moment it
    /// was recorded (backlog c8d22e63). `None` on every row written
    /// before the stamp existed: unknown, never guessed.
    ///
    /// ABSENT, NOT NULL, WHEN UNSTAMPED. The correction ledger binds a
    /// diagnosis to its original by `move_diagnosis::hash` of this
    /// struct's JSON, and every original it holds was hashed before this
    /// field existed; serialising `None` as a key would change every
    /// such hash and orphan the ledger. Skipped, an unstamped row hashes
    /// byte-for-byte as it always did.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mover_version: Option<String>,
}

impl Recorded {
    /// Whether THIS build's mover wrote the row ([`mover_version`]). An
    /// unstamped row is not: unknown, never guessed. Read through
    /// [`Disposition::of`], the one definition of
    /// what a row counts as (backlog 3976779c).
    pub fn by_current_mover(&self) -> bool {
        self.mover_version.as_deref() == Some(current_mover_version())
    }
}

/// What a recorded row counts as — the ONE definition the moves read's
/// `dispositions` and `history` and the undeclared band's `crossings`
/// all take (backlog 3976779c, review 4c89fc8e N1). Until then the band
/// skipped a row another mover wrote (c8d22e63) while the read still
/// called it active, so the two disagreed after every deploy that
/// touched the mover.
///
/// LIVES HERE, NOT IN `move_diagnosis`. That file is an input of the
/// diagnosis model's content address (`move_diagnosis::model`,
/// `registered-agent-handoff-v2`), pinned byte-for-byte by
/// `handoff_model_source.rs`; this changes no diagnosis answer — it
/// reads [`crate::move_diagnosis::active`] as it was and only decides
/// which rows the READ counts as what — so it must not move that
/// identifier (car 50ded63d made the same call).
///
/// THE MOVER IS JUDGED FIRST: a row another mover wrote — superseded,
/// or written before the stamp — is [`Disposition::SupersededMover`]
/// whatever corrections it carries, because the band never counts it
/// either way. Of the current mover's rows, the band counts
/// [`Disposition::Active`] and [`Disposition::Unknown`] (unavailable
/// proof leaves the original counted) and skips
/// [`Disposition::Superseded`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Disposition {
    Active,
    /// An active correction supersedes the row; its id.
    Superseded(Uuid),
    /// Written by a mover other than this build's, or before the stamp;
    /// the version it carries, `None` when unstamped.
    SupersededMover(Option<String>),
    /// The correction ledger cannot be judged for this row; why.
    Unknown(String),
}

impl Disposition {
    pub fn of(entries: &[crate::move_diagnosis::Entry], original: &Recorded) -> Self {
        if !original.by_current_mover() {
            return Self::SupersededMover(original.mover_version.clone());
        }
        match crate::move_diagnosis::active(entries, original) {
            Ok(Some(id)) => Self::Superseded(id),
            Ok(None) => Self::Active,
            Err(why) => Self::Unknown(why),
        }
    }

    /// Whether the undeclared band counts the row.
    pub fn crossed(&self) -> bool {
        matches!(self, Self::Active | Self::Unknown(_))
    }
}

/// The moves read's `history`: every row in the window counted once by
/// its [`Disposition`], so `raw` is conserved as active + superseded +
/// unknown + superseded_mover, and the band's crossings are exactly
/// active + unknown (backlog 3976779c). It replaces
/// `move_diagnosis::Conservation` as what [`MovesStore::correction_conservation`]
/// answers; that struct stays where it is, unchanged, because its file
/// is hashed into the diagnosis model.
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct History {
    pub raw: i64,
    pub active: i64,
    pub superseded: i64,
    pub unknown: i64,
    /// Rows another mover wrote: held in the record and in `raw`,
    /// counted by no band.
    pub superseded_mover: i64,
}

impl History {
    pub fn observe(&mut self, entries: &[crate::move_diagnosis::Entry], original: &Recorded) {
        self.raw += 1;
        match Disposition::of(entries, original) {
            Disposition::Superseded(_) => self.superseded += 1,
            Disposition::Active => self.active += 1,
            Disposition::Unknown(_) => self.unknown += 1,
            Disposition::SupersededMover(_) => self.superseded_mover += 1,
        }
    }
}

/// THE MOVER'S READING VERSION (backlog c8d22e63): a content address of
/// the source that decides where a move says a packet went — this file
/// (the diff, the lineage, the record) and the partition it places by
/// ([`crate::regions::members`]). Computed, never bumped by hand, the way
/// `move_diagnosis::model` is, so no one has to remember it.
///
/// WHY IT EXISTS. A recorded move keeps the from/to its mover read, and
/// the undeclared band ([`crate::region_states::MOVES_UNDECLARED`])
/// re-judges those rows against the routes derived now — so on
/// 2026-09-27 a landed mover fix (train #726, backlog 47880c86) left
/// arrivals and the garage TROUBLED for up to 24h on the OLD mover's
/// misreadings (shed -> arrivals x48, off the map -> garage x1), and an
/// urgent item was filed on residue. Each row now carries the version
/// that wrote it, and [`MovesStore::crossings`] counts only the current
/// one's, so the deploy that carries a mover fix clears the band its
/// predecessor raised. The rows themselves stay in the record unchanged.
///
/// Any edit to either file — a comment included — is a new version, and
/// its deploy starts the band's window afresh. That is the cheap side of
/// the trade: a genuine undeclared route is re-observed the next time a
/// packet takes it, while a misreading can only ever age out. Their
/// TESTS live in sibling files (`moves/tests.rs`,
/// `regions/partition/tests.rs`), which are not hashed, so a test-only
/// edit no longer restarts the window (backlog 3976779c, review 4c89fc8e
/// N2: 15 distinct deploy days in 30 had touched either file).
///
/// The hash names no build; the moves read serves it beside
/// `mover_build`, the commit of the build computing it
/// ([`boss_core::startup::build_commit`]), so a version is traceable to
/// the source it hashed (review N3).
pub fn mover_version() -> String {
    current_mover_version().to_string()
}

/// [`mover_version`] borrowed, so a per-row comparison allocates nothing.
fn current_mover_version() -> &'static str {
    static VERSION: std::sync::LazyLock<String> = std::sync::LazyLock::new(|| {
        use sha2::{Digest, Sha256};
        let mut h = Sha256::new();
        for part in [
            include_str!("moves.rs"),
            include_str!("regions/partition.rs"),
        ] {
            h.update(part.as_bytes());
        }
        format!(
            "yard-mover-v1:{}",
            h.finalize()
                .iter()
                .map(|byte| format!("{byte:02x}"))
                .collect::<String>()
        )
    });
    VERSION.as_str()
}

/// Which packets are linked to which, over the facts of both readings.
struct Links<'a> {
    by_branch: BTreeMap<&'a str, BTreeSet<&'a str>>,
    /// packet -> the packets it names, with the key.
    names: BTreeMap<&'a str, Vec<(&'static str, &'a str)>>,
    /// packet -> the packets that name it, with the key.
    named_by: BTreeMap<&'a str, Vec<(&'static str, &'a str)>>,
}

impl<'a> Links<'a> {
    fn of(prev: &'a Snapshot, next: &'a Snapshot) -> Self {
        let mut links = Links {
            by_branch: BTreeMap::new(),
            names: BTreeMap::new(),
            named_by: BTreeMap::new(),
        };
        // The newer reading's facts win; a packet only the older reading
        // holds (one that left every read) keeps what it was.
        let facts = prev
            .facts
            .iter()
            .filter(|(id, _)| !next.facts.contains_key(*id))
            .chain(&next.facts);
        for (id, f) in facts {
            if let Some(b) = &f.branch {
                links.by_branch.entry(b).or_default().insert(id);
            }
            for (key, other) in &f.names {
                links.names.entry(id).or_default().push((key, other));
                links.named_by.entry(other).or_default().push((key, id));
            }
        }
        links
    }

    /// Every packet linked to `p`, with the key that links them.
    fn around(&self, p: &'a str) -> Vec<(&'a str, &'static str)> {
        let named = self
            .names
            .get(p)
            .into_iter()
            .chain(self.named_by.get(p))
            .flatten()
            .map(|(key, q)| (*q, *key));
        let shared = self
            .by_branch
            .values()
            .filter(|ids| ids.contains(p))
            .flatten()
            .map(|q| (*q, SAME_BRANCH));
        named.chain(shared).filter(|(q, _)| *q != p).collect()
    }

    /// Whether `carrier` names `p` as cargo ([`CARRIES`]).
    fn carries(&self, carrier: &str, p: &str) -> bool {
        self.names
            .get(carrier)
            .is_some_and(|named| named.iter().any(|(key, q)| *key == CARRIES && *q == p))
    }

    /// The packet that CARRIES `p` ([`CARRIES`]), if one stands on the
    /// map in `reading` — and only while `p` is open. A closed packet
    /// rides nothing: it left by its terminal, whatever train still
    /// names it. A car proven in the shed closes while the train that
    /// landed it stands in arrivals, and `boarded_jobs` is never pruned,
    /// so the mover drew every such close as a ride from the shed to
    /// arrivals — 66 on 2026-09-26, a route no packet takes (backlog
    /// 47880c86).
    fn carrier(&self, p: &str, reading: &Snapshot) -> Option<(&'a str, &'static str)> {
        if reading.facts.get(p).is_some_and(|f| f.closed) {
            return None;
        }
        if let Some(carrier) = reading.aboard.get(p) {
            // The partition's current carrier wins over historical cargo
            // links retained by earlier trains (814e6c66).
            return self
                .names
                .keys()
                .copied()
                .find(|name| *name == carrier)
                .filter(|name| self.carries(name, p) && reading.place(name).region().is_some())
                .map(|name| (name, CARRIES));
        }
        self.named_by
            .get(p)
            .into_iter()
            .flatten()
            .filter(|(key, q)| *key == CARRIES && reading.place(q).region().is_some())
            .map(|(key, q)| (*q, *key))
            .min()
    }
}

/// A tie between two predecessors breaks on the lineage: a shared
/// branch first, then [`LINK_KEYS`] in order.
fn rank(key: &str) -> usize {
    if key == SAME_BRANCH {
        return 0;
    }
    LINK_KEYS
        .iter()
        .position(|k| *k == key)
        .map_or(usize::MAX, |i| i + 1)
}

/// THE DIFF: the moves between two readings that the events in between
/// account for — pure, so the same readings and the same events give
/// the same rows on every replica and every replay.
///
/// ONLY PACKETS THE BATCH NAMES ARE JUDGED, and the packets one lineage
/// hop from them (a car's train, a gate-run's car): a packet no event
/// touched did not move in the record, whatever the readings say. Among
/// those, every packet whose place changed — both readings placing it,
/// neither [`Where::Unknown`] — becomes one row, resolved three ways:
///
/// * FILED ONTO THE MAP, and linked to a packet that stood on it: a
///   HAND-OFF, from where the predecessor stood (the car from the gates
///   its gate-run's green left). A predecessor that departed in this same
///   batch is preferred, and its departure is folded into this row.
///   Linked through [`CARRIES`] it is a ride instead (a car peeling off
///   its train to the shed; a train made up of the cars on the dock),
///   and no hand-off is named.
/// * LEFT THE MAP while a train that names it stands on it: it rides
///   that train, and its row goes where the train stands.
/// * Otherwise the move as read — region to region, onto the map, or
///   off it.
///
/// Each row cites the newest event in the batch naming the packet, else
/// the packet it is linked to, else any packet one hop away. Rows are
/// ordered by that event, then by packet.
pub fn moves(prev: &Snapshot, next: &Snapshot, causes: &[Cause]) -> Vec<Move> {
    let links = Links::of(prev, next);
    let mut newest: BTreeMap<&str, &Cause> = BTreeMap::new();
    for c in causes {
        let slot = newest.entry(c.packet.as_str()).or_insert(c);
        if c.seq > slot.seq {
            *slot = c;
        }
    }
    let judged: BTreeSet<&str> = newest
        .keys()
        .flat_map(|p| std::iter::once(*p).chain(links.around(p).into_iter().map(|(q, _)| q)))
        .collect();
    let changed: BTreeMap<&str, (Where, Where)> = judged
        .into_iter()
        .filter_map(|p| {
            let (was, is) = (prev.place(p), next.place(p));
            (was != is && was != Where::Unknown && is != Where::Unknown).then_some((p, (was, is)))
        })
        .collect();

    // Each changed packet's row, before its cause is attached: (packet,
    // from, to, the packet it is linked to, the key, whether a hand-off).
    type Draft<'a> = (
        &'a str,
        Option<&'static str>,
        Option<&'static str>,
        Option<(&'a str, &'static str)>,
        bool,
    );
    let mut drafts: Vec<Draft<'_>> = Vec::new();
    let mut folded: BTreeSet<&str> = BTreeSet::new();
    for (p, (was, is)) in &changed {
        if *was != Where::Off {
            continue;
        }
        let predecessor = links
            .around(p)
            .into_iter()
            .filter(|(q, _)| prev.place(q).region().is_some())
            .min_by_key(|(q, key)| (!changed.contains_key(q), rank(key), *q));
        match predecessor {
            Some((q, key)) => {
                let handoff = key != CARRIES;
                let departed = changed.get(q).is_some_and(|(_, qis)| *qis == Where::Off);
                if handoff && departed && links.carrier(q, next).is_none() {
                    folded.insert(q);
                }
                // WHERE IT STOOD IS WHERE IT STANDS, AND NOTHING CROSSED
                // (backlog 47880c86). Cargo set down where its carrier
                // stood — a cancelled train's car back on the dock — was
                // aboard there, so it did not move. And a hand-off whose
                // predecessor stood here and left in this batch — a green
                // stranded in the garage, handed to the held car it filed
                // there — crosses no border either: the predecessor's
                // departure is folded, and writing the successor as filed
                // from nowhere made the region gain a packet it never lost.
                let stood_here = prev.place(q).region() == is.region();
                let cargo = key == CARRIES && links.carries(q, p);
                if stood_here && (cargo || (handoff && departed)) {
                    continue;
                }
                // A predecessor that stood where this packet now stands
                // did not send it anywhere: a train made up on the dock
                // from the cars standing there (car R1) is filed ONTO
                // the map at the dock, the lineage still named.
                let from = prev.place(q).region().filter(|r| Some(*r) != is.region());
                drafts.push((p, from, is.region(), Some((q, key)), handoff));
            }
            None => drafts.push((p, None, is.region(), None, false)),
        }
    }
    for (p, (was, is)) in &changed {
        if *was == Where::Off {
            continue;
        }
        if *is == Where::Off {
            if let Some((c, key)) = links.carrier(p, next) {
                drafts.push((
                    p,
                    was.region(),
                    next.place(c).region(),
                    Some((c, key)),
                    false,
                ));
                continue;
            }
            if folded.contains(p) {
                continue;
            }
        }
        drafts.push((p, was.region(), is.region(), None, false));
    }

    let facts = |p: &str| next.facts.get(p).or_else(|| prev.facts.get(p));
    let mut out: Vec<Move> = drafts
        .into_iter()
        // A ROW THAT GOES NOWHERE IS NOT A MOVE. Since a train is placed
        // by its active step (car R1), a car that boards stands aboard a
        // train that is still on the dock, and its row is drawn where the
        // train stands — from the dock to the dock. The table refuses
        // exactly that (`yard_moves_goes_somewhere`), and a refused
        // batch is retried every tick, so one boarding would have wedged
        // the mover for good. The car did not move on the map; it rides
        // the train, whose own row carries it (`aboard`).
        .filter(|(_, from, to, _, _)| from != to)
        .filter_map(|(p, from, to, linked, handoff)| {
            let cause = newest
                .get(p)
                .or_else(|| linked.and_then(|(q, _)| newest.get(q)))
                .or_else(|| {
                    links
                        .around(p)
                        .into_iter()
                        .filter_map(|(q, _)| newest.get(q))
                        .max_by_key(|c| c.seq)
                })?;
            let f = facts(p);
            Some(Move {
                at: cause.at,
                packet: p.to_string(),
                kind: f.map(|f| f.kind.clone()).unwrap_or_default(),
                label: f.map_or_else(|| p.to_string(), |f| f.label.clone()),
                from: from.map(str::to_string),
                to: to.map(str::to_string),
                // Judged by the mover against the derived routes
                // ([`judged`]); the diff itself knows no routes.
                declared: None,
                cause_event_id: cause.event_id,
                cause_seq: cause.seq,
                cause_kind: cause.kind.clone(),
                handoff_from: linked.filter(|_| handoff).map(|(q, _)| q.to_string()),
                lineage: linked.map(|(_, key)| key.to_string()),
                aboard: f
                    .map(|f| f.carried().map(str::to_string).collect())
                    .unwrap_or_default(),
                // Read back by the mover ([`described`]).
                terminal: None,
                dispatch: linked
                    .filter(|(_, key)| handoff && *key == "packet")
                    .and_then(|_| next.dispatch.get(p).cloned()),
            })
        })
        .collect();
    out.sort_by(|a, b| (a.cause_seq, &a.packet).cmp(&(b.cause_seq, &b.packet)));
    out
}

/// STAMP each move with whether a derived route supports it
/// ([`crate::routes::RouteMap::declares`]) — the mover's judgement at
/// record time, so a stream frame says whether its dot rides a drawn
/// line.
pub fn judged(moves: Vec<Move>, routes: &crate::routes::RouteMap) -> Vec<Move> {
    moves
        .into_iter()
        .map(|m| Move {
            declared: if m
                .dispatch
                .as_ref()
                .is_some_and(|d| !d.belongs_to(&m.packet, m.handoff_from.as_deref()))
            {
                Some(false)
            } else {
                routes.judges_context(
                    m.from.as_deref(),
                    m.to.as_deref(),
                    Some(m.kind.as_str()),
                    m.terminal.as_deref(),
                    m.lineage.as_deref(),
                    m.dispatch.as_ref(),
                )
            },
            ..m
        })
        .collect()
}

/// What the mover holds between ticks: the log position its last
/// reading accounts for, and that reading.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Baseline {
    pub cursor: i64,
    pub snapshot: Snapshot,
}

/// One tick's evidence: the log's head, the packet-naming events after
/// the baseline's cursor up to it, and the map read now.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Tick {
    pub head: i64,
    pub causes: Vec<Cause>,
    /// More than [`MAX_BATCH`] events stood between the cursor and the
    /// head, so `causes` is not all of them.
    pub truncated: bool,
    pub snapshot: Snapshot,
}

/// What one tick concluded.
#[derive(Debug, Clone, PartialEq)]
pub enum Reading {
    /// No moves: the reading is a new baseline, for the reason given.
    /// A restart never animates a flood that did not happen.
    Resync(String),
    Moves(Vec<Move>),
}

/// THE MOVER, as a fold: the baseline and one tick in, the next baseline
/// and what the tick concluded out. Pure, so a fixture log replayed
/// twice concludes the same rows twice.
///
/// A FIRST READING, A LOG THAT MOVED BACKWARDS (an epoch trim), or a
/// batch past [`MAX_BATCH`] is a resync: the reading becomes the
/// baseline and no move is written, because a move attributed across a
/// gap would be a guess.
///
/// A PACKET THE NEW READING CANNOT PLACE ([`Where::Unknown`], its region
/// unread) keeps its old place in the next baseline, so the move is
/// judged when the region reads again rather than lost to one failed
/// read — the unread region is never read as empty.
pub fn advance(baseline: Option<&Baseline>, tick: Tick) -> (Baseline, Reading) {
    let resync = |why: String, tick: Tick| {
        (
            Baseline {
                cursor: tick.head,
                snapshot: tick.snapshot,
            },
            Reading::Resync(why),
        )
    };
    let Some(prev) = baseline else {
        return resync(
            "the mover started: this reading is its baseline".into(),
            tick,
        );
    };
    if tick.head < prev.cursor {
        return resync(
            format!(
                "the log's head moved back from {} to {} (an epoch trim)",
                prev.cursor, tick.head
            ),
            tick,
        );
    }
    if tick.truncated {
        return resync(
            format!(
                "more than {MAX_BATCH} events since the last reading at {}",
                prev.cursor
            ),
            tick,
        );
    }
    let found = moves(&prev.snapshot, &tick.snapshot, &tick.causes);
    let mut next = tick.snapshot;
    for (packet, region) in &prev.snapshot.places {
        if next.place(packet) == Where::Unknown {
            next.places.insert(packet.clone(), region);
            if let Some(f) = prev.snapshot.facts.get(packet) {
                next.facts
                    .entry(packet.clone())
                    .or_insert_with(|| f.clone());
            }
        }
    }
    (
        Baseline {
            cursor: tick.head,
            snapshot: next,
        },
        Reading::Moves(found),
    )
}

/// One route and how many moves took it in a window (design e765b3fc
/// §2b, source 3): a region, or `None` off the map, at either end. An
/// EXIT is counted per kind and terminal — the off-ramp it took — and
/// both are `None` on a move between regions (backlog e23005c0).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RouteCount {
    pub from: Option<String>,
    pub to: Option<String>,
    #[serde(default)]
    pub kind: Option<String>,
    #[serde(default)]
    pub terminal: Option<String>,
    #[serde(default)]
    pub lineage: Option<String>,
    #[serde(default)]
    pub dispatch: Option<DispatchContext>,
    /// Current route judgment. Null for missing original dispatch
    /// context, not a claim that the historical crossing was valid.
    #[serde(default)]
    pub declared: Option<bool>,
    pub moves: i64,
    pub last_at: Instant,
}

impl RouteCount {
    /// What one move is counted under: its route, and on an exit its
    /// kind and terminal.
    fn key_of(
        m: &Move,
    ) -> Result<
        (
            Option<String>,
            Option<String>,
            Option<String>,
            Option<String>,
            Option<String>,
            Option<String>,
        ),
        MovesError,
    > {
        let exit = m.to.is_none();
        let dispatch =
            m.kind == crate::agent_budget::RUN_KIND && m.lineage.as_deref() == Some("packet");
        Ok((
            m.from.clone(),
            m.to.clone(),
            (exit || dispatch)
                .then(|| m.kind.clone())
                .filter(|k| !k.is_empty()),
            exit.then(|| m.terminal.clone()).flatten(),
            dispatch.then(|| m.lineage.clone()).flatten(),
            dispatch
                .then_some(m.dispatch.as_ref())
                .flatten()
                .map(serde_json::to_string)
                .transpose()
                .map_err(|e| MovesError::Storage(e.to_string()))?,
        ))
    }
}

#[derive(Debug, thiserror::Error)]
pub enum MovesError {
    #[error("storage: {0}")]
    Storage(String),
}

fn validate_dispatch(m: &Move) -> Result<(), MovesError> {
    if m.dispatch
        .as_ref()
        .is_some_and(|d| !d.belongs_to(&m.packet, m.handoff_from.as_deref()))
    {
        return Err(MovesError::Storage(
            "dispatch context names another move's run or predecessor".into(),
        ));
    }
    Ok(())
}

/// THE PORT: the audit log's packet-naming events (read) and the moves
/// record (written once per move, read by seq).
#[async_trait]
pub trait MovesStore: Send + Sync {
    async fn admission_observation(
        &self,
        _id: Uuid,
    ) -> Result<Option<crate::move_admission::Observation>, MovesError> {
        Err(MovesError::Storage(
            "admission observations unavailable".into(),
        ))
    }
    async fn append_admission_observation(
        &self,
        _o: &crate::move_admission::Observation,
        _e: &boss_core::event::Event,
    ) -> Result<bool, MovesError> {
        Err(MovesError::Storage(
            "admission observations unavailable".into(),
        ))
    }
    /// `MAX(audit_log.id)`, 0 on an empty log — the one read a quiet
    /// second costs.
    async fn log_head(&self) -> Result<i64, MovesError>;
    /// The events after `after` up to and including `upto` that name a
    /// packet, oldest first, at most `limit` of them.
    async fn causes(&self, after: i64, upto: i64, limit: i64) -> Result<Vec<Cause>, MovesError>;
    /// Record moves; one already recorded for the same cause and packet
    /// is skipped. Answers how many were new.
    async fn record(&self, moves: &[Move]) -> Result<u64, MovesError>;
    /// The recorded moves after `seq`, oldest first, at most `limit`.
    async fn since(&self, seq: i64, limit: i64) -> Result<Vec<Recorded>, MovesError>;
    /// Append-only diagnosis/correction ledger. Unwired adapters refuse.
    async fn correction_entries(&self) -> Result<Vec<crate::move_diagnosis::Entry>, MovesError> {
        Err(MovesError::Storage(
            "move correction ledger unavailable".into(),
        ))
    }
    async fn append_correction(
        &self,
        _entry: &crate::move_diagnosis::Entry,
        _event: &boss_core::event::Event,
    ) -> Result<bool, MovesError> {
        Err(MovesError::Storage(
            "move correction ledger unavailable".into(),
        ))
    }
    async fn correction_conservation(&self, _since: Instant) -> Result<History, MovesError> {
        Err(MovesError::Storage(
            "move correction conservation unavailable".into(),
        ))
    }
    /// The newest seq recorded, 0 when there is none.
    async fn latest_seq(&self) -> Result<i64, MovesError>;
    /// Every route a move took at or after `since` — drawn or not, onto
    /// and off the map included — with how many did. Whether each is
    /// declared is judged against the routes derived NOW
    /// ([`with_undeclared`]), not the stamp a row was written with.
    ///
    /// ONLY THE CURRENT MOVER'S ROWS (backlog c8d22e63): a row whose
    /// `mover_version` is not [`mover_version`] — written by a superseded
    /// mover, or before the stamp existed — is held in the record and
    /// read by [`Self::since`], but counted here as no route, so a mover
    /// fix stops its predecessor's misreadings troubling a region on the
    /// deploy that carries it. The rows counted are exactly those whose
    /// [`Disposition::crossed`] holds — the
    /// definition [`Self::correction_conservation`] counts by (3976779c).
    async fn crossings(&self, since: Instant) -> Result<Vec<RouteCount>, MovesError>;
}

/// In-memory adapter: a log a test appends to, and the record.
#[derive(Default)]
pub struct InMemoryMoves {
    admission_observations: std::sync::Mutex<BTreeMap<Uuid, crate::move_admission::Observation>>,
    admission_events: std::sync::Mutex<Vec<boss_core::event::Event>>,
    log: std::sync::Mutex<Vec<Cause>>,
    rows: std::sync::Mutex<Vec<Recorded>>,
    corrections: std::sync::Mutex<Vec<crate::move_diagnosis::Entry>>,
    correction_events: std::sync::Mutex<Vec<boss_core::event::Event>>,
}

impl InMemoryMoves {
    pub fn admission_events(&self) -> Result<Vec<boss_core::event::Event>, MovesError> {
        Ok(self.admission_events.lock().map_err(poisoned)?.clone())
    }
    pub fn new() -> Self {
        Self::default()
    }

    /// Immutable events written atomically with the correction projection.
    pub fn correction_events(&self) -> Result<Vec<boss_core::event::Event>, MovesError> {
        Ok(self.correction_events.lock().map_err(poisoned)?.clone())
    }

    /// Append an event to the log this adapter reads.
    pub fn push_event(&self, cause: Cause) {
        if let Ok(mut log) = self.log.lock() {
            log.push(cause);
        }
    }

    /// Record `moves` as the mover `mover_version` would have — `None`
    /// for a row written before the stamp existed. [`MovesStore::record`]
    /// is this with [`mover_version`]; the double offers the rest so a
    /// test can hold a SUPERSEDED mover's rows, as Postgres holds them
    /// (backlog c8d22e63).
    pub fn record_by(
        &self,
        mover_version: Option<&str>,
        moves: &[Move],
    ) -> Result<u64, MovesError> {
        moves.iter().try_for_each(validate_dispatch)?;
        let mut rows = self.rows.lock().map_err(poisoned)?;
        let mut added = 0;
        // The table's own check (`yard_moves_goes_somewhere`), kept here
        // so the in-memory record refuses what Postgres refuses.
        if let Some(m) = moves.iter().find(|m| m.from == m.to) {
            return Err(MovesError::Storage(format!(
                "a move goes somewhere: {} from {:?} to {:?}",
                m.packet, m.from, m.to
            )));
        }
        for m in moves {
            let known = rows.iter().any(|r| {
                r.r#move.cause_event_id == m.cause_event_id && r.r#move.packet == m.packet
            });
            if !known {
                let seq = rows.last().map_or(1, |r| r.seq + 1);
                rows.push(Recorded {
                    seq,
                    r#move: m.clone(),
                    mover_version: mover_version.map(str::to_string),
                });
                added += 1;
            }
        }
        Ok(added)
    }
}

fn poisoned<T>(_: T) -> MovesError {
    MovesError::Storage("the in-memory lock was poisoned".into())
}

/// A page size, refused when negative as Postgres refuses `LIMIT -1`.
/// It read `usize::try_from(..).unwrap_or(0)` until the adapters-agree
/// suite (backlog be459ab9), so the double answered a caller's bug with
/// a confident empty page where production errored.
fn page(limit: i64) -> Result<usize, MovesError> {
    usize::try_from(limit)
        .map_err(|_| MovesError::Storage(format!("a page is never negative: limit {limit}")))
}

#[async_trait]
impl MovesStore for InMemoryMoves {
    async fn admission_observation(
        &self,
        id: Uuid,
    ) -> Result<Option<crate::move_admission::Observation>, MovesError> {
        Ok(self
            .admission_observations
            .lock()
            .map_err(poisoned)?
            .get(&id)
            .cloned())
    }
    async fn append_admission_observation(
        &self,
        o: &crate::move_admission::Observation,
        e: &boss_core::event::Event,
    ) -> Result<bool, MovesError> {
        crate::move_admission::validate_event(o, e).map_err(MovesError::Storage)?;
        let originals = self.rows.lock().map_err(poisoned)?;
        let original = originals
            .iter()
            .find(|r| r.seq == o.diagnosis.original.seq)
            .ok_or_else(|| MovesError::Storage("original missing".into()))?;
        let entries = self.corrections.lock().map_err(poisoned)?;
        crate::move_admission::validate_binding(o, original, &entries)
            .map_err(MovesError::Storage)?;
        let mut observations = self.admission_observations.lock().map_err(poisoned)?;
        if let Some(old) = observations.get(&o.id) {
            if !crate::move_admission::same_request(old, o) {
                return Err(MovesError::Storage(
                    "observation request identity conflict".into(),
                ));
            }
            return Ok(false);
        }
        let mut events = self.admission_events.lock().map_err(poisoned)?;
        if events.iter().any(|event| event.id == e.id) {
            return Err(MovesError::Storage(
                "observation event identity conflict".into(),
            ));
        }
        #[cfg(feature = "postgres")]
        if serde_json::to_vec(e)
            .map_err(|e| MovesError::Storage(e.to_string()))?
            .len()
            > boss_events::outbox::MAX_STAGED_EVENT_BYTES
        {
            return Err(MovesError::Storage(
                "whole observation event exceeds outbox transport bound".into(),
            ));
        }
        observations.insert(o.id, o.clone());
        events.push(e.clone());
        Ok(true)
    }
    async fn log_head(&self) -> Result<i64, MovesError> {
        Ok(self
            .log
            .lock()
            .map_err(poisoned)?
            .iter()
            .map(|c| c.seq)
            .max()
            .unwrap_or(0))
    }

    async fn causes(&self, after: i64, upto: i64, limit: i64) -> Result<Vec<Cause>, MovesError> {
        let mut out: Vec<Cause> = self
            .log
            .lock()
            .map_err(poisoned)?
            .iter()
            .filter(|c| c.seq > after && c.seq <= upto)
            .cloned()
            .collect();
        out.sort_by_key(|c| c.seq);
        out.truncate(page(limit)?);
        Ok(out)
    }

    async fn record(&self, moves: &[Move]) -> Result<u64, MovesError> {
        self.record_by(Some(&mover_version()), moves)
    }

    async fn since(&self, seq: i64, limit: i64) -> Result<Vec<Recorded>, MovesError> {
        Ok(self
            .rows
            .lock()
            .map_err(poisoned)?
            .iter()
            .filter(|r| r.seq > seq)
            .take(page(limit)?)
            .cloned()
            .collect())
    }

    async fn correction_entries(&self) -> Result<Vec<crate::move_diagnosis::Entry>, MovesError> {
        Ok(self.corrections.lock().map_err(poisoned)?.clone())
    }
    async fn append_correction(
        &self,
        entry: &crate::move_diagnosis::Entry,
        event: &boss_core::event::Event,
    ) -> Result<bool, MovesError> {
        if event.kind != entry.event_kind()
            || event.payload.get("entry")
                != Some(
                    &serde_json::to_value(entry).map_err(|e| MovesError::Storage(e.to_string()))?,
                )
        {
            return Err(MovesError::Storage("correction event mismatch".into()));
        }
        let rows = self.rows.lock().map_err(poisoned)?;
        let original = rows
            .iter()
            .find(|r| r.seq == entry.original_seq())
            .ok_or_else(|| MovesError::Storage("original missing".into()))?;
        let mut entries = self.corrections.lock().map_err(poisoned)?;
        let mut events = self.correction_events.lock().map_err(poisoned)?;
        let added = crate::move_diagnosis::append(&entries, entry, original)
            .map_err(MovesError::Storage)?;
        if added {
            let event = crate::move_diagnosis::conserved_event(
                entry,
                event,
                original,
                entries.len() as i64 + 1,
            )
            .map_err(MovesError::Storage)?;
            entries.push(entry.clone());
            events.push(event);
        }
        Ok(added)
    }

    async fn correction_conservation(&self, since: Instant) -> Result<History, MovesError> {
        let rows = self.rows.lock().map_err(poisoned)?;
        let entries = self.corrections.lock().map_err(poisoned)?;
        let mut counts = History::default();
        for row in rows.iter().filter(|r| r.r#move.at >= since) {
            counts.observe(&entries, row);
        }
        Ok(counts)
    }
    async fn latest_seq(&self) -> Result<i64, MovesError> {
        Ok(self
            .rows
            .lock()
            .map_err(poisoned)?
            .last()
            .map_or(0, |r| r.seq))
    }

    async fn crossings(&self, since: Instant) -> Result<Vec<RouteCount>, MovesError> {
        let rows = self.rows.lock().map_err(poisoned)?;
        let mut out = BTreeMap::new();
        let entries = self.corrections.lock().map_err(poisoned)?;
        for row in rows.iter() {
            // The one definition of what a row counts as (3976779c): only
            // the current mover's readings are judged (c8d22e63), and
            // unavailable proof leaves the original counted. A row
            // skipped here stays in the record; raw range reads never change.
            if !Disposition::of(&entries, row).crossed() {
                continue;
            }
            let m = &row.r#move;
            if m.at < since {
                continue;
            }
            let key = RouteCount::key_of(m)?;
            let slot = out.entry(key.clone()).or_insert_with(|| RouteCount {
                from: key.0,
                to: key.1,
                kind: key.2,
                terminal: key.3,
                lineage: key.4,
                dispatch: (m.kind == crate::agent_budget::RUN_KIND
                    && m.lineage.as_deref() == Some("packet"))
                .then(|| m.dispatch.clone())
                .flatten(),
                declared: None,
                moves: 0,
                last_at: m.at,
            });
            slot.moves += 1;
            slot.last_at = slot.last_at.max(m.at);
        }
        Ok(out.into_values().collect())
    }
}

#[cfg(feature = "postgres")]
pub(crate) mod pg {
    use super::*;
    use sqlx::{PgPool, Row};

    /// Postgres adapter: `yard_moves` here; the reads of `audit_log`
    /// through boss-events, which owns that table's SQL.
    pub struct PgMoves {
        pool: PgPool,
    }

    impl PgMoves {
        pub fn new(pool: PgPool) -> Self {
            Self { pool }
        }
    }

    fn storage(e: impl std::fmt::Display) -> MovesError {
        MovesError::Storage(e.to_string())
    }

    pub(crate) fn recorded(r: &sqlx::postgres::PgRow) -> Result<Recorded, MovesError> {
        let aboard: Value = r.try_get("aboard").map_err(storage)?;
        Ok(Recorded {
            seq: r.try_get("seq").map_err(storage)?,
            r#move: Move {
                at: r.try_get("at").map_err(storage)?,
                packet: r.try_get("packet").map_err(storage)?,
                kind: r.try_get("kind").map_err(storage)?,
                label: r.try_get("label").map_err(storage)?,
                from: r.try_get("from_region").map_err(storage)?,
                to: r.try_get("to_region").map_err(storage)?,
                declared: r.try_get("declared").map_err(storage)?,
                cause_event_id: r.try_get("cause_event_id").map_err(storage)?,
                cause_seq: r.try_get("cause_seq").map_err(storage)?,
                cause_kind: r.try_get("cause_kind").map_err(storage)?,
                handoff_from: r.try_get("handoff_from").map_err(storage)?,
                lineage: r.try_get("lineage").map_err(storage)?,
                aboard: serde_json::from_value(aboard).map_err(storage)?,
                terminal: r.try_get("terminal").map_err(storage)?,
                dispatch: r
                    .try_get::<Option<String>, _>("dispatch_context")
                    .map_err(storage)?
                    .map(|s| serde_json::from_str(&s))
                    .transpose()
                    .map_err(storage)?,
            },
            mover_version: r.try_get("mover_version").map_err(storage)?,
        })
    }

    #[async_trait]
    impl MovesStore for PgMoves {
        async fn admission_observation(
            &self,
            id: Uuid,
        ) -> Result<Option<crate::move_admission::Observation>, MovesError> {
            crate::move_admission_pg::get(&self.pool, id)
                .await
                .map_err(storage)
        }
        async fn append_admission_observation(
            &self,
            o: &crate::move_admission::Observation,
            e: &boss_core::event::Event,
        ) -> Result<bool, MovesError> {
            crate::move_admission_pg::append(&self.pool, o, e)
                .await
                .map_err(storage)
        }
        async fn log_head(&self) -> Result<i64, MovesError> {
            boss_events::tail_http::log_head(&self.pool)
                .await
                .map_err(storage)
        }

        async fn causes(
            &self,
            after: i64,
            upto: i64,
            limit: i64,
        ) -> Result<Vec<Cause>, MovesError> {
            Ok(
                boss_events::tail_http::packet_events(&self.pool, after, upto, limit)
                    .await
                    .map_err(storage)?
                    .into_iter()
                    .map(|r| Cause {
                        seq: r.id,
                        event_id: r.event_id,
                        at: r.created_at,
                        kind: r.kind,
                        packet: r.packet,
                    })
                    .collect(),
            )
        }

        async fn record(&self, moves: &[Move]) -> Result<u64, MovesError> {
            moves.iter().try_for_each(validate_dispatch)?;
            let mover = mover_version();
            let mut tx = self.pool.begin().await.map_err(storage)?;
            let mut added = 0;
            for m in moves {
                let done = sqlx::query(
                    "INSERT INTO yard_moves (at, packet, kind, label, from_region, to_region, \
                     declared, cause_event_id, cause_seq, cause_kind, handoff_from, lineage, aboard, \
                     terminal, dispatch_context, mover_version) \
                     VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, $15, $16) \
                     ON CONFLICT (cause_event_id, packet) DO NOTHING",
                )
                .bind(m.at)
                .bind(&m.packet)
                .bind(&m.kind)
                .bind(&m.label)
                .bind(&m.from)
                .bind(&m.to)
                .bind(m.declared)
                .bind(m.cause_event_id)
                .bind(m.cause_seq)
                .bind(&m.cause_kind)
                .bind(&m.handoff_from)
                .bind(&m.lineage)
                .bind(serde_json::json!(m.aboard))
                .bind(&m.terminal)
                .bind(m.dispatch.as_ref().map(serde_json::to_string).transpose().map_err(storage)?)
                .bind(&mover)
                .execute(&mut *tx)
                .await
                .map_err(storage)?;
                added += done.rows_affected();
            }
            tx.commit().await.map_err(storage)?;
            Ok(added)
        }

        async fn since(&self, seq: i64, limit: i64) -> Result<Vec<Recorded>, MovesError> {
            let rows = sqlx::query(
                "SELECT seq, at, packet, kind, label, from_region, to_region, declared, \
                     cause_event_id, cause_seq, cause_kind, handoff_from, lineage, aboard, terminal, dispatch_context, \
                     mover_version \
                 FROM yard_moves WHERE seq > $1 ORDER BY seq LIMIT $2",
            )
            .bind(seq)
            .bind(limit)
            .fetch_all(&self.pool)
            .await
            .map_err(storage)?;
            rows.iter().map(recorded).collect()
        }

        async fn correction_entries(
            &self,
        ) -> Result<Vec<crate::move_diagnosis::Entry>, MovesError> {
            let rows: Vec<(String,)> =
                sqlx::query_as("SELECT body FROM yard_move_corrections ORDER BY seq LIMIT 4097")
                    .fetch_all(&self.pool)
                    .await
                    .map_err(storage)?;
            if rows.len() > 4096 {
                return Err(storage("move correction ledger bound exceeded"));
            }
            rows.into_iter()
                .map(|(body,)| serde_json::from_str(&body).map_err(storage))
                .collect()
        }
        async fn append_correction(
            &self,
            entry: &crate::move_diagnosis::Entry,
            event: &boss_core::event::Event,
        ) -> Result<bool, MovesError> {
            if event.source != "jobs"
                || event.kind != entry.event_kind()
                || event.payload.get("entry")
                    != Some(&serde_json::to_value(entry).map_err(storage)?)
            {
                return Err(storage("correction event mismatch"));
            }
            let mut tx = self.pool.begin().await.map_err(storage)?;
            // One writer over the ledger, preserving global request identity and order.
            sqlx::query("LOCK TABLE yard_move_corrections IN SHARE ROW EXCLUSIVE MODE")
                .execute(&mut *tx)
                .await
                .map_err(storage)?;
            let row = sqlx::query("SELECT * FROM yard_moves WHERE seq=$1 FOR SHARE")
                .bind(entry.original_seq())
                .fetch_optional(&mut *tx)
                .await
                .map_err(storage)?
                .ok_or_else(|| storage("original missing"))?;
            let original = recorded(&row)?;
            let rows: Vec<(String,)> =
                sqlx::query_as("SELECT body FROM yard_move_corrections ORDER BY seq LIMIT 4097")
                    .fetch_all(&mut *tx)
                    .await
                    .map_err(storage)?;
            if rows.len() > 4096 {
                return Err(storage("move correction ledger bound exceeded"));
            }
            let entries: Vec<crate::move_diagnosis::Entry> = rows
                .into_iter()
                .map(|(body,)| serde_json::from_str(&body).map_err(storage))
                .collect::<Result<_, _>>()?;
            let added =
                crate::move_diagnosis::append(&entries, entry, &original).map_err(storage)?;
            if added {
                let event = crate::move_diagnosis::conserved_event(
                    entry,
                    event,
                    &original,
                    entries.len() as i64 + 1,
                )
                .map_err(storage)?;
                sqlx::query("INSERT INTO yard_move_corrections(seq,id,original_seq,body,event_id) VALUES($5,$1,$2,$3,$4)")
                    .bind(entry.id()).bind(entry.original_seq()).bind(serde_json::to_string(entry).map_err(storage)?).bind(event.id)
                    .bind(entries.len() as i64 + 1)
                    .execute(&mut *tx).await.map_err(storage)?;
                boss_events::outbox::record_event_in_tx(&mut tx, &event)
                    .await
                    .map_err(storage)?;
            }
            tx.commit().await.map_err(storage)?;
            Ok(added)
        }

        async fn correction_conservation(&self, since: Instant) -> Result<History, MovesError> {
            let entries = self.correction_entries().await?;
            let candidates: Vec<i64> = entries
                .iter()
                .filter_map(|e| match e {
                    crate::move_diagnosis::Entry::Correction { original_seq, .. } => {
                        Some(*original_seq)
                    }
                    _ => None,
                })
                .collect();
            let rows = sqlx::query("SELECT * FROM yard_moves WHERE seq=ANY($1) AND at >= $2")
                .bind(&candidates)
                .bind(since)
                .fetch_all(&self.pool)
                .await
                .map_err(storage)?;
            let mut counts = History::default();
            for row in rows {
                counts.observe(&entries, &recorded(&row)?);
            }
            // Every other row carries no correction, so its disposition
            // is its mover's alone (`Disposition::of`, 3976779c): active
            // when this build's mover wrote it, else superseded-mover.
            // `IS DISTINCT FROM` counts an unstamped row as another
            // mover's, as `by_current_mover` does.
            let (rest, other_mover): (i64, i64) = sqlx::query_as(
                "SELECT COUNT(*)::BIGINT, \
                 COUNT(*) FILTER (WHERE mover_version IS DISTINCT FROM $3)::BIGINT \
                 FROM yard_moves WHERE at >= $1 AND NOT (seq = ANY($2))",
            )
            .bind(since)
            .bind(&candidates)
            .bind(mover_version())
            .fetch_one(&self.pool)
            .await
            .map_err(storage)?;
            counts.raw += rest;
            counts.active += rest - other_mover;
            counts.superseded_mover += other_mover;
            Ok(counts)
        }
        async fn latest_seq(&self) -> Result<i64, MovesError> {
            sqlx::query_scalar("SELECT COALESCE(MAX(seq), 0) FROM yard_moves")
                .fetch_one(&self.pool)
                .await
                .map_err(storage)
        }

        async fn crossings(&self, since: Instant) -> Result<Vec<RouteCount>, MovesError> {
            // An exit is counted per kind and terminal
            // (`RouteCount::key_of`, the in-memory adapter's key).
            //
            // THE ORDER is that key's derived `Ord`: `None` (off the map)
            // first, then byte order — `COLLATE "C" NULLS FIRST` on every
            // column. It was `ORDER BY 1, 2, 3, 4` until the
            // adapters-agree suite (backlog be459ab9): NULLs LAST, and
            // names in the database's locale, which ignores `-` and folds
            // case (backlog 2987fb2d) — and `with_undeclared` names a
            // troubled region's routes in this order, so the two adapters
            // wrote the same reading two ways.
            let entries = self.correction_entries().await?;
            let candidates: Vec<i64> = entries
                .iter()
                .filter_map(|e| match e {
                    crate::move_diagnosis::Entry::Correction { original_seq, .. } => {
                        Some(*original_seq)
                    }
                    _ => None,
                })
                .collect();
            let originals = sqlx::query("SELECT * FROM yard_moves WHERE seq=ANY($1)")
                .bind(&candidates)
                .fetch_all(&self.pool)
                .await
                .map_err(storage)?;
            let mut excluded = Vec::new();
            for row in originals {
                let original = recorded(&row)?;
                // A changed/unsupported proof cannot demote the raw finding.
                if crate::move_diagnosis::active(&entries, &original)
                    .ok()
                    .flatten()
                    .is_some()
                {
                    excluded.push(original.seq);
                }
            }
            let rows = sqlx::query(
                "SELECT * FROM ( \
                 SELECT from_region, to_region, \
                 CASE WHEN (to_region IS NULL OR (kind = $2 AND lineage = 'packet')) AND kind <> '' THEN kind END AS crossing_kind, \
                 CASE WHEN to_region IS NULL THEN terminal END AS exit_terminal, \
                 CASE WHEN kind = $2 AND lineage = 'packet' THEN lineage END AS dispatch_lineage, \
                 CASE WHEN kind = $2 AND lineage = 'packet' THEN dispatch_context END AS dispatch_context, \
                 COUNT(*)::BIGINT AS moves, MAX(at) AS last_at \
                 FROM yard_moves WHERE at >= $1 AND NOT (seq = ANY($3)) AND mover_version = $4 \
                 GROUP BY 1, 2, 3, 4, 5, 6) routes \
                 ORDER BY from_region COLLATE \"C\" NULLS FIRST, \
                 to_region COLLATE \"C\" NULLS FIRST, \
                 crossing_kind COLLATE \"C\" NULLS FIRST, \
                 exit_terminal COLLATE \"C\" NULLS FIRST, \
                 dispatch_lineage COLLATE \"C\" NULLS FIRST, \
                 dispatch_context COLLATE \"C\" NULLS FIRST",
            )
            .bind(since)
            .bind(crate::agent_budget::RUN_KIND)
            .bind(excluded)
            // Only the current mover's readings (c8d22e63): `=` never
            // matches a NULL, so an unstamped row is uncounted too.
            .bind(mover_version())
            .fetch_all(&self.pool)
            .await
            .map_err(storage)?;
            rows.iter()
                .map(|r| {
                    Ok(RouteCount {
                        from: r.try_get("from_region").map_err(storage)?,
                        to: r.try_get("to_region").map_err(storage)?,
                        kind: r.try_get("crossing_kind").map_err(storage)?,
                        terminal: r.try_get("exit_terminal").map_err(storage)?,
                        lineage: r.try_get("dispatch_lineage").map_err(storage)?,
                        dispatch: r
                            .try_get::<Option<String>, _>("dispatch_context")
                            .map_err(storage)?
                            .map(|s| serde_json::from_str(&s))
                            .transpose()
                            .map_err(storage)?,
                        declared: None,
                        moves: r.try_get("moves").map_err(storage)?,
                        last_at: r.try_get("last_at").map_err(storage)?,
                    })
                })
                .collect()
        }
    }
}

#[cfg(feature = "postgres")]
pub use pg::PgMoves;

/// Fold the observed-undeclared reading into the regions read, and let
/// it JUDGE (design e765b3fc §2b; car R2 switched it on).
///
/// `crossings` is every route the moves record saw taken in the window;
/// each is judged against the routes derived now
/// ([`crate::routes::RouteMap::declares`]), and a region carries the
/// undeclared ones that end in it — or, for an exit, that leave it for
/// off the map, since a route to nowhere has no other region to name.
/// A region carrying any is TROUBLED on
/// [`crate::region_states::MOVES_UNDECLARED`], at once and naming each
/// route with its count: a packet took a route no protocol, rule or verb
/// declares, so either the map or the record is wrong, and zero is what
/// a healthy network reads.
///
/// UNREAD IS NOT EMPTY. `None` for either input — the record could not
/// be read, or the routes could not be derived — leaves every region's
/// reading `null` and judges nothing: an empty route set would call
/// every move undeclared, and an empty record would say none was.
pub fn with_undeclared(
    map: crate::regions::Regions,
    crossings: Option<Vec<RouteCount>>,
    routes: Option<&crate::routes::RouteMap>,
    now: Instant,
) -> crate::regions::Regions {
    use crate::region_states::{Finding, MOVES_UNDECLARED, decide};
    use crate::regions::RegionState;
    let undeclared: Option<Vec<RouteCount>> = crossings.zip(routes).map(|(all, routes)| {
        all.into_iter()
            .filter(|c| c.from != c.to)
            .map(|c| RouteCount {
                declared: routes.judges_observation(&c),
                ..c
            })
            .filter(|c| c.declared != Some(true))
            .collect()
    });
    let regions = map
        .regions
        .into_iter()
        .map(|r| {
            let mine: Option<Vec<RouteCount>> = undeclared.as_ref().map(|all| {
                all.iter()
                    .filter(|c| match &c.to {
                        Some(to) => *to == r.name,
                        None => c.from.as_deref() == Some(r.name.as_str()),
                    })
                    .cloned()
                    .collect()
            });
            let said: Vec<String> = mine
                .iter()
                .flatten()
                .filter(|c| c.moves > 0)
                .map(|c| {
                    // An exit names the off-ramp it took (e23005c0).
                    let by = match (&c.kind, &c.terminal) {
                        (Some(k), Some(t)) => format!(" by {k} `{t}`"),
                        _ => String::new(),
                    };
                    format!(
                        "{} → {}{by} ×{}{}",
                        c.from.as_deref().unwrap_or("off the map"),
                        c.to.as_deref().unwrap_or("off the map"),
                        c.moves,
                        if c.declared.is_none() {
                            " (unknown: original dispatch cannot be judged)"
                        } else {
                            ""
                        }
                    )
                })
                .collect();
            let region = crate::regions::Region {
                undeclared: mine,
                ..r
            };
            if said.is_empty() || region.state == RegionState::Troubled {
                return region;
            }
            let has_unknown = region
                .undeclared
                .as_ref()
                .is_some_and(|counts| counts.iter().any(|c| c.declared.is_none()));
            let lead = format!(
                "packets took {} {}: {}",
                if said.len() == 1 { "a route" } else { "routes" },
                if has_unknown {
                    "whose declaration is false or unjudged"
                } else {
                    "no protocol declares"
                },
                said.join(", ")
            );
            crate::regions::Region {
                state: RegionState::Troubled,
                band: Some(decide(
                    &Finding::new(MOVES_UNDECLARED, None, String::new(), lead.clone()),
                    now,
                )),
                why: format!("{lead} · {}", region.why),
                ..region
            }
        })
        .collect();
    crate::regions::Regions { regions, ..map }
}

/// One frame of `GET /api/yard/moves/stream`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "frame", rename_all = "kebab-case")]
pub enum Frame {
    Move(Box<Recorded>),
    /// Take the current placement as the baseline; nothing moved. Sent
    /// when the mover (re)starts, after a gap, and to a viewer that
    /// connects with nothing to resume from or fell behind.
    Resync {
        seq: i64,
        reason: String,
    },
}

/// What this replica's mover is doing, as `GET /api/yard/moves` says
/// it: a loop that has stopped recording must say so where the record
/// is read, not only in a log nobody tails.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Default)]
pub struct MoverStatus {
    /// `starting`, `recording`, or `failing`.
    pub state: String,
    pub why: String,
    /// The log position its baseline accounts for.
    pub cursor: i64,
    /// When it last read the log (wall clock).
    pub read_at: Option<Instant>,
}

/// How many frames a slow viewer may fall behind before it is sent a
/// `resync` instead of the backlog.
pub const FRAME_BUFFER: usize = 256;

/// THE FEED the jobs API serves moves from: the record, the frames the
/// mover publishes to every open stream on this replica, and the
/// mover's own status.
pub struct MovesFeed {
    pub store: std::sync::Arc<dyn MovesStore>,
    frames: tokio::sync::broadcast::Sender<Frame>,
    status: std::sync::RwLock<MoverStatus>,
}

impl MovesFeed {
    pub fn new(store: std::sync::Arc<dyn MovesStore>) -> Self {
        let (frames, _) = tokio::sync::broadcast::channel(FRAME_BUFFER);
        MovesFeed {
            store,
            frames,
            status: std::sync::RwLock::new(MoverStatus {
                state: "starting".into(),
                why: "the mover has not read the log yet".into(),
                ..MoverStatus::default()
            }),
        }
    }

    pub fn subscribe(&self) -> tokio::sync::broadcast::Receiver<Frame> {
        self.frames.subscribe()
    }

    /// Send a frame to every open stream. No stream open is not an
    /// error: the record holds the move either way.
    pub fn publish(&self, frame: Frame) {
        let _ = self.frames.send(frame);
    }

    pub fn status(&self) -> MoverStatus {
        self.status
            .read()
            .map(|s| s.clone())
            .unwrap_or_else(|_| MoverStatus {
                state: "failing".into(),
                why: "the mover's status lock was poisoned".into(),
                ..MoverStatus::default()
            })
    }

    pub fn set_status(&self, status: MoverStatus) {
        if let Ok(mut s) = self.status.write() {
            *s = status;
        }
    }
}

// THE TESTS LIVE BESIDE THIS FILE, NOT IN IT (backlog 3976779c, review
// 4c89fc8e N2): [`mover_version`] hashes this file, so a test-only edit
// here used to mint a new version and restart the undeclared band's
// window on its deploy.
#[cfg(test)]
#[path = "moves/tests.rs"]
mod tests;
