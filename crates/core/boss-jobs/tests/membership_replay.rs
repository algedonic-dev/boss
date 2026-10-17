//! Synthetic original publications/transactions using the entire
//! native 19-row predicate population read on 2026-10-16. The copied
//! rows prove predicate SHAPES, not historical publication instants.
use boss_clock_client::ClockMode;
use boss_clock_client::mapping_history::{ClockMappingFact, ClockTimeline};
use boss_core::{
    event::Event,
    job::{Job, JobStatus, Priority, Step, StepStatus, Subject},
};
use boss_events::original_group::{GroupReference, OriginalGroup};
use boss_jobs::{
    membership_replay::{ReplayGroup, ReplayInput, replay},
    move_diagnosis::HistoricalEvent,
    port::JobScope,
    stations::StationSpec,
};
use chrono::{DateTime, TimeZone, Utc};
use serde_json::json;
use uuid::Uuid;

fn t(s: i64) -> DateTime<Utc> {
    Utc.timestamp_opt(1_760_000_000 + s, 0).unwrap()
}
fn group(seq: &mut i64, at: i64, events: Vec<Event>) -> ReplayGroup {
    let mut rows = Vec::new();
    for event in events {
        *seq += 1;
        rows.push(HistoricalEvent {
            seq: *seq,
            created_at: Some(t(at)),
            event,
        });
    }
    let ids: Vec<_> = rows.iter().map(|r| r.event.id).collect();
    let original = OriginalGroup {
        reference: GroupReference {
            id: Uuid::new_v4(),
            schema_version: 1,
            event_count: ids.len() as i64,
            member_hash: OriginalGroup::roster_hash(&ids),
        },
        event_ids: ids,
        audit_sequences: rows.iter().map(|r| Some(r.seq)).collect(),
        complete: true,
    };
    ReplayGroup {
        original: Some(original),
        rows,
    }
}
fn event(kind: &str, value: serde_json::Value, at: i64) -> Event {
    Event::new("jobs", kind, value, t(at))
}
fn input(groups: Vec<ReplayGroup>, since: i64, until: i64) -> ReplayInput {
    let fact = ClockMappingFact {
        schema_version: 1,
        revision: "clock".into(),
        previous_revision: None,
        epoch: Some("boot".into()),
        mode: Some(ClockMode::Wall),
        params: None,
        captured_at: t(-100),
        audit_created_at: t(-99),
        seq: 1,
        complete_group: true,
    };
    ReplayInput {
        groups,
        since: t(since),
        until: t(until),
        audit_cutoff: 100_000,
        actor: Some("alice".into()),
        scope: JobScope::All,
        currently_authorized: None,
        clock: ClockTimeline::build(&[fact], Some("clock"), t(-100), t(until)),
        registry_prefix_complete: true,
        packet_prefix_complete: true,
        source_unavailable: vec![],
    }
}
fn population() -> Vec<StationSpec> {
    let source: serde_json::Value = serde_json::from_str(include_str!(
        "fixtures/station-predicate-population-20261016.json"
    ))
    .unwrap();
    assert_eq!(source["total"], 19);
    serde_json::from_value(source["data"].clone()).unwrap()
}
fn eligible(spec: &StationSpec) -> (Job, Step) {
    let mut job = Job::new(
        spec.predicate.kind.as_deref().unwrap_or("fixture"),
        Subject::new("custom", "fixture"),
        "Original packet",
        "alice",
        Priority::Standard,
        t(0).date_naive(),
    );
    job.status = spec.predicate.status.unwrap_or(JobStatus::Open);
    for key in &spec.predicate.metadata_present {
        job.metadata[key] = json!("present");
    }
    for (key, value) in &spec.predicate.metadata_equals {
        job.metadata[key] = json!(if value == "@me" { "alice" } else { value });
    }
    let mut step = Step::new(
        job.id,
        spec.predicate
            .step
            .as_ref()
            .and_then(|s| s.kind.as_deref())
            .unwrap_or("task"),
        "Original step",
        0,
    );
    step.status = StepStatus::Ready;
    if let Some(p) = &spec.predicate.step {
        step.spec_slug = p.slug.clone();
        step.status = p.status_in.first().copied().unwrap_or(StepStatus::Ready);
        for (key, value) in &p.metadata_equals {
            step.metadata[key] = json!(if value == "@me" { "alice" } else { value });
        }
    }
    (job, step)
}
fn registration(spec: &StationSpec, job: &Job) -> Vec<Event> {
    let mut workflow = boss_jobs::registry::WorkflowSpec::platform_seed(
        &job.kind,
        "Original protocol",
        "fixture",
        vec!["custom".into()],
        vec![],
    );
    workflow.created_at = t(0);
    vec![
        event(boss_jobs::events::WORKFLOW_PUBLISHED, json!(workflow), 0),
        event(boss_jobs::events::STATION_PUBLISHED, json!(spec), 0),
    ]
}

#[test]
fn all_nineteen_native_predicates_count_one_packet_with_two_eligible_steps() {
    for spec in population() {
        let (job, step) = eligible(&spec);
        let mut second = Step::new(job.id, step.kind.clone(), "Second eligible step", 1);
        second.status = step.status;
        second.metadata = step.metadata.clone();
        second.spec_slug = step.spec_slug.clone();
        let mut seq = 1;
        let published = group(&mut seq, 0, registration(&spec, &job));
        let admitted = group(
            &mut seq,
            10,
            vec![
                event(boss_jobs::events::JOB_CREATED, json!(job), 10),
                event(boss_jobs::events::STEP_CREATED, json!(step), 10),
                event(boss_jobs::events::STEP_CREATED, json!(second), 10),
            ],
        );
        let out = replay(&input(vec![published, admitted], 5, 20));
        let flow = out
            .stations
            .iter()
            .find(|r| r.station == spec.name)
            .unwrap();
        assert_eq!(flow.arrived, Some(1), "{}: {:?}", spec.name, flow.unknown);
        assert_eq!(flow.departed, Some(0), "{}", spec.name);
    }
}

#[test]
fn metadata_only_departure_and_reentry_are_not_deduplicated_as_step_obligations() {
    let spec = population()
        .into_iter()
        .find(|s| s.name == "q.platform-admin.task")
        .unwrap();
    let (mut job, step) = eligible(&spec);
    let mut seq = 1;
    let mut groups = vec![
        group(&mut seq, 0, registration(&spec, &job)),
        group(
            &mut seq,
            10,
            vec![
                event(boss_jobs::events::JOB_CREATED, json!(job), 10),
                event(boss_jobs::events::STEP_CREATED, json!(step), 10),
            ],
        ),
    ];
    job.metadata["proof_probe"] = json!("original-proof");
    groups.push(group(
        &mut seq,
        12,
        vec![event(boss_jobs::events::JOB_UPDATED, json!(job), 12)],
    ));
    job.metadata.as_object_mut().unwrap().remove("proof_probe");
    groups.push(group(
        &mut seq,
        14,
        vec![event(boss_jobs::events::JOB_UPDATED, json!(job), 14)],
    ));
    let out = replay(&input(groups, 5, 20));
    let flow = &out.stations[0];
    assert_eq!(flow.arrived, Some(2));
    assert_eq!(flow.departed, Some(1));
    assert_eq!(
        flow.served,
        Some(0),
        "metadata changes do not prove work served"
    );
}

#[test]
fn conserved_first_record_full_step_changes_membership_without_an_update_marker() {
    let spec = population()
        .into_iter()
        .find(|s| s.name == "q.platform-admin.task")
        .unwrap();
    let (job, mut step) = eligible(&spec);
    let mut seq = 1;
    let mut groups = vec![
        group(&mut seq, 0, registration(&spec, &job)),
        group(
            &mut seq,
            10,
            vec![
                event(boss_jobs::events::JOB_CREATED, json!(job), 10),
                event(boss_jobs::events::STEP_CREATED, json!(step), 10),
            ],
        ),
    ];
    // This station excludes packets with proof_probe on the Job, so use
    // a custom original predicate on the full Step receipt instead.
    let mut changed = spec.clone();
    changed
        .predicate
        .step
        .as_mut()
        .unwrap()
        .metadata_equals
        .insert("decision".into(), "ready".into());
    groups.push(group(
        &mut seq,
        11,
        vec![event(
            boss_jobs::events::STATION_PUBLISHED,
            json!(changed),
            11,
        )],
    ));
    step.metadata["decision"] = json!("ready");
    let mut stamp = boss_core::publisher::EventStamp::new(
        "jobs",
        boss_core::actor::ActorId::Automation("test".into()),
    );
    stamp.timestamp = t(12);
    let record = boss_jobs::first_record::FirstRecord::new(
        job.id,
        step.id,
        "decision",
        &json!("ready"),
        &stamp,
        Uuid::new_v4(),
    );
    groups.push(group(&mut seq, 12, vec![record.event(&step, &stamp)]));
    let out = replay(&input(groups, 11, 20));
    let flow = out
        .stations
        .iter()
        .find(|f| f.station == spec.name)
        .unwrap();
    assert_eq!(flow.arrived, Some(1), "{:?}", flow.unknown);
    assert_eq!(flow.served, Some(0));
}

#[test]
fn exact_claim_receipt_is_a_full_step_transition_including_reconstructed_chunks() {
    use sha2::Digest;
    for chunked in [false, true] {
        let mut spec = population()
            .into_iter()
            .find(|s| s.name == "q.platform-admin.task")
            .unwrap();
        spec.predicate.step.as_mut().unwrap().status_in = vec![StepStatus::Ready];
        let (job, mut step) = eligible(&spec);
        let mut seq = 1;
        let mut groups = vec![
            group(&mut seq, 0, registration(&spec, &job)),
            group(
                &mut seq,
                10,
                vec![
                    event(boss_jobs::events::JOB_CREATED, json!(job), 10),
                    event(boss_jobs::events::STEP_CREATED, json!(step), 10),
                ],
            ),
        ];
        step.status = StepStatus::Active;
        step.assignee_id = Some("alice".into());
        let mut stamp = boss_core::publisher::EventStamp::new(
            "jobs",
            boss_core::actor::ActorId::Automation("test".into()),
        );
        stamp.timestamp = t(12);
        let id = Uuid::new_v4();
        let station = json!(spec);
        let receipt=boss_jobs::station_claim::SelectionReceipt::from_json(json!({"version":2,
            "request_id":Uuid::new_v4(),"event_id":id,"event_actor":stamp.actor(),"actor":"alice",
            "station":station,"station_hash":hex::encode(sha2::Sha256::digest(boss_core::job::canonical_json_bytes(&station))),
            "selected_at":t(12),"selected":{"step_id":step.id,"job_id":job.id},"claimed_step":step}).to_string()).unwrap();
        let mut claim = stamp.event(
            boss_jobs::events::STATION_CLAIMED,
            json!({"receipt_json":receipt.json()}),
        );
        claim.id = id;
        let events = if chunked {
            boss_jobs::selection_evidence::conserve(&receipt, claim, &stamp).unwrap()
        } else {
            vec![claim]
        };
        if chunked {
            for event in &events {
                assert_eq!(
                    event.payload["job_id"],
                    json!(job.id),
                    "routing copies the immutable selected packet identity onto every original sibling"
                );
            }
        }
        groups.push(group(&mut seq, 12, events));
        let out = replay(&input(groups.clone(), 11, 20));
        let flow = out
            .stations
            .iter()
            .find(|f| f.station == spec.name)
            .unwrap();
        assert_eq!(
            flow.departed,
            Some(1),
            "chunked={chunked}: {:?}",
            flow.unknown
        );
        assert_eq!(flow.served, Some(0));
        if chunked {
            let mut late = groups.clone();
            let last = late.last_mut().unwrap();
            let sequences = last.rows.iter().map(|r| r.seq).rev().collect::<Vec<_>>();
            for (row, sequence) in last.rows.iter_mut().zip(sequences) {
                row.seq = sequence;
            }
            let original = last.original.as_mut().unwrap();
            original.audit_sequences = original
                .event_ids
                .iter()
                .map(|id| last.rows.iter().find(|r| r.event.id == *id).map(|r| r.seq))
                .collect();
            last.rows.sort_by_key(|r| r.seq);
            let out = replay(&input(late, 11, 20));
            assert_eq!(
                out.stations
                    .iter()
                    .find(|f| f.station == spec.name)
                    .unwrap()
                    .departed,
                Some(1),
                "complete original producer order, rather than relay order, reconstructs its own receipt"
            );
        }
        let mut malformed = groups;
        malformed.last_mut().unwrap().rows[0].event.payload["job_id"] = json!(Uuid::new_v4());
        let out = replay(&input(malformed.clone(), 11, 20));
        assert_eq!(
            out.stations
                .iter()
                .find(|f| f.station == spec.name)
                .unwrap()
                .departed,
            None,
            "a redundant routing copy cannot disagree with exact receipt-selected identity (chunked={chunked})"
        );
        let mut narrow = input(malformed, 11, 20);
        narrow.scope = JobScope::OwnerIs("alice".into());
        narrow.currently_authorized = Some([job.id.to_string()].into_iter().collect());
        assert_eq!(
            replay(&narrow)
                .stations
                .iter()
                .find(|f| f.station == spec.name)
                .unwrap()
                .departed,
            None,
            "a conflicting optional routing copy cannot conceal an owned exact receipt (chunked={chunked})"
        );
    }
}

#[test]
fn assignment_departure_records_reassignment_without_service() {
    let mut spec = population()
        .into_iter()
        .find(|s| s.name == "q.platform-admin.task")
        .unwrap();
    spec.predicate.step.as_mut().unwrap().assignee_id = Some("@me".into());
    let (job, mut step) = eligible(&spec);
    step.assignee_id = Some("alice".into());
    let mut seq = 1;
    let mut groups = vec![
        group(&mut seq, 0, registration(&spec, &job)),
        group(
            &mut seq,
            10,
            vec![
                event(boss_jobs::events::JOB_CREATED, json!(job), 10),
                event(boss_jobs::events::STEP_CREATED, json!(step), 10),
            ],
        ),
    ];
    step.assignee_id = Some("bob".into());
    groups.push(group(
        &mut seq,
        12,
        vec![event(boss_jobs::events::STEP_UPDATED, json!(step), 12)],
    ));
    let out = replay(&input(groups, 5, 20));
    let row = &out.stations[0];
    assert_eq!(row.arrived, Some(1));
    assert_eq!(row.departed, Some(1));
    assert_eq!(row.served, Some(0));
    assert_eq!(row.departure_causes.get("reassignment"), Some(&1));
}

#[test]
fn terminal_retention_expires_without_a_step_marker_and_does_not_claim_service() {
    let spec = population()
        .into_iter()
        .find(|s| s.name == "design-decided")
        .unwrap();
    let (mut job, step) = eligible(&spec);
    let mut seq = 1;
    let mut groups = vec![
        group(&mut seq, 0, registration(&spec, &job)),
        group(
            &mut seq,
            10,
            vec![
                event(boss_jobs::events::JOB_CREATED, json!(job), 10),
                event(boss_jobs::events::STEP_CREATED, json!(step), 10),
            ],
        ),
    ];
    job.status = JobStatus::Closed;
    job.closed_on = Some(t(0).date_naive());
    groups.push(group(
        &mut seq,
        12,
        vec![event(boss_jobs::events::JOB_UPDATED, json!(job), 12)],
    ));
    let out = replay(&input(groups, 5, 9 * 86400));
    let flow = &out.stations[0];
    assert_eq!(flow.arrived, Some(1));
    assert_eq!(flow.departed, Some(1));
    assert_eq!(flow.served, Some(0));
    assert_eq!(flow.departure_causes.get("terminal-expiry"), Some(&1));
}

#[test]
fn retention_window_counts_start_boundary_but_excludes_end_boundary() {
    let spec = population()
        .into_iter()
        .find(|s| s.name == "design-decided")
        .unwrap();
    let (mut job, step) = eligible(&spec);
    let mut seq = 1;
    job.status = JobStatus::Closed;
    job.closed_on = Some(t(0).date_naive());
    let expiry = job
        .closed_on
        .unwrap()
        .checked_add_days(chrono::Days::new(
            u64::from(spec.terminal_window_days.unwrap()) + 1,
        ))
        .unwrap()
        .and_hms_opt(0, 0, 0)
        .unwrap()
        .and_utc();
    let seconds = (expiry - t(0)).num_seconds();
    let groups = vec![
        group(&mut seq, 0, registration(&spec, &job)),
        group(
            &mut seq,
            10,
            vec![
                event(boss_jobs::events::JOB_CREATED, json!(job), 10),
                event(boss_jobs::events::STEP_CREATED, json!(step), 10),
            ],
        ),
    ];
    let ends = replay(&input(groups.clone(), seconds - 10, seconds));
    assert_eq!(ends.stations[0].departed, Some(0));
    let starts = replay(&input(groups, seconds, seconds + 10));
    assert_eq!(
        starts.stations[0].departed,
        Some(1),
        "inclusive start must not drop timer transitions"
    );
}

#[test]
fn unrelated_step_completion_cannot_turn_metadata_departure_into_service() {
    let spec = population()
        .into_iter()
        .find(|s| s.name == "q.platform-admin.task")
        .unwrap();
    let (mut job, step) = eligible(&spec);
    let mut unrelated = Step::new(job.id, "sign-off", "Unrelated", 1);
    let mut seq = 1;
    let mut groups = vec![
        group(&mut seq, 0, registration(&spec, &job)),
        group(
            &mut seq,
            10,
            vec![
                event(boss_jobs::events::JOB_CREATED, json!(job), 10),
                event(boss_jobs::events::STEP_CREATED, json!(step), 10),
                event(boss_jobs::events::STEP_CREATED, json!(unrelated), 10),
            ],
        ),
    ];
    job.metadata["proof_probe"] = json!("original");
    unrelated.status = StepStatus::Completed;
    groups.push(group(
        &mut seq,
        12,
        vec![
            event(boss_jobs::events::JOB_UPDATED, json!(job), 12),
            event(boss_jobs::events::STEP_UPDATED, json!(unrelated), 12),
        ],
    ));
    let out = replay(&input(groups, 5, 20));
    assert_eq!(out.stations[0].departed, Some(1));
    assert_eq!(
        out.stations[0].served,
        Some(0),
        "only formerly eligible work can prove service"
    );
}

#[test]
fn original_atomic_group_straddling_the_end_cut_is_unknown_not_a_zero() {
    let spec = population()
        .into_iter()
        .find(|s| s.name == "q.platform-admin.task")
        .unwrap();
    let (job, step) = eligible(&spec);
    let mut seq = 1;
    let mut groups = vec![group(&mut seq, 0, registration(&spec, &job))];
    let mut admission = group(
        &mut seq,
        15,
        vec![
            event(boss_jobs::events::JOB_CREATED, json!(job), 15),
            event(boss_jobs::events::STEP_CREATED, json!(step), 15),
        ],
    );
    admission.rows[1].created_at = Some(t(25));
    groups.push(admission);
    let out = replay(&input(groups, 5, 20));
    assert_eq!(
        out.stations[0].arrived, None,
        "a whole group outside the end cannot erase its original in-window prefix"
    );
    assert!(
        out.stations[0]
            .unknown
            .iter()
            .any(|u| u.reason.contains("boundary"))
    );
}

#[test]
fn original_atomic_group_crossing_the_start_has_no_invented_initial_coverage() {
    let spec = population()
        .into_iter()
        .find(|s| s.name == "q.platform-admin.task")
        .unwrap();
    let (job, step) = eligible(&spec);
    let mut seq = 1;
    let mut admission = group(
        &mut seq,
        1,
        vec![
            event(boss_jobs::events::JOB_CREATED, json!(job), 1),
            event(boss_jobs::events::STEP_CREATED, json!(step), 1),
        ],
    );
    admission.rows[1].created_at = Some(t(10));
    let published = group(&mut seq, 0, registration(&spec, &job));
    // Rebuild fresh monotone audit identities: the registry precedes
    // the admission whose own siblings straddle the requested start.
    let mut seq = 1;
    let published = group(
        &mut seq,
        0,
        published.rows.into_iter().map(|r| r.event).collect(),
    );
    let mut admitted = group(
        &mut seq,
        1,
        admission.rows.into_iter().map(|r| r.event).collect(),
    );
    admitted.rows[1].created_at = Some(t(10));
    let out = replay(&input(vec![published, admitted], 5, 20));
    assert!(out.stations[0].covered.is_empty());
}

#[test]
fn retirement_names_an_original_version_instead_of_deleting_a_newer_active_row() {
    let old = population()
        .into_iter()
        .find(|s| s.name == "q.platform-admin.task")
        .unwrap();
    let (mut job, step) = eligible(&old);
    let mut seq = 1;
    let mut groups = vec![
        group(&mut seq, 0, registration(&old, &job)),
        group(
            &mut seq,
            10,
            vec![
                event(boss_jobs::events::JOB_CREATED, json!(job), 10),
                event(boss_jobs::events::STEP_CREATED, json!(step), 10),
            ],
        ),
    ];
    let mut newer = old.clone();
    newer.version += 1;
    newer.predicate.metadata_absent.clear();
    newer.predicate.metadata_present.push("proof_probe".into());
    groups.push(group(
        &mut seq,
        12,
        vec![event(
            boss_jobs::events::STATION_PUBLISHED,
            json!(newer),
            12,
        )],
    ));
    let mut retired = old;
    retired.status = boss_jobs::registry::WorkflowStatus::Retired;
    groups.push(group(
        &mut seq,
        14,
        vec![event(
            boss_jobs::events::STATION_RETIRED,
            json!(retired),
            14,
        )],
    ));
    job.metadata["proof_probe"] = json!("original");
    groups.push(group(
        &mut seq,
        16,
        vec![event(boss_jobs::events::JOB_UPDATED, json!(job), 16)],
    ));
    let out = replay(&input(groups, 5, 20));
    assert_eq!(out.stations[0].arrived, Some(2));
    assert_eq!(out.stations[0].departed, Some(1));
}

#[test]
fn native_group_roster_binds_each_event_to_its_own_audit_sequence() {
    let spec = population()
        .into_iter()
        .find(|s| s.name == "q.platform-admin.task")
        .unwrap();
    let (job, step) = eligible(&spec);
    let mut seq = 1;
    let mut groups = vec![group(&mut seq, 0, registration(&spec, &job))];
    let mut admitted = group(
        &mut seq,
        10,
        vec![
            event(boss_jobs::events::JOB_CREATED, json!(job), 10),
            event(boss_jobs::events::STEP_CREATED, json!(step), 10),
        ],
    );
    admitted
        .original
        .as_mut()
        .unwrap()
        .audit_sequences
        .swap(0, 1);
    groups.push(admitted);
    assert_eq!(
        replay(&input(groups, 5, 20)).stations[0].arrived,
        None,
        "a valid set of sequence numbers is not their identity binding"
    );
}

#[test]
fn a_private_malformed_claim_cannot_change_the_public_scoped_read() {
    let spec = population()
        .into_iter()
        .find(|s| s.name == "q.platform-admin.task")
        .unwrap();
    let (job, step) = eligible(&spec);
    let id = job.id.to_string();
    let mut seq = 1;
    let groups = vec![
        group(&mut seq, 0, registration(&spec, &job)),
        group(
            &mut seq,
            10,
            vec![
                event(boss_jobs::events::JOB_CREATED, json!(job), 10),
                event(boss_jobs::events::STEP_CREATED, json!(step), 10),
            ],
        ),
    ];
    let mut original = input(groups, 5, 20);
    original.scope = JobScope::OwnerIs("alice".into());
    original.currently_authorized = Some([id].into_iter().collect());
    let private = group(
        &mut seq,
        14,
        vec![event(
            boss_jobs::events::STATION_CLAIMED,
            json!({"job_id":Uuid::new_v4(),"receipt_manifest":{"version":2}}),
            14,
        )],
    );
    original.groups.push(group(
        &mut seq,
        16,
        vec![event(boss_jobs::events::JOB_UPDATED, json!(job), 16)],
    ));
    let before = serde_json::to_value(replay(&original)).unwrap();
    original.groups.insert(original.groups.len() - 1, private);
    assert_eq!(
        serde_json::to_value(replay(&original)).unwrap(),
        before,
        "a private reconstruction failure must be invisible"
    );
}

#[test]
fn owned_malformed_claim_is_unknown_even_when_exact_receipt_cannot_be_reconstructed() {
    let spec = population()
        .into_iter()
        .find(|s| s.name == "q.platform-admin.task")
        .unwrap();
    let (job, step) = eligible(&spec);
    let mut seq = 1;
    let groups = vec![
        group(&mut seq, 0, registration(&spec, &job)),
        group(
            &mut seq,
            10,
            vec![
                event(boss_jobs::events::JOB_CREATED, json!(job), 10),
                event(boss_jobs::events::STEP_CREATED, json!(step), 10),
            ],
        ),
        group(
            &mut seq,
            14,
            vec![event(
                boss_jobs::events::STATION_CLAIMED,
                json!({"job_id":job.id,"receipt_manifest":{"version":2}}),
                14,
            )],
        ),
    ];
    let mut original = input(groups, 5, 20);
    original.scope = JobScope::OwnerIs("alice".into());
    original.currently_authorized = Some([job.id.to_string()].into_iter().collect());
    assert_eq!(replay(&original).stations[0].arrived, None);
}

#[test]
fn interleaved_original_groups_cannot_guess_packet_state_order_from_relay_completion() {
    let spec = population()
        .into_iter()
        .find(|s| s.name == "q.platform-admin.task")
        .unwrap();
    let (mut job, mut step) = eligible(&spec);
    let mut seq = 1;
    let mut groups = vec![
        group(&mut seq, 0, registration(&spec, &job)),
        group(
            &mut seq,
            10,
            vec![
                event(boss_jobs::events::JOB_CREATED, json!(job), 10),
                event(boss_jobs::events::STEP_CREATED, json!(step), 10),
            ],
        ),
    ];
    job.metadata["proof_probe"] = json!("original");
    step.notes = Some("same original transaction".into());
    let mut first = group(
        &mut seq,
        12,
        vec![
            event(boss_jobs::events::JOB_UPDATED, json!(job), 12),
            event(boss_jobs::events::STEP_UPDATED, json!(step), 12),
        ],
    );
    job.metadata.as_object_mut().unwrap().remove("proof_probe");
    let mut second = group(
        &mut seq,
        14,
        vec![event(boss_jobs::events::JOB_UPDATED, json!(job), 14)],
    );
    let middle = first.rows[1].seq;
    let end = second.rows[0].seq;
    first.rows[1].seq = end;
    first.rows[1].created_at = Some(t(16));
    first.original.as_mut().unwrap().audit_sequences[1] = Some(end);
    second.rows[0].seq = middle;
    second.original.as_mut().unwrap().audit_sequences[0] = Some(middle);
    groups.extend([second, first]);
    let out = replay(&input(groups, 5, 20));
    assert_eq!(out.stations[0].arrived, None);
    assert!(
        out.stations[0]
            .unknown
            .iter()
            .any(|u| u.reason.contains("interleav"))
    );
}

#[test]
fn missing_original_step_creation_before_the_window_is_not_an_idle_zero() {
    let spec = population()
        .into_iter()
        .find(|s| s.name == "q.platform-admin.task")
        .unwrap();
    let (job, step) = eligible(&spec);
    let mut seq = 1;
    let groups = vec![
        group(&mut seq, 0, registration(&spec, &job)),
        group(
            &mut seq,
            1,
            vec![
                event(boss_jobs::events::JOB_CREATED, json!(job), 1),
                event(boss_jobs::events::STEP_UPDATED, json!(step), 1),
            ],
        ),
    ];
    let out = replay(&input(groups, 5, 20));
    assert_eq!(
        out.stations[0].arrived, None,
        "an unestablished original initial state must remain unavailable even in a quiet window"
    );
}

#[test]
fn original_publication_and_retirement_keep_their_distinct_nonservice_causes() {
    let mut spec = population()
        .into_iter()
        .find(|s| s.name == "q.platform-admin.task")
        .unwrap();
    let (mut job, step) = eligible(&spec);
    let mut seq = 1;
    let mut groups = vec![
        group(&mut seq, 0, registration(&spec, &job)),
        group(
            &mut seq,
            10,
            vec![
                event(boss_jobs::events::JOB_CREATED, json!(job), 10),
                event(boss_jobs::events::STEP_CREATED, json!(step), 10),
            ],
        ),
    ];
    spec.version += 1;
    spec.predicate.metadata_absent.clear();
    spec.predicate.metadata_present.push("proof_probe".into());
    groups.push(group(
        &mut seq,
        12,
        vec![event(boss_jobs::events::STATION_PUBLISHED, json!(spec), 12)],
    ));
    job.metadata["proof_probe"] = json!("original");
    groups.push(group(
        &mut seq,
        14,
        vec![event(boss_jobs::events::JOB_UPDATED, json!(job), 14)],
    ));
    spec.status = boss_jobs::registry::WorkflowStatus::Retired;
    groups.push(group(
        &mut seq,
        16,
        vec![event(boss_jobs::events::STATION_RETIRED, json!(spec), 16)],
    ));
    let out = replay(&input(groups, 5, 20));
    let row = &out.stations[0];
    assert_eq!(row.arrived, Some(2));
    assert_eq!(row.departed, Some(2));
    assert_eq!(row.served, Some(0));
    assert_eq!(row.departure_causes.get("registry-publication"), Some(&1));
    assert_eq!(row.departure_causes.get("registry-retirement"), Some(&1));
    assert_eq!(
        row.versions.iter().map(|v| v.version).collect::<Vec<_>>(),
        vec![1, 2]
    );
}

#[test]
fn authored_precedence_and_derived_membership_use_original_workflow_publications() {
    let spec = population()
        .into_iter()
        .find(|s| s.name == "q.platform-admin.task")
        .unwrap();
    let (job, step) = eligible(&spec);
    let mut seq = 1;
    let mut workflow = boss_jobs::registry::WorkflowSpec::platform_seed(
        &job.kind,
        "Original",
        "fixture",
        vec!["custom".into()],
        vec![boss_jobs::registry::StepSpec {
            kind: "task".into(),
            authority_role: Some("platform-admin".into()),
            ..Default::default()
        }],
    );
    let mut authored = spec.clone();
    authored
        .predicate
        .metadata_present
        .push("original_author_override".into());
    let mut groups = vec![
        group(
            &mut seq,
            0,
            vec![
                event(boss_jobs::events::WORKFLOW_PUBLISHED, json!(workflow), 0),
                event(boss_jobs::events::STATION_PUBLISHED, json!(authored), 0),
            ],
        ),
        group(
            &mut seq,
            10,
            vec![
                event(boss_jobs::events::JOB_CREATED, json!(job), 10),
                event(boss_jobs::events::STEP_CREATED, json!(step), 10),
            ],
        ),
    ];
    authored.status = boss_jobs::registry::WorkflowStatus::Retired;
    groups.push(group(
        &mut seq,
        12,
        vec![event(
            boss_jobs::events::STATION_RETIRED,
            json!(authored),
            12,
        )],
    ));
    workflow.status = boss_jobs::registry::WorkflowStatus::Retired;
    groups.push(group(
        &mut seq,
        14,
        vec![event(
            boss_jobs::events::WORKFLOW_RETIRED,
            json!(workflow),
            14,
        )],
    ));
    let out = replay(&input(groups, 5, 20));
    let row = out
        .stations
        .iter()
        .find(|r| r.station == spec.name)
        .unwrap();
    assert_eq!(row.arrived, Some(1));
    assert_eq!(row.departed, Some(1));
    assert_eq!(row.served, Some(0));
    assert!(
        row.versions
            .iter()
            .any(|v| v.source == "authored-publication")
    );
    assert!(
        row.versions
            .iter()
            .any(|v| v.source == "original-workflow-publications")
    );
}

#[test]
fn known_prefix_coverage_survives_a_later_missing_original() {
    let spec = population()
        .into_iter()
        .find(|s| s.name == "q.platform-admin.task")
        .unwrap();
    let (job, step) = eligible(&spec);
    let mut seq = 1;
    let groups = vec![
        group(&mut seq, 0, registration(&spec, &job)),
        group(
            &mut seq,
            10,
            vec![
                event(boss_jobs::events::JOB_CREATED, json!(job), 10),
                event(boss_jobs::events::STEP_CREATED, json!(step), 10),
            ],
        ),
        group(
            &mut seq,
            14,
            vec![event(
                boss_jobs::events::STATION_CLAIMED,
                json!({"job_id":job.id,"receipt_manifest":{"version":2}}),
                14,
            )],
        ),
    ];
    let out = replay(&input(groups, 5, 20));
    let row = serde_json::to_value(&out.stations[0]).unwrap();
    assert_eq!(row["arrived"], serde_json::Value::Null);
    assert_eq!(row["known_arrived"], 1);
    assert_eq!(
        row["covered"],
        json!([{"since":t(5),"until":t(14)}]),
        "a later gap cannot erase original earlier coverage"
    );
}

#[test]
fn model_metadata_and_repaired_ci_leave_without_claiming_work_service() {
    for name in ["a.platform-admin.opus-5-5", "repair"] {
        let spec = population().into_iter().find(|s| s.name == name).unwrap();
        let (mut job, mut step) = eligible(&spec);
        let mut seq = 1;
        let mut groups = vec![
            group(&mut seq, 0, registration(&spec, &job)),
            group(
                &mut seq,
                10,
                vec![
                    event(boss_jobs::events::JOB_CREATED, json!(job), 10),
                    event(boss_jobs::events::STEP_CREATED, json!(step), 10),
                ],
            ),
        ];
        let key = if name == "repair" {
            "result"
        } else {
            "agent_model"
        };
        let original = step.metadata[key].clone();
        step.metadata[key] = json!("changed-original");
        groups.push(group(
            &mut seq,
            12,
            vec![event(boss_jobs::events::STEP_UPDATED, json!(step), 12)],
        ));
        step.metadata[key] = original;
        groups.push(group(
            &mut seq,
            14,
            vec![event(boss_jobs::events::STEP_UPDATED, json!(step), 14)],
        ));
        job.status = JobStatus::Cancelled;
        groups.push(group(
            &mut seq,
            16,
            vec![event(boss_jobs::events::JOB_UPDATED, json!(job), 16)],
        ));
        let request = input(groups, 5, 20);
        let out = replay(&request);
        let row = out.stations.iter().find(|r| r.station == name).unwrap();
        assert_eq!(row.arrived, Some(2), "{name}");
        assert_eq!(row.departed, Some(2));
        assert_eq!(row.served, Some(0));
        assert_eq!(row.departure_causes.get("cancellation"), Some(&1));
        assert_eq!(
            serde_json::to_value(&out).unwrap(),
            serde_json::to_value(replay(&request)).unwrap(),
            "identical originals replay deterministically"
        );
    }
}

#[test]
fn watchlist_dismissal_and_caller_binding_use_original_submitted_by() {
    let spec = population()
        .into_iter()
        .find(|s| s.name == "my-watchlist")
        .unwrap();
    let (mut job, step) = eligible(&spec);
    let mut seq = 1;
    let mut groups = vec![
        group(&mut seq, 0, registration(&spec, &job)),
        group(
            &mut seq,
            10,
            vec![
                event(boss_jobs::events::JOB_CREATED, json!(job), 10),
                event(boss_jobs::events::STEP_CREATED, json!(step), 10),
            ],
        ),
    ];
    job.metadata["watchlist_dismissed"] = json!(true);
    groups.push(group(
        &mut seq,
        12,
        vec![event(boss_jobs::events::JOB_UPDATED, json!(job), 12)],
    ));
    let mut request = input(groups, 5, 20);
    let alice = replay(&request);
    assert_eq!(alice.stations[0].caller_binding.as_deref(), Some("alice"));
    assert_eq!(alice.stations[0].arrived, Some(1));
    assert_eq!(alice.stations[0].departed, Some(1));
    assert_eq!(alice.stations[0].served, Some(0));
    request.actor = Some("bob".into());
    assert_eq!(replay(&request).stations[0].arrived, Some(0));
    request.actor = None;
    assert_eq!(
        replay(&request).stations[0].arrived,
        None,
        "missing actual caller is not somebody else's queue"
    );
}

#[test]
fn original_pause_resume_and_reset_keep_known_expiry_and_unknown_activation_intervals() {
    let spec = population()
        .into_iter()
        .find(|s| s.name == "design-decided")
        .unwrap();
    let (mut job, step) = eligible(&spec);
    let mut seq = 1;
    job.status = JobStatus::Closed;
    job.closed_on = Some(t(0).date_naive());
    let groups = vec![
        group(&mut seq, 0, registration(&spec, &job)),
        group(
            &mut seq,
            1,
            vec![
                event(boss_jobs::events::JOB_CREATED, json!(job), 1),
                event(boss_jobs::events::STEP_CREATED, json!(step), 1),
            ],
        ),
    ];
    let base = boss_clock_client::SimClockParams {
        epoch_start: t(0).date_naive(),
        epoch_end: None,
        warp_factor: 86400.0,
        wall_anchor: t(0),
        paused: false,
        paused_at: None,
        paused_offset_seconds: 0.0,
        restart_in_progress: false,
    };
    let mut paused = base;
    paused.paused = true;
    paused.paused_at = Some(t(3));
    let mut resumed = base;
    resumed.paused_offset_seconds = 27.0;
    let mut reset = base;
    reset.wall_anchor = t(36);
    let facts = vec![
        ("a", None, -100, -99, base),
        ("b", Some("a"), 3, 4, paused),
        ("c", Some("b"), 30, 31, resumed),
        ("d", Some("c"), 36, 37, reset),
    ]
    .into_iter()
    .enumerate()
    .map(
        |(index, (revision, previous, capture, audit, params))| ClockMappingFact {
            schema_version: 1,
            revision: revision.into(),
            previous_revision: previous.map(str::to_owned),
            epoch: Some(
                if revision == "d" {
                    "new-epoch"
                } else {
                    "old-epoch"
                }
                .into(),
            ),
            mode: Some(ClockMode::Sim),
            params: Some(params),
            captured_at: t(capture),
            audit_created_at: t(audit),
            seq: index as i64 + 1,
            complete_group: true,
        },
    )
    .collect::<Vec<_>>();
    let mut request = input(groups, 0, 40);
    request.clock = ClockTimeline::build(&facts, Some("d"), t(-100), t(40));
    let row = &replay(&request).stations[0];
    assert_eq!(row.departed, None, "capture is not original activation");
    assert_eq!(row.known_departed, 1);
    assert_eq!(row.known_served, 0);
    assert!(
        row.unknown
            .iter()
            .any(|u| u.since == t(3) && u.until == t(4))
    );
    assert!(
        row.unknown
            .iter()
            .any(|u| u.since == t(36) && u.until == t(37))
    );
    assert!(
        row.covered
            .iter()
            .any(|c| c.since == t(31) && c.until == t(36))
    );
}

#[test]
fn narrow_read_does_not_disclose_an_unrelated_private_group_refusal() {
    let spec = population()
        .into_iter()
        .find(|s| s.name == "q.platform-admin.task")
        .unwrap();
    let (job, step) = eligible(&spec);
    let id = job.id.to_string();
    let mut seq = 1;
    let groups = vec![
        group(&mut seq, 0, registration(&spec, &job)),
        group(
            &mut seq,
            10,
            vec![
                event(boss_jobs::events::JOB_CREATED, json!(job), 10),
                event(boss_jobs::events::STEP_CREATED, json!(step), 10),
            ],
        ),
    ];
    let mut original = input(groups, 5, 20);
    original.scope = JobScope::OwnerIs("alice".into());
    original.currently_authorized = Some([id].into_iter().collect());
    let before = serde_json::to_value(replay(&original)).unwrap();
    let mut private = Job::new(
        "fixture",
        Subject::new("custom", "private"),
        "Private",
        "bob",
        Priority::Standard,
        t(0).date_naive(),
    );
    private.status = JobStatus::Open;
    let mut hidden = group(
        &mut seq,
        14,
        vec![event(boss_jobs::events::JOB_CREATED, json!(private), 14)],
    );
    hidden.original = None;
    original.groups.push(hidden);
    let after = serde_json::to_value(replay(&original)).unwrap();
    assert_eq!(
        before, after,
        "scope cannot become a side channel through UNKNOWN"
    );
}
