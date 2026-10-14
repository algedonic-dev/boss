//! `jobs.complete_step_matching` never completes nothing in silence.
//!
//! Until backlog f4cfb793 (2026-10-09) an event whose payload had no
//! `id` — `ledger.period.locked`, which names a period and no packet —
//! returned before the handler's first log line, so a rule on it
//! selected every lock and completed nothing with no record anywhere.
//! A non-packet event now completes under its own event id; one that
//! carries NEITHER a payload id nor an event id cannot be evidenced,
//! and that refusal is a warning naming the rule.
//!
//! **This file holds exactly one test, and that is the point.** It
//! asserts what the handler logged, and a captured log is deterministic
//! only when the capturing test is alone in its process with the
//! subscriber installed globally before the first event
//! (`boss-testing/tests/a_log_capturing_test_owns_its_process.rs`).

use boss_dispatcher::rules::expr::Value;
use boss_dispatcher::rules::handler::{Handler, InvocationContext};
use boss_dispatcher_handlers::handlers::common::api_client;
use boss_dispatcher_handlers::handlers::jobs_complete_step_matching::JobsCompleteStepMatching;
use serde_json::json;
use std::io::Write;
use std::sync::{Arc, Mutex};

#[derive(Clone, Default)]
struct Captured(Arc<Mutex<Vec<u8>>>);

impl Captured {
    fn text(&self) -> String {
        String::from_utf8_lossy(&self.0.lock().unwrap()).into_owned()
    }
}

impl Write for Captured {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(buf);
        Ok(buf.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for Captured {
    type Writer = Captured;
    fn make_writer(&'a self) -> Self::Writer {
        self.clone()
    }
}

#[tokio::test]
async fn an_event_with_no_identity_is_refused_by_name() {
    let log = Captured::default();
    let subscriber = tracing_subscriber::fmt()
        .with_writer(log.clone())
        .with_ansi(false)
        .finish();
    tracing::subscriber::set_global_default(subscriber)
        .expect("this binary holds one test, so nothing else has claimed the subscriber");

    let args: Vec<(String, Value)> = vec![
        ("kind".into(), Value::String("close-a-period".into())),
        ("step".into(), Value::String("lock".into())),
        ("match_step".into(), Value::String("period".into())),
        ("match_field".into(), Value::String("period_id".into())),
        ("event_path".into(), Value::String("period_id".into())),
    ];
    // No payload id, and the two event ids a runner can hand over that
    // name nothing: empty, and its `unknown` stand-in for an envelope
    // with no id. Nothing is fetched on this path, so the base is never
    // dialled.
    let h = JobsCompleteStepMatching::with_client(api_client(), "http://127.0.0.1:9");
    for event_id in ["", "unknown"] {
        let ctx = InvocationContext {
            event_timestamp: None,
            rule_name: "a-period-lock-completes-its-close".into(),
            triggering_event_id: event_id.into(),
            triggering_topic: "ledger.period.locked".into(),
            event_payload: json!({
                "period_id": "66666666-6666-6666-6666-666666666666",
                "locked_by": "emp-david",
                "checksum": "sha256:d26b",
            }),
        };
        h.invoke(&args, &ctx)
            .await
            .expect("a redelivery would find the same absence, so it is not an error");
    }

    let out = log.text();
    let refusals: Vec<&str> = out
        .lines()
        .filter(|l| l.contains("WARN") && l.contains("a-period-lock-completes-its-close"))
        .collect();
    assert_eq!(
        refusals.len(),
        2,
        "each delivery warns, naming the rule: {out}"
    );
    assert!(
        refusals
            .iter()
            .all(|l| l.contains("completing nothing") && l.contains("ledger.period.locked")),
        "the warning says what it did and on which topic: {out}"
    );
}
