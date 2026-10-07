//! Daily requests reuse the signed plan verb; the clock grants no delete authority.
use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use axum::{Json, Router, extract::State, http::StatusCode, routing::post};
use boss_core::calendar::Cadence;
use boss_dispatcher::rules::{
    expr::{EvalError, HelperResolver, NoHelpers, Value},
    handler::{Handler, InvocationContext},
    jobs_spawn::JobsSpawn,
    registry::{MatchedRule, RawRule, Registry, match_event, parse_raw_path},
    schedule_runner::ScheduleRunner,
};
use boss_dispatcher_handlers::handlers::{
    cadence_roster::{Guard, clock_cadences},
    cadence_silence::{
        Verdict, Watched, explained_by, measurement, newest_packet_at, oldest_open_block, verdict,
    },
};
use chrono::{Duration, NaiveDate, TimeZone, Utc};
use serde_json::json;

const RULE: &str = "request-a-pod-reap-daily";
const WATCH: &str = "watch-pod-reap-outcomes";
const SUBJECT: &str = "pod-reap";
const VERB: &str = "reap-terminated-pods";

fn row(name: &str) -> RawRule {
    parse_raw_path(boss_testing::dispatcher_rules_dir().join(format!("{name}.toml")))
        .expect("the authored rule parses")
        .rules
        .remove(0)
}

struct Open {
    answer: Result<bool, ()>,
    asked: Mutex<Vec<(String, String)>>,
}

impl Open {
    fn new(answer: Result<bool, ()>) -> Self {
        Self {
            answer,
            asked: Mutex::new(Vec::new()),
        }
    }
}

impl HelperResolver for Open {
    fn call(&self, name: &str, args: &[Value]) -> Result<Value, EvalError> {
        assert_eq!(name, "open_job_exists");
        let [Value::String(kind), Value::String(subject)] = args else {
            panic!("the guard must ask about an exact kind and subject: {args:?}")
        };
        self.asked
            .lock()
            .unwrap()
            .push((kind.clone(), subject.clone()));
        self.answer
            .map(Value::Bool)
            .map_err(|()| EvalError::UnknownHelper("unreadable jobs port".into()))
    }
}

fn day(offset: u64) -> NaiveDate {
    NaiveDate::from_ymd_opt(2026, 10, 3).unwrap() + chrono::Days::new(offset)
}

fn fire(offset: u64, open: &Open) -> Vec<MatchedRule> {
    let reg = Registry::from_raw(boss_dispatcher::rules::registry::RawRegistry {
        rules: vec![row(RULE)],
    })
    .expect("compile the authored rule");
    ScheduleRunner::matched_for_day(&reg, &HashMap::new(), day(offset), open).0
}

fn arg<'a>(args: &'a [(String, Value)], key: &str) -> &'a Value {
    &args
        .iter()
        .find(|(k, _)| k == key)
        .unwrap_or_else(|| panic!("missing {key}: {args:?}"))
        .1
}

#[test]
fn every_day_requests_the_same_bounded_signed_verb() {
    assert_eq!(row(RULE).schedule.unwrap().cadence, Cadence::Daily);
    for offset in 0..7 {
        let open = Open::new(Ok(false));
        let matched = fire(offset, &open);
        assert_eq!(matched.len(), 1);
        assert_eq!(matched[0].invocations.len(), 1);
        let invocation = &matched[0].invocations[0];
        assert_eq!(invocation.handler, "jobs.spawn");
        for (key, expected) in [
            ("kind", "ops-request"),
            ("subject_kind", "custom"),
            ("subject", SUBJECT),
            ("metadata.host", "forge"),
            ("metadata.verb", VERB),
        ] {
            assert_eq!(arg(&invocation.args, key), &Value::String(expected.into()));
        }
        assert_eq!(
            arg(&invocation.args, "metadata.requires_approval"),
            &Value::Bool(true)
        );
        // Empty command arguments are the envelope, not signed authority.
        assert_eq!(arg(&invocation.args, "metadata.args"), &Value::List(vec![]));
        for key in [
            "metadata.plan",
            "metadata.plan_sha256",
            "metadata.signature",
        ] {
            assert!(
                !invocation.args.iter().any(|(k, _)| k == key),
                "the clock may not supply signed authority"
            );
        }
        assert_eq!(
            *open.asked.lock().unwrap(),
            [("ops-request".into(), SUBJECT.into())]
        );
    }
}

#[test]
fn an_unsigned_request_dedups_each_day_but_a_drained_queue_opens_again() {
    for offset in 1..5 {
        assert!(fire(offset, &Open::new(Ok(true))).is_empty());
    }
    assert_eq!(fire(5, &Open::new(Ok(false))).len(), 1);
}

#[test]
fn an_unreadable_guard_never_opens_a_request() {
    assert!(fire(0, &Open::new(Err(()))).is_empty());
}

fn watched() -> Watched {
    let (cadences, excluded) = clock_cadences(&[row(RULE)]);
    assert!(excluded.is_empty());
    assert_eq!(cadences.len(), 1);
    let c = &cadences[0];
    assert_eq!(c.label(), "ops-request/pod-reap");
    assert_eq!(c.interval_min, 1440);
    assert!(
        matches!(&c.guard, Some(Guard::OpenPacket { kind, subject, .. }) if kind == "ops-request" && subject == SUBJECT)
    );
    Watched::from_clock_rule(c)
}

fn opening() -> chrono::DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 10, 3, 0, 0, 0).unwrap()
}

#[test]
fn the_declared_cadence_reports_and_then_alarms_an_unsigned_hold() {
    let w = watched();
    let rows = [
        json!({"id":"11111111-1111-4111-8111-111111111111", "title":"Signed pod reap awaits approval", "metadata":{"opened_at":opening().to_rfc3339()}}),
    ];
    let block = oldest_open_block(&rows, row(RULE).when.as_deref().unwrap()).unwrap();
    for (minutes, alarming) in [
        (2 * 1440 + 1, false),
        (3 * 1440, false),
        (3 * 1440 + 1, true),
    ] {
        let now = opening() + Duration::minutes(minutes);
        let v = explained_by(
            verdict(&w.declared, newest_packet_at(&rows), None, now),
            &block,
            now,
        );
        assert!(
            matches!(&v, Verdict::Suppressed { by_job, alarming: a, .. } if by_job == &block.job_id && *a == alarming)
        );
        assert_eq!(v.is_finding(), alarming);
        let m = measurement(&w, &v, now);
        assert_eq!(m["suppressed_by_job"], rows[0]["id"]);
        assert_eq!(m["suppression_threshold_minutes"], 3 * 1440);
        assert_eq!(m["past_suppression_threshold"], alarming);
        assert_eq!(m["cadence_declared_by"], format!("rule:{RULE}"));
    }
}

#[test]
fn a_missing_arrival_alarms_after_two_intervals_and_unread_provenance_stays_loud() {
    let w = watched();
    assert_eq!(
        verdict(
            &w.declared,
            Some(opening()),
            None,
            opening() + Duration::days(2)
        ),
        Verdict::Fresh
    );
    assert!(
        verdict(
            &w.declared,
            Some(opening()),
            None,
            opening() + Duration::days(2) + Duration::minutes(1)
        )
        .is_finding()
    );
    assert!(verdict(&w.declared, None, None, opening()).is_finding());
    let unread = [json!({"id":"holding", "metadata":{"opened_at":"not a timestamp"}})];
    assert!(oldest_open_block(&unread, row(RULE).when.as_deref().unwrap()).is_none());
    assert!(verdict(&w.declared, newest_packet_at(&unread), None, opening()).is_finding());
}

fn predicate(source: &str, payload: &serde_json::Value) -> Value {
    boss_expr::eval(
        &boss_expr::parse(source).unwrap(),
        &boss_expr::Context {
            payload,
            helpers: &NoHelpers,
        },
    )
    .unwrap()
}

#[tokio::test]
async fn the_real_spawn_port_carries_boolean_approval_and_preserves_provenance() {
    async fn create(
        State(writes): State<Arc<Mutex<Vec<serde_json::Value>>>>,
        Json(body): Json<serde_json::Value>,
    ) -> (StatusCode, Json<serde_json::Value>) {
        writes.lock().unwrap().push(body);
        (
            StatusCode::CREATED,
            Json(json!({"id":"11111111-1111-4111-8111-111111111111"})),
        )
    }
    let writes = Arc::new(Mutex::new(Vec::new()));
    let server = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = server.local_addr().unwrap();
    let app = Router::new()
        .route("/api/jobs", post(create))
        .with_state(writes.clone());
    let serving = tokio::spawn(async move { axum::serve(server, app).await.unwrap() });
    let matched = fire(0, &Open::new(Ok(false)));
    let context = InvocationContext {
        rule_name: RULE.into(),
        triggering_event_id: "clock-day-2026-10-03".into(),
        triggering_topic: "clock.day".into(),
        event_payload: json!({"_day":"2026-10-03"}),
        event_timestamp: None,
    };
    JobsSpawn::new(format!("http://{address}"))
        .invoke(&matched[0].invocations[0].args, &context)
        .await
        .unwrap();
    serving.abort();
    let writes = writes.lock().unwrap();
    assert_eq!(writes.len(), 1);
    let body = &writes[0];
    assert_eq!(body["metadata"]["requires_approval"], true);
    assert_eq!(body["metadata"]["spawned_by_rule"], RULE);
    assert_eq!(
        body["metadata"]["triggered_by_event_id"],
        context.triggering_event_id
    );
    assert_eq!(body["metadata"]["triggered_by_topic"], "clock.day");
    assert_eq!(body["owner_id"], format!("rule:{RULE}"));
    let wf = boss_jobs::registry::seedable_platform_workflows()
        .into_iter()
        .find(|w| w.kind == "ops-request")
        .unwrap();
    let approve = wf.steps.iter().find(|s| s.title == "approve").unwrap();
    let execute = wf.steps.iter().find(|s| s.title == "execute").unwrap();
    assert_eq!(
        approve.assurance_required,
        Some(boss_core::job::Assurance::Presence)
    );
    for (done, decision, ready) in [
        (false, "approved", false),
        (true, "declined", false),
        (true, "approved", true),
    ] {
        let payload = json!({"job":body,"steps":{"filed":{"done":true},"approve":{"done":done,"metadata":{"decision":decision}}}});
        assert_eq!(predicate(&approve.ready_when, &payload), Value::Bool(true));
        assert_eq!(predicate(&execute.ready_when, &payload), Value::Bool(ready));
    }
}

fn watch(outcome: &str, kind: &str) -> Vec<MatchedRule> {
    let reg = Registry::from_raw(boss_dispatcher::rules::registry::RawRegistry {
        rules: vec![row(WATCH)],
    })
    .unwrap();
    match_event(
        &reg,
        "jobs.job.closed",
        &json!({"id":"11111111-1111-4111-8111-111111111111", "kind":kind,"outcome":outcome}),
        &NoHelpers,
    )
    .matched
}

#[test]
fn every_answered_or_refused_reap_is_read_without_a_follow_on() {
    for outcome in ["answered", "refused"] {
        let hits = watch(outcome, "ops-request");
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].invocations.len(), 1);
        let inv = &hits[0].invocations[0];
        assert_eq!(inv.handler, "ops.judge");
        assert_eq!(arg(&inv.args, "verb"), &Value::String(VERB.into()));
        for key in ["then_verb", "then_host", "then_args"] {
            assert!(!inv.args.iter().any(|(k, _)| k == key));
        }
    }
    assert!(watch("cancelled", "ops-request").is_empty());
    assert!(watch("answered", "backlog-item").is_empty());
}

#[test]
fn the_watch_uses_the_verbs_exact_effect_not_an_early_reaping_line() {
    let hits = watch("answered", "ops-request");
    let args = &hits[0].invocations[0].args;
    let Value::String(pattern) = arg(args, "verdict_pattern") else {
        panic!("pattern must be a string")
    };
    let spec: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(
            boss_testing::repo_root().join("infra/ops/verbs/reap-terminated-pods.json"),
        )
        .unwrap(),
    )
    .unwrap();
    assert_eq!(pattern, spec["effect"].as_str().unwrap());
    assert_eq!(arg(args, "when"), &Value::String("true".into()));
    let re = regex::Regex::new(pattern).unwrap();
    for line in [
        "reap-terminated-pods: reaped 3 pod(s)",
        "reap-terminated-pods: reaped 0 pod(s)",
        "reap-terminated-pods: plan abc still holds and names no pod — nothing to reap",
    ] {
        assert!(re.is_match(line), "{line}");
    }
    for line in [
        "reap-terminated-pods: plan abc still holds — reaping 3 pod(s)",
        "reap-terminated-pods: FAILED — one target survived",
        "plan-sha256: abc",
        "unrelated: reap-terminated-pods: reaped 3 pod(s)",
        "reap-terminated-pods: reaped 3 pod(s) extra",
    ] {
        assert!(!re.is_match(line), "{line}");
    }
}
