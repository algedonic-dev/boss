use super::*;
use crate::regions::fixtures::*;

#[test]
fn moves_snapshot_does_not_invent_an_unread_packet_holder_for_sensors() {
    let status = empty_status();
    let input = inputs(&status, &[], &[], &[], &[], Some(&[]), Some(&[]));
    let snapshot = crate::moves::Snapshot::of(&input);
    assert!(
        snapshot.unread.is_empty(),
        "all packet reads answered: {:?}",
        snapshot.unread
    );
    assert_eq!(snapshot.place("no-packet"), crate::moves::Where::Off);
}

#[test]
fn moves_snapshot_knows_a_car_is_aboard_instead_of_retaining_its_old_dock() {
    let car = parked("fix/native-carriage-witness", json!({}));
    let car_id = car.0.id.to_string();
    let train = job(
        "pr-train",
        "native carriage witness",
        JobStatus::Open,
        json!({"boarded_jobs":[car_id]}),
    );
    let train_id = train.id.to_string();
    let trains = vec![(
        train.clone(),
        vec![step(&train, "ci", StepStatus::Ready, None)],
    )];
    let status = build_status_for(
        YardInputs {
            open_trains: &trains,
            now: Some(t(NOW)),
            ..Default::default()
        },
        Reading::Read,
        BoardingReadings::default(),
    );
    let cars = vec![car];
    let input = inputs(&status, &trains, &[], &cars, &[], Some(&[]), Some(&[]));
    assert_eq!(
        place(&car_id, &Lookups::of(&input)),
        Placed::Aboard(train_id)
    );
    assert_eq!(
        crate::moves::Snapshot::of(&input).place(&car_id),
        crate::moves::Where::Off
    );
}

#[test]
fn moves_snapshot_preserves_known_carriage_when_an_unrelated_packet_region_is_unread() {
    use crate::moves::{Baseline, CARRIES, Cause, Reading as MoveReading, Snapshot, Tick, Where};
    let car = parked("fix/native-carriage-witness", json!({}));
    let car_id = car.0.id.to_string();
    let train = job(
        "pr-train",
        "native carriage witness",
        JobStatus::Open,
        json!({"boarded_jobs":[car_id]}),
    );
    let train_id = train.id.to_string();
    let trains = vec![(
        train.clone(),
        vec![
            step(&train, "pr", StepStatus::Completed, Some(NOW)),
            step(&train, "ci", StepStatus::Ready, None),
        ],
    )];
    let status = build_status_for(
        YardInputs {
            open_trains: &trains,
            now: Some(t(NOW)),
            ..Default::default()
        },
        Reading::Read,
        BoardingReadings::default(),
    );
    let cars = vec![car];
    let input = inputs(&status, &trains, &[], &cars, &[], None, Some(&[]));
    assert_eq!(
        place(&car_id, &Lookups::of(&input)),
        Placed::Aboard(train_id.clone())
    );
    let next = Snapshot::of(&input);
    assert_eq!(next.place(&car_id), Where::Off);
    assert_eq!(next.place("unread-packet"), Where::Unknown);
    let mut previous = Snapshot::default();
    previous.places.insert(car_id.clone(), "dock");
    previous
        .facts
        .insert(car_id.clone(), crate::moves::Facts::of(&cars[0].0));
    let (baseline, reading) = crate::moves::advance(
        Some(&Baseline {
            cursor: 1,
            snapshot: previous,
        }),
        Tick {
            head: 2,
            causes: vec![Cause {
                seq: 2,
                event_id: uuid::Uuid::new_v4(),
                at: t(NOW),
                kind: "jobs.job.updated".into(),
                packet: train_id,
            }],
            truncated: false,
            snapshot: next,
        },
    );
    let MoveReading::Moves(moves) = reading else {
        panic!("a continuous observed batch")
    };
    let car_moves: Vec<_> = moves.iter().filter(|m| m.packet == car_id).collect();
    assert_eq!(car_moves.len(), 1);
    assert_eq!(
        (car_moves[0].from.as_deref(), car_moves[0].to.as_deref()),
        (Some("dock"), Some("gates"))
    );
    assert_eq!(car_moves[0].lineage.as_deref(), Some(CARRIES));
    assert_eq!(
        baseline.snapshot.place(&car_id),
        Where::Off,
        "the baseline must not retain the old dock"
    );
}

#[test]
fn moves_snapshot_refuses_carriage_whose_carrier_region_is_not_observed() {
    let car = parked("fix/native-carriage-witness", json!({}));
    let car_id = car.0.id.to_string();
    let train = job(
        "pr-train",
        "unplaced carrier",
        JobStatus::Open,
        json!({"boarded_jobs":[car_id]}),
    );
    let train_id = train.id.to_string();
    let trains = vec![(
        train.clone(),
        vec![step(&train, "ci", StepStatus::Ready, None)],
    )];
    let status = empty_status();
    let cars = vec![car];
    let input = inputs(&status, &trains, &[], &cars, &[], None, Some(&[]));
    assert_eq!(
        place(&car_id, &Lookups::of(&input)),
        Placed::Aboard(train_id)
    );
    assert_eq!(
        crate::moves::Snapshot::of(&input).place(&car_id),
        crate::moves::Where::Unknown
    );
}

#[test]
fn moves_snapshot_conserves_partition_carriage_even_without_a_separate_car_facts_row() {
    let car = parked("fix/native-carriage-witness", json!({}));
    let car_id = car.0.id.to_string();
    let train = job(
        "pr-train",
        "native carriage witness",
        JobStatus::Open,
        json!({"boarded_jobs":[car_id]}),
    );
    let train_id = train.id.to_string();
    let trains = vec![(
        train.clone(),
        vec![
            step(&train, "pr", StepStatus::Completed, Some(NOW)),
            step(&train, "ci", StepStatus::Ready, None),
        ],
    )];
    let status = build_status_for(
        YardInputs {
            open_trains: &trains,
            now: Some(t(NOW)),
            ..Default::default()
        },
        Reading::Read,
        BoardingReadings::default(),
    );
    let input = inputs(&status, &trains, &[], &[], &[], None, Some(&[]));
    assert_eq!(
        place(&car_id, &Lookups::of(&input)),
        Placed::Aboard(train_id)
    );
    assert_eq!(
        crate::moves::Snapshot::of(&input).place(&car_id),
        crate::moves::Where::Off
    );
}

/// RECEIVING UNTIL THE INTAKE STEP COMPLETES, MARSHALLING ONLY AFTER.
/// Measured 2026-09-24: 216 of the 232 open backlog-items had
/// `triage` completed and waited on `build` at
/// `q.platform-admin.task`, and every one was counted in receiving
/// AND marshalling — "669 waiting at the borders" was 325 + 323 of
/// largely the same packets. Both packets below stand at that one
/// station, because a ready task lands there whether it is a triage
/// or a build; each is counted in exactly one region.
#[test]
fn a_packet_is_in_receiving_until_its_intake_completes_and_in_marshalling_only_after() {
    let untriaged = admitted(
        "backlog-item",
        "untriaged",
        &[("triage", "task", StepStatus::Ready)],
    );
    let triaged = admitted(
        "backlog-item",
        "triaged",
        &[
            ("triage", "task", StepStatus::Completed),
            ("build", "task", StepStatus::Ready),
        ],
    );
    let (u, tr) = (untriaged.0.id.to_string(), triaged.0.id.to_string());
    let inbound = vec![untriaged, triaged];
    let stations = vec![holding("q.platform-admin.task", &[&u, &tr])];
    let status = empty_status();
    let out = regions(&inputs(
        &status,
        &[],
        &[],
        &[],
        &[],
        Some(&inbound),
        Some(&stations),
    ));
    let r = by_name(&out, "receiving");
    assert_eq!(r.count, Some(1), "only the untriaged one: {}", r.why);
    let m = by_name(&out, "marshalling");
    assert_eq!(m.count, Some(1), "only the triaged one: {}", m.why);
    assert!(taken_in(&inbound[1].1) && !taken_in(&inbound[0].1));
    // A trigger alone takes nothing in, and neither does a step
    // skipped past.
    let (_, mut skipped) = admitted(
        "backlog-item",
        "skipped",
        &[("triage", "task", StepStatus::Ready)],
    );
    skipped[1].status = StepStatus::Skipped;
    assert!(!taken_in(&skipped));
}

/// A TRAIN STANDS WHERE ITS ACTIVE STEP IS (design e765b3fc §2a, car
/// R1): made up at the dock (`collect`, `assemble`, `pr`), at the
/// gates while its train gate runs (`ci`), on the track once it has
/// merged. Its own gate rides with it — the train IS the thing under
/// test, so the bay it holds is counted once, as the train — and the
/// cars aboard ride it too. Each region's count is what the
/// partition places there.
#[test]
fn a_train_stands_where_its_active_step_is_and_what_it_carries_rides_it() {
    let made_up = job("pr-train", "train made up", JobStatus::Open, json!({}));
    let made_up_steps = vec![step(&made_up, "collect", StepStatus::Ready, None)];
    let boarded_car = parked("fix/aboard", json!({}));
    let under_test = job(
        "pr-train",
        "train under test",
        JobStatus::Open,
        json!({ "boarded_jobs": [boarded_car.0.id.to_string()] }),
    );
    let under_test_steps = vec![
        step(
            &under_test,
            "collect",
            StepStatus::Completed,
            Some("2026-09-19T11:00:00Z"),
        ),
        step(
            &under_test,
            "pr",
            StepStatus::Completed,
            Some("2026-09-19T11:01:00Z"),
        ),
        step(&under_test, "ci", StepStatus::Ready, None),
        step(&under_test, "merged", StepStatus::Ready, None),
    ];
    let in_transit = job("pr-train", "train in transit", JobStatus::Open, json!({}));
    let in_transit_steps = vec![
        step(
            &in_transit,
            "pr",
            StepStatus::Completed,
            Some("2026-09-19T10:01:00Z"),
        ),
        step(
            &in_transit,
            "ci",
            StepStatus::Completed,
            Some("2026-09-19T10:20:00Z"),
        ),
        step(
            &in_transit,
            "merged",
            StepStatus::Completed,
            Some("2026-09-19T10:21:00Z"),
        ),
        step(&in_transit, "deployed", StepStatus::Ready, None),
    ];
    let (made_up_id, under_test_id, in_transit_id) = (
        made_up.id.to_string(),
        under_test.id.to_string(),
        in_transit.id.to_string(),
    );
    let open = vec![
        (made_up, made_up_steps),
        (under_test, under_test_steps),
        (in_transit, in_transit_steps),
    ];
    let mut status = build_status_for(
        YardInputs {
            open_trains: &open,
            now: Some(t(NOW)),
            ..Default::default()
        },
        Reading::Read,
        BoardingReadings::default(),
    );
    // Two bays in use: a car's gate, and the train's own.
    status.gates.active = vec![
        crate::yard::ActiveGate {
            branch: "fix/in-a-bay".into(),
            packet_id: "gate-car".into(),
            since: "2026-09-19T11:30:00Z".into(),
            stale: false,
            train: None,
            ..Default::default()
        },
        crate::yard::ActiveGate {
            branch: "train/2026-09-19-1101".into(),
            packet_id: "gate-train".into(),
            since: "2026-09-19T11:02:00Z".into(),
            stale: false,
            train: Some(under_test_id.clone()),
            ..Default::default()
        },
    ];
    let cars = vec![boarded_car.clone()];
    let i = inputs(&status, &open, &[], &cars, &[], Some(&[]), Some(&[]));

    let lookups = Lookups::of(&i);
    assert_eq!(place(&made_up_id, &lookups), Placed::At("dock"));
    assert_eq!(place(&under_test_id, &lookups), Placed::At("gates"));
    assert_eq!(place(&in_transit_id, &lookups), Placed::At("track"));
    assert_eq!(place("gate-car", &lookups), Placed::At("gates"));
    assert_eq!(
        place("gate-train", &lookups),
        Placed::Aboard(under_test_id.clone()),
        "the train's gate is the train under test, not a second packet in a bay"
    );
    assert_eq!(
        place(&boarded_car.0.id.to_string(), &lookups),
        Placed::Aboard(under_test_id.clone())
    );
    assert_eq!(place("nobody", &lookups), Placed::Off);

    // The partition still sums: every region's count, line and judged
    // are what it holds (backlog bce67f09).
    let out = regions(&i);
    let held = members(&i);
    for r in &out.regions {
        let (_, ids) = held.iter().find(|(n, _)| *n == r.name).unwrap();
        assert_eq!(
            r.count
                .map(|n| n + r.waiting.unwrap_or(0) + r.judged.unwrap_or(0)),
            ids.as_ref().map(Ids::len),
            "{}: its count, line and judged and its members disagree — {ids:?}",
            r.name
        );
    }
    assert_eq!(
        by_name(&out, "dock").count,
        Some(1),
        "the train being made up"
    );
    assert_eq!(
        by_name(&out, "gates").count,
        Some(2),
        "the car's bay and the train under test, its own bay counted once"
    );
    assert_eq!(by_name(&out, "track").count, Some(1), "the merged train");
}

/// A PACKET A LATER READ SAW CLOSED STANDS AT NO STATION (backlog
/// e23005c0). The stations are judged over the open packets read at
/// the START of a reading; the shop floor's runs are read seconds
/// later. A run that closed in between was still at its station in
/// the first read and closed in the second, so the shop floor let it
/// go and marshalling — the remainder — took it: measured on the
/// moves record 2026-09-26, 19 of the 20 agent-runs that left from
/// marshalling moved there from the shop floor seconds before their
/// own close (run 8e1ea9ae: shop-floor -> marshalling 15:45:29,
/// closed on `landed` 15:45:43). The newer read is the truth: a
/// closed packet stands at no station, so it stands where its own
/// region puts it, or off the map.
#[test]
fn a_packet_another_read_saw_closed_stands_at_no_station() {
    let (mut run, steps) = admitted(
        crate::agent_budget::RUN_KIND,
        "a run that landed mid-read",
        &[("briefed", "task", StepStatus::Completed)],
    );
    run.status = JobStatus::Closed;
    let id = run.id.to_string();
    let runs = vec![(run, steps)];
    let stations = vec![holding("q.platform-admin.task", &[&id])];
    let status = empty_status();
    let mut i = inputs(&status, &[], &[], &[], &[], Some(&[]), Some(&stations));
    i.agent_runs = Some(&runs);
    assert_eq!(
        place(&id, &Lookups::of(&i)),
        Placed::Off,
        "a run closed by the time the shop floor was read is in no region"
    );
    // An open packet at the same station is still marshalling's.
    let (item, item_steps) = admitted(
        "backlog-item",
        "triaged",
        &[
            ("triage", "task", StepStatus::Completed),
            ("build", "task", StepStatus::Ready),
        ],
    );
    let item_id = item.id.to_string();
    let inbound = vec![(item, item_steps)];
    let stations = vec![holding("q.platform-admin.task", &[&id, &item_id])];
    let mut i = inputs(&status, &[], &[], &[], &[], Some(&inbound), Some(&stations));
    i.agent_runs = Some(&runs);
    let lookups = Lookups::of(&i);
    assert_eq!(place(&item_id, &lookups), Placed::At("marshalling"));
    assert_eq!(place(&id, &lookups), Placed::Off);
}

/// NO JOB ID IS COUNTED IN TWO REGIONS, and every region's `count`
/// is the size of what [`members`] says it holds — so the partition
/// is the definition each card's number is pinned to, not a second
/// opinion beside it (CLAUDE.md §9a).
///
/// The fixture is the live overlap made total: ONE station holds a
/// packet from every region that can stand at one — an untriaged and
/// a triaged item, a run being briefed, a landed car's `proven`, a
/// parked car's `review`, an open train, a gate-run in a bay, three
/// garage lanes and a publish packet with its pull request open —
/// because a station's predicate matches a STEP, and a ready task
/// lands on it whoever it belongs to.
#[test]
fn no_job_id_is_counted_in_two_regions() {
    let untriaged = admitted(
        "backlog-item",
        "untriaged",
        &[("triage", "task", StepStatus::Ready)],
    );
    let triaged = admitted(
        "backlog-item",
        "triaged",
        &[
            ("triage", "task", StepStatus::Completed),
            ("build", "task", StepStatus::Ready),
        ],
    );
    // An agent-run is an inbound kind by the registry's rule, and
    // before `briefed` completes it has taken nothing in — but it is
    // a run in flight, and the shop floor holds it.
    let run = admitted(
        crate::agent_budget::RUN_KIND,
        "a run",
        &[("briefed", "task", StepStatus::Ready)],
    );
    // A publish packet whose pull request is open on the mirror.
    let publish = {
        let (j, mut steps) = admitted(
            PUBLISH_KIND,
            "Publish to the public mirror",
            &[("open-pr", "task", StepStatus::Completed)],
        );
        steps[1].metadata = json!({ "pr_url": "https://mirror/pull/9" });
        (j, steps)
    };
    let shed_car = {
        let j = job(
            "ship-a-change",
            "fix/landed",
            JobStatus::Open,
            json!({ "branch": "fix/landed" }),
        );
        let s = vec![step(&j, "proven", StepStatus::Ready, None)];
        (j, s)
    };
    let dock_car = parked("fix/parked", json!({}));
    let train = job("pr-train", "train #9", JobStatus::Open, json!({}));
    let train_steps = vec![
        step(
            &train,
            "pr",
            StepStatus::Completed,
            Some("2026-09-19T11:01:00Z"),
        ),
        step(&train, "ci", StepStatus::Ready, None),
    ];
    // A train whose gate has spoken: `ci` judged red, not merged.
    let judged = job("pr-train", "train #10", JobStatus::Open, json!({}));
    let mut judged_ci = step(&judged, "ci", StepStatus::Completed, Some(NOW));
    judged_ci.metadata = json!({ "result": "failing" });
    let judged_steps = vec![
        step(
            &judged,
            "pr",
            StepStatus::Completed,
            Some("2026-09-19T11:01:00Z"),
        ),
        judged_ci,
        step(&judged, "merged", StepStatus::Ready, None),
    ];
    let open_trains = vec![(train, train_steps), (judged, judged_steps)];
    let arrived = job(
        "pr-train",
        "train #8",
        JobStatus::Closed,
        json!({ "outcome": "arrived", "closed_at": "2026-09-19T09:00:00Z" }),
    );
    let closed_trains = vec![(arrived, Vec::new())];

    let mut status = build_status_for(
        YardInputs {
            open_trains: &open_trains,
            now: Some(t(NOW)),
            ..Default::default()
        },
        Reading::Read,
        BoardingReadings::default(),
    );
    status.dock = vec![crate::yard::dock_car(&dock_car.0)];
    status.gates.active = vec![crate::yard::ActiveGate {
        branch: "fix/in-a-bay".into(),
        packet_id: "gate-in-a-bay".into(),
        since: "2026-09-19T11:30:00Z".into(),
        stale: false,
        train: None,
        ..Default::default()
    }];
    // A run waiting for a bay: placed at the gates, once (357fe1da).
    status.gates.queued = vec![crate::yard::QueuedGate {
        branch: "fix/in-line".into(),
        packet_id: "gate-in-line".into(),
        next_owner: None,
        queued_at: "2026-09-19T11:40:00Z".into(),
        position: 1,
        waiting_seconds: None,
        estimated_wait_seconds: None,
        train: None,
    }];
    status.held = vec![crate::yard::HeldGreen {
        branch: "feat/held".into(),
        reason: "not yet".into(),
        since: "2026-09-19T02:00:00Z".into(),
        packet_id: "gate-held".into(),
        sha: None,
    }];
    status.limbo = vec![crate::yard::LimboCar {
        branch: "fix/lost".into(),
        verdict: "lost".into(),
        since: "2026-09-19T02:00:00Z".into(),
        packet_id: "gate-lost".into(),
        sha: None,
    }];
    status.garage = vec![crate::yard::GaragedCar {
        branch: "fix/red".into(),
        failed_check: None,
        failed_line: None,
        since: "2026-09-19T02:00:00Z".into(),
        packet_id: "gate-red".into(),
        sha: None,
    }];

    let id = |j: &Job| j.id.to_string();
    let everyone = [
        id(&untriaged.0),
        id(&triaged.0),
        id(&run.0),
        id(&publish.0),
        id(&shed_car.0),
        id(&dock_car.0),
        id(&open_trains[0].0),
        "gate-in-a-bay".to_string(),
        "gate-in-line".to_string(),
        "gate-held".to_string(),
        "gate-lost".to_string(),
        "gate-red".to_string(),
    ];
    let at_the_station: Vec<&str> = everyone.iter().map(String::as_str).collect();
    let stations = vec![holding("q.platform-admin.task", &at_the_station)];
    let inbound = vec![
        untriaged.clone(),
        triaged.clone(),
        run.clone(),
        publish.clone(),
    ];
    let runs = vec![run.clone()];
    let publishes = vec![publish.clone()];
    let cars = vec![shed_car.clone(), dock_car.clone()];
    let mut i = inputs(
        &status,
        &open_trains,
        &closed_trains,
        &cars,
        &[],
        Some(&inbound),
        Some(&stations),
    );
    i.agent_runs = Some(&runs);
    i.publish_packets = Some(&publishes);

    let out = regions(&i);
    let held = members(&i);
    assert_eq!(
        held.iter().map(|(n, _)| *n).collect::<Vec<_>>(),
        REGIONS,
        "one entry per region, in map order"
    );

    // No id twice — naming the id and both regions when it drifts.
    let mut owner: std::collections::BTreeMap<&str, &str> = Default::default();
    for (region_name, ids) in &held {
        for pid in ids.iter().flatten() {
            if let Some(first) = owner.insert(pid.as_str(), region_name) {
                panic!("{pid} is counted in {first} AND {region_name}");
            }
        }
    }
    // Every region's count IS its members — with the gates' line, its
    // second figure (backlog 357fe1da), and its judged, the third
    // (backlog bce67f09), beside the bays.
    for r in &out.regions {
        let (_, ids) = held.iter().find(|(n, _)| *n == r.name).unwrap();
        assert_eq!(
            r.count
                .map(|n| n + r.waiting.unwrap_or(0) + r.judged.unwrap_or(0)),
            ids.as_ref().map(Ids::len),
            "{}: its count, line and judged and its members disagree — {ids:?}",
            r.name
        );
    }
    // The line: the run waiting for a bay and train #9, under test
    // with no gate-run. Train #10's gate has spoken: judged, not line.
    let gates = by_name(&out, "gates");
    assert_eq!(
        (gates.count, gates.waiting, gates.judged),
        (Some(1), Some(2), Some(1)),
        "{held:?}"
    );
    // And the station's packets are all accounted for, each once.
    for pid in &everyone {
        assert!(owner.contains_key(pid.as_str()), "{pid} is in no region");
    }
    let region_of = |pid: &str| owner.get(pid).copied();
    assert_eq!(region_of(&id(&untriaged.0)), Some("receiving"));
    assert_eq!(region_of(&id(&triaged.0)), Some("marshalling"));
    assert_eq!(region_of(&id(&run.0)), Some("shop-floor"));
    assert_eq!(region_of(&id(&publish.0)), Some("publish"));
    assert_eq!(region_of(&id(&shed_car.0)), Some("shed"));
    assert_eq!(region_of(&id(&dock_car.0)), Some("dock"));
    assert_eq!(region_of("gate-in-a-bay"), Some("gates"));
    assert_eq!(region_of("gate-in-line"), Some("gates"));
    assert_eq!(region_of("gate-red"), Some("garage"));

    // The two borders out of the upstream pair wait on the same
    // partition: one packet each, never the sum of the overlap.
    let b = crate::borders::borders(&crate::borders::BorderInputs {
        regions: &i,
        firings: Some(&[]),
        dispatcher_firings: Some(&[]),
        machines_withheld: None,
    });
    let waiting = |from: &str| {
        b.borders
            .iter()
            .find(|x| x.from == from && x.to != "garage")
            .and_then(|x| x.waiting)
    };
    assert_eq!(waiting("receiving"), Some(1));
    assert_eq!(waiting("marshalling"), Some(1));
}
