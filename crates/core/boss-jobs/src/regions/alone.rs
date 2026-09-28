//! ONE PACKET, PLACED BY ITSELF (design e765b3fc §2b, car R2): where
//! [`place`] would stand a packet if it were the only one on the map.
//!
//! The route derivation (`crate::routes`) walks a protocol's states and
//! asks, at each one, where the packet stands. It must ask the SAME
//! placement function every region's count is pinned to — a second
//! answer to "where does a train at `ci` stand" is the defect CLAUDE.md
//! §9a names — so this builds the one-packet reading a handler would
//! have built had it read only this packet, and hands it to [`place`].
//!
//! WHICH INPUT A KIND ARRIVES THROUGH ([`Family`]) is the one thing this
//! adds, and it is not a new fact: the handler (`http/regions.rs`) reads
//! each kind into its own slot of [`RegionInputs`] — trains, cars,
//! gate-runs, runs, publish packets, the inbound kinds the registry
//! names — and [`family_of`] says the same thing once, for one packet.
//! A kind in none of the named families is placed by the stations alone
//! ([`Family::Stationed`]), because that is how the live read places it.
//!
//! STATIONS ARE ASKED THEIR OWN QUESTION. The dock's membership is the
//! loading-dock row's predicate as [`crate::yard::on_the_dock`] reads it
//! (so a held car stands in the held lane, as it does live), and a
//! station holds an inbound packet exactly when its predicate matches —
//! the question the handler asks of every open packet.

use super::*;
use crate::station_queue::StationPredicate;
use crate::yard::{BoardingReadings, YardInputs, build_status_for};

/// The pr-train kind — the conductor's packet.
pub const TRAIN_KIND: &str = "pr-train";
/// The ship-a-change kind — a car.
pub const CAR_KIND: &str = "ship-a-change";
/// The gate-run kind — one gate in a bay.
pub const GATE_RUN_KIND: &str = "gate-run";

/// Which slot of [`RegionInputs`] a kind is read into — and so which
/// regions can hold it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Family {
    /// `open_trains` / `closed_trains`: dock, gates, track, arrivals.
    Train,
    /// `cars` and the dock read: dock, garage (held), shed.
    Car,
    /// `gate_runs`: gates, garage.
    GateRun,
    /// `agent_runs`: the shop floor.
    Run,
    /// `publish_packets`: publish.
    Publish,
    /// `inbound` and the stations: receiving, marshalling.
    Inbound,
    /// The stations alone: marshalling. Every other kind — an
    /// ops-request, a maintenance chore, a park-a-job — is placed by the
    /// live read only through the station it stands at, because the
    /// stations are judged over EVERY open packet (backlog e23005c0).
    Stationed,
}

/// The family `kind` arrives through. `inbound` is [`inbound_kinds`]
/// over the registry — the receiving yard's own rule, never a list typed
/// here. The pipeline kinds are named first because an `agent-run` is an
/// inbound kind by that rule and the shop floor claims it ([`place`]'s
/// order).
///
/// EVERY KIND HAS ONE. Until backlog e23005c0 a kind in none of the
/// named families answered `None`, "a kind the map does not place" —
/// untrue of the live read: the stations are judged over every open
/// packet, so an ops-request at its `execute` station stood in
/// marshalling, and 217 of them left the map from there on 2026-09-26
/// by a terminal no route named. Such a kind is [`Family::Stationed`].
pub fn family_of(kind: &str, inbound: &[String]) -> Family {
    match kind {
        TRAIN_KIND => Family::Train,
        CAR_KIND => Family::Car,
        GATE_RUN_KIND => Family::GateRun,
        crate::agent_budget::RUN_KIND => Family::Run,
        PUBLISH_KIND => Family::Publish,
        k if inbound.iter().any(|i| i == k) => Family::Inbound,
        _ => Family::Stationed,
    }
}

/// The station predicates a one-packet reading asks: the loading dock's,
/// and every other station's — bound as the service reads them, so a
/// per-actor station (one that names `@me`) holds nothing here.
#[derive(Debug, Clone, Default)]
pub struct Stations {
    pub dock: Option<StationPredicate>,
    pub others: Vec<(String, StationPredicate)>,
}

impl Stations {
    /// From the active station rows, as the regions read binds them.
    pub fn of(specs: &[crate::stations::StationSpec]) -> Self {
        let mut out = Stations::default();
        for spec in specs {
            let Some(bound) = spec.bind_self(None) else {
                continue;
            };
            if spec.name == "loading-dock" {
                out.dock = Some(bound.predicate);
            } else {
                out.others.push((spec.name.clone(), bound.predicate));
            }
        }
        out
    }

    /// Every predicate whose `kind` clause admits `kind` (or names none).
    pub fn for_kind(&self, kind: &str) -> impl Iterator<Item = &StationPredicate> {
        self.dock
            .iter()
            .chain(self.others.iter().map(|(_, p)| p))
            .filter(move |p| p.kind.as_deref().is_none_or(|k| k == kind))
    }
}

/// WHERE `packet` STANDS when it is the only packet on the map — the
/// reading the handler would build from this one row, handed to
/// [`place`]. Every read answers (an unread region would place nothing,
/// which is a different question).
pub fn place_alone(
    packet: &(Job, Vec<Step>),
    family: Family,
    stations: &Stations,
    now: Instant,
    window_hours: i64,
) -> Placed {
    let (job, steps) = packet;
    let one = std::slice::from_ref(packet);
    let none: &[(Job, Vec<Step>)] = &[];
    let only = |f: Family| if family == f { one } else { none };
    let open = job.status == JobStatus::Open;
    let (open_trains, closed_trains) = match (family, open) {
        (Family::Train, true) => (one, none),
        (Family::Train, false) => (none, one),
        _ => (none, none),
    };
    let dock_cars: Vec<(Job, Vec<Step>)> = match (family, &stations.dock) {
        (Family::Car, Some(dock)) if crate::yard::on_the_dock(dock, job, steps) => {
            vec![packet.clone()]
        }
        _ => Vec::new(),
    };
    let gate_rows: Vec<Job> = if family == Family::GateRun {
        vec![job.clone()]
    } else {
        Vec::new()
    };
    let status = build_status_for(
        YardInputs {
            open_trains,
            closed_trains,
            dock_cars: &dock_cars,
            gate_runs: only(Family::GateRun),
            now: Some(now),
            ..Default::default()
        },
        Reading::Read,
        BoardingReadings::default(),
    );
    // The live stations are judged over the OPEN packets only (a station
    // predicate's universe), so a closed packet stands at none.
    let at_stations: Vec<StationReading> = match family {
        Family::Inbound | Family::Stationed if open => stations
            .others
            .iter()
            .filter(|(_, p)| p.matches(job, steps))
            .map(|(name, _)| StationReading {
                name: name.clone(),
                over_limit: false,
                members: vec![job.id.to_string()],
                served: None,
                previous_served: None,
                opened: Default::default(),
            })
            .collect(),
        _ => Vec::new(),
    };
    let inputs = RegionInputs {
        status: &status,
        dock_reading: Reading::Read,
        open_trains,
        closed_trains,
        cars: only(Family::Car),
        gate_runs: &gate_rows,
        inbound: Some(only(Family::Inbound)),
        stations: Some(&at_stations),
        conductor: None,
        ops_requests: Some(none),
        agent_runs: Some(only(Family::Run)),
        sessions: Some(&[]),
        publish_packets: Some(only(Family::Publish)),
        run_capacity: None,
        runner_hosts: Some(&[]),
        predecessors: &[],
        now,
        window_hours,
    };
    place(&job.id.to_string(), &Lookups::of(&inputs))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::regions::fixtures::*;

    /// The one-packet reading places a packet exactly where the full
    /// partition does: a train at `ci` at the gates, a merged one on the
    /// track, an untaken item in receiving, a run on the shop floor.
    #[test]
    fn a_packet_alone_stands_where_the_partition_stands_it() {
        let now = t(NOW);
        let train = job(TRAIN_KIND, "t", JobStatus::Open, json!({}));
        let at_ci = vec![
            step(&train, "pr", StepStatus::Completed, Some(NOW)),
            step(&train, "ci", StepStatus::Ready, None),
        ];
        let none = Stations::default();
        assert_eq!(
            place_alone(&(train.clone(), at_ci), Family::Train, &none, now, 24),
            Placed::At("gates")
        );
        let merged = vec![
            step(&train, "pr", StepStatus::Completed, Some(NOW)),
            step(&train, "merged", StepStatus::Completed, Some(NOW)),
            step(&train, "deployed", StepStatus::Ready, None),
        ];
        assert_eq!(
            place_alone(&(train, merged), Family::Train, &none, now, 24),
            Placed::At("track")
        );
        let item = admitted(
            "backlog-item",
            "item",
            &[("triage", "task", StepStatus::Ready)],
        );
        assert_eq!(
            place_alone(&item, Family::Inbound, &none, now, 24),
            Placed::At("receiving")
        );
        let run = admitted(
            crate::agent_budget::RUN_KIND,
            "run",
            &[("briefed", "task", StepStatus::Ready)],
        );
        assert_eq!(
            place_alone(&run, Family::Run, &none, now, 24),
            Placed::At("shop-floor")
        );
        assert_eq!(
            family_of("backlog-item", &["backlog-item".to_string()]),
            Family::Inbound
        );
        assert_eq!(
            family_of(
                crate::agent_budget::RUN_KIND,
                &[crate::agent_budget::RUN_KIND.to_string()]
            ),
            Family::Run,
            "the shop floor claims a run before receiving can"
        );
        assert_eq!(family_of("design-doc", &[]), Family::Stationed);
    }

    /// A KIND IN NO OTHER FAMILY STANDS WHERE ITS STATION IS (backlog
    /// e23005c0). The live stations are judged over EVERY open packet,
    /// so an ops-request waiting on its `execute` step stands at its
    /// station and marshalling holds it — 217 of them left the map from
    /// there on 2026-09-26 by `answered` — while the walk, asking only
    /// the families it knew, placed an ops-request nowhere and never
    /// walked it. Placed alone it now stands where the partition stands
    /// it: at the station while one holds it, off the map when none does.
    #[test]
    fn a_kind_in_no_other_family_stands_in_marshalling_while_a_station_holds_it() {
        let now = t(NOW);
        assert_eq!(family_of("ops-request", &[]), Family::Stationed);
        let request = admitted(
            "ops-request",
            "converge",
            &[("execute", "task", StepStatus::Ready)],
        );
        let station = crate::station_queue::StationPredicate {
            kind: Some("ops-request".into()),
            ..Default::default()
        };
        let stations = Stations {
            dock: None,
            others: vec![("q.ops".into(), station)],
        };
        assert_eq!(
            place_alone(&request, Family::Stationed, &stations, now, 24),
            Placed::At("marshalling")
        );
        assert_eq!(
            place_alone(&request, Family::Stationed, &Stations::default(), now, 24),
            Placed::Off,
            "no station holds it"
        );
    }
}
