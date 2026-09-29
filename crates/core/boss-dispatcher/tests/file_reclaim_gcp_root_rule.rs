//! `file-reclaim-gcp-root-while-disk-tight-boss-gcp` — the estate reading
//! that says boss-gcp is below its disk floor files the passkey-gated
//! request that remedies it, so David's only act is the passkey on the
//! rendered plan (backlog 3df309bf, car 1).
//!
//! Until this rule the request was a HAND act: on 2026-09-28 the alarm
//! d3c7eada (`disk_tight:boss-gcp`, open since the third below-floor
//! reading) waited on David typing `boss ops boss-gcp reclaim-gcp-root`
//! from a box that signs as him, and its `steps_for_david` still told him
//! to type a 64-hex hash the CLI has refused since 149553f67. Filing
//! grants nothing — the runner renders the plan onto the approve step and
//! runs the write only under a named approver's passkey on those bytes
//! (design 17835005) — so filing is the machine's.
//!
//! Pinned over the shipped rule FILE (`common::authored_rule`), against
//! the comparison payload as the live system of record carried it
//! (`GET /api/estate/comparisons?scope=host&host=boss-gcp`, event
//! b7c2d05a, 2026-09-28T10:25:02Z), not a shape reconstructed from the
//! handler.

use boss_dispatcher::rules::expr::{EvalError, HelperResolver, Value};
use boss_dispatcher::rules::registry::{Registry, match_event};
use serde_json::json;

mod common;

const RULE: &str = "file-reclaim-gcp-root-while-disk-tight-boss-gcp";
const TOPIC: &str = "jobs.estate.compared";
const VERB: &str = "reclaim-gcp-root";
/// The dedup key: the request's OWN subject, verb-specific, so a
/// read-only request against the same host (`custom/boss-gcp`, dozens a
/// day) never reads as "the reclaim is already asked for".
const SUBJECT: &str = "reclaim-gcp-root@boss-gcp";

fn rule() -> Registry {
    common::authored_rule(RULE)
}

/// The week the rule's window spans, in the hours `recent_job_exists`
/// takes: the reclaim alone cannot clear the finding (review of car
/// 4d80316f), so a request answered this week must not be refiled.
const WINDOW_HOURS: i64 = 168;

/// The two dedup helpers answering fixed values, recording every call
/// as `(helper, kind, subject, window)` so a test can assert the KEY, the
/// window, and that the read was made at all.
struct Requests {
    open: bool,
    recent: bool,
    asked: std::sync::Mutex<Vec<(String, String, String, Option<i64>)>>,
}

impl Requests {
    fn new(open: bool, recent: bool) -> Self {
        Self {
            open,
            recent,
            asked: std::sync::Mutex::new(Vec::new()),
        }
    }
    fn none() -> Self {
        Self::new(false, false)
    }
    fn asked(&self) -> Vec<(String, String, String, Option<i64>)> {
        self.asked.lock().expect("stub lock").clone()
    }
}

impl HelperResolver for Requests {
    fn call(&self, name: &str, args: &[Value]) -> Result<Value, EvalError> {
        let answer = match name {
            "open_job_exists" => self.open,
            "recent_job_exists" => self.recent,
            other => return Err(EvalError::UnknownHelper(other.to_string())),
        };
        if let (Some(Value::String(k)), Some(Value::String(s))) = (args.first(), args.get(1)) {
            let window = match args.get(2) {
                Some(Value::Int(h)) => Some(*h),
                _ => None,
            };
            self.asked.lock().expect("stub lock").push((
                name.to_string(),
                k.clone(),
                s.clone(),
                window,
            ));
        }
        Ok(Value::Bool(answer))
    }
}

fn asked_open() -> (String, String, String, Option<i64>) {
    (
        "open_job_exists".into(),
        "ops-request".into(),
        SUBJECT.into(),
        None,
    )
}

fn asked_recent() -> (String, String, String, Option<i64>) {
    (
        "recent_job_exists".into(),
        "ops-request".into(),
        SUBJECT.into(),
        Some(WINDOW_HOURS),
    )
}

/// The boss-gcp host comparison of 2026-09-28T10:25:01Z, verbatim from
/// the system of record (the `ops_credentials_absent` prose shortened).
fn boss_gcp_below_its_floor() -> serde_json::Value {
    json!({
        "_actor": "automation:rule:estate-compare-on-observation",
        "counts": {"disk_tight": 1, "drift": 1, "not_ready": 0, "observed": 1,
                   "observed_not_declared": 0, "ops_credentials_absent": 1},
        "findings": {
            "disk_tight": [{"disk_gb": 47, "floor_gb": 17, "free_gb": 11, "id": "boss-gcp"}],
            "drift": [{"fields": {"memory_gb": {"declared": 15, "observed": 16}}, "id": "boss-gcp"}],
            "not_ready": [],
            "observed_not_declared": [],
            "ops_credentials_absent": [{"act": "place /etc/boss-ops/kubeconfig …", "id": "boss-gcp",
                                        "state": "not ready: talosconfig:absent kubeconfig:absent"}],
            "ops_credentials_unmeasured": []
        },
        "host": "boss-gcp",
        "observed_at": "2026-09-28T10:25:01Z",
        "observer": "boss-estate-observe-host",
        "scope": "host"
    })
}

fn spawns(payload: &serde_json::Value, open: &Requests) -> Vec<Vec<(String, Value)>> {
    let out = match_event(&rule(), TOPIC, payload, open);
    assert!(
        out.skipped.is_empty(),
        "the rule must never skip-with-error on a comparison (a PredicateFailed is a NAK and, \
         eight redeliveries later, a dead letter): {:?}",
        out.skipped
    );
    out.matched
        .into_iter()
        .flat_map(|h| h.invocations)
        .filter(|i| i.handler == "jobs.spawn")
        .map(|i| i.args)
        .collect()
}

fn get(args: &[(String, Value)], k: &str) -> Value {
    args.iter()
        .find(|(n, _)| n == k)
        .map(|(_, v)| v.clone())
        .unwrap_or_else(|| panic!("arg {k} missing; got {args:?}"))
}

/// THE SHAPE THE RULE EXISTS FOR: the persisting finding, with no request
/// open, files exactly the request `boss ops boss-gcp reclaim-gcp-root`
/// would — the args an EMPTY LIST (the runner appends the hash from the
/// signed plan and refuses a request whose args are absent), and
/// `requires_approval` true, which is what makes the approve step ready.
#[test]
fn the_disk_tight_boss_gcp_finding_files_the_passkey_gated_reclaim() {
    let open = Requests::none();
    let hits = spawns(&boss_gcp_below_its_floor(), &open);
    assert_eq!(hits.len(), 1, "one request per firing; got {hits:?}");
    let a = &hits[0];

    assert_eq!(get(a, "kind"), Value::String("ops-request".into()));
    assert_eq!(get(a, "subject_kind"), Value::String("custom".into()));
    assert_eq!(get(a, "subject"), Value::String(SUBJECT.into()));
    // `jobs.spawn` strips the `metadata.` prefix and lands each of these
    // on the packet's metadata — the keys the runner and the approve
    // step's `ready_when` read.
    assert_eq!(get(a, "metadata.host"), Value::String("boss-gcp".into()));
    assert_eq!(get(a, "metadata.verb"), Value::String(VERB.into()));
    assert_eq!(
        get(a, "metadata.args"),
        Value::List(Vec::new()),
        "an empty LIST, never absent: verify_approval refuses a request whose args is not an array"
    );
    assert_eq!(get(a, "metadata.requires_approval"), Value::Bool(true));
    assert!(
        !a.iter().any(|(n, _)| n == "metadata.requested_by"),
        "no requested_by: nothing reads it, and `spawned_by_rule`, which jobs.spawn \
         stamps on every spawn, already names the requester: {a:?}"
    );
    assert_eq!(
        get(a, "metadata.remedies"),
        Value::String("disk_tight:boss-gcp".into()),
        "the finding it remedies, keyed exactly as estate.alarm keys the alarm"
    );
    let title = match get(a, "title") {
        Value::String(s) => s,
        other => panic!("title is a string: {other:?}"),
    };
    assert!(
        title.contains(VERB) && title.contains("disk_tight:boss-gcp"),
        "the packet says what it asks and why: {title}"
    );

    assert_eq!(
        open.asked(),
        vec![asked_open(), asked_recent()],
        "both dedups ask about the request's own verb-specific subject, once each, \
         the window a week"
    );
}

/// AT MOST ONE OPEN REQUEST. The boss-gcp host series is daily, so while
/// the floor stays breached every morning's comparison matches; with the
/// request already open (waiting on its passkey, or running) it files
/// nothing more — however old it is.
#[test]
fn an_open_reclaim_request_files_nothing_more() {
    let open = Requests::new(true, false);
    assert!(
        spawns(&boss_gcp_below_its_floor(), &open).is_empty(),
        "an open request is the one request"
    );
    assert_eq!(open.asked(), vec![asked_open()]);
}

/// THE REVIEW'S BLOCKER (car 4d80316f): the reclaim frees ~2.3 GB and
/// leaves boss-gcp at ~16.4 GB free against its 17 GB floor, so the
/// finding survives a SUCCESSFUL run. With only the open-set guard, the
/// request closing `answered` let the next morning file another — a
/// passkey prompt a day, the noise this rule exists to remove. A request
/// opened this week, whatever became of it, files nothing.
#[test]
fn a_request_answered_this_week_is_not_refiled_while_the_finding_persists() {
    let recent = Requests::new(false, true);
    assert!(
        spawns(&boss_gcp_below_its_floor(), &recent).is_empty(),
        "a reclaim answered, refused or rejected within the week is not asked for again"
    );
    assert_eq!(recent.asked(), vec![asked_open(), asked_recent()]);
}

/// Every comparison that does not carry `disk_tight:boss-gcp` files
/// nothing, and never reaches the dedup read — the helper costs a jobs
/// API call, and AND short-circuits, so the cheap payload checks go first.
#[test]
fn a_comparison_without_the_finding_files_nothing_and_asks_nothing() {
    // boss-gcp above its floor.
    let mut clean = boss_gcp_below_its_floor();
    clean["counts"]["disk_tight"] = json!(0);
    clean["findings"]["disk_tight"] = json!([]);

    // The forge below ITS floor: a different finding, a different verb.
    let mut forge = boss_gcp_below_its_floor();
    forge["host"] = json!("forge");
    forge["findings"]["disk_tight"][0]["id"] = json!("forge");

    // boss-gcp's five-minute unit series: same host, a scope whose
    // counts carry no `disk_tight` at all (Absent, which orders false).
    let units = json!({
        "counts": {"units_unhealthy": 0, "watched": 9},
        "findings": {"units_unhealthy": []},
        "host": "boss-gcp", "observed_at": "2026-09-28T15:30:01Z",
        "observer": "boss-estate-observe-units", "scope": "host-units"
    });

    // The cluster series: no host at all.
    let cluster = json!({
        "counts": {"disk_tight": 1, "observed": 6},
        "findings": {"disk_tight": [{"id": "w-2", "free_gb": 3, "floor_gb": 17, "disk_gb": 110}]},
        "observed_at": "2026-09-28T15:30:01Z",
        "observer": "boss-estate-observe", "scope": "kubernetes-nodes"
    });

    for (name, payload) in [
        ("clean", clean),
        ("forge", forge),
        ("units", units),
        ("cluster", cluster),
    ] {
        let open = Requests::none();
        assert!(
            spawns(&payload, &open).is_empty(),
            "{name}: no disk_tight:boss-gcp, no request"
        );
        assert!(
            open.asked().is_empty(),
            "{name}: the dedup is read only when the finding is present; asked {:?}",
            open.asked()
        );
    }
}

/// THE REQUEST THE RULE FILES IS ONE THE RUNNER WILL TAKE. The fact that
/// `args` is empty lives twice — in the rule's `[]` and in the verb
/// file's params, where `plan_sha256` is the last and only one — so it is
/// pinned (CLAUDE.md §9a): the verb exists, serves boss-gcp, declares
/// `requires_approval` with a plan verb and named approvers, and takes
/// exactly as many args before its hash as the rule files. A verb that
/// grows a param turns this red here, not as a refusal on David's queue.
#[test]
fn the_filed_request_matches_the_verb_file_the_runner_reads() {
    let path = boss_testing::repo_root().join(format!("infra/ops/verbs/{VERB}.json"));
    let verb: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display())),
    )
    .expect("the verb file is JSON");

    assert_eq!(verb["requires_approval"], json!(true), "{verb}");
    assert!(
        verb["plan_verb"].as_str().is_some_and(|p| !p.is_empty()),
        "a passkey signs a rendered plan, so the verb names its plan verb: {verb}"
    );
    assert!(
        verb["approvers"].as_array().is_some_and(|a| !a.is_empty()),
        "{verb}"
    );
    assert!(
        verb["hosts"]
            .as_array()
            .is_some_and(|h| h.contains(&json!("boss-gcp"))),
        "the verb serves the host the rule files it for: {verb}"
    );
    let params = verb["params"].as_array().expect("params");
    assert_eq!(
        params.last().and_then(|p| p["name"].as_str()),
        Some("plan_sha256"),
        "{verb}"
    );

    let filed = spawns(&boss_gcp_below_its_floor(), &Requests::none());
    let Value::List(args) = get(&filed[0], "metadata.args") else {
        panic!("args is a list")
    };
    assert_eq!(
        args.len(),
        params.len() - 1,
        "the runner refuses a request whose args are not exactly the verb's params minus the hash"
    );
}
