use boss_clock::mapping_history::{ClockMappingFact, ClockTimeline};
use boss_clock::types::ClockMode;
use chrono::{DateTime, TimeZone, Utc};

fn t(second: i64) -> DateTime<Utc> {
    Utc.timestamp_opt(1_760_000_000 + second, 0).unwrap()
}

fn wall(
    revision: &str,
    previous: Option<&str>,
    capture: i64,
    audit: i64,
    seq: i64,
) -> ClockMappingFact {
    ClockMappingFact {
        schema_version: 1,
        revision: revision.into(),
        previous_revision: previous.map(str::to_string),
        epoch: Some("original-boot".into()),
        mode: Some(ClockMode::Wall),
        params: None,
        captured_at: t(capture),
        audit_created_at: t(audit),
        seq,
        complete_group: true,
    }
}

#[test]
fn original_capture_and_audit_bracket_activation_without_erasing_known_spans() {
    let facts = vec![wall("a", None, 0, 2, 1), wall("b", Some("a"), 10, 12, 2)];
    let timeline = ClockTimeline::build(&facts, Some("b"), t(0), t(20));
    assert!(timeline.date_at(t(1)).is_err());
    assert_eq!(timeline.date_at(t(5)).unwrap(), t(5).date_naive());
    assert!(
        timeline.date_at(t(11)).is_err(),
        "precommit capture is not activation"
    );
    assert_eq!(timeline.date_at(t(15)).unwrap(), t(15).date_naive());
    assert_eq!(timeline.known.len(), 2);
    assert_eq!(timeline.unknown.len(), 2);
}

#[test]
fn outcome_date_bounds_do_not_turn_a_complete_wall_gap_into_clock_coverage() {
    let facts = vec![wall("a", None, 0, 2, 1), wall("b", Some("a"), 10, 12, 2)];
    let timeline = ClockTimeline::build(&facts, Some("b"), t(0), t(20));
    assert!(timeline.date_at(t(11)).is_err());
    assert_eq!(
        timeline.outcome_date_bounds(t(10), t(12), 2).unwrap(),
        (t(10).date_naive(), t(12).date_naive()),
    );
    assert!(
        timeline.date_at(t(11)).is_err(),
        "an outcome witness is not activation evidence"
    );
    assert!(
        timeline.outcome_date_bounds(t(0), t(2), 2).is_err(),
        "the initial model is missing before first activation"
    );
    assert!(
        timeline.outcome_date_bounds(t(10), t(12), 1).is_err(),
        "a future original cannot prove an older audit cut"
    );
}

#[test]
fn outcome_date_bounds_refuse_incomplete_or_overlapping_original_model_rosters() {
    let good = vec![wall("a", None, 0, 2, 1), wall("b", Some("a"), 10, 12, 2)];
    for case in 0..6 {
        let mut facts = good.clone();
        let mut head = "b";
        match case {
            0 => facts[1].previous_revision = Some("missing".into()),
            1 => facts[0].previous_revision = Some("missing-root".into()),
            2 => facts[1].complete_group = false,
            3 => facts[1].captured_at = t(1),
            4 => head = "unrelayed",
            _ => facts[1].revision = "a".into(),
        }
        assert!(
            ClockTimeline::build(&facts, Some(head), t(0), t(20))
                .outcome_date_bounds(t(10), t(12), 2)
                .is_err(),
            "case{case}"
        );
    }
}

#[test]
fn an_outcome_witness_survives_serialization_but_never_appears_on_legacy_or_changed_spans() {
    let facts = vec![wall("a", None, 0, 2, 1), wall("b", Some("a"), 10, 12, 2)];
    let clock = ClockTimeline::build(&facts, Some("b"), t(0), t(20));
    let mut wire = serde_json::to_value(&clock).unwrap();
    let restored: ClockTimeline = serde_json::from_value(wire.clone()).unwrap();
    assert_eq!(
        restored.outcome_date_bounds(t(10), t(12), 2).unwrap(),
        (t(10).date_naive(), t(12).date_naive())
    );
    wire.as_object_mut().unwrap().remove("outcome_originals");
    let legacy: ClockTimeline = serde_json::from_value(wire).unwrap();
    assert!(legacy.outcome_date_bounds(t(10), t(12), 2).is_err());
    let mut changed = clock;
    changed.unknown[1].until = t(13);
    assert!(changed.outcome_date_bounds(t(10), t(12), 2).is_err());
}

#[test]
fn a_future_complete_wall_revision_does_not_rewrite_an_original_prefix() {
    let a = wall("a", None, 0, 2, 1);
    let old = ClockTimeline::build(std::slice::from_ref(&a), Some("a"), t(0), t(9));
    let next = ClockTimeline::build(
        &[a, wall("b", Some("a"), 10, 12, 2)],
        Some("b"),
        t(0),
        t(20),
    );
    assert_eq!(
        old.outcome_evidence_prefix(t(9)),
        next.outcome_evidence_prefix(t(9))
    );
}

#[test]
fn missing_groups_or_revisions_remain_unknown_and_later_full_facts_recover() {
    let mut a = wall("a", None, 0, 2, 1);
    a.complete_group = false;
    let b = wall("b", Some("a"), 10, 12, 2);
    let timeline = ClockTimeline::build(&[a, b], Some("b"), t(0), t(20));
    assert!(timeline.date_at(t(5)).is_err());
    assert!(timeline.date_at(t(15)).is_ok());
    let timeline =
        ClockTimeline::build(&[wall("a", None, 0, 2, 1)], Some("unrelayed"), t(0), t(20));
    assert!(
        timeline.date_at(t(15)).is_err(),
        "a committed unrelayed head is not coverage"
    );
    let facts = vec![
        wall("a", None, 0, 2, 1),
        wall("c", Some("missing"), 10, 12, 3),
    ];
    let timeline = ClockTimeline::build(&facts, Some("c"), t(0), t(20));
    assert!(
        timeline.date_at(t(5)).is_err(),
        "missing revision can hide an earlier change"
    );
    assert!(timeline.date_at(t(15)).is_ok());
}

#[test]
fn repeated_or_out_of_order_originals_are_not_silently_sorted_into_proof() {
    let a = wall("a", None, 0, 2, 2);
    let b = wall("b", Some("a"), 10, 12, 1);
    let timeline = ClockTimeline::build(&[a, b], Some("b"), t(0), t(20));
    assert!(timeline.known.is_empty());
    assert!(timeline.date_at(t(15)).is_err());
}

#[test]
fn malformed_formula_and_reused_revision_do_not_claim_covered_intervals() {
    let mut invalid = wall("a", None, 0, 2, 1);
    invalid.mode = Some(ClockMode::Sim);
    invalid.params = Some(boss_clock::types::SimClockParams {
        epoch_start: t(0).date_naive(),
        epoch_end: None,
        warp_factor: 1.0,
        wall_anchor: t(0),
        paused: true,
        paused_at: None,
        paused_offset_seconds: 0.0,
        restart_in_progress: false,
    });
    let timeline = ClockTimeline::build(&[invalid], Some("a"), t(0), t(20));
    assert!(
        timeline.known.is_empty(),
        "a malformed formula is not covered evidence"
    );
    let facts = vec![
        wall("a", None, 0, 2, 1),
        wall("b", Some("a"), 5, 6, 2),
        wall("a", Some("b"), 10, 12, 3),
    ];
    let timeline = ClockTimeline::build(&facts, Some("a"), t(0), t(20));
    assert!(
        timeline.known.is_empty(),
        "reused nonadjacent original identities are refused"
    );
}
