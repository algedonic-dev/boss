//! THE PARTITION (design 62de32ae decision 4, decided 2026-09-24): every
//! packet a region counts, it counts ALONE. The eight regions whose own
//! predicates name what they hold claim first ([`claimed`]); receiving
//! and marshalling are the remainder ([`members`]).
//!
//! ONE PLACEMENT FUNCTION (design e765b3fc §2a, car R1). Which region
//! holds a packet is decided packet by packet by [`place`], over the
//! lookups one reading builds ([`Lookups`]); [`members`] is a fold of
//! `place` over every packet the reading names. So what a region counts
//! and where a packet stands are one answer — the counts, the rails and
//! the moves record (`crate::moves`) cannot disagree (CLAUDE.md §9a).
//!
//! A TRAIN IS PLACED BY ITS ACTIVE STEP, not all on the track: made up
//! at the dock, at the gates while its train gate runs, on the track once
//! it has merged ([`train_region`]). David, on feedback 84cba7e2: "like
//! how the dock routes a train over to the gates before it departs onto
//! the tracks" — and that is what the conductor does. What a train
//! carries rides it ([`Placed::Aboard`]): the cars it boarded, and its
//! own train gate while it stands at the gates, which is the train under
//! test and holds its bay as the train, counted once.

use super::*;

/// Where ONE packet stands on one reading.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Placed {
    /// In a region of the map, which counts it.
    At(&'static str),
    /// Riding the open train with this id — a car it boarded, or its own
    /// train gate while the train stands at the gates. In no region of
    /// its own: the train is counted, what it carries is not.
    Aboard(String),
    /// On no region of the map this reading drew.
    Off,
}

/// WHERE A TRAIN STANDS, by the step it is at — read off the yard's own
/// phase ([`crate::yard::TrainPhase`], the one reading of a train's
/// steps), matched exhaustively so a new phase cannot go unplaced:
///
/// | phase | the train is | region |
/// |---|---|---|
/// | boarding (`collect`, `assemble`, `pr`) | being made up | dock |
/// | awaiting CI, awaiting the merge (`ci`) | under its train gate | gates |
/// | deploying, converging (`merged` done) | in transit | track |
///
/// An open train whose arrival step has completed is still in transit
/// until it closes; a closed one is in arrivals (the window's).
///
/// The track is [`crate::yard::TrainPhase::in_transit`] — the one
/// definition `boss orient`'s IN TRANSIT reads too (backlog 3eddffc4) —
/// and the match stays exhaustive so a new phase cannot go unplaced.
pub fn train_region(phase: crate::yard::TrainPhase) -> &'static str {
    use crate::yard::TrainPhase::*;
    if phase.in_transit() {
        return "track";
    }
    match phase {
        Boarding => "dock",
        AwaitingCi | AwaitingMerge => "gates",
        Deploying | Converging | Arrived => "track",
    }
}

/// The step kind a protocol's admission step carries. A trigger
/// completes when the packet is admitted, so completing it takes
/// nothing in ([`taken_in`]).
pub const TRIGGER_STEP_KIND: &str = "trigger";

/// Whether a packet has been TAKEN IN: its intake step completed.
///
/// THE INTAKE STEP IS THE FIRST STEP AFTER THE TRIGGER, read off the
/// step kinds rather than named per kind (a `match` on kinds is the
/// registry the protocols already are): `triage` on a backlog-item or a
/// user-feedback, `decide` on an approval, whatever step a protocol put
/// first. The trigger completes at admission, so it takes nothing in;
/// any OTHER step completed means an actor has picked the packet up, and
/// from then on it is queued work or work in a region of its own — never
/// inbound again. A skipped step is not a completed one.
pub fn taken_in(steps: &[Step]) -> bool {
    steps
        .iter()
        .any(|s| s.status == StepStatus::Completed && s.kind != TRIGGER_STEP_KIND)
}

/// WHEN a packet was taken in: the earliest completion among the steps
/// [`taken_in`] counts — the instant it crossed from receiving into
/// marshalling, which is what the border between the two counts
/// (`borders`, design 62de32ae). `None` for a packet nothing has taken
/// in, and for one whose completion carries no stamp: an instant nobody
/// recorded is not invented.
pub(crate) fn taken_in_at(steps: &[Step]) -> Option<Instant> {
    steps
        .iter()
        .filter(|s| s.status == StepStatus::Completed && s.kind != TRIGGER_STEP_KIND)
        .filter_map(|s| s.completed_at)
        .min()
}

/// Packet ids, ordered so a test that names an offender names it the
/// same way every run.
pub type Ids = std::collections::BTreeSet<String>;

/// Why marshalling cannot be counted when the stations were read but the
/// inbound packets were not: which of the stations' packets are still in
/// receiving is exactly what that read decides, and a count that
/// included them would be the double count this partition ends.
const MARSHALLING_NEEDS_INBOUND: &str = "the inbound packets could not be read, so which of the \
     stations' packets are still in receiving cannot be told";

/// The eight regions whose OWN predicates name what they hold — a car on
/// the dock or in the shed, a gate-run in a bay or the garage, a train at
/// the dock, the gates or on the track ([`train_region`]) or arrived, a
/// gate-run in line for a bay ([`at_the_gates`]), a
/// run on the shop floor, a pull request on the mirror — each as the ids
/// its predicate names, in claim order. `None` exactly where the region's
/// count is `None`: an unread region claims nothing, and says so on its
/// own card. A train's own gate, while the train stands at the gates, is
/// not a gate-run the gates claim: it rides the train ([`carried`]).
fn claimed(inputs: &RegionInputs<'_>) -> [(&'static str, Option<Ids>); 8] {
    fn ids_of<'j>(rows: impl Iterator<Item = &'j Job>) -> Ids {
        rows.map(|j| j.id.to_string()).collect()
    }
    let s = inputs.status;
    let w = Windows::of(inputs.now, inputs.window_hours);
    let trains_at = |region: &str| -> Ids {
        s.trains
            .iter()
            .filter(|t| train_region(t.phase) == region)
            .map(|t| t.id.clone())
            .collect()
    };
    let under_test = trains_at("gates");
    let garage: Ids = s
        .held_cars
        .iter()
        .map(|h| h.car.id.clone())
        .chain(s.held.iter().map(|g| g.packet_id.clone()))
        .chain(s.stranded.iter().map(|g| g.packet_id.clone()))
        .chain(s.limbo.iter().map(|l| l.packet_id.clone()))
        .chain(s.garage.iter().map(|g| g.packet_id.clone()))
        .collect();
    [
        (
            "dock",
            (inputs.dock_reading == Reading::Read).then(|| {
                s.dock
                    .iter()
                    .map(|d| d.id.clone())
                    .chain(trains_at("dock"))
                    .collect()
            }),
        ),
        (
            "gates",
            Some(
                s.gates
                    .active
                    .iter()
                    .filter(|g| !g.train.as_ref().is_some_and(|t| under_test.contains(t)))
                    .map(|g| g.packet_id.clone())
                    // THE LINE STANDS AT THE GATES (backlog 357fe1da, design
                    // 85c3f793 decision 1): a run waiting for a bay is drawn
                    // at the station it waits for, as a train waiting for a
                    // platform is. Until then it stood on no region, and a
                    // run that died in line was first seen in the garage.
                    .chain(
                        s.gates
                            .queued
                            .iter()
                            .filter(|q| !q.train.as_ref().is_some_and(|t| under_test.contains(t)))
                            .map(|q| q.packet_id.clone()),
                    )
                    .chain(under_test.iter().cloned())
                    .collect(),
            ),
        ),
        ("track", Some(trains_at("track"))),
        (
            "shed",
            Some(ids_of(
                awaiting_proof(inputs.cars).into_iter().map(|(j, _)| j),
            )),
        ),
        (
            "arrivals",
            Some(ids_of(
                inputs
                    .closed_trains
                    .iter()
                    .map(|(j, _)| j)
                    .filter(|j| {
                        j.metadata.get("outcome").and_then(Value::as_str) == Some("arrived")
                    })
                    .filter(|j| closed_at(j).is_some_and(|t| w.current(t))),
            )),
        ),
        ("garage", Some(garage)),
        (
            "shop-floor",
            inputs.agent_runs.map(|runs| {
                ids_of(
                    runs.iter()
                        .map(|(j, _)| j)
                        .filter(|j| j.status == JobStatus::Open),
                )
            }),
        ),
        (
            "publish",
            inputs.publish_packets.map(|packets| {
                publish_prs(packets)
                    .into_iter()
                    .filter(PublishPr::awaiting)
                    .map(|p| p.packet_id)
                    .collect()
            }),
        ),
    ]
}

/// WHAT EACH OPEN TRAIN CARRIES, packet -> train: the cars it names in
/// `boarded_jobs`, and — while the train stands at the gates — its own
/// train gate, which is the train under test rather than a second packet
/// in the bay.
fn carried(inputs: &RegionInputs<'_>) -> std::collections::BTreeMap<String, String> {
    let s = inputs.status;
    let at_gates = |train: &str| {
        s.trains
            .iter()
            .any(|t| t.id == train && train_region(t.phase) == "gates")
    };
    let cars = inputs.open_trains.iter().flat_map(|(train, _)| {
        let id = train.id.to_string();
        train
            .metadata
            .get("boarded_jobs")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
            .map(move |car| (car.to_string(), id.clone()))
    });
    let in_bays = s.gates.active.iter().map(|g| (&g.packet_id, &g.train));
    // A train gate still in line rides its train the same way (backlog
    // 357fe1da): the train under test stands in the line, counted once.
    let in_line = s.gates.queued.iter().map(|q| (&q.packet_id, &q.train));
    let gates = in_bays.chain(in_line).filter_map(|(packet, train)| {
        let train = train.as_ref().filter(|t| at_gates(t))?;
        Some((packet.clone(), train.clone()))
    });
    cars.chain(gates).collect()
}

/// WHERE AT THE GATES each packet [`place`] puts there stands — three
/// disjoint sets whose union is the region's members, so `count +
/// waiting + judged` is the partition by construction (backlog bce67f09).
#[derive(Debug, Default)]
pub(crate) struct AtTheGates {
    /// In a bay: a gate-run running, or a train whose own gate runs.
    pub bays: Ids,
    /// THE LINE (backlog 357fe1da, design 85c3f793 decision 2): a
    /// gate-run waiting for a bay, a train whose own gate waits for one,
    /// and a train under test with NO gate-run yet. The region's second
    /// figure, `waiting` — never part of its count of bays in use.
    pub line: Ids,
    /// THE JUDGED (backlog bce67f09): a train whose own gate has already
    /// spoken — green and awaiting forge CI or the merge, or red and
    /// blocked. It needs no bay and waits in no line, so until this set it
    /// stood in the line and read "0 bays in use, 1 waiting" while the
    /// "N waiting for a slot" text, which counts queued runs only, said
    /// none. The region's third figure, `judged`.
    pub judged: Ids,
}

/// The open trains whose own gate has spoken: the `ci` step holds a
/// verdict (the yard's [`crate::yard::TrainPhase::AwaitingMerge`]), or a
/// train gate of theirs closed `completed` or `failed`, which is the gate
/// green while forge CI is still out. A gate closed `lost` produced no
/// verdict, so its train waits for the next one, in line.
fn judged_trains(inputs: &RegionInputs<'_>) -> Ids {
    let by_ci = inputs
        .status
        .trains
        .iter()
        .filter(|t| t.phase == crate::yard::TrainPhase::AwaitingMerge)
        .map(|t| t.id.clone());
    let by_gate = inputs
        .gate_runs
        .iter()
        .filter(|g| g.status != JobStatus::Open)
        .filter(|g| {
            matches!(
                g.metadata.get("outcome").and_then(Value::as_str),
                Some("completed" | "failed")
            )
        })
        .filter_map(crate::yard::train_of_gate);
    by_ci.chain(by_gate).collect()
}

/// The gates' members, split into [`AtTheGates`]. ONE build of the
/// partition for the region's three figures — it used to be two, one for
/// the line and one for the count (the cost named on backlog bce67f09).
/// A train whose gate is in a bay or in line stands there whatever an
/// earlier gate of its said: a re-gate is the train under test again.
/// `None` where the gates were not read.
pub(crate) fn at_the_gates(inputs: &RegionInputs<'_>) -> Option<AtTheGates> {
    let g = &inputs.status.gates;
    let in_a_bay: Ids = g
        .active
        .iter()
        .flat_map(|a| std::iter::once(&a.packet_id).chain(a.train.as_ref()))
        .cloned()
        .collect();
    let gate_in_line: Ids = g.queued.iter().filter_map(|q| q.train.clone()).collect();
    let judged = judged_trains(inputs);
    let ids = members(inputs)
        .into_iter()
        .find(|(name, _)| *name == "gates")
        .and_then(|(_, ids)| ids)?;
    let (bays, rest): (Ids, Ids) = ids.into_iter().partition(|id| in_a_bay.contains(id));
    let (judged, line): (Ids, Ids) = rest
        .into_iter()
        .partition(|id| judged.contains(id) && !gate_in_line.contains(id));
    Some(AtTheGates { bays, line, judged })
}

/// WHAT [`place`] READS: everything one reading says about which packet
/// could stand where, built once per reading.
pub struct Lookups {
    /// The eight regions of their own, in claim order ([`claimed`]).
    claims: [(&'static str, Option<Ids>); 8],
    /// What the open trains carry ([`carried`]).
    aboard: std::collections::BTreeMap<String, String>,
    /// The open inbound packets nothing has taken in ([`taken_in`]).
    /// `None` when the inbound read failed.
    untaken: Option<Ids>,
    /// Every packet standing at a station. `None` when the station
    /// registry or the inbound read failed — without the second, which
    /// station packets are still in receiving cannot be told.
    stationed: Option<Ids>,
}

impl Lookups {
    pub fn of(inputs: &RegionInputs<'_>) -> Self {
        let untaken: Option<Ids> = inputs.inbound.map(|inbound| {
            inbound
                .iter()
                .filter(|(j, steps)| j.status == JobStatus::Open && !taken_in(steps))
                .map(|(j, _)| j.id.to_string())
                .collect()
        });
        // THE NEWER READ WINS (backlog e23005c0). The stations were judged
        // over the open packets read FIRST; every other row was read
        // after. A packet one of those later reads holds closed has closed
        // in between, and a closed packet stands at no station — left in,
        // the remainder took it, and a run that landed mid-read left the
        // map from marshalling instead of the shop floor (19 of 20 such
        // exits, measured 2026-09-26).
        let closed: Ids = inputs
            .open_trains
            .iter()
            .chain(inputs.closed_trains)
            .chain(inputs.cars)
            .chain(inputs.inbound.unwrap_or_default())
            .chain(inputs.agent_runs.unwrap_or_default())
            .chain(inputs.ops_requests.unwrap_or_default())
            .chain(inputs.publish_packets.unwrap_or_default())
            .map(|(j, _)| j)
            .chain(inputs.gate_runs)
            .filter(|j| j.status != JobStatus::Open)
            .map(|j| j.id.to_string())
            .collect();
        let stationed = inputs.stations.filter(|_| untaken.is_some()).map(|st| {
            st.iter()
                .flat_map(|s| s.members.iter())
                .filter(|m| !closed.contains(*m))
                .cloned()
                .collect()
        });
        Lookups {
            claims: claimed(inputs),
            aboard: carried(inputs),
            untaken,
            stationed,
        }
    }

    /// Whether this territory's packet lookup answered. `None` means it
    /// holds no packets (sensors measures readings); `Some(false)` is an
    /// unread packet lookup. The mover must not collapse them (814e6c66).
    pub fn packet_read(&self, region: &str) -> Option<bool> {
        match region {
            "receiving" => Some(self.untaken.is_some()),
            "marshalling" => Some(self.stationed.is_some()),
            _ => self
                .claims
                .iter()
                .find(|(name, _)| *name == region)
                .map(|(_, ids)| ids.is_some()),
        }
    }

    /// Whether `region` was read at all: `None` in [`members`] otherwise.
    fn read(&self, region: &str) -> bool {
        self.packet_read(region).unwrap_or(false)
    }

    /// Carriage that the placement actually chooses, retaining its exact
    /// carrier id even when no separate cargo facts row was read.
    pub fn carriage(&self) -> std::collections::BTreeMap<String, String> {
        self.aboard
            .iter()
            .filter(|(packet, _)| matches!(place(packet, self), Placed::Aboard(_)))
            .map(|(packet, carrier)| (packet.clone(), carrier.clone()))
            .collect()
    }

    /// Every packet any lookup names.
    fn packets(&self) -> Ids {
        self.claims
            .iter()
            .filter_map(|(_, ids)| ids.as_ref())
            .flatten()
            .chain(self.aboard.keys())
            .chain(self.untaken.iter().flatten())
            .chain(self.stationed.iter().flatten())
            .cloned()
            .collect()
    }
}

/// WHERE ONE PACKET STANDS — the placement function (design e765b3fc
/// §2a, car R1).
///
/// ORDER IS THE RULE. The eight regions of [`claimed`] are defined by
/// predicates of their own and claim their packets first, in map order;
/// then a train's cargo rides the train; then the upstream pair takes
/// the remainder. RECEIVING takes an open inbound packet not yet taken in
/// that nothing else holds; MARSHALLING takes what stands at a station
/// that neither receiving nor any other region holds. A packet matching
/// five predicates still stands in one place.
///
/// A region of its own wins because its predicate is the specific one:
/// an `agent-run` is an inbound kind by the registry's rule, and until
/// its `briefed` step completes it has taken nothing in — but it is a run
/// in flight, and the shop floor counts it. And a station's predicate
/// matches a STEP: a ready task lands on `q.platform-admin.task` whether
/// it is an untriaged item's `triage`, a triaged one's `build`, a landed
/// car's `proven` or a run's `briefed`; only the second is marshalling's.
pub fn place(packet: &str, lookups: &Lookups) -> Placed {
    let holds = |ids: &Option<Ids>| ids.as_ref().is_some_and(|ids| ids.contains(packet));
    if let Some((region, _)) = lookups.claims.iter().find(|(_, ids)| holds(ids)) {
        return Placed::At(region);
    }
    if let Some(train) = lookups.aboard.get(packet) {
        return Placed::Aboard(train.clone());
    }
    if holds(&lookups.untaken) {
        return Placed::At("receiving");
    }
    if holds(&lookups.stationed) {
        return Placed::At("marshalling");
    }
    Placed::Off
}

/// RECEIVING's packets: the open inbound packets [`place`] puts there.
/// `None` when the inbound read failed.
pub(crate) fn receiving_standing<'a>(inputs: &RegionInputs<'a>) -> Option<Vec<&'a Job>> {
    let inbound = inputs.inbound?;
    let lookups = Lookups::of(inputs);
    Some(
        inbound
            .iter()
            .filter(|(j, _)| place(&j.id.to_string(), &lookups) == Placed::At("receiving"))
            .map(|(j, _)| j)
            .collect(),
    )
}

/// MARSHALLING's view of the stations: each station with only the
/// packets [`place`] puts in marshalling — neither still in receiving,
/// nor riding a train, nor held by a region of their own. `Err` names the
/// read that failed — the station registry, or the inbound read without
/// which the line cannot be drawn.
pub(crate) fn marshalling_view(
    inputs: &RegionInputs<'_>,
) -> Result<Vec<StationReading>, &'static str> {
    let stations = inputs
        .stations
        .ok_or("the station registry could not be read")?;
    inputs.inbound.ok_or(MARSHALLING_NEEDS_INBOUND)?;
    let lookups = Lookups::of(inputs);
    Ok(stations
        .iter()
        .map(|s| StationReading {
            members: s
                .members
                .iter()
                .filter(|m| place(m, &lookups) == Placed::At("marshalling"))
                .cloned()
                .collect(),
            ..s.clone()
        })
        .collect())
}

/// WHAT EACH REGION COUNTS, by packet id, in [`REGIONS`] order — a fold
/// of [`place`] over every packet the reading names, and the partition
/// every region's `count` is pinned to (the tests
/// `no_job_id_is_counted_in_two_regions` and
/// `a_train_stands_where_its_active_step_is_and_what_it_carries_rides_it`).
/// `None` exactly where the region was not read.
pub fn members(inputs: &RegionInputs<'_>) -> Vec<(&'static str, Option<Ids>)> {
    let lookups = Lookups::of(inputs);
    let mut placed: std::collections::BTreeMap<&'static str, Ids> = Default::default();
    for packet in lookups.packets() {
        if let Placed::At(region) = place(&packet, &lookups) {
            placed.entry(region).or_default().insert(packet);
        }
    }
    REGIONS
        .iter()
        .map(|region| {
            (
                *region,
                lookups
                    .read(region)
                    .then(|| placed.remove(region).unwrap_or_default()),
            )
        })
        .collect()
}

/// How many packets the partition places in `region` — the count a
/// region whose members are not one list of its own reports (the dock,
/// the gates and the track, which trains and their cargo cross). `None`
/// where the region was not read.
pub(crate) fn count_in(inputs: &RegionInputs<'_>, region: &str) -> Option<usize> {
    members(inputs)
        .into_iter()
        .find(|(name, _)| *name == region)
        .and_then(|(_, ids)| ids.map(|ids| ids.len()))
}

/// The open trains [`train_region`] stands in `region`, with steps.
pub(crate) fn trains_in<'a>(inputs: &RegionInputs<'a>, region: &str) -> Vec<&'a (Job, Vec<Step>)> {
    inputs
        .open_trains
        .iter()
        .filter(|(j, _)| {
            let id = j.id.to_string();
            inputs
                .status
                .trains
                .iter()
                .any(|t| t.id == id && train_region(t.phase) == region)
        })
        .collect()
}

// The tests live beside this file, not in it (backlog 3976779c, review
// 4c89fc8e N2): [`crate::moves::mover_version`] hashes this file, so a
// test-only edit here used to restart the undeclared band's window.
#[cfg(test)]
#[path = "partition/tests.rs"]
mod tests;
