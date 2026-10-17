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
