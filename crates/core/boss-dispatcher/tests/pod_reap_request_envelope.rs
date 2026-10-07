//! The daily request must carry the zero-argument envelope before approval.
use boss_dispatcher::rules::expr::{EvalError, HelperResolver, Value};
use boss_dispatcher::rules::jobs_spawn::metadata_arg_json;
use boss_dispatcher::rules::schedule_runner::ScheduleRunner;
use chrono::NaiveDate;
use std::collections::HashMap;

mod common;

struct OpenRequest(bool);
impl HelperResolver for OpenRequest {
    fn call(&self, name: &str, args: &[Value]) -> Result<Value, EvalError> {
        assert_eq!(name, "open_job_exists");
        assert_eq!(
            args,
            &[
                Value::String("ops-request".into()),
                Value::String("pod-reap".into())
            ]
        );
        Ok(Value::Bool(self.0))
    }
}

#[test]
fn daily_reap_materializes_empty_args_without_approval_material() {
    let registry = common::authored_rule("request-a-pod-reap-daily");
    let day = NaiveDate::from_ymd_opt(2026, 10, 4).unwrap();
    let (matched, _) =
        ScheduleRunner::matched_for_day(&registry, &HashMap::new(), day, &OpenRequest(false));
    assert_eq!(matched.len(), 1);
    assert_eq!(matched[0].invocations.len(), 1);
    let invocation = &matched[0].invocations[0];
    assert_eq!(invocation.handler, "jobs.spawn");
    let metadata: serde_json::Map<String, serde_json::Value> = invocation
        .args
        .iter()
        .filter_map(|(key, value)| {
            key.strip_prefix("metadata.")
                .and_then(|key| metadata_arg_json(value).map(|value| (key.to_owned(), value)))
        })
        .collect();
    assert_eq!(
        metadata.get("args"),
        Some(&serde_json::json!([])),
        "the signed-plan runner requires an actual empty array, not an absent or null argument list"
    );
    assert_eq!(metadata.get("host"), Some(&serde_json::json!("forge")));
    assert_eq!(
        metadata.get("verb"),
        Some(&serde_json::json!("reap-terminated-pods"))
    );
    assert_eq!(
        metadata.get("requires_approval"),
        Some(&serde_json::json!(true))
    );
    for key in ["plan_sha256", "approval_record", "decision", "signature"] {
        assert!(
            !metadata.contains_key(key),
            "the clock must not supply {key}"
        );
    }
    let (held, _) =
        ScheduleRunner::matched_for_day(&registry, &HashMap::new(), day, &OpenRequest(true));
    assert!(
        held.is_empty(),
        "an existing request still suppresses its twin"
    );
}
