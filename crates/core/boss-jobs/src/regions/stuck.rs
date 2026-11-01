//! What is stuck, per third (backlog 4142d821, design cf820810 car 2) —
//! and [`THIRDS`], the partition of the regions into the operator
//! surface's three thirds.

use super::*;

/// THE THREE THIRDS of the operator surface (David, 2026-09-08: queue
/// management upstream, actors building in the middle, delivery
/// downstream), in reading order, each with ALL of its regions in flow
/// order.
///
/// A PARTITION since design 00774ca8 decision 1 (approved 2026-09-24):
/// every one of the [`REGIONS`] belongs to exactly one third, pinned
/// by `every_region_stands_in_exactly_one_third`. Until then the table
/// listed only the regions whose populations make up each third's stuck
/// figure (design cf820810 Q2), and the HUD would have had to hold the
/// rest of each row's membership itself — the same fact in two places
/// (CLAUDE.md 9a). [`stuck`] still reads only the regions that have a
/// stuck part; the others simply contribute none.
pub const THIRDS: [(&str, &[&str]); 3] = [
    ("queue-management", &["sensors", "receiving", "marshalling"]),
    ("actors-building", &["shop-floor", "gates", "garage"]),
    (
        "delivery",
        &["dock", "track", "arrivals", "shed", "publish"],
    ),
];

/// One third's STUCK READING (backlog 4142d821, design cf820810 car 2).
///
/// STUCK is a packet past its own place's declared bound whose next move
/// is OURS; WAITING is one held on a declared owner outside us — the
/// world, an event, a predecessor still in flight. The two are shown side
/// by side and NEVER summed, into each other or across thirds: a total
/// would bury the one shed car that is ours under two hundred intake
/// packets past their triage band, which is the averaging-away the item
/// refuses (Q1, Q3).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ThirdStuck {
    /// `queue-management`, `actors-building` or `delivery`.
    pub third: String,
    /// Distinct packets stuck — each counted once, though it may stand in
    /// two of the third's regions.
    pub stuck: usize,
    /// Distinct packets waiting on a declared owner outside us.
    pub waiting: usize,
    /// What could not be counted, one sentence each: a station whose flow
    /// the cube is blind to, an input that could not be read. NON-EMPTY
    /// MEANS `stuck` IS A FLOOR — the third reads "at least n, plus
    /// unknown", never n (Q5: unknown is not zero).
    pub unknown: Vec<String>,
    /// Hours since the oldest STUCK packet whose age the record holds
    /// opened (a station reading carries no age). `None` when nothing is
    /// stuck.
    pub oldest_hours: Option<i64>,
    /// The regions that own what is counted here — stuck, waiting or
    /// unknown — in map order: the click-through.
    pub regions: Vec<String>,
}

/// One region's contribution to its third's stuck block: the packets
/// stuck (by id, with their age in hours where the record holds one),
/// the packets waiting, and what could not be counted.
struct StuckPart {
    region: &'static str,
    stuck: Vec<(String, Option<i64>)>,
    waiting: Vec<String>,
    unknown: Vec<String>,
}

impl StuckPart {
    fn new(region: &'static str) -> Self {
        StuckPart {
            region,
            stuck: Vec::new(),
            waiting: Vec::new(),
            unknown: Vec::new(),
        }
    }
    fn unread(region: &'static str, why: &str) -> Self {
        StuckPart {
            unknown: vec![why.to_string()],
            ..StuckPart::new(region)
        }
    }
}

/// Hours from a stamp to `now`: an RFC 3339 instant, or a bare date read
/// at its midnight (a dock row's `parked_since` is `opened_on`).
fn hours_since(stamp: &str, now: Instant) -> Option<i64> {
    stamp_instant(stamp).map(|t| (now - t).num_hours())
}

/// RECEIVING: intake packets past the [`AGING_DAYS`] triage band — the
/// band [`receiving`] turns attention on, read the same way (whole days since
/// `opened_on`). Every one is ours: nothing has taken it in.
fn receiving_stuck(inputs: &RegionInputs<'_>) -> StuckPart {
    let Some(standing) = receiving_standing(inputs) else {
        return StuckPart::unread(
            "receiving",
            "receiving: the workflow registry that names the inbound kinds could not be read",
        );
    };
    let today = inputs.now.date_naive();
    StuckPart {
        stuck: standing
            .into_iter()
            .filter(|j| (today - j.opened_on).num_days() > AGING_DAYS)
            .map(|j| {
                let age = opened_at(j).map(|t| (inputs.now - t).num_hours());
                (j.id.to_string(), age)
            })
            .collect(),
        ..StuckPart::new("receiving")
    }
}

/// THE STATIONS: packets standing at a station over its WIP limit, or at
/// one nothing left in the window — [`marshalling`]'s own two troubles.
/// A station whose flow the cube is blind to, with work standing, is a
/// question mark and never a zero (design cf820810 Q5); one with nothing
/// standing holds nothing that could be stuck.
fn stations_stuck(inputs: &RegionInputs<'_>) -> StuckPart {
    // Marshalling's own packets only (design 62de32ae decision 4): one
    // still in receiving is that region's to be stuck in, and one held
    // by a region of its own is stuck — or not — there.
    let stations = match marshalling_view(inputs) {
        Ok(view) => view,
        Err(why) => return StuckPart::unread("marshalling", &format!("marshalling: {why}")),
    };
    // A packet at a station that served nothing is WAITING when every
    // step that put it there waits on its named owner inside its declared
    // bound — the region's own attention arm, read per packet — and stuck
    // otherwise (backlog 6254e602). Over the limit, every member is stuck.
    let mut part = StuckPart::new("marshalling");
    for s in &stations {
        for m in &s.members {
            if s.over_limit {
                part.stuck.push((m.clone(), None));
            } else if super::marshalling::not_draining(s) {
                match super::marshalling::member_wait(s, m, inputs.internal_protocols, inputs.now) {
                    Ok(_) => part.waiting.push(m.clone()),
                    Err(_) => part.stuck.push((m.clone(), None)),
                }
            }
        }
    }
    part.unknown = stations
        .iter()
        .filter_map(super::marshalling::unread_station_flow)
        .collect();
    part
}

/// THE GARAGE: cars held on the dock by hand and greens held before
/// parking — the yard's own held lanes. A hold is a brake someone here
/// set, so releasing it is ours.
///
/// AND WHAT TROUBLES IT: greens no car claims (stranded) and gate-runs
/// never judged (limbo), once they have stood past the garage's own
/// grace, [`bands::GARAGE_STRANDED`] — the same line that turns the
/// card troubled, so the card and this count cross it together. Car 2
/// left them out and the garage could read troubled beside "stuck 0";
/// the operator decided from the company frame that they count
/// (backlog 4142d821, car 2's handback): a green nobody parks and a
/// question the gate never answered each wait on nobody but us. Inside
/// the grace a green is still becoming a car, so it counts as neither.
fn garage_stuck(inputs: &RegionInputs<'_>) -> StuckPart {
    let s = inputs.status;
    let mut part = StuckPart::new("garage");
    for held in &s.held_cars {
        let qualified = inputs
            .cars
            .iter()
            .find(|(car, _)| car.id.to_string() == held.car.id)
            .is_some_and(|(car, steps)| {
                super::garage::recorded_review_wait(car, steps, inputs).is_ok()
            });
        if qualified && held.kind != crate::yard::CarHoldKind::RegateRed {
            part.waiting.push(held.car.id.clone());
        } else {
            part.stuck.push((
                held.car.id.clone(),
                hours_since(&held.car.parked_since, inputs.now),
            ));
        }
    }
    let greens = s
        .held
        .iter()
        .map(|g| (g.packet_id.clone(), hours_since(&g.since, inputs.now)));
    // A `since` the record cannot date is past the grace: an unknown
    // onset is stated at once (region_states), never read as a pass.
    let past_grace = |since: &str| {
        stamp_instant(since)
            .is_none_or(|t| (inputs.now - t).num_minutes() >= bands::GARAGE_STRANDED.hold_minutes)
    };
    let unclaimed = s
        .stranded
        .iter()
        .map(|g| (&g.packet_id, &g.since))
        .chain(s.limbo.iter().map(|l| (&l.packet_id, &l.since)))
        .filter(|(_, since)| past_grace(since))
        .map(|(id, since)| (id.clone(), hours_since(since, inputs.now)));
    part.stuck.extend(greens.chain(unclaimed));
    part
}

/// THE DOCK: parked cars held on a declared ordering edge, judged by the
/// conductor's own function ([`dock_edges`]). Its two hold kinds ARE the
/// split: an edge that can never clear waits for a person here (stuck); a
/// predecessor still in flight clears itself (waiting). An edge nobody
/// could judge is unknown — the conductor boards it anyway, and the dock
/// says so.
fn dock_stuck(inputs: &RegionInputs<'_>) -> StuckPart {
    use crate::car::{EDGE_HOLD_NEEDS_HUMAN, EdgeOutcome};
    if inputs.dock_reading == Reading::Unread {
        return StuckPart::unread(
            "dock",
            "dock: the loading-dock station row could not be read",
        );
    }
    let mut part = StuckPart::new("dock");
    let mut unjudged = 0usize;
    for (car, outcome) in dock_edges(inputs) {
        match outcome {
            EdgeOutcome::Hold(h) if h.kind == EDGE_HOLD_NEEDS_HUMAN => part
                .stuck
                .push((car.id.clone(), hours_since(&car.parked_since, inputs.now))),
            EdgeOutcome::Hold(_) => part.waiting.push(car.id.clone()),
            EdgeOutcome::BoardUnjudged(_) => unjudged += 1,
            EdgeOutcome::Board => {}
        }
    }
    if unjudged > 0 {
        part.unknown.push(format!(
            "dock: {} could not be read",
            plural(unjudged, "ordering edge", "ordering edges")
        ));
    }
    part
}

/// THE SHED: immediate proof faults and cars past [`PROOF_STALE_HOURS`], split by
/// [`stale_proof`] — the shed's own answer to whose move each is.
fn shed_stuck(inputs: &RegionInputs<'_>) -> StuckPart {
    let mut part = StuckPart::new("shed");
    for (j, steps) in awaiting_proof(inputs.cars) {
        let opened = proof_opened_at(j);
        let age_seconds = opened.map(|opened| (inputs.now - opened).num_seconds());
        let hours = opened.map(|opened| (inputs.now - opened).num_hours());
        match super::stale_proof::recorded_proof_wait(j, steps, inputs.now) {
            Ok(Some(_)) => {
                part.waiting.push(j.id.to_string());
                continue;
            }
            Err(_) => {
                part.stuck.push((j.id.to_string(), hours));
                continue;
            }
            Ok(None) => {}
        }
        if super::stale_proof::proof_requires_action(&j.metadata, age_seconds) {
            part.stuck.push((j.id.to_string(), hours));
            continue;
        }
        let Some(hours) = hours else {
            continue;
        };
        if hours < PROOF_STALE_HOURS {
            continue;
        }
        match stale_proof(&j.metadata, hours) {
            StaleProof::Theirs(_) => part.waiting.push(j.id.to_string()),
            _ => part.stuck.push((j.id.to_string(), Some(hours))),
        }
    }
    part
}

/// ONE THIRD from its regions' parts: each packet counted once, a packet
/// stuck anywhere is not also waiting, and the owners named in map order.
fn third_stuck(third: &str, parts: &[&StuckPart]) -> ThirdStuck {
    let mut stuck: std::collections::BTreeMap<&str, Option<i64>> = Default::default();
    for (id, age) in parts.iter().flat_map(|p| p.stuck.iter()) {
        let e = stuck.entry(id.as_str()).or_insert(None);
        *e = (*e).max(*age);
    }
    let waiting: std::collections::BTreeSet<&str> = parts
        .iter()
        .flat_map(|p| p.waiting.iter().map(String::as_str))
        .filter(|id| !stuck.contains_key(id))
        .collect();
    ThirdStuck {
        third: third.to_string(),
        stuck: stuck.len(),
        waiting: waiting.len(),
        unknown: parts.iter().flat_map(|p| p.unknown.clone()).collect(),
        oldest_hours: stuck.values().flatten().copied().max(),
        regions: REGIONS
            .iter()
            .filter(|name| {
                parts.iter().any(|p| {
                    p.region == **name
                        && !(p.stuck.is_empty() && p.waiting.is_empty() && p.unknown.is_empty())
                })
            })
            .map(|name| (*name).to_string())
            .collect(),
    }
}

/// WHAT IS STUCK, per third, in [`THIRDS`] order — the union of five
/// populations that already have an owner, with no new measure (design
/// cf820810 Q2): intake past its triage band and stations not draining
/// or over their limit (queue management); cars and greens held by hand,
/// and greens no car claims and gate-runs never judged past the garage's
/// grace (actors building); cars held on an ordering edge and landed cars past
/// a day unproven (delivery). Each part is its region's own predicate,
/// so every number clicks back to the card that owns it.
pub fn stuck(inputs: &RegionInputs<'_>) -> Vec<ThirdStuck> {
    let parts = stuck_parts(inputs);
    THIRDS
        .iter()
        .map(|(third, owned)| {
            let mine: Vec<&StuckPart> =
                parts.iter().filter(|p| owned.contains(&p.region)).collect();
            third_stuck(third, &mine)
        })
        .collect()
}

/// The five populations [`stuck`] is the union of, each its region's own.
fn stuck_parts(inputs: &RegionInputs<'_>) -> [StuckPart; 5] {
    [
        receiving_stuck(inputs),
        stations_stuck(inputs),
        garage_stuck(inputs),
        dock_stuck(inputs),
        shed_stuck(inputs),
    ]
}

/// EVERY PACKET [`stuck`] COUNTS, by id, before it is folded into
/// thirds. The IT map's borders class what stands at each rail by it
/// (design 31bade8f decision 8, car M1 on backlog d220022f), so a rail's
/// red sediment and the HUD's stuck block are one predicate and cannot
/// disagree.
pub(crate) fn stuck_ids(inputs: &RegionInputs<'_>) -> std::collections::BTreeSet<String> {
    stuck_parts(inputs)
        .into_iter()
        .flat_map(|p| p.stuck.into_iter().map(|(id, _)| id))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::regions::fixtures::*;

    fn third<'a>(r: &'a Regions, name: &str) -> &'a ThirdStuck {
        r.stuck
            .iter()
            .find(|t| t.third == name)
            .unwrap_or_else(|| panic!("no third {name}: {:?}", r.stuck))
    }

    /// A landed car at `proven`, opened at `opened`, with this metadata.
    fn landed(branch: &str, opened: &str, extra: Value) -> (Job, Vec<Step>) {
        let mut md = json!({ "branch": branch, "merged": true, "opened_at": opened });
        if let (Some(dst), Some(src)) = (md.as_object_mut(), extra.as_object()) {
            for (k, v) in src {
                dst.insert(k.clone(), v.clone());
            }
        }
        let j = job("ship-a-change", branch, JobStatus::Open, md);
        let s = vec![
            step(&j, "gate", StepStatus::Completed, Some(opened)),
            step(&j, "proven", StepStatus::Ready, None),
        ];
        (j, s)
    }

    /// The three thirds, in the operator surface's reading order, each
    /// answered even when nothing is stuck — an absent third would read
    /// as a third nobody asked about. And the block rides the payload
    /// under `stuck`, while a payload from an older server (no block)
    /// still reads back.
    #[test]
    fn the_stuck_block_answers_every_third_and_an_empty_yard_has_nothing_stuck() {
        let status = empty_status();
        let out = regions(&inputs(&status, &[], &[], &[], &[], Some(&[]), Some(&[])));
        let names: Vec<&str> = out.stuck.iter().map(|t| t.third.as_str()).collect();
        assert_eq!(names, ["queue-management", "actors-building", "delivery"]);
        for t in &out.stuck {
            assert_eq!((t.stuck, t.waiting, t.oldest_hours), (0, 0, None), "{t:?}");
            assert!(t.unknown.is_empty() && t.regions.is_empty(), "{t:?}");
        }
        let v = serde_json::to_value(&out).unwrap();
        let first = &v["stuck"][0];
        for key in [
            "third",
            "stuck",
            "waiting",
            "unknown",
            "oldest_hours",
            "regions",
        ] {
            assert!(first.get(key).is_some(), "the block carries {key}: {first}");
        }
        let older: Regions =
            serde_json::from_value(json!({ "window_hours": 24, "regions": [] })).unwrap();
        assert!(older.stuck.is_empty());
    }

    #[test]
    fn immediate_proof_faults_are_counted_before_the_age_band() {
        let status = empty_status();
        for metadata in [
            json!({}),
            json!({"proof_probe": "true", "proof_attempt": {"exit": 1}}),
            json!({"proof_probe": "true", "proof_attempt": {"exit": null}}),
            json!({"proof_probe": "true", "proof_attempt": {"exit": 75},
                "waits_on": {"on": "the event", "seen": "true"}}),
            json!({"proof_probe": "true", "proof_attempt": {"exit": 75},
                "waits_on": {"on": "the event", "owner": "world"}}),
        ] {
            let cars = vec![landed("fix/immediate", "2026-09-19T11:00:00Z", metadata)];
            let out = regions(&inputs(&status, &[], &[], &cars, &[], Some(&[]), Some(&[])));
            assert_eq!(by_name(&out, "shed").state, RegionState::Troubled);
            let delivery = third(&out, "delivery");
            assert_eq!(
                delivery.stuck, 1,
                "a red proof fault must be counted: {delivery:?}"
            );
            assert_eq!(delivery.waiting, 0);
        }
        let cars = vec![landed(
            "fix/scheduled",
            "2026-09-19T11:00:00Z",
            json!({"proof_probe": "true"}),
        )];
        let out = regions(&inputs(&status, &[], &[], &cars, &[], Some(&[]), Some(&[])));
        assert_eq!(third(&out, "delivery").stuck, 0);
    }

    #[test]
    fn a_declared_wait_deadline_is_not_hidden_by_the_stale_band() {
        let status = empty_status();
        let car = |opened| {
            landed(
                "fix/bounded-wait",
                opened,
                observed_wait_fixture(json!({
                    "proof_probe": "true", "proof_attempt": {"exit": 75, "waits_on_exit": 1},
                    "waits_on": {"on": "the event", "seen": "true", "owner": "world", "max_wait_hours": 1}
                })),
            )
        };
        let cars = vec![car("2026-09-19T09:00:00Z")];
        let out = regions(&inputs(&status, &[], &[], &cars, &[], Some(&[]), Some(&[])));
        let shed = by_name(&out, "shed");
        assert_eq!(shed.state, RegionState::Troubled, "{}", shed.why);
        assert!(shed.why.contains("max wait") && shed.why.contains("fix/bounded-wait"));
        assert_eq!(third(&out, "delivery").stuck, 1);
        // One minute past the recorded bound is overdue too; rounding
        // down to whole hours cannot grant another hour of silence.
        let cars = vec![car("2026-09-19T10:59:00Z")];
        let out = regions(&inputs(&status, &[], &[], &cars, &[], Some(&[]), Some(&[])));
        assert_eq!(by_name(&out, "shed").state, RegionState::Troubled);
        assert_eq!(third(&out, "delivery").stuck, 1);
        let cars = vec![car("2026-09-19T11:00:00Z")];
        let out = regions(&inputs(&status, &[], &[], &cars, &[], Some(&[]), Some(&[])));
        assert_eq!(by_name(&out, "shed").state, RegionState::Attention);
        assert_eq!(third(&out, "delivery").stuck, 0);
    }

    #[test]
    fn mutable_metadata_cannot_move_a_native_wait_deadline() {
        let status = empty_status();
        let mut car = landed(
            "fix/native-clock",
            "2026-09-19T11:00:00Z",
            observed_wait_fixture(json!({
                "proof_probe": "true", "proof_attempt": {"exit": 75, "waits_on_exit": 1},
                "waits_on": {"on": "the event", "seen": "true", "owner": "world", "max_wait_hours": 1}
            })),
        );
        car.0.opened_at = Some(t("2026-09-19T09:00:00Z"));
        let cars = vec![car.clone()];
        let out = regions(&inputs(&status, &[], &[], &cars, &[], Some(&[]), Some(&[])));
        assert_eq!(by_name(&out, "shed").state, RegionState::Troubled);
        assert_eq!(third(&out, "delivery").stuck, 1);
        // The reverse mismatch cannot invent an overdue native record.
        car.0.opened_at = Some(t("2026-09-19T11:00:00Z"));
        car.0.metadata["opened_at"] = json!("2026-09-19T09:00:00Z");
        let cars = vec![car];
        let out = regions(&inputs(&status, &[], &[], &cars, &[], Some(&[]), Some(&[])));
        assert_eq!(by_name(&out, "shed").state, RegionState::Attention);
        assert_eq!(third(&out, "delivery").stuck, 0);
    }

    #[test]
    fn an_unread_clock_does_not_erase_a_known_proof_fault() {
        let status = empty_status();
        let mut car = landed(
            "fix/unread-clock",
            "2026-09-19T11:00:00Z",
            json!({
                "proof_probe": "true", "proof_attempt": {"exit": 1}
            }),
        );
        car.0.metadata.as_object_mut().unwrap().remove("opened_at");
        assert!(car.0.opened_at.is_none());
        let cars = vec![car];
        let out = regions(&inputs(&status, &[], &[], &cars, &[], Some(&[]), Some(&[])));
        assert_eq!(by_name(&out, "shed").state, RegionState::Troubled);
        let delivery = third(&out, "delivery");
        assert_eq!(delivery.stuck, 1);
        assert_eq!(delivery.oldest_hours, None, "no timestamp is invented");
    }

    #[test]
    fn a_satisfied_condition_with_a_not_yet_proof_requires_action_now() {
        let status = empty_status();
        let car = |seen: Value| {
            landed(
                "fix/seen-event",
                "2026-09-19T11:00:00Z",
                observed_wait_fixture(json!({
                    "proof_probe": "true", "proof_attempt": {"at": NOW, "probe": "true", "exit": 75, "waits_on_exit": 0,
                        "waits_on_seen_at": seen},
                    "waits_on": {"on": "the recorded event", "seen": "true", "owner": "world", "max_wait_hours": 24}
                })),
            )
        };
        let cars = vec![car(json!("2026-09-19T11:59:00Z"))];
        let out = regions(&inputs(&status, &[], &[], &cars, &[], Some(&[]), Some(&[])));
        let shed = by_name(&out, "shed");
        assert_eq!(shed.state, RegionState::Troubled, "{}", shed.why);
        assert!(shed.why.contains("seen") && shed.why.contains("fix/seen-event"));
        assert_eq!(third(&out, "delivery").stuck, 1);
        let mut control = car(Value::Null);
        control.0.metadata["proof_attempt"]["waits_on_exit"] = json!(1);
        control.0.metadata["proof_attempt"]["waits_on_observation"] = json!("unseen");
        let cars = vec![control];
        let out = regions(&inputs(&status, &[], &[], &cars, &[], Some(&[]), Some(&[])));
        assert_eq!(by_name(&out, "shed").state, RegionState::Attention);
        assert_eq!(third(&out, "delivery").stuck, 0);
    }

    /// DELIVERY: stuck and waiting side by side, never summed. A landed
    /// car past a day whose move is ours is stuck; one waiting on its
    /// declared owner is waiting. A parked car behind an edge that can
    /// never clear is stuck; one behind a predecessor still in flight is
    /// waiting — the conductor's own two hold kinds. A car inside its
    /// day is neither. The oldest age is the oldest STUCK packet, and the
    /// owning regions are the click-through, in map order.
    #[test]
    fn delivery_counts_stuck_and_waiting_apart_from_the_dock_and_the_shed() {
        // NOW is 2026-09-19T12:00:00Z.
        let first = parked("fix/first-half", json!({}));
        let first_id = first.0.id.to_string();
        let behind = parked("fix/second-half", json!({ "boards_after": first_id }));
        let gone = "cccccccc-1111-2222-3333-444444444444".to_string();
        let orphan = parked("fix/orphan", json!({ "boards_after": gone }));
        let never_run = landed(
            "fix/never-run",
            "2026-09-16T12:00:00Z",
            json!({ "proof_probe": "true" }),
        );
        let world = landed(
            "feat/stripe",
            "2026-09-15T10:00:00Z",
            observed_wait_fixture(json!({
                "proof_probe": "true",
                "proof_attempt": {
                    "at": "2026-09-19T11:00:00Z", "not_yet": true, "exit": 75, "probe": "true", "waits_on_exit": 1,
                    (crate::car::NOT_YET_SINCE): "2026-09-15T11:00:00Z",
                    (crate::car::NOT_YET_RUNS): 96,
                },
                (crate::car::WAITS_ON): {
                    "on": "a Stripe charge", "seen": "true",
                    (crate::car::WAITS_ON_OWNER): "world",
                },
            })),
        );
        let fresh = landed(
            "fix/fresh",
            "2026-09-19T06:00:00Z",
            json!({ "proof_probe": "true" }),
        );
        let status = dock_at_depth(&[first.clone(), behind.clone(), orphan.clone()]);
        let cars = vec![first.clone(), behind, orphan, never_run, world, fresh];
        let preds = vec![
            (
                first_id,
                crate::car::Predecessor::Found(packet_value(&first.0, &first.1)),
            ),
            (gone, crate::car::Predecessor::Absent),
        ];
        let mut i = inputs(&status, &[], &[], &cars, &[], Some(&[]), Some(&[]));
        i.predecessors = &preds;
        let out = regions(&i);
        let d = third(&out, "delivery");
        assert_eq!(
            d.stuck, 2,
            "the orphan's edge and the never-run probe are ours: {d:?}"
        );
        assert_eq!(
            d.waiting, 2,
            "the in-flight predecessor and the world's event are not: {d:?}"
        );
        assert_eq!(
            d.oldest_hours,
            Some(72),
            "the never-run car, open three days: {d:?}"
        );
        assert_eq!(d.regions, ["dock", "shed"]);
        assert!(d.unknown.is_empty(), "{d:?}");
        // Per third, never across: nothing here leaks into the others.
        assert_eq!(third(&out, "queue-management").stuck, 0);
        assert_eq!(third(&out, "actors-building").stuck, 0);
    }

    /// QUEUE MANAGEMENT: intake past the three-day triage band, and the
    /// packets standing at a station that is not draining or is over its
    /// WIP limit — each packet ONCE, though it stands in receiving and at
    /// a station both. A station whose flow the cube cannot count is a
    /// question mark, never a zero (design cf820810 Q5): the third names
    /// it in `unknown`, so its stuck figure reads as a floor. A blind
    /// station with nothing standing holds nothing that could be stuck.
    #[test]
    fn queue_management_counts_each_stuck_packet_once_and_a_blind_station_is_unknown() {
        let mut aging = job("backlog-item", "aging", JobStatus::Open, json!({}));
        aging.opened_on = chrono::NaiveDate::from_ymd_opt(2026, 9, 14).unwrap();
        let mut fresh = job("user-feedback", "fresh", JobStatus::Open, json!({}));
        fresh.opened_on = chrono::NaiveDate::from_ymd_opt(2026, 9, 18).unwrap();
        let aging_id = aging.id.to_string();
        let inbound = untaken(vec![aging, fresh]);
        let station =
            |name: &str, members: &[&str], served: Option<i64>, over: bool| StationReading {
                name: name.into(),
                over_limit: over,
                members: members.iter().map(|m| (*m).to_string()).collect(),
                served,
                flow_unread: None,
                previous_served: served,
                opened: Default::default(),
                standing: Vec::new(),
            };
        let stations = vec![
            station("design-review", &["px", aging_id.as_str()], Some(0), false),
            station("q.platform-admin.task", &["p1"], Some(3), false),
            station("over", &["p9"], Some(5), true),
            station("a.platform-admin.opus-5-1m", &["b1", "b2"], None, false),
            station("my-watchlist", &[], None, false),
        ];
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
        let q = third(&out, "queue-management");
        assert_eq!(
            q.stuck, 3,
            "aging (once), px not draining, p9 over the limit: {q:?}"
        );
        assert_eq!(q.waiting, 0);
        assert_eq!(
            q.oldest_hours,
            Some(132),
            "aging opened 2026-09-14, read at its midnight: {q:?}"
        );
        assert_eq!(q.regions, ["receiving", "marshalling"]);
        assert_eq!(q.unknown.len(), 1, "{q:?}");
        assert!(
            q.unknown[0].contains("a.platform-admin.opus-5-1m")
                && q.unknown[0].contains("2 standing"),
            "name the blind station and what stands there: {q:?}"
        );
    }

    /// Unknown is not zero at the level of a whole read, too: a region
    /// whose input could not be read puts its reason in its third's
    /// `unknown` and names itself as an owner, so the click-through leads
    /// to the card that already says "could not be read".
    #[test]
    fn an_unread_input_is_unknown_in_its_third_and_never_a_zero() {
        let status = empty_status();
        let mut i = inputs(&status, &[], &[], &[], &[], None, None);
        i.dock_reading = Reading::Unread;
        let out = regions(&i);
        let q = third(&out, "queue-management");
        assert_eq!(q.unknown.len(), 2, "{q:?}");
        assert_eq!(q.regions, ["receiving", "marshalling"]);
        let d = third(&out, "delivery");
        assert!(
            d.unknown.len() == 1 && d.unknown[0].contains("loading-dock"),
            "{d:?}"
        );
        assert_eq!(d.regions, ["dock"]);
    }

    /// ACTORS BUILDING: a car held on the dock by hand and a green held
    /// before parking — the garage's two held lanes, read off the yard's
    /// own judgement (`yard::HeldCar`, `yard::HeldGreen`).
    #[test]
    fn the_held_lanes_are_the_actors_thirds_stuck() {
        let mut held = job("ship-a-change", "fix/held", JobStatus::Open, json!({}));
        held.opened_on = chrono::NaiveDate::from_ymd_opt(2026, 9, 17).unwrap();
        let mut status = empty_status();
        status.held_cars = vec![crate::yard::HeldCar {
            car: crate::yard::dock_car(&held),
            reason: "wait for the migration".into(),
            kind: crate::yard::CarHoldKind::Operator,
            release: None,
            auto_release: None,
        }];
        status.held = vec![crate::yard::HeldGreen {
            branch: "feat/held-green".into(),
            reason: "not yet".into(),
            since: "2026-09-19T02:00:00Z".into(),
            packet_id: "g1".into(),
            sha: None,
        }];
        let out = regions(&inputs(&status, &[], &[], &[], &[], Some(&[]), Some(&[])));
        let a = third(&out, "actors-building");
        assert_eq!((a.stuck, a.waiting), (2, 0), "{a:?}");
        assert_eq!(a.oldest_hours, Some(60), "held since 2026-09-17: {a:?}");
        assert_eq!(a.regions, ["garage"]);
    }

    /// A STRANDED GREEN AND A NEVER-JUDGED GATE-RUN ARE STUCK once they
    /// have stood past the garage's own 15-minute grace (backlog 4142d821,
    /// car 2's handback, decided from the company frame): a troubled
    /// garage card must not sit beside "stuck 0". Inside the grace a
    /// green is still becoming a car, so it counts as neither.
    #[test]
    fn stranded_greens_and_never_judged_runs_past_the_grace_are_the_actors_thirds_stuck() {
        let mut status = empty_status();
        let green = |branch: &str, since: &str, id: &str| crate::yard::StrandedGreen {
            branch: branch.into(),
            packet_id: id.into(),
            sha: None,
            since: since.into(),
        };
        status.stranded = vec![
            green("fix/old-green", "2026-09-19T04:00:00Z", "g-old"),
            green("fix/fresh-green", "2026-09-19T11:55:00Z", "g-fresh"),
        ];
        status.limbo = vec![crate::yard::LimboCar {
            branch: "fix/lost".into(),
            verdict: "lost".into(),
            since: "2026-09-19T10:00:00Z".into(),
            packet_id: "g-lost".into(),
            sha: None,
        }];
        let out = regions(&inputs(&status, &[], &[], &[], &[], Some(&[]), Some(&[])));
        let a = third(&out, "actors-building");
        assert_eq!(
            (a.stuck, a.waiting),
            (2, 0),
            "the eight-hour green and the lost run; the five-minute green is inside the grace: {a:?}"
        );
        assert_eq!(a.oldest_hours, Some(8), "{a:?}");
        assert_eq!(a.regions, ["garage"]);
    }
}
