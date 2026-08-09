//! The `notify-on-step-done-marked` rule (migration 106) — BOSS
//! alerting its operators that a wait is over.
//!
//! Pins the exact `when` expression the migration ships against the
//! expr engine: `notify_on_done = true` over the ALWAYS-PRESENT
//! top-level payload field (the binder resolves flat identifiers
//! only; this test caught the dotted-path version failing as
//! UnknownIdentifier → PredicateFailed → a dead-letter storm on
//! every step.done). Must match the opted-in payload, NOT match
//! the unmarked majority — one rule, explicitly-marked steps only,
//! is what keeps this from feeding the inbox-noise fire (6bf43b6f).

use boss_dispatcher::rules::expr::NoHelpers;
use boss_dispatcher::rules::registry::{Registry, match_event};

/// The migration row, expressed as the same TOML the registry loader
/// accepts — expression + args verbatim from 106.
const RULE: &str = r#"
[[rule]]
name = "notify-on-step-done-marked"
on_event = "step.done.*"
when = "notify_on_done = true"
[[rule.do]]
handler = "messages.notify"
args = { id_prefix = "\"done\"" }
"#;

#[test]
fn marked_step_done_matches_and_unmarked_does_not() {
    let reg = Registry::from_toml(RULE).expect("rule parses");

    let marked = serde_json::json!({
        "job_id": "j1", "step_id": "s1", "kind": "task", "notify_on_done": true,
        "metadata": { "authority_role": "platform-admin", "notify_on_done": true }
    });
    let hits = match_event(&reg, "step.done.task", &marked, &NoHelpers).expect("eval");
    assert_eq!(hits.len(), 1, "opted-in step notifies");
    let (k, v) = &hits[0].invocations[0].args[0];
    assert_eq!(k, "id_prefix");
    assert_eq!(
        v,
        &boss_dispatcher::rules::expr::Value::String("done".into()),
        "the dedup prefix rides the rule args"
    );

    let unmarked = serde_json::json!({
        "job_id": "j1", "step_id": "s2", "kind": "task", "notify_on_done": false,
        "metadata": { "authority_role": "platform-admin" }
    });
    let hits = match_event(&reg, "step.done.task", &unmarked, &NoHelpers).expect("eval");
    assert!(hits.is_empty(), "the unmarked majority stays silent");
}
