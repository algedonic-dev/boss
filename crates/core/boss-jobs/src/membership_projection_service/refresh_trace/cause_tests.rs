use super::*;
use crate::membership_replay::{MembershipReading, projection::ProjectionRefusal};
use serde_json::json;

fn reading(reason: Option<&str>, metrics: usize) -> MembershipReading {
    let until = chrono::DateTime::parse_from_rfc3339("2026-10-26T00:54:01.950227Z")
        .unwrap()
        .with_timezone(&chrono::Utc);
    let since = until - chrono::Duration::hours(24);
    serde_json::from_value(json!({
        "stations": [{"station": "synthetic", "basis": "synthetic fixture",
            "arrived": 1, "departed": 0, "served": 0,
            "known_arrived": 1, "known_departed": 0, "known_served": 0,
            "departure_causes": {}, "covered": [],
            "versions": [{"version": 1, "source": "synthetic", "publication_sequences": vec![7; metrics]}],
            "unknown": reason.map(|reason| vec![json!({"since": since, "until": until, "reason": reason})]).unwrap_or_default(),
            "scope": {"OwnerIs": "alice"}, "caller_binding": "alice"}],
        "since": since, "until": until, "audit_cutoff": 2890800, "denied": false
    })).unwrap()
}

#[test]
fn never_retained_aggregate_keeps_its_actual_omission_reason() {
    let trace = Trace::new(Instant::now(), Arc::new(AtomicBool::new(false)));
    trace.refusal(
        "delta",
        &ProjectionRefusal::Unread(Box::new(reading(Some("first"), 50_000))),
    );
    let first = trace.snapshot().unwrap().refusals[0].clone();
    assert!(!first.complete);
    assert!(first.original.is_none());
    assert_eq!(first.retained_bytes, 0);
    assert_eq!(
        first.unavailable_reason,
        Some("original refusal exceeds flight diagnostic byte bound")
    );
    trace.refusal(
        "rebuild_48h",
        &ProjectionRefusal::Unread(Box::new(reading(Some("second"), 15_000))),
    );
    assert!(trace.snapshot().unwrap().refusals[1].complete);
    trace.refusal(
        "fallback_24h",
        &ProjectionRefusal::Unread(Box::new(reading(Some(&"x".repeat(40_000)), 0))),
    );
    let after = trace.snapshot().unwrap();
    assert!(
        !after.refusals[1].complete,
        "control requires actual second aggregate eviction"
    );
    assert!(
        after.refusals[2]
            .cause_projection
            .as_ref()
            .unwrap()
            .complete
    );
    assert_eq!(
        after.refusals[0].unavailable_reason, first.unavailable_reason,
        "never-retained original must not falsely claim displacement"
    );
    assert_eq!(
        serde_json::to_value(&after.refusals[0]).unwrap(),
        serde_json::to_value(&first).unwrap(),
        "an absent aggregate and its original causes remain unchanged"
    );
    assert!(
        after.refusals[1]
            .unavailable_reason
            .unwrap()
            .contains("displaced")
    );
    assert!(retained_bytes(&after) <= ORIGINAL_BYTES_PER_FLIGHT);
}

#[test]
fn later_causes_displace_prior_success_metrics_but_not_prior_causes() {
    let trace = Trace::new(Instant::now(), Arc::new(AtomicBool::new(false)));
    trace.refusal(
        "delta",
        &ProjectionRefusal::Unread(Box::new(reading(Some("first synthetic cause"), 20_000))),
    );
    let before = trace.snapshot().unwrap();
    assert!(before.refusals[0].complete);
    let first_causes = serde_json::to_value(&before.refusals[0].cause_projection).unwrap();
    trace.refusal(
        "rebuild_48h",
        &ProjectionRefusal::RebuildRequired("original rebuild reason".into()),
    );
    let why = "synthetic α\n\"".repeat(2_000);
    trace.refusal(
        "fallback_24h",
        &ProjectionRefusal::Unread(Box::new(reading(Some(&why), 0))),
    );
    trace.finish("refused");
    let flight = trace.snapshot().unwrap();
    let first = &flight.refusals[0];
    assert!(
        !first.complete,
        "control: later cause must actually require eviction"
    );
    assert!(first.original.is_none());
    assert_eq!(first.retained_bytes, 0);
    assert_eq!(first.retained_causes, 0);
    assert_eq!(first.omitted_causes, first.total_causes);
    assert_eq!(first.omitted_stations, first.total_stations);
    assert_eq!(first.omitted_spans, first.total_spans);
    assert!(first.unavailable_reason.unwrap().contains("displaced"));
    assert_eq!(
        serde_json::to_value(&first.cause_projection).unwrap(),
        first_causes
    );
    assert_eq!(
        flight.refusals[1].original.as_ref().unwrap()["rebuild_required"],
        "original rebuild reason"
    );
    let last = serde_json::to_value(&flight.refusals[2].cause_projection).unwrap();
    assert_eq!(last["complete"], true);
    assert_eq!(
        last["original_causes"]["stations"][0]["unknown"][0]["reason"],
        why
    );
    assert!(retained_bytes(&flight) <= ORIGINAL_BYTES_PER_FLIGHT);
    assert!(!flight.record_complete);
}

#[test]
fn genuine_cause_overflow_counts_utf8_and_escaping_without_retaining_a_prefix() {
    let why = "synthetic α\n\"".repeat(10_000);
    let input = reading(Some(&why), 0);
    let captured = causes::CauseProjection::capture(&input);
    let expected = json!({"stations": [{"station": "synthetic", "scope": {"OwnerIs": "alice"},
        "caller_binding": "alice", "unknown": input.stations[0].unknown}],
        "since": input.since, "until": input.until, "audit_cutoff": input.audit_cutoff, "denied": input.denied});
    assert_eq!(
        captured.total_bytes,
        serde_json::to_vec(&expected).unwrap().len()
    );
    assert!(captured.total_bytes_complete);
    assert!(!captured.complete);
    assert_eq!(captured.retained_bytes, 0);
    assert_eq!(captured.omitted_stations, 1);
    assert_eq!(captured.omitted_spans, 1);
    assert_eq!(captured.omitted_causes, 1);
    assert!(captured.original_causes.is_none());
    assert!(captured.unavailable_reason.is_some());
}

#[test]
fn cause_budget_is_shared_and_a_rejected_cause_does_not_evict_retained_evidence() {
    let trace = Trace::new(Instant::now(), Arc::new(AtomicBool::new(false)));
    let why = "α\n\"".repeat(5_000);
    for attempt in ["delta", "rebuild_48h", "fallback_24h"] {
        trace.refusal(
            attempt,
            &ProjectionRefusal::Unread(Box::new(reading(Some(&why), 50_000))),
        );
    }
    let flight = trace.snapshot().unwrap();
    assert!(
        flight.refusals[0]
            .cause_projection
            .as_ref()
            .unwrap()
            .complete
    );
    assert!(
        flight.refusals[1]
            .cause_projection
            .as_ref()
            .unwrap()
            .complete
    );
    let omitted = flight.refusals[2].cause_projection.as_ref().unwrap();
    assert!(!omitted.complete);
    assert_eq!(omitted.omitted_causes, 1);
    assert_eq!(omitted.retained_bytes, 0);
    assert!(omitted.total_bytes_complete);
    assert!(retained_bytes(&flight) <= ORIGINAL_BYTES_PER_FLIGHT);
}

#[test]
fn an_exact_cause_byte_boundary_is_retained_and_one_more_byte_is_not() {
    let base = causes::CauseProjection::capture(&reading(Some(""), 0)).total_bytes;
    let why = "a".repeat(ORIGINAL_BYTES_PER_FLIGHT - base);
    let exact = causes::CauseProjection::capture(&reading(Some(&why), 0));
    assert_eq!(exact.total_bytes, ORIGINAL_BYTES_PER_FLIGHT);
    assert!(exact.complete);
    let overflow = causes::CauseProjection::capture(&reading(Some(&(why + "a")), 0));
    assert_eq!(overflow.total_bytes, ORIGINAL_BYTES_PER_FLIGHT + 1);
    assert!(!overflow.complete);
    assert!(overflow.original_causes.is_none());
}

#[test]
fn empty_unknowns_preserve_context_without_inventing_causes() {
    let captured = causes::CauseProjection::capture(&reading(None, 50_000));
    assert!(captured.complete);
    assert_eq!(captured.retained_stations, 1);
    assert_eq!(captured.total_spans, 0);
    assert_eq!(captured.total_causes, 0);
    let value = serde_json::to_value(&captured.original_causes).unwrap();
    assert_eq!(value["stations"][0]["unknown"], json!([]));
    assert_eq!(value["stations"][0]["scope"], json!({"OwnerIs": "alice"}));
}

#[test]
fn serialization_failure_and_count_overflow_are_not_successful_prefixes() {
    struct Fails;
    impl Serialize for Fails {
        fn serialize<S: serde::Serializer>(&self, _: S) -> Result<S::Ok, S::Error> {
            Err(serde::ser::Error::custom("synthetic serialization refusal"))
        }
    }
    let mut writer = BoundedOriginal {
        bytes: Vec::new(),
        total: 0,
        bound: 4,
    };
    assert!(serde_json::to_writer(&mut writer, &Fails).is_err());
    assert!(writer.bytes.is_empty());
    writer.total = usize::MAX;
    assert!(std::io::Write::write(&mut writer, b"x").is_err());
}
