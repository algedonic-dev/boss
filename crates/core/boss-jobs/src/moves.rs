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
    /// Regions whose reading failed. While any is unread, a packet that
    /// is in no read region might be in one of these — so it is
    /// [`Where::Unknown`], never "off the map".
    pub unread: Vec<&'static str>,
}

/// Where one reading puts one packet.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Where {
    At(&'static str),
    /// On no region of the map, and every region was read.
    Off,
    /// On no READ region, while some region was unread.
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
        let mut places = BTreeMap::new();
        let mut unread = Vec::new();
        for (region, ids) in crate::regions::members(inputs) {
            match ids {
                Some(ids) => places.extend(ids.into_iter().map(|id| (id, region))),
                None => unread.push(region),
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
        let facts = rows.map(|j| (j.id.to_string(), Facts::of(j))).collect();
        Snapshot {
            places,
            facts,
            unread,
        }
    }

    pub fn place(&self, packet: &str) -> Where {
        match self.places.get(packet) {
            Some(r) => Where::At(r),
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
            declared: Some(routes.judges(
                m.from.as_deref(),
                m.to.as_deref(),
                Some(m.kind.as_str()),
                m.terminal.as_deref(),
            )),
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
    pub moves: i64,
    pub last_at: Instant,
}

impl RouteCount {
    /// What one move is counted under: its route, and on an exit its
    /// kind and terminal.
    fn key_of(
        m: &Move,
    ) -> (
        Option<String>,
        Option<String>,
        Option<String>,
        Option<String>,
    ) {
        let exit = m.to.is_none();
        (
            m.from.clone(),
            m.to.clone(),
            exit.then(|| m.kind.clone()).filter(|k| !k.is_empty()),
            exit.then(|| m.terminal.clone()).flatten(),
        )
    }
}

#[derive(Debug, thiserror::Error)]
pub enum MovesError {
    #[error("storage: {0}")]
    Storage(String),
}

/// THE PORT: the audit log's packet-naming events (read) and the moves
/// record (written once per move, read by seq).
#[async_trait]
pub trait MovesStore: Send + Sync {
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
    /// The newest seq recorded, 0 when there is none.
    async fn latest_seq(&self) -> Result<i64, MovesError>;
    /// Every route a move took at or after `since` — drawn or not, onto
    /// and off the map included — with how many did. Whether each is
    /// declared is judged against the routes derived NOW
    /// ([`with_undeclared`]), not the stamp a row was written with.
    async fn crossings(&self, since: Instant) -> Result<Vec<RouteCount>, MovesError>;
}

/// In-memory adapter: a log a test appends to, and the record.
#[derive(Default)]
pub struct InMemoryMoves {
    log: std::sync::Mutex<Vec<Cause>>,
    rows: std::sync::Mutex<Vec<Recorded>>,
}

impl InMemoryMoves {
    pub fn new() -> Self {
        Self::default()
    }

    /// Append an event to the log this adapter reads.
    pub fn push_event(&self, cause: Cause) {
        if let Ok(mut log) = self.log.lock() {
            log.push(cause);
        }
    }
}

fn poisoned<T>(_: T) -> MovesError {
    MovesError::Storage("the in-memory lock was poisoned".into())
}

#[async_trait]
impl MovesStore for InMemoryMoves {
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
        out.truncate(usize::try_from(limit).unwrap_or(0));
        Ok(out)
    }

    async fn record(&self, moves: &[Move]) -> Result<u64, MovesError> {
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
                });
                added += 1;
            }
        }
        Ok(added)
    }

    async fn since(&self, seq: i64, limit: i64) -> Result<Vec<Recorded>, MovesError> {
        Ok(self
            .rows
            .lock()
            .map_err(poisoned)?
            .iter()
            .filter(|r| r.seq > seq)
            .take(usize::try_from(limit).unwrap_or(0))
            .cloned()
            .collect())
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
        for m in rows.iter().map(|r| &r.r#move) {
            if m.at < since {
                continue;
            }
            let key = RouteCount::key_of(m);
            let slot = out.entry(key.clone()).or_insert_with(|| RouteCount {
                from: key.0,
                to: key.1,
                kind: key.2,
                terminal: key.3,
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
mod pg {
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

    fn recorded(r: &sqlx::postgres::PgRow) -> Result<Recorded, MovesError> {
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
            },
        })
    }

    #[async_trait]
    impl MovesStore for PgMoves {
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
            let mut tx = self.pool.begin().await.map_err(storage)?;
            let mut added = 0;
            for m in moves {
                let done = sqlx::query(
                    "INSERT INTO yard_moves (at, packet, kind, label, from_region, to_region, \
                     declared, cause_event_id, cause_seq, cause_kind, handoff_from, lineage, aboard, \
                     terminal) \
                     VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14) \
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
                 cause_event_id, cause_seq, cause_kind, handoff_from, lineage, aboard, terminal \
                 FROM yard_moves WHERE seq > $1 ORDER BY seq LIMIT $2",
            )
            .bind(seq)
            .bind(limit)
            .fetch_all(&self.pool)
            .await
            .map_err(storage)?;
            rows.iter().map(recorded).collect()
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
            let rows = sqlx::query(
                "SELECT from_region, to_region, \
                 CASE WHEN to_region IS NULL AND kind <> '' THEN kind END AS exit_kind, \
                 CASE WHEN to_region IS NULL THEN terminal END AS exit_terminal, \
                 COUNT(*)::BIGINT AS moves, MAX(at) AS last_at \
                 FROM yard_moves WHERE at >= $1 \
                 GROUP BY 1, 2, 3, 4 ORDER BY 1, 2, 3, 4",
            )
            .bind(since)
            .fetch_all(&self.pool)
            .await
            .map_err(storage)?;
            rows.iter()
                .map(|r| {
                    Ok(RouteCount {
                        from: r.try_get("from_region").map_err(storage)?,
                        to: r.try_get("to_region").map_err(storage)?,
                        kind: r.try_get("exit_kind").map_err(storage)?,
                        terminal: r.try_get("exit_terminal").map_err(storage)?,
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
            .filter(|c| c.from != c.to && !routes.judges_count(c))
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
                        "{} → {}{by} ×{}",
                        c.from.as_deref().unwrap_or("off the map"),
                        c.to.as_deref().unwrap_or("off the map"),
                        c.moves
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
            let lead = format!(
                "packets took {} the map does not declare: {}",
                if said.len() == 1 { "a route" } else { "routes" },
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

#[cfg(test)]
mod tests {
    use super::*;

    fn t(s: &str) -> Instant {
        chrono::DateTime::parse_from_rfc3339(s)
            .unwrap()
            .with_timezone(&chrono::Utc)
    }

    fn facts(kind: &str, branch: Option<&str>, names: &[(&'static str, &str)]) -> Facts {
        Facts {
            kind: kind.into(),
            label: branch.unwrap_or("a title").into(),
            branch: branch.map(str::to_string),
            names: names.iter().map(|(k, v)| (*k, (*v).to_string())).collect(),
            closed: false,
        }
    }

    fn closed(f: Facts) -> Facts {
        Facts { closed: true, ..f }
    }

    fn snapshot(places: &[(&str, &'static str)], known: &[(&str, Facts)]) -> Snapshot {
        Snapshot {
            places: places.iter().map(|(p, r)| ((*p).to_string(), *r)).collect(),
            facts: known
                .iter()
                .map(|(p, f)| ((*p).to_string(), f.clone()))
                .collect(),
            unread: Vec::new(),
        }
    }

    fn cause(seq: i64, packet: &str, kind: &str) -> Cause {
        Cause {
            seq,
            event_id: Uuid::from_u128(seq as u128),
            at: t("2026-09-26T03:18:00Z") + chrono::Duration::seconds(seq),
            kind: kind.into(),
            packet: packet.into(),
        }
    }

    /// A GATE-RUN GREEN, THEN A CAR FILED, IS ONE HAND-OFF. The gate-run
    /// leaves the gates (its green closes it; its car claims it, so it
    /// is not a stranded green) and the car appears on the dock — two
    /// packets, one journey. The record writes ONE move: the car, from
    /// the gates to the dock, handed off from the gate-run by the branch
    /// the two share. The gate-run's own departure is folded into it,
    /// not written as an exit beside it.
    #[test]
    fn a_green_then_a_car_filed_is_one_hand_off_with_a_lineage_link() {
        let gate = facts("gate-run", Some("fix/x"), &[("agent_run", "run-1")]);
        let car = facts("ship-a-change", Some("fix/x"), &[("agent_run", "run-1")]);
        let prev = snapshot(&[("gate-1", "gates")], &[("gate-1", gate.clone())]);
        let next = snapshot(&[("car-1", "dock")], &[("gate-1", gate), ("car-1", car)]);
        let batch = [
            cause(101, "gate-1", "jobs.step.updated"),
            cause(102, "gate-1", "jobs.job.updated"),
            cause(103, "car-1", "jobs.job.created"),
            cause(104, "car-1", "jobs.step.created"),
        ];
        let out = moves(&prev, &next, &batch);
        assert_eq!(out.len(), 1, "one journey, one row: {out:#?}");
        let m = &out[0];
        assert_eq!(m.packet, "car-1");
        assert_eq!(
            (m.from.as_deref(), m.to.as_deref()),
            (Some("gates"), Some("dock"))
        );
        assert_eq!(m.handoff_from.as_deref(), Some("gate-1"));
        assert_eq!(m.lineage.as_deref(), Some(SAME_BRANCH));
        assert_eq!(
            m.declared, None,
            "the diff knows no routes; the mover judges it against the derived ones"
        );
        // It cites the newest event that names the car itself.
        assert_eq!(m.cause_seq, 104);
        assert_eq!(m.cause_event_id, Uuid::from_u128(104));
        assert_eq!(
            m.at,
            t("2026-09-26T03:18:00Z") + chrono::Duration::seconds(104)
        );
        assert_eq!(
            (m.kind.as_str(), m.label.as_str()),
            ("ship-a-change", "fix/x")
        );
    }

    /// With NO lineage key between them, a departure and an arrival are
    /// two facts: the gate-run leaves the map, the car is filed onto it,
    /// and nothing travels between them.
    #[test]
    fn with_no_lineage_a_departure_and_an_arrival_are_two_rows_and_no_hand_off() {
        let gate = facts("gate-run", Some("fix/x"), &[]);
        let car = facts("ship-a-change", Some("fix/other"), &[]);
        let prev = snapshot(&[("gate-1", "gates")], &[("gate-1", gate.clone())]);
        let next = snapshot(&[("car-1", "dock")], &[("gate-1", gate), ("car-1", car)]);
        let out = moves(
            &prev,
            &next,
            &[
                cause(1, "gate-1", "jobs.job.updated"),
                cause(2, "car-1", "jobs.job.created"),
            ],
        );
        let rows: Vec<_> = out
            .iter()
            .map(|m| {
                (
                    m.packet.as_str(),
                    m.from.as_deref(),
                    m.to.as_deref(),
                    m.handoff_from.as_deref(),
                )
            })
            .collect();
        assert_eq!(
            rows,
            [
                ("gate-1", Some("gates"), None, None),
                ("car-1", None, Some("dock"), None)
            ]
        );
        assert!(
            out.iter().all(|m| m.declared.is_none()),
            "on or off the map is not a drawn route"
        );
    }

    /// A packet whose place changed with NO event naming it or its
    /// lineage did not move in the record: an arrival ageing out of the
    /// window. No row.
    #[test]
    fn a_place_changed_by_no_event_is_not_a_move() {
        let train = facts("pr-train", None, &[]);
        let prev = snapshot(&[("train-1", "arrivals")], &[("train-1", train.clone())]);
        let next = snapshot(&[], &[("train-1", train)]);
        assert!(
            moves(
                &prev,
                &next,
                &[cause(1, "someone-else", "jobs.job.updated")]
            )
            .is_empty()
        );
    }

    /// A CAR LEAVES THE DOCK ABOARD A TRAIN: it rides the train (the
    /// train names it in `boarded_jobs`), so its row goes where the
    /// train stands — not off the map — and the train's own row carries
    /// who is aboard. When the train arrives, its cars peel off to the
    /// shed from where the train stood.
    #[test]
    fn a_car_rides_its_train_and_peels_off_to_the_shed_when_it_lands() {
        let car = facts("ship-a-change", Some("fix/x"), &[]);
        let train = facts("pr-train", None, &[(CARRIES, "car-1")]);
        let parked = snapshot(&[("car-1", "dock")], &[("car-1", car.clone())]);
        let boarded = snapshot(
            &[("train-1", "track")],
            &[("car-1", car.clone()), ("train-1", train.clone())],
        );
        let out = moves(
            &parked,
            &boarded,
            &[
                cause(1, "train-1", "jobs.job.created"),
                cause(2, "car-1", "jobs.job.updated"),
            ],
        );
        let rows: Vec<_> = out
            .iter()
            .map(|m| {
                (
                    m.packet.as_str(),
                    m.from.as_deref(),
                    m.to.as_deref(),
                    m.lineage.as_deref(),
                )
            })
            .collect();
        assert_eq!(
            rows,
            [
                ("train-1", Some("dock"), Some("track"), Some(CARRIES)),
                ("car-1", Some("dock"), Some("track"), Some(CARRIES)),
            ]
        );
        assert_eq!(out[0].aboard, ["car-1"]);
        assert!(
            out.iter().all(|m| m.handoff_from.is_none()),
            "riding is not a hand-off"
        );

        let landed = snapshot(
            &[("train-1", "arrivals"), ("car-1", "shed")],
            &[("car-1", car), ("train-1", train)],
        );
        let out = moves(
            &boarded,
            &landed,
            &[
                cause(3, "train-1", "jobs.job.updated"),
                cause(4, "car-1", "jobs.job.updated"),
            ],
        );
        let rows: Vec<_> = out
            .iter()
            .map(|m| {
                (
                    m.packet.as_str(),
                    m.from.as_deref(),
                    m.to.as_deref(),
                    m.declared,
                )
            })
            .collect();
        assert_eq!(
            rows,
            [
                ("train-1", Some("track"), Some("arrivals"), None),
                // A row of its own, from wherever the train stood in the
                // older reading. The live conductor closes its cars before
                // the train's `merged`, so in the record they peel off at
                // the gates, which train-reconcile's hand-off declares
                // (backlog 47880c86); this fixture only pins the diff.
                ("car-1", Some("track"), Some("shed"), None),
            ]
        );
    }

    /// A CAR BOARDS A TRAIN STILL BEING MADE UP ON THE DOCK (car R1 places
    /// a train by its active step): the car rides the train and stands
    /// where it stands — the dock — so it has not moved on the map, and
    /// no row is written. Written, it would go from the dock to the dock,
    /// which the table refuses (`yard_moves_goes_somewhere`) — and a
    /// refused batch is judged again every tick, so the mover would never
    /// have recorded another move. The in-memory record refuses it too.
    #[tokio::test]
    async fn a_car_boarding_a_train_on_the_dock_goes_nowhere_and_writes_no_row() {
        let car = facts("ship-a-change", Some("fix/x"), &[]);
        let train = facts("pr-train", None, &[(CARRIES, "car-1")]);
        let parked = snapshot(&[("car-1", "dock")], &[("car-1", car.clone())]);
        let made_up = snapshot(
            &[("train-1", "dock")],
            &[("car-1", car.clone()), ("train-1", train.clone())],
        );
        let out = moves(
            &parked,
            &made_up,
            &[
                cause(1, "train-1", "jobs.job.created"),
                cause(2, "car-1", "jobs.job.updated"),
            ],
        );
        let rows: Vec<_> = out
            .iter()
            .map(|m| (m.packet.as_str(), m.from.as_deref(), m.to.as_deref()))
            .collect();
        assert_eq!(
            rows,
            [("train-1", None, Some("dock"))],
            "the train is filed onto the dock; the car it collected did not move"
        );
        let store = InMemoryMoves::new();
        assert_eq!(store.record(&out).await.unwrap(), 1);
        let mut nowhere = out[0].clone();
        nowhere.from = nowhere.to.clone();
        assert!(
            store.record(&[nowhere]).await.is_err(),
            "the in-memory record refuses what the table refuses"
        );
    }

    /// THE MOVER'S STAMP is the derived routes' answer: a move on a
    /// declared route is `declared`, one on no route is not, and a move
    /// the diff wrote carries no stamp until it is judged.
    #[test]
    fn a_move_is_stamped_against_the_derived_routes() {
        let routes = crate::routes::derive(
            &[],
            &[],
            Some(&[RouteCount {
                from: Some("track".into()),
                to: Some("shed".into()),
                kind: None,
                terminal: None,
                moves: 1,
                last_at: t("2026-09-26T03:00:00Z"),
            }]),
        );
        let gate = facts("gate-run", Some("fix/x"), &[]);
        let car = facts("ship-a-change", Some("fix/x"), &[]);
        let prev = snapshot(&[("gate-1", "gates")], &[("gate-1", gate.clone())]);
        let next = snapshot(&[("car-1", "dock")], &[("gate-1", gate), ("car-1", car)]);
        let found = moves(
            &prev,
            &next,
            &[
                cause(1, "gate-1", "jobs.job.updated"),
                cause(2, "car-1", "jobs.job.created"),
            ],
        );
        assert!(found.iter().all(|m| m.declared.is_none()));
        let stamped = judged(found, &routes);
        assert_eq!(
            stamped.iter().map(|m| m.declared).collect::<Vec<_>>(),
            [Some(false)],
            "observed only, gates -> dock is not declared by an empty route set"
        );
    }

    /// A FIXTURE LOG: the mover's first reading, then a green that files
    /// a car, then the car boarding a train, then the train landing —
    /// each tick the events the log would hold and the reading the map
    /// would give.
    fn fixture_log() -> Vec<Tick> {
        let gate = facts("gate-run", Some("fix/x"), &[]);
        let car = facts("ship-a-change", Some("fix/x"), &[]);
        let train = facts("pr-train", None, &[(CARRIES, "car-1")]);
        let all = [
            ("gate-1", gate.clone()),
            ("car-1", car.clone()),
            ("train-1", train.clone()),
        ];
        let tick = |head: i64, causes: Vec<Cause>, places: &[(&str, &'static str)]| Tick {
            head,
            causes,
            truncated: false,
            snapshot: snapshot(places, &all),
        };
        vec![
            tick(10, vec![], &[("gate-1", "gates")]),
            tick(
                12,
                vec![
                    cause(11, "gate-1", "jobs.job.updated"),
                    cause(12, "car-1", "jobs.job.created"),
                ],
                &[("car-1", "dock")],
            ),
            tick(
                14,
                vec![
                    cause(13, "train-1", "jobs.job.created"),
                    cause(14, "car-1", "jobs.job.updated"),
                ],
                &[("train-1", "track")],
            ),
            tick(
                16,
                vec![
                    cause(15, "train-1", "jobs.job.updated"),
                    cause(16, "car-1", "jobs.job.updated"),
                ],
                &[("train-1", "arrivals"), ("car-1", "shed")],
            ),
        ]
    }

    fn replay(ticks: Vec<Tick>) -> Vec<Reading> {
        let mut baseline: Option<Baseline> = None;
        let mut out = Vec::new();
        for tick in ticks {
            let (next, reading) = advance(baseline.as_ref(), tick);
            baseline = Some(next);
            out.push(reading);
        }
        out
    }

    /// IDEMPOTENCE AND DETERMINISM: replaying the same log twice
    /// concludes the same rows twice, and recording both replays leaves
    /// the record holding each move once.
    #[tokio::test]
    async fn replaying_a_fixture_log_twice_yields_identical_rows() {
        let first = replay(fixture_log());
        let second = replay(fixture_log());
        assert_eq!(first, second);
        assert!(
            matches!(&first[0], Reading::Resync(_)),
            "the first reading is a baseline, not a flood: {:?}",
            first[0]
        );
        let rows: Vec<Move> = first
            .iter()
            .filter_map(|r| match r {
                Reading::Moves(m) => Some(m.clone()),
                Reading::Resync(_) => None,
            })
            .flatten()
            .collect();
        let path: Vec<_> = rows
            .iter()
            .map(|m| (m.packet.as_str(), m.from.as_deref(), m.to.as_deref()))
            .collect();
        assert_eq!(
            path,
            [
                ("car-1", Some("gates"), Some("dock")),
                ("train-1", Some("dock"), Some("track")),
                ("car-1", Some("dock"), Some("track")),
                ("train-1", Some("track"), Some("arrivals")),
                ("car-1", Some("track"), Some("shed")),
            ]
        );

        let store = InMemoryMoves::new();
        assert_eq!(store.record(&rows).await.unwrap(), 5);
        let again: Vec<Move> = second
            .into_iter()
            .filter_map(|r| match r {
                Reading::Moves(m) => Some(m),
                Reading::Resync(_) => None,
            })
            .flatten()
            .collect();
        assert_eq!(
            store.record(&again).await.unwrap(),
            0,
            "a replay writes nothing twice"
        );
        let held: Vec<Move> = store
            .since(0, 100)
            .await
            .unwrap()
            .into_iter()
            .map(|r| r.r#move)
            .collect();
        assert_eq!(held, rows);
        assert_eq!(store.latest_seq().await.unwrap(), 5);
        let taken = store.crossings(t("2026-09-26T00:00:00Z")).await.unwrap();
        assert_eq!(
            taken
                .iter()
                .map(|r| (r.from.as_deref(), r.to.as_deref(), r.moves))
                .collect::<Vec<_>>(),
            [
                (Some("dock"), Some("track"), 2),
                (Some("gates"), Some("dock"), 1),
                (Some("track"), Some("arrivals"), 1),
                (Some("track"), Some("shed"), 1),
            ],
            "every route the fixture took, counted, whether declared or not"
        );
    }

    /// AN EXIT NAMES THE TERMINAL IT LEFT BY (backlog e23005c0). The
    /// design exhibit f0313eda had to read every exiting packet's
    /// outcome step to say which terminal each of 1105 moves left by;
    /// the record now carries it. The diff itself reads no steps — a
    /// closed row often arrives without them — so the mover describes
    /// each exit from the packet's own row read back, and a packet it
    /// could not read keeps `terminal: None` (unjudged by terminal,
    /// never a guessed one). A move between regions names none, and a
    /// row the reading held no facts for (an ops-request, placed by its
    /// station) takes its kind from the same read.
    #[tokio::test]
    async fn an_exit_names_the_terminal_it_left_by_and_is_counted_by_it() {
        let run = facts("agent-run", None, &[]);
        let prev = snapshot(
            &[
                ("run-1", "shop-floor"),
                ("run-2", "shop-floor"),
                ("ops-1", "marshalling"),
            ],
            &[("run-1", run.clone()), ("run-2", run.clone())],
        );
        let next = snapshot(&[("run-2", "marshalling")], &[("run-2", run)]);
        let found = moves(
            &prev,
            &next,
            &[
                cause(1, "run-1", "jobs.job.closed"),
                cause(2, "run-2", "jobs.job.updated"),
                cause(3, "ops-1", "jobs.job.closed"),
            ],
        );
        let read: BTreeMap<String, Described> = [
            ("run-1", "agent-run", Some("landed")),
            ("run-2", "agent-run", None),
            ("ops-1", "ops-request", Some("answered")),
        ]
        .into_iter()
        .map(|(p, k, t)| {
            (
                p.to_string(),
                Described {
                    kind: k.into(),
                    terminal: t.map(str::to_string),
                },
            )
        })
        .collect();
        let out = described(found, &read);
        let rows: Vec<_> = out
            .iter()
            .map(|m| {
                (
                    m.packet.as_str(),
                    m.kind.as_str(),
                    m.from.as_deref(),
                    m.to.as_deref(),
                    m.terminal.as_deref(),
                )
            })
            .collect();
        assert_eq!(
            rows,
            [
                (
                    "run-1",
                    "agent-run",
                    Some("shop-floor"),
                    None,
                    Some("landed")
                ),
                (
                    "run-2",
                    "agent-run",
                    Some("shop-floor"),
                    Some("marshalling"),
                    None
                ),
                (
                    "ops-1",
                    "ops-request",
                    Some("marshalling"),
                    None,
                    Some("answered")
                ),
            ]
        );
        assert_eq!(
            described_ids(&out),
            Vec::<String>::new(),
            "every row is described once it has been"
        );
        let store = InMemoryMoves::new();
        store.record(&out).await.unwrap();
        let counted = store.crossings(t("2026-09-26T00:00:00Z")).await.unwrap();
        assert_eq!(
            counted
                .iter()
                .map(|c| (
                    c.from.as_deref(),
                    c.to.as_deref(),
                    c.kind.as_deref(),
                    c.terminal.as_deref(),
                    c.moves
                ))
                .collect::<Vec<_>>(),
            [
                (
                    Some("marshalling"),
                    None,
                    Some("ops-request"),
                    Some("answered"),
                    1
                ),
                (
                    Some("shop-floor"),
                    None,
                    Some("agent-run"),
                    Some("landed"),
                    1
                ),
                (Some("shop-floor"), Some("marshalling"), None, None, 1),
            ],
            "an exit is counted per kind and terminal; a move between regions by its route"
        );
    }

    /// WHICH STEP IS THE TERMINAL is read off the protocol the packet
    /// runs under, not the step kind: page-audit's `styled` is an
    /// `outcome` step completed on the way to the close and is no
    /// terminal, so a closed page-audit names the terminal it closed on,
    /// never its marker. An open packet names none.
    #[test]
    fn the_terminal_a_packet_closed_on_is_read_off_its_protocol() {
        use boss_core::job::{JobId, Step, StepStatus};
        let specs = crate::seed_loader::load_workflows(crate::registry::platform_bundle_path())
            .expect("the platform bundle parses");
        let audit = specs
            .iter()
            .find(|s| s.kind == "page-audit")
            .expect("the tree has page-audit");
        let marker = audit
            .steps
            .iter()
            .position(|s| s.title == "styled" && s.terminal.is_none())
            .expect("page-audit's styled is a marker, not a terminal");
        let (end, terminal) = audit
            .steps
            .iter()
            .enumerate()
            .find(|(_, s)| s.terminal.is_some())
            .map(|(i, s)| (i, s.title.clone()))
            .expect("page-audit has a terminal");
        let job = JobId::new();
        let at = |i: usize, status: StepStatus| {
            let spec = &audit.steps[i];
            let mut s = Step::new(job, spec.kind.as_str(), spec.title.as_str(), i as i32);
            s.spec_slug = Some(spec.title.clone());
            s.status = status;
            s
        };
        let closed = [
            at(marker, StepStatus::Completed),
            at(end, StepStatus::Completed),
        ];
        assert_eq!(terminal_of(audit, &closed), Some(terminal));
        let open = [
            at(marker, StepStatus::Completed),
            at(end, StepStatus::Ready),
        ];
        assert_eq!(terminal_of(audit, &open), None, "an open packet has none");
    }

    /// CONSERVATION PER REGION over a replayed log (the correctness
    /// protocol's second property): for every region, what stood there
    /// at the first reading plus every move into it equals what stands
    /// there at the last plus every move out of it — exits included, so
    /// a packet that leaves the map leaves by a recorded row, and one
    /// counted in is counted out. The log is the shape of the exhibit's
    /// exits: a green handed off to its car, a run landing off the shop
    /// floor, a request answered out of marshalling, the car held.
    #[test]
    fn every_region_conserves_its_packets_over_a_replayed_log() {
        let gate = facts("gate-run", Some("fix/x"), &[]);
        let car = facts("ship-a-change", Some("fix/x"), &[]);
        let run = facts("agent-run", None, &[]);
        let request = facts("ops-request", None, &[]);
        let all = [
            ("gate-1", gate),
            ("car-1", car),
            ("run-1", run),
            ("ops-1", request),
        ];
        let tick = |head: i64, causes: Vec<Cause>, places: &[(&str, &'static str)]| Tick {
            head,
            causes,
            truncated: false,
            snapshot: snapshot(places, &all),
        };
        let log = vec![
            tick(
                10,
                vec![],
                &[
                    ("gate-1", "gates"),
                    ("run-1", "shop-floor"),
                    ("ops-1", "marshalling"),
                ],
            ),
            tick(
                14,
                vec![
                    cause(11, "gate-1", "jobs.job.closed"),
                    cause(12, "car-1", "jobs.job.created"),
                    cause(13, "run-1", "jobs.job.closed"),
                    cause(14, "ops-1", "jobs.job.closed"),
                ],
                &[("car-1", "dock")],
            ),
            tick(
                15,
                vec![cause(15, "car-1", "jobs.step.updated")],
                &[("car-1", "garage")],
            ),
        ];
        let first = log[0].snapshot.clone();
        let last = log[log.len() - 1].snapshot.clone();
        let rows: Vec<Move> = replay(log)
            .into_iter()
            .filter_map(|r| match r {
                Reading::Moves(m) => Some(m),
                Reading::Resync(_) => None,
            })
            .flatten()
            .collect();
        assert_eq!(
            rows.iter().filter(|m| m.to.is_none()).count(),
            2,
            "the run and the request exit; the green is handed off: {rows:#?}"
        );
        assert_conserved(&first, &last, &rows);
    }

    /// How many packets stand in `region` on one reading, COUNTING
    /// CARRIAGE: every packet placed there, and every open packet riding
    /// a carrier placed there — a car aboard its train is in no region of
    /// its own, and stands where the train stands (backlog 47880c86).
    fn standing(s: &Snapshot, region: &str) -> usize {
        let placed = s.places.values().filter(|r| **r == region).count();
        let aboard = s
            .places
            .iter()
            .filter(|(_, r)| **r == region)
            .filter_map(|(q, _)| s.facts.get(q))
            .flat_map(Facts::carried)
            .filter(|c| !s.places.contains_key(*c))
            .filter(|c| s.facts.get(*c).is_some_and(|f| !f.closed))
            .count();
        placed + aboard
    }

    /// CONSERVATION PER REGION (the correctness protocol's second
    /// property): for every region, what stood there at the first reading
    /// plus every move into it equals what stands there at the last plus
    /// every move out of it ([`Move::leaves`]), carriage counted
    /// explicitly ([`standing`]).
    fn assert_conserved(first: &Snapshot, last: &Snapshot, rows: &[Move]) {
        let regions: BTreeSet<&str> = first
            .places
            .values()
            .chain(last.places.values())
            .copied()
            .chain(rows.iter().filter_map(|m| m.from.as_deref()))
            .chain(rows.iter().filter_map(|m| m.to.as_deref()))
            .collect();
        for region in regions {
            let stood = standing(first, region);
            let stands = standing(last, region);
            let came = rows
                .iter()
                .filter(|m| m.to.as_deref() == Some(region))
                .count();
            let left = rows.iter().filter(|m| m.leaves() == Some(region)).count();
            assert_eq!(
                stood + came,
                stands + left,
                "{region}: {stood} stood + {came} in != {stands} stand + {left} out: {rows:#?}"
            );
        }
    }

    fn moved(readings: Vec<Reading>) -> Vec<Move> {
        readings
            .into_iter()
            .filter_map(|r| match r {
                Reading::Moves(m) => Some(m),
                Reading::Resync(_) => None,
            })
            .flatten()
            .collect()
    }

    /// CONSERVATION PER REGION, WITH TRAINS (backlog 47880c86). Three cars
    /// on the dock; one train made up of two of them departs for the gates
    /// in the same tick, a second is made up of the third and stays. The
    /// first merges — its cars peel off to the shed at the gates while it
    /// departs onto the track — and the second is cancelled, setting its
    /// car down on the dock where it stood. The first arrives; one of its
    /// cars is proven and closes on `merged`.
    ///
    /// Carriage is what makes this hard, and it is counted explicitly: a
    /// car aboard stands where its train stands, so a car set down where
    /// its train stood did not move; and a train made up and departed in
    /// one tick is drawn from the dock it was made up on, but it never
    /// stood there — its cars did, and their own rows carry them — so it
    /// leaves nothing (`Move::leaves`). Before this car the dock counted
    /// that departure twice, a car set down on the dock was drawn filed
    /// from nowhere, and a car closing in the shed rode the arrived train
    /// that still named it into arrivals (66 on 2026-09-26).
    #[test]
    fn every_region_conserves_its_packets_with_trains_carrying_cars() {
        let car = |b: &str| facts("ship-a-change", Some(b), &[]);
        let train = |cars: &[&str]| {
            facts(
                "pr-train",
                None,
                &cars.iter().map(|c| (CARRIES, *c)).collect::<Vec<_>>(),
            )
        };
        let cars = [
            ("car-1", car("fix/1")),
            ("car-2", car("fix/2")),
            ("car-3", car("fix/3")),
        ];
        let with = |extra: &[(&str, Facts)]| -> Vec<(String, Facts)> {
            cars.iter()
                .map(|(p, f)| ((*p).to_string(), f.clone()))
                .filter(|(p, _)| !extra.iter().any(|(q, _)| q == p))
                .chain(extra.iter().map(|(p, f)| ((*p).to_string(), f.clone())))
                .collect()
        };
        let tick = |head: i64,
                    causes: Vec<Cause>,
                    places: &[(&str, &'static str)],
                    known: Vec<(String, Facts)>| {
            let known: Vec<(&str, Facts)> =
                known.iter().map(|(p, f)| (p.as_str(), f.clone())).collect();
            Tick {
                head,
                causes,
                truncated: false,
                snapshot: snapshot(places, &known),
            }
        };
        let trains = |one: Facts, two: Facts| [("train-1", one), ("train-2", two)];
        let open = trains(train(&["car-1", "car-2"]), train(&["car-3"]));
        let log = vec![
            tick(
                10,
                vec![],
                &[("car-1", "dock"), ("car-2", "dock"), ("car-3", "dock")],
                with(&[]),
            ),
            tick(
                15,
                vec![
                    cause(11, "train-1", "jobs.job.created"),
                    cause(12, "car-1", "jobs.job.updated"),
                    cause(13, "car-2", "jobs.job.updated"),
                    cause(14, "train-2", "jobs.job.created"),
                    cause(15, "car-3", "jobs.job.updated"),
                ],
                &[("train-1", "gates"), ("train-2", "dock")],
                with(&open),
            ),
            tick(
                20,
                vec![
                    cause(16, "car-1", "step.assigned.task"),
                    cause(17, "car-2", "step.assigned.task"),
                    cause(18, "train-1", "step.ready.task"),
                    cause(19, "train-2", "jobs.job.closed"),
                    cause(20, "car-3", "jobs.job.updated"),
                ],
                &[
                    ("train-1", "track"),
                    ("car-1", "shed"),
                    ("car-2", "shed"),
                    ("car-3", "dock"),
                ],
                with(&trains(open[0].1.clone(), closed(open[1].1.clone()))),
            ),
            tick(
                21,
                vec![cause(21, "train-1", "jobs.job.closed")],
                &[
                    ("train-1", "arrivals"),
                    ("car-1", "shed"),
                    ("car-2", "shed"),
                    ("car-3", "dock"),
                ],
                with(&trains(
                    closed(open[0].1.clone()),
                    closed(open[1].1.clone()),
                )),
            ),
            tick(
                22,
                vec![cause(22, "car-1", "jobs.job.closed")],
                &[
                    ("train-1", "arrivals"),
                    ("car-2", "shed"),
                    ("car-3", "dock"),
                ],
                with(&[
                    ("car-1", closed(car("fix/1"))),
                    ("train-1", closed(open[0].1.clone())),
                    ("train-2", closed(open[1].1.clone())),
                ]),
            ),
        ];
        let first = log[0].snapshot.clone();
        let last = log[log.len() - 1].snapshot.clone();
        let rows = moved(replay(log));
        let path: Vec<_> = rows
            .iter()
            .map(|m| (m.packet.as_str(), m.from.as_deref(), m.to.as_deref()))
            .collect();
        assert_eq!(
            path,
            [
                ("train-1", Some("dock"), Some("gates")),
                ("car-1", Some("dock"), Some("gates")),
                ("car-2", Some("dock"), Some("gates")),
                ("train-2", None, Some("dock")),
                ("car-1", Some("gates"), Some("shed")),
                ("car-2", Some("gates"), Some("shed")),
                ("train-1", Some("gates"), Some("track")),
                ("train-2", Some("dock"), None),
                ("train-1", Some("track"), Some("arrivals")),
                ("car-1", Some("shed"), None),
            ]
        );
        assert_eq!(
            rows[0].leaves(),
            None,
            "a train made up and departed in one tick never stood on the dock"
        );
        assert_eq!(rows[1].leaves(), Some("dock"), "its cars did");
        assert_conserved(&first, &last, &rows);
    }

    /// A CAR THAT CLOSES IN THE SHED LEAVES BY ITS TERMINAL (backlog
    /// 47880c86): its arrived train still names it in `boarded_jobs`, but
    /// a closed packet rides nothing. The mover drew 66 such closes on
    /// 2026-09-26 as shed -> arrivals, a route no packet takes; the exit
    /// is shed -> off the map, and the mover names its terminal.
    #[test]
    fn a_car_that_closes_in_the_shed_leaves_by_its_terminal_not_aboard_its_train() {
        let car = facts("ship-a-change", Some("fix/x"), &[]);
        let train = closed(facts("pr-train", None, &[(CARRIES, "car-1")]));
        let prev = snapshot(
            &[("car-1", "shed"), ("train-1", "arrivals")],
            &[("car-1", car.clone()), ("train-1", train.clone())],
        );
        let next = snapshot(
            &[("train-1", "arrivals")],
            &[("car-1", closed(car)), ("train-1", train)],
        );
        let out = moves(&prev, &next, &[cause(1, "car-1", "jobs.job.closed")]);
        let rows: Vec<_> = out
            .iter()
            .map(|m| {
                (
                    m.packet.as_str(),
                    m.from.as_deref(),
                    m.to.as_deref(),
                    m.lineage.as_deref(),
                )
            })
            .collect();
        assert_eq!(rows, [("car-1", Some("shed"), None, None)]);
        assert_eq!(described_ids(&out), ["car-1"], "its terminal is read back");
    }

    /// A HAND-OFF WHERE THE PACKET STOOD IS NO MOVE (backlog 47880c86): a
    /// green gate-run the reading saw stranded in the garage, handed off
    /// to the car it filed held — which stands in the garage too. Nothing
    /// crossed a border. The mover drew it as a car filed onto the garage
    /// from nowhere (1 on 2026-09-26, car 3e4bd1cd) while the green's own
    /// departure was folded into it, so the garage gained a packet it
    /// never lost.
    #[test]
    fn a_hand_off_where_the_packet_stood_is_no_move() {
        let gate = facts("gate-run", Some("fix/x"), &[]);
        let car = facts("ship-a-change", Some("fix/x"), &[]);
        let prev = snapshot(&[("gate-1", "garage")], &[("gate-1", gate.clone())]);
        let next = snapshot(
            &[("car-1", "garage")],
            &[("gate-1", closed(gate)), ("car-1", car)],
        );
        let out = moves(
            &prev,
            &next,
            &[
                cause(1, "gate-1", "jobs.job.updated"),
                cause(2, "car-1", "jobs.job.created"),
            ],
        );
        assert!(out.is_empty(), "{out:#?}");
        assert_conserved(&prev, &next, &out);
    }

    /// A log that moved BACKWARDS (an epoch trim) and a batch past
    /// [`MAX_BATCH`] are gaps: the reading becomes the baseline and no
    /// move is guessed across it.
    #[test]
    fn a_log_that_moved_back_or_overflowed_resyncs_rather_than_guessing() {
        let log = fixture_log();
        let (base, _) = advance(None, log[0].clone());
        let mut back = log[1].clone();
        back.head = 5;
        let (after, reading) = advance(Some(&base), back);
        assert!(
            matches!(&reading, Reading::Resync(why) if why.contains("moved back from 10 to 5")),
            "{reading:?}"
        );
        assert_eq!(after.cursor, 5);
        let mut flood = log[1].clone();
        flood.truncated = true;
        let (_, reading) = advance(Some(&base), flood);
        assert!(
            matches!(&reading, Reading::Resync(why) if why.contains("more than 5000 events")),
            "{reading:?}"
        );
    }

    /// An unread region's packets keep their place in the baseline, so
    /// the move is judged when the region reads again.
    #[test]
    fn an_unread_region_carries_its_packets_to_the_next_reading() {
        let car = facts("ship-a-change", Some("fix/x"), &[]);
        let base = Baseline {
            cursor: 1,
            snapshot: snapshot(&[("car-1", "dock")], &[("car-1", car.clone())]),
        };
        let mut dark = snapshot(&[], &[]);
        dark.unread = vec!["dock"];
        let (held, reading) = advance(
            Some(&base),
            Tick {
                head: 2,
                causes: vec![cause(2, "car-1", "jobs.job.updated")],
                truncated: false,
                snapshot: dark,
            },
        );
        assert_eq!(reading, Reading::Moves(vec![]));
        assert_eq!(held.snapshot.place("car-1"), Where::At("dock"));
        let (_, reading) = advance(
            Some(&held),
            Tick {
                head: 3,
                causes: vec![cause(3, "car-1", "jobs.job.updated")],
                truncated: false,
                snapshot: snapshot(&[("car-1", "garage")], &[("car-1", car)]),
            },
        );
        let Reading::Moves(rows) = reading else {
            panic!("a read region is judged: {reading:?}")
        };
        assert_eq!(
            rows.iter()
                .map(|m| (m.from.as_deref(), m.to.as_deref()))
                .collect::<Vec<_>>(),
            [(Some("dock"), Some("garage"))]
        );
    }

    /// WHILE A REGION IS UNREAD, a packet in no read region might be in
    /// it: no move is written for it, in either direction.
    #[test]
    fn a_packet_that_might_be_in_an_unread_region_does_not_move() {
        let car = facts("ship-a-change", Some("fix/x"), &[]);
        let prev = snapshot(&[("car-1", "dock")], &[("car-1", car.clone())]);
        let mut next = snapshot(&[], &[("car-1", car)]);
        next.unread = vec!["dock"];
        assert_eq!(next.place("car-1"), Where::Unknown);
        assert!(moves(&prev, &next, &[cause(1, "car-1", "jobs.job.updated")]).is_empty());
    }
}
