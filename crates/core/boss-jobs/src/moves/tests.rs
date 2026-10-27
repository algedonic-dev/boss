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
        dispatch: BTreeMap::new(),
        places: places.iter().map(|(p, r)| ((*p).to_string(), *r)).collect(),
        facts: known
            .iter()
            .map(|(p, f)| ((*p).to_string(), f.clone()))
            .collect(),
        aboard: BTreeMap::new(),
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

/// A declared dispatch line cannot establish an arbitrary metadata
/// packet link. Historical missing registration is unjudged, never
/// promoted to a pass merely because its two regions match.
#[test]
fn registered_dispatch_route_does_not_vouch_for_an_unregistered_packet_link() {
    let routes = crate::routes::RouteMap {
        routes: vec![crate::routes::Route {
            from: Some("shed".into()),
            to: Some("shop-floor".into()),
            declared: true,
            sources: vec![crate::routes::Source::HandOff {
                agent_version: None,
                by: "verb:dispatch".into(),
                from: Some("ship-a-change@landed-review".into()),
                to: Some("agent-run@briefed".into()),
                why: "registered agent step, not a region exemption".into(),
                terminal: None,
            }],
        }],
        walked: vec![],
        refused: vec![],
    };
    let car = facts("ship-a-change", None, &[]);
    let run = facts("agent-run", None, &[("packet", "car")]);
    let previous = snapshot(&[("car", "shed")], &[("car", car.clone())]);
    let next = snapshot(
        &[("car", "shed"), ("unregistered", "shop-floor")],
        &[("car", car), ("unregistered", run)],
    );
    let found = moves(
        &previous,
        &next,
        &[cause(1, "unregistered", "jobs.job.created")],
    );
    assert_eq!(found.len(), 1);
    assert_eq!(found[0].handoff_from.as_deref(), Some("car"));
    assert_eq!(found[0].lineage.as_deref(), Some("packet"));
    assert_ne!(judged(found, &routes)[0].declared, Some(true));
}

#[test]
fn an_observed_partition_carrier_wins_over_a_closed_trains_historical_cargo_link() {
    let car_id = "00000000-0000-0000-0000-000000000003";
    let old_id = "00000000-0000-0000-0000-000000000001";
    let current_id = "00000000-0000-0000-0000-000000000002";
    let car = facts("ship-a-change", Some("fix/exact-carrier"), &[]);
    let previous = snapshot(&[(car_id, "dock")], &[(car_id, car.clone())]);
    let mut next = snapshot(
        &[(old_id, "arrivals"), (current_id, "gates")],
        &[
            (car_id, car),
            (
                old_id,
                closed(facts("pr-train", None, &[(CARRIES, car_id)])),
            ),
            (current_id, facts("pr-train", None, &[(CARRIES, car_id)])),
        ],
    );
    next.aboard.insert(car_id.into(), current_id.into());
    next.unread.push("receiving");
    let found = moves(&previous, &next, &[cause(101, car_id, "jobs.job.updated")]);
    let car_moves: Vec<_> = found.iter().filter(|m| m.packet == car_id).collect();
    assert_eq!(car_moves.len(), 1);
    assert_eq!(
        (car_moves[0].from.as_deref(), car_moves[0].to.as_deref()),
        (Some("dock"), Some("gates"))
    );
    assert_eq!(car_moves[0].lineage.as_deref(), Some(CARRIES));
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
            lineage: None,
            dispatch: None,
            declared: None,
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

/// A ROW FROM A SUPERSEDED MOVER DOES NOT TROUBLE A REGION (backlog
/// c8d22e63). Measured 2026-09-27: train #726 fixed the mover, and
/// arrivals stayed TROUBLED for most of a day on the OLD mover's
/// `shed -> arrivals` misreadings, because the band re-judged their
/// stored from/to against today's routes and nothing said which
/// mover had read them. The same misreading is held three ways —
/// written before the stamp existed, by a superseded mover, and by
/// the current one — and only the current one's troubles arrivals.
/// The superseded rows are not deleted or rewritten: the record
/// still holds all three, each naming the mover that wrote it.
#[tokio::test]
async fn a_row_from_a_superseded_mover_does_not_trouble_a_region() {
    let misread = |n: u128| Move {
        at: t("2026-09-26T23:41:36Z"),
        packet: format!("train-{n}"),
        kind: "pr-train".into(),
        label: "a train".into(),
        from: Some("shed".into()),
        to: Some("arrivals".into()),
        declared: Some(false),
        cause_event_id: Uuid::from_u128(n),
        cause_seq: n as i64,
        cause_kind: "jobs.job.updated".into(),
        handoff_from: None,
        lineage: None,
        aboard: Vec::new(),
        terminal: None,
        dispatch: None,
    };
    let store = InMemoryMoves::new();
    store.record_by(None, &[misread(1)]).unwrap();
    store
        .record_by(Some("yard-mover-v1:superseded"), &[misread(2)])
        .unwrap();
    let since = t("2026-09-26T00:00:00Z");
    // No route is declared, so any counted crossing is undeclared.
    let routes = crate::routes::RouteMap {
        routes: vec![],
        walked: vec![],
        refused: vec![],
    };
    let arrivals = |crossings: Vec<RouteCount>| {
        let map: crate::regions::Regions = serde_json::from_value(serde_json::json!({
            "window_hours": 24,
            "regions": [{
                "name": "arrivals", "count": 0, "state": "clear", "why": "quiet",
                "trend": {"metric": "arrivals", "unit": "per day", "current": null,
                          "previous": null, "samples": 0, "previous_samples": 0},
            }],
        }))
        .unwrap();
        with_undeclared(
            map,
            Some(crossings),
            Some(&routes),
            t("2026-09-27T00:00:00Z"),
        )
        .regions
        .remove(0)
    };

    let before = arrivals(store.crossings(since).await.unwrap());
    assert_eq!(
        before.state,
        crate::regions::RegionState::Clear,
        "a superseded mover's misreading troubles no region: {}",
        before.why
    );
    assert_eq!(before.undeclared, Some(vec![]), "read, and empty");

    store.record(&[misread(3)]).await.unwrap();
    let after = arrivals(store.crossings(since).await.unwrap());
    assert_eq!(
        after.state,
        crate::regions::RegionState::Troubled,
        "the CURRENT mover's reading is still judged: {}",
        after.why
    );
    assert!(after.why.contains("shed → arrivals ×1"), "{}", after.why);

    // ONE DEFINITION OF AN ACTIVE ROW (backlog 3976779c): the moves
    // read's conservation counts what the band counts. Both other
    // movers' rows are held, conserved in `raw`, and active in neither.
    let history = store.correction_conservation(since).await.unwrap();
    assert_eq!(
        history,
        History {
            raw: 3,
            active: 1,
            superseded: 0,
            unknown: 0,
            superseded_mover: 2,
        },
        "only the current mover's row is active, as the band reads it, and raw is conserved"
    );
    let crossed: i64 = store
        .crossings(since)
        .await
        .unwrap()
        .iter()
        .map(|r| r.moves)
        .sum();
    assert_eq!(
        crossed,
        history.active + history.unknown,
        "the band counts exactly the active and unknown rows"
    );

    let held: Vec<Option<String>> = store
        .since(0, 10)
        .await
        .unwrap()
        .into_iter()
        .map(|r| r.mover_version)
        .collect();
    assert_eq!(
        held,
        [
            None,
            Some("yard-mover-v1:superseded".to_string()),
            Some(mover_version())
        ],
        "every row stays in the record, naming the mover that wrote it"
    );
}

/// THE STAMP IS A CONTENT ADDRESS, NOT A HAND-BUMPED NUMBER, and an
/// UNSTAMPED ROW SERIALISES AS IT ALWAYS DID: the correction ledger
/// binds a diagnosis to `move_diagnosis::hash` of its original's
/// JSON, so a `mover_version: null` key would orphan every original
/// recorded before the stamp (backlog c8d22e63).
#[test]
fn the_mover_version_is_content_addressed_and_an_unstamped_row_hashes_as_before() {
    let v = mover_version();
    let hex = v.strip_prefix("yard-mover-v1:").expect("versioned prefix");
    assert_eq!(hex.len(), 64, "{v}");
    assert!(hex.bytes().all(|b| b.is_ascii_hexdigit()), "{v}");
    assert_eq!(v, mover_version(), "one version per build");

    let row = Recorded {
        seq: 7,
        r#move: Move {
            at: t("2026-09-26T17:49:59Z"),
            packet: "car-1".into(),
            kind: "ship-a-change".into(),
            label: "fix/x".into(),
            from: None,
            to: Some("garage".into()),
            declared: Some(false),
            cause_event_id: Uuid::from_u128(794),
            cause_seq: 794,
            cause_kind: "jobs.job.updated".into(),
            handoff_from: None,
            lineage: None,
            aboard: Vec::new(),
            terminal: None,
            dispatch: None,
        },
        mover_version: None,
    };
    let json = serde_json::to_value(&row).unwrap();
    assert!(json.get("mover_version").is_none(), "{json}");
    let back: Recorded = serde_json::from_value(json).unwrap();
    assert_eq!(back, row, "an old original reads back unstamped");
    let stamped = Recorded {
        mover_version: Some(v.clone()),
        ..row
    };
    assert_eq!(
        serde_json::to_value(&stamped).unwrap()["mover_version"],
        serde_json::Value::String(v)
    );
}
