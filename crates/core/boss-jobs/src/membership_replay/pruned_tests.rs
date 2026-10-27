//! Synthetic conserved originals: verifier controls, not database-selection proof.
use super::pruned::{PrunedCensus, PrunedPacket};
use super::*;
use boss_clock_client::ClockMode;
use boss_clock_client::mapping_history::ClockMappingFact;
use boss_events::original_group::{GroupReference, OriginalGroup};
use chrono::TimeZone;
use serde_json::json;
use uuid::Uuid;

fn at(seconds: i64) -> DateTime<Utc> {
    Utc.timestamp_opt(1_700_000_000 + seconds, 0).unwrap()
}

fn fixture() -> (ReplayInput, StationSpec, VersionBasis) {
    let population: serde_json::Value = serde_json::from_str(include_str!(
        "../../tests/fixtures/station-predicate-population-20261016.json"
    ))
    .unwrap();
    let mut spec: StationSpec = serde_json::from_value(population["data"][0].clone()).unwrap();
    spec.name = "pruned-verifier.fixture".into();
    spec.predicate = crate::station_queue::StationPredicate {
        kind: Some("fixture".into()),
        status: Some(JobStatus::Closed),
        ..Default::default()
    };
    spec.terminal_window_days = Some(0);
    let event = boss_core::event::Event::new(
        "jobs",
        crate::events::STATION_PUBLISHED,
        json!(spec),
        at(-50),
    );
    let row = HistoricalEvent {
        seq: 3,
        created_at: Some(at(-50)),
        event,
    };
    let ids = vec![row.event.id];
    let original = OriginalGroup {
        reference: GroupReference {
            id: Uuid::new_v4(),
            schema_version: 1,
            event_count: 1,
            member_hash: OriginalGroup::roster_hash(&ids),
        },
        event_ids: ids,
        audit_sequences: vec![Some(3)],
        complete: true,
    };
    let first = ClockMappingFact {
        schema_version: 1,
        revision: "first".into(),
        previous_revision: None,
        epoch: Some("one".into()),
        mode: Some(ClockMode::Wall),
        params: None,
        captured_at: at(-100),
        audit_created_at: at(-99),
        seq: 1,
        complete_group: true,
    };
    let next = ClockMappingFact {
        revision: "next".into(),
        previous_revision: Some("first".into()),
        epoch: Some("two".into()),
        captured_at: at(10),
        audit_created_at: at(12),
        seq: 2,
        ..first.clone()
    };
    let mut input = ReplayInput {
        groups: vec![ReplayGroup {
            original: Some(original.clone()),
            rows: vec![row.clone()],
        }],
        since: at(5),
        until: at(20),
        audit_cutoff: 10,
        actor: Some("alice".into()),
        scope: JobScope::All,
        currently_authorized: Some(["quiet".into()].into_iter().collect()),
        clock: ClockTimeline::build(&[first, next], Some("next"), at(-100), at(20)),
        registry_prefix_complete: true,
        packet_prefix_complete: true,
        source_unavailable: vec![],
        initial: InitialState {
            cut: 5,
            pruned_terminal: 1,
            ..Default::default()
        },
    };
    input.initial.pruned_census = Some(PrunedCensus {
        schema: 1,
        cut: 5,
        audit_cutoff: 10,
        since: input.since,
        until: input.until,
        binding: projection::ProjectionBinding::from_input(&input),
        metadata_keys: vec![],
        packets: vec![PrunedPacket {
            id: "quiet".into(),
            job_seq: Some(4),
            last_seq: Some(4),
            last_at: Some(at(-40)),
            status: Some("closed".into()),
            closed_on: Some((at(0).date_naive() - chrono::Duration::days(2)).to_string()),
            kind: Some("fixture".into()),
            tags: Some(json!([])),
            metadata: Some(json!({})),
        }],
    });
    let basis = VersionBasis {
        version: spec.version,
        source: "authored-publication".into(),
        publication_sequences: vec![3],
    };
    (input, spec, basis)
}

fn certificate(input: &ReplayInput, spec: &StationSpec, basis: &VersionBasis) -> bool {
    super::pruned::exclusion(input, spec, basis, at(10), at(12), None).is_some()
}

#[test]
fn pruned_verifier_uses_audit_activation_not_the_earlier_command_time() {
    let (mut input, spec, basis) = fixture();
    assert!(certificate(&input, &spec, &basis));
    input.groups[0].rows[0].created_at = Some(at(10));
    assert!(
        !certificate(&input, &spec, &basis),
        "publication command time before the edge does not establish audit activation before it"
    );
}

#[test]
fn pruned_verifier_refuses_a_packet_touched_in_the_window() {
    let (mut input, spec, basis) = fixture();
    assert!(certificate(&input, &spec, &basis));
    let event = boss_core::event::Event::new(
        "jobs",
        crate::events::JOB_UPDATED,
        json!({"id": "quiet"}),
        at(6),
    );
    let ids = vec![event.id];
    input.groups.push(ReplayGroup {
        rows: vec![HistoricalEvent {
            seq: 6,
            created_at: Some(at(6)),
            event,
        }],
        original: Some(OriginalGroup {
            reference: GroupReference {
                id: Uuid::new_v4(),
                schema_version: 1,
                event_count: 1,
                member_hash: OriginalGroup::roster_hash(&ids),
            },
            event_ids: ids,
            audit_sequences: vec![Some(6)],
            complete: true,
        }),
    });
    assert!(
        !certificate(&input, &spec, &basis),
        "an otherwise complete census cannot certify a packet touched after its cut"
    );
}

fn set_spec(input: &mut ReplayInput, spec: &StationSpec) {
    // Change the actual original publication, rather than pass a mismatched spec.
    input.groups[0].rows[0].event.payload = json!(spec);
}

#[test]
fn pruned_verifier_positive_preserves_unknown_clock_and_original_source_cut() {
    let (input, spec, basis) = fixture();
    assert!(input.clock.date_at(at(11)).is_err());
    let certificate =
        super::pruned::exclusion(&input, &spec, &basis, at(10), at(12), None).unwrap();
    assert_eq!(certificate.source_cut, 5);
    assert_eq!(certificate.station_version, spec.version);
    assert_eq!(certificate.publication_sequences, vec![3]);
    assert_eq!(certificate.packets, 1);
    assert_eq!((certificate.since, certificate.until), (at(10), at(12)));
}

#[test]
fn pruned_verifier_refuses_incomplete_duplicate_or_unbound_census() {
    type Mutation = fn(&mut ReplayInput);
    let cases: &[(&str, Mutation)] = &[
        ("missing packet", |i| {
            i.initial.pruned_census.as_mut().unwrap().packets.clear();
        }),
        ("extra packet", |i| {
            let w = i.initial.pruned_census.as_mut().unwrap();
            let mut p = w.packets[0].clone();
            p.id = "extra".into();
            w.packets.push(p);
        }),
        ("duplicate IDs at matching scalar count", |i| {
            let w = i.initial.pruned_census.as_mut().unwrap();
            w.packets.push(w.packets[0].clone());
            i.initial.pruned_terminal = 2;
        }),
        ("unauthorized substitute", |i| {
            i.initial.pruned_census.as_mut().unwrap().packets[0].id = "denied".into();
        }),
        ("scalar only", |i| i.initial.pruned_census = None),
        ("wrong caller", |i| i.actor = Some("bob".into())),
        ("wrong scope", |i| i.scope = JobScope::None),
        ("changed authorization", |i| {
            i.currently_authorized = Some(BTreeSet::new())
        }),
        ("wrong source cut", |i| i.initial.cut += 1),
        ("wrong audit cut", |i| i.audit_cutoff += 1),
        ("wrong window start", |i| i.since = at(6)),
        ("wrong window end", |i| i.until = at(21)),
        ("legacy witness", |i| {
            i.initial.pruned_census.as_mut().unwrap().schema = 0
        }),
        ("missing snapshot", |i| {
            i.initial.pruned_census.as_mut().unwrap().packets[0].job_seq = None
        }),
        ("snapshot after last original", |i| {
            i.initial.pruned_census.as_mut().unwrap().packets[0].job_seq = Some(5)
        }),
        ("last original after cut", |i| {
            i.initial.pruned_census.as_mut().unwrap().packets[0].last_seq = Some(6)
        }),
        ("missing original capture", |i| {
            i.initial.pruned_census.as_mut().unwrap().packets[0].last_at = None
        }),
        ("original at window start", |i| {
            i.initial.pruned_census.as_mut().unwrap().packets[0].last_at = Some(i.since)
        }),
        ("incomplete registry prefix", |i| {
            i.registry_prefix_complete = false
        }),
        ("incomplete packet prefix", |i| {
            i.packet_prefix_complete = false
        }),
        ("missing registry carrier", |i| i.groups.clear()),
        ("incomplete registry group", |i| {
            i.groups[0].original.as_mut().unwrap().complete = false
        }),
        ("changed publication payload", |i| {
            i.groups[0].rows[0].event.payload["version"] = json!(999)
        }),
    ];
    for (name, mutate) in cases {
        let (mut input, spec, basis) = fixture();
        assert!(
            certificate(&input, &spec, &basis),
            "positive fixture: {name}"
        );
        mutate(&mut input);
        assert!(!certificate(&input, &spec, &basis), "must refuse {name}");
    }
}

#[test]
fn pruned_verifier_requires_exact_station_basis_and_supported_job_predicate() {
    let (input, spec, basis) = fixture();
    for (name, changed) in [
        (
            "wrong version",
            VersionBasis {
                version: basis.version + 1,
                ..basis.clone()
            },
        ),
        (
            "derived station",
            VersionBasis {
                source: "workflow".into(),
                ..basis.clone()
            },
        ),
        (
            "wrong publication",
            VersionBasis {
                publication_sequences: vec![4],
                ..basis.clone()
            },
        ),
    ] {
        assert!(!certificate(&input, &spec, &changed), "must refuse {name}");
    }
    let (mut input, mut spec, basis) = fixture();
    spec.predicate.step = Some(Default::default());
    set_spec(&mut input, &spec);
    assert!(
        !certificate(&input, &spec, &basis),
        "Step truth is not in a pruned Job census"
    );
    let (mut input, mut spec, basis) = fixture();
    spec.predicate.metadata_present = vec!["required-original".into()];
    set_spec(&mut input, &spec);
    assert!(
        !certificate(&input, &spec, &basis),
        "unconserved key cannot manufacture a nonmatch"
    );
    input
        .initial
        .pruned_census
        .as_mut()
        .unwrap()
        .metadata_keys
        .push("required-original".into());
    assert!(
        certificate(&input, &spec, &basis),
        "conserved absent key genuinely excludes this packet"
    );
}

#[test]
fn pruned_verifier_source_unavailability_uses_half_open_partition() {
    let (mut input, spec, basis) = fixture();
    for (from, to, expected) in [
        (9, 10, true),
        (12, 13, true),
        (9, 11, false),
        (11, 13, false),
    ] {
        input.source_unavailable = vec![Unavailable {
            since: at(from),
            until: at(to),
            reason: "pending original".into(),
        }];
        assert_eq!(
            certificate(&input, &spec, &basis),
            expected,
            "[{from},{to}) against [10,12)"
        );
    }
}

#[test]
fn pruned_expiry_inequality_matches_the_shared_retention_predicate() {
    for days in [0, 1, 7] {
        for age in [-1, 0, 1, 2, 8, 30] {
            let (mut input, mut spec, basis) = fixture();
            spec.terminal_window_days = Some(days);
            set_spec(&mut input, &spec);
            let closed = at(0).date_naive() - chrono::Duration::days(age);
            input.initial.pruned_census.as_mut().unwrap().packets[0].closed_on =
                Some(closed.to_string());
            let mut job = Job::new(
                "fixture",
                boss_core::job::Subject::new("custom", "test"),
                "Shared retention",
                "alice",
                boss_core::job::Priority::Standard,
                at(0).date_naive(),
            );
            job.status = JobStatus::Closed;
            job.closed_on = Some(closed);
            assert_eq!(
                certificate(&input, &spec, &basis),
                !crate::station_queue::in_window(&job, at(10).date_naive(), Some(days)),
                "closed age{age}, terminal window{days}"
            );
        }
    }
}

#[test]
fn pruned_verifier_keeps_microsecond_midnight_expiry_boundaries() {
    let (mut input, spec, basis) = fixture();
    let midnight = at(0)
        .date_naive()
        .succ_opt()
        .unwrap()
        .and_hms_opt(0, 0, 0)
        .unwrap()
        .and_utc();
    let first = ClockMappingFact {
        schema_version: 1,
        revision: "first".into(),
        previous_revision: None,
        epoch: Some("one".into()),
        mode: Some(ClockMode::Wall),
        params: None,
        captured_at: at(-100),
        audit_created_at: at(-99),
        seq: 1,
        complete_group: true,
    };
    let next = ClockMappingFact {
        revision: "next".into(),
        previous_revision: Some("first".into()),
        epoch: Some("two".into()),
        captured_at: midnight - chrono::Duration::microseconds(1),
        audit_created_at: midnight + chrono::Duration::microseconds(3),
        seq: 2,
        ..first.clone()
    };
    input.since = midnight - chrono::Duration::microseconds(2);
    input.until = midnight + chrono::Duration::microseconds(4);
    input.clock = ClockTimeline::build(
        &[first.clone(), next.clone()],
        Some("next"),
        at(-100),
        input.until,
    );
    let witness = input.initial.pruned_census.as_mut().unwrap();
    witness.since = input.since;
    witness.until = input.until;
    witness.packets[0].closed_on = Some(at(0).date_naive().to_string());
    assert!(input.clock.date_at(midnight).is_err());
    assert!(
        super::pruned::exclusion(
            &input,
            &spec,
            &basis,
            midnight - chrono::Duration::microseconds(1),
            midnight,
            None
        )
        .is_none()
    );
    assert!(
        super::pruned::exclusion(
            &input,
            &spec,
            &basis,
            midnight,
            midnight + chrono::Duration::microseconds(1),
            None
        )
        .is_none(),
        "expiry at the inclusive start needs the preceding original microsecond"
    );
    assert!(
        input
            .clock
            .date_at(midnight + chrono::Duration::microseconds(1))
            .is_err()
    );
    assert!(
        super::pruned::exclusion(
            &input,
            &spec,
            &basis,
            midnight + chrono::Duration::microseconds(1),
            midnight + chrono::Duration::microseconds(2),
            None
        )
        .is_some(),
        "already expired after the edge is a nonvacuous positive inside unknown clock coverage"
    );
    assert!(
        super::pruned::exclusion(
            &input,
            &spec,
            &basis,
            midnight + chrono::Duration::microseconds(3),
            midnight + chrono::Duration::microseconds(4),
            None
        )
        .is_some()
    );
    input.initial.pruned_census.as_mut().unwrap().packets[0].closed_on =
        Some((at(0).date_naive() - chrono::Duration::days(2)).to_string());
    assert!(
        super::pruned::exclusion(
            &input,
            &spec,
            &basis,
            midnight - chrono::Duration::microseconds(1),
            midnight + chrono::Duration::microseconds(1),
            None
        )
        .is_some(),
        "an older expired original excludes both sides of the boundary"
    );
    let mut changed = input.clone();
    let mut previous = spec.clone();
    previous.terminal_window_days = Some(1);
    set_spec(&mut changed, &previous);
    changed.initial.pruned_census.as_mut().unwrap().packets[0].closed_on =
        Some((at(0).date_naive() - chrono::Duration::days(1)).to_string());
    let mut current = spec.clone();
    current.version += 1;
    let event = boss_core::event::Event::new(
        "jobs",
        crate::events::STATION_PUBLISHED,
        json!(current),
        midnight,
    );
    let ids = vec![event.id];
    let group = OriginalGroup {
        reference: GroupReference {
            id: Uuid::new_v4(),
            schema_version: 1,
            event_count: 1,
            member_hash: OriginalGroup::roster_hash(&ids),
        },
        event_ids: ids,
        audit_sequences: vec![Some(6)],
        complete: true,
    };
    let row = HistoricalEvent {
        seq: 6,
        created_at: Some(midnight),
        event,
    };
    changed.groups.push(ReplayGroup {
        rows: vec![row.clone()],
        original: Some(group.clone()),
    });
    let current_basis = VersionBasis {
        version: current.version,
        source: "authored-publication".into(),
        publication_sequences: vec![6],
    };
    assert!(
        changed
            .clock
            .outcome_date_bounds(
                midnight - chrono::Duration::microseconds(1),
                midnight + chrono::Duration::microseconds(1),
                changed.audit_cutoff
            )
            .is_ok()
    );
    assert!(
        super::pruned::exclusion(
            &changed,
            &current,
            &current_basis,
            midnight,
            midnight + chrono::Duration::microseconds(1),
            None
        )
        .is_none(),
        "the new version's absence cannot prove the preceding version's boundary crossing absent"
    );
    assert!(
        super::pruned::exclusion(
            &changed,
            &current,
            &current_basis,
            midnight + chrono::Duration::nanoseconds(500),
            midnight + chrono::Duration::microseconds(2),
            None
        )
        .is_none(),
        "original captured_at can retain nanoseconds: publication must govern the actual preceding microsecond"
    );
    assert!(
        super::pruned::exclusion(
            &changed,
            &current,
            &current_basis,
            midnight + chrono::Duration::microseconds(1),
            midnight + chrono::Duration::microseconds(2),
            None
        )
        .is_some(),
        "the same original version is established before a later proof partition"
    );
    input.since = midnight;
    input.initial.pruned_census.as_mut().unwrap().since = midnight;
    input.clock = ClockTimeline::build(&[first, next], Some("next"), midnight, input.until);
    assert!(
        super::pruned::exclusion(
            &input,
            &spec,
            &basis,
            midnight,
            midnight + chrono::Duration::microseconds(1),
            None
        )
        .is_none(),
        "missing pre-edge source provenance must not expand the original clock horizon"
    );
}
