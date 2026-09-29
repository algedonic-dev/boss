//! `jobs.complete_linked_step` under the rule
//! complete-publish-pr-step-on-publish-github-pr-answered — both legs,
//! against ops-request c98a782f's EXACT recorded output (backlog
//! f47861a5, measured 2026-09-19 on publish 254177e2).
//!
//! The defect: the verb printed `publish-github-pr: FAILED — pushing
//! publish/2026-09-18 to the forge (…) as david: fatal: detected
//! dubious ownership …`, exited 1, the runner completed `execute` with
//! `exit_code: "1"`, the request closed `answered` (the verb ran) and
//! nothing read how it went: the publish's open-pr step sat `ready` for
//! five hours, no packet was filed, and the yard drew the publish like
//! one in progress. Here the rule's args are read from its file, the
//! handler is driven the way the dispatcher drives it, and every write
//! it makes is recorded by a stand-in jobs API.
//!
//! Lives under `tests/` rather than beside the handler because the
//! recorded output names the forge by address, and the-estate-address-
//! lives-once allows a fixture to spell what it stubs only here.

use boss_dispatcher::rules::expr::{NoHelpers, Value};
use boss_dispatcher::rules::handler::{Handler, HandlerError, InvocationContext};
use boss_dispatcher::rules::registry::{Registry, match_event};
use boss_dispatcher_handlers::handlers::jobs_complete_linked_step::JobsCompleteLinkedStep;
use serde_json::json;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};

const RULE: &str = "complete-publish-pr-step-on-publish-github-pr-answered";
/// The request, the publish, and the step — the ids the measurement
/// was made on.
const REQUEST: &str = "c98a782f-adb4-40a9-860a-456063cfe66a";
const PUBLISH: &str = "254177e2-6c1a-4b7e-9d2f-3a4b5c6d7e8f";
const OPEN_PR: &str = "9a8b7c6d-5e4f-4a3b-8c2d-1e0f9a8b7c6d";
const MINTED: &str = "0f1e2d3c-4b5a-4968-8776-655443322110";
const OWNER: &str = "emp-owner";

/// c98a782f's execute-step `output`, byte for byte (the em dashes, the
/// tab git prints before its safe.directory hint, the trailing
/// newline).
const FAILED_OUTPUT: &str = "publish-github-pr: packet 254177e2 — open-pr ready; publishing forge main as publish/2026-09-18\n\
publish-github-pr: snapshot 1a904069d2fa2003c7f731bd987212751e910d02 (tree 01186def6e50833f23d324088d6d0d0002ce4848 of forge 11e3541c2eac3b5ddf0118cce271496c4abc102f, parent mirror 158553d75eef681e2ff0d101e751cb83e0366a10)\n\
publish-github-pr: fork dauld/boss-mirror confirmed in algedonic-dev/boss's network (fork=true parent=algedonic-dev/boss source=algedonic-dev/boss private=?)\n\
publish-github-pr: FAILED — pushing publish/2026-09-18 to the forge (http://10.20.0.15:3000/david/boss.git) as david: fatal: detected dubious ownership in repository at '/var/lib/boss-publish/boss.git' To add an exception for this directory, call:  \tgit config --global --add safe.directory /var/lib/boss-publish/boss.git . Without it on the forge, the push mirror prunes the PR's head at the next train\n";

/// The FAILED line alone — what the step and the alert must carry.
const FAILED_LINE: &str = "publish-github-pr: FAILED — pushing publish/2026-09-18 to the forge (http://10.20.0.15:3000/david/boss.git) as david: fatal: detected dubious ownership in repository at '/var/lib/boss-publish/boss.git' To add an exception for this directory, call:  \tgit config --global --add safe.directory /var/lib/boss-publish/boss.git . Without it on the forge, the push mirror prunes the PR's head at the next train";

const PR_URL: &str = "https://github.com/algedonic-dev/boss/pull/241";
/// The snapshot the PR's head holds — what read-publish-checks reads
/// the checks of, and refuses the packet without (backlog 1f0aa60d).
const SNAPSHOT: &str = "28554177812cc9645ab0eff2158574c25286f34a";

/// The verb's happy path, as infra/forge/publish-github-pr.sh prints it.
fn opened_output() -> String {
    format!(
        "publish-github-pr: packet 254177e2 — open-pr ready; publishing forge main as publish/2026-09-18-<snapshot>\n\
         publish-github-pr: snapshot {SNAPSHOT} (tree t of forge f, parent mirror m) — branch publish/2026-09-18-28554177812c\n\
         publish-github-pr: pushed publish/2026-09-18-28554177812c to the forge as david — the off-site push carries it too\n\
         publish-github-pr: pushed dauld:publish/2026-09-18-28554177812c\n\
         publish-github-pr: opened {PR_URL} at {SNAPSHOT}\n\
         publish-github-pr: done — {PR_URL} (open-pr on 254177e2 completed; the merge is David's)\n"
    )
}

/// The re-run's line: the PR found open for the run's own branch.
fn reused_output() -> String {
    format!(
        "publish-github-pr: pushed dauld:publish/2026-09-18-28554177812c\n\
         publish-github-pr: PR already open for dauld:publish/2026-09-18-28554177812c — reusing {PR_URL} at {SNAPSHOT}\n\
         publish-github-pr: done — {PR_URL} (open-pr on 254177e2 completed; the merge is David's)\n"
    )
}

type Writes = Arc<Mutex<Vec<(String, String, serde_json::Value)>>>;

fn ctx() -> InvocationContext {
    ctx_for(RULE, REQUEST)
}

/// The same close marker for any rule and any answered request — the
/// release leg (v7) closes a different request under a different rule.
fn ctx_for(rule: &str, request_id: &str) -> InvocationContext {
    InvocationContext {
        event_timestamp: None,
        rule_name: rule.into(),
        triggering_event_id: format!("evt-close-{}", &request_id[..8]),
        triggering_topic: "jobs.job.closed".into(),
        event_payload: json!({
            "id": request_id, "kind": "ops-request", "outcome": "answered",
            "closed_on": "2026-09-18", "parent_step_id": null,
        }),
    }
}

/// The rule's args as the dispatcher hands them over — read from the
/// file, matched against the close marker, never retyped.
fn rule_args() -> Vec<(String, Value)> {
    rule_args_of(RULE)
}

/// The same, for any rule file that reacts to an answered ops-request
/// with this handler — the release rule (v7) is driven from its own
/// file, unedited, because that is the claim: a rule nobody touched
/// inherits the failure mode.
fn rule_args_of(rule: &str) -> Vec<(String, Value)> {
    let path = boss_testing::dispatcher_rules_dir().join(format!("{rule}.toml"));
    let toml =
        std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()));
    let reg = Registry::from_toml(&toml).expect("the rule file parses");
    let matched = match_event(&reg, "jobs.job.closed", &ctx().event_payload, &NoHelpers).matched;
    assert_eq!(
        matched.len(),
        1,
        "an answered ops-request fires the rule once"
    );
    let inv = &matched[0].invocations[0];
    assert_eq!(inv.handler, "jobs.complete_linked_step");
    inv.args.clone()
}

/// The answered request as the runner leaves it: the verb's output and
/// exit on `execute` — the one place the exit is recorded (50fede8b) —
/// and the publish's id under `for_publish` (the filing rule's v2).
fn request(exit: &str, output: &str) -> serde_json::Value {
    json!({
        "id": REQUEST,
        "kind": "ops-request",
        "title": "publish the mirror PR from the forge — a publish was approved",
        "status": "closed",
        "subject": { "subject_kind": "custom", "id": "forge" },
        "metadata": { "host": "forge", "verb": "publish-github-pr",
                      "for_publish": PUBLISH, "outcome": "answered",
                      "spawned_by_rule": "publish-github-pr-on-open-pr-ready" },
        "steps": [
            { "id": "r-filed", "spec_slug": "filed", "status": "completed", "metadata": {} },
            { "id": "r-execute", "spec_slug": "execute", "status": "completed",
              "metadata": { "authority_role": "platform-admin", "disposition": "answered",
                            "exit_code": exit, "output": output, "runner_host": "forge" } },
            { "id": "r-answered", "spec_slug": "answered", "status": "completed",
              "metadata": { "outcome_kind": "completed" } },
        ],
    })
}

/// The publish packet at its open-pr step.
fn publish(open_pr_status: &str, open_pr_metadata: serde_json::Value) -> serde_json::Value {
    json!({
        "id": PUBLISH,
        "kind": "publish-to-github",
        "title": "Publish to the public GitHub mirror — 2026-09-18",
        "status": "open",
        "subject": { "subject_kind": "custom", "id": "github-mirror" },
        "metadata": {},
        "steps": [
            { "id": "s-approve", "spec_slug": "approve", "status": "completed",
              "metadata": { "decision": "approved" } },
            { "id": OPEN_PR, "spec_slug": "open-pr", "status": open_pr_status,
              "metadata": open_pr_metadata },
            { "id": "s-pr-opened", "spec_slug": "pr-opened", "status": "pending", "metadata": {} },
        ],
    })
}

/// A stand-in jobs API: GET by id, the open listing (for the alert's
/// dedup), and every write recorded as (method, path, body).
async fn mock_jobs(
    jobs: Vec<serde_json::Value>,
    open_items: Vec<serde_json::Value>,
) -> (String, Writes) {
    use axum::{Json, Router, extract::Path, extract::Query, routing::get};
    let writes: Writes = Arc::new(Mutex::new(Vec::new()));
    let by_id: Arc<Mutex<HashMap<String, serde_json::Value>>> = Arc::new(Mutex::new(
        jobs.into_iter()
            .map(|j| (j["id"].as_str().unwrap_or_default().to_string(), j))
            .collect(),
    ));
    let open = Arc::new(open_items);

    let app = Router::new()
        .route("/api/jobs", {
            let open = open.clone();
            let writes = writes.clone();
            get(move |Query(q): Query<HashMap<String, String>>| {
                let open = open.clone();
                async move {
                    let kind = q.get("kind").cloned().unwrap_or_default();
                    let rows: Vec<serde_json::Value> =
                        open.iter().filter(|j| j["kind"] == kind).cloned().collect();
                    Json(json!({ "data": rows, "total": rows.len() }))
                }
            })
            .post(move |Json(body): Json<serde_json::Value>| {
                let writes = writes.clone();
                async move {
                    writes
                        .lock()
                        .unwrap()
                        .push(("POST".into(), "/api/jobs".into(), body));
                    (
                        axum::http::StatusCode::CREATED,
                        Json(json!({ "id": MINTED })),
                    )
                }
            })
        })
        .route("/api/jobs/{id}", {
            let by_id = by_id.clone();
            get(move |Path(id): Path<String>| {
                let by_id = by_id.clone();
                async move {
                    by_id
                        .lock()
                        .unwrap()
                        .get(&id)
                        .cloned()
                        .map(Json)
                        .ok_or(axum::http::StatusCode::NOT_FOUND)
                }
            })
        })
        .route("/api/jobs/{id}/metadata", {
            let writes = writes.clone();
            axum::routing::patch(
                move |Path(id): Path<String>, Json(body): Json<serde_json::Value>| {
                    let writes = writes.clone();
                    async move {
                        writes.lock().unwrap().push((
                            "PATCH".into(),
                            format!("/api/jobs/{id}/metadata"),
                            body,
                        ));
                        axum::http::StatusCode::NO_CONTENT
                    }
                },
            )
        })
        .route("/api/jobs/{id}/steps/{step_id}", {
            let writes = writes.clone();
            axum::routing::put(
                move |Path((id, step_id)): Path<(String, String)>,
                      Json(body): Json<serde_json::Value>| {
                    let writes = writes.clone();
                    async move {
                        // The step PUT as the decided end state of
                        // design 93d2bddb has it (e39a9d2a): a body
                        // carrying metadata is refused 409 and routed
                        // to the merge door below. Recorded as
                        // "PUT (409)", so a test sees the attempt.
                        if body.get("metadata").is_some() {
                            writes.lock().unwrap().push((
                                "PUT (409)".into(),
                                format!("/api/jobs/{id}/steps/{step_id}"),
                                body,
                            ));
                            return (
                                axum::http::StatusCode::CONFLICT,
                                Json(json!({
                                    "error": "a step PUT carries no metadata",
                                    "merge_door":
                                        format!("/api/jobs/{id}/steps/{step_id}/metadata"),
                                })),
                            );
                        }
                        writes.lock().unwrap().push((
                            "PUT".into(),
                            format!("/api/jobs/{id}/steps/{step_id}"),
                            body,
                        ));
                        (axum::http::StatusCode::OK, Json(json!({ "ok": true })))
                    }
                },
            )
        })
        .route("/api/jobs/{id}/steps/{step_id}/metadata", {
            let writes = writes.clone();
            axum::routing::patch(
                move |Path((id, step_id)): Path<(String, String)>,
                      Json(body): Json<serde_json::Value>| {
                    let writes = writes.clone();
                    async move {
                        writes.lock().unwrap().push((
                            "PATCH".into(),
                            format!("/api/jobs/{id}/steps/{step_id}/metadata"),
                            body,
                        ));
                        axum::http::StatusCode::NO_CONTENT
                    }
                },
            )
        });
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    (format!("http://{addr}"), writes)
}

fn handler(base: String) -> Arc<JobsCompleteLinkedStep> {
    JobsCompleteLinkedStep::with_client(
        boss_dispatcher_handlers::handlers::common::api_client(),
        base,
        Arc::new(boss_core::platform_owner::Fixed(OWNER.into())),
    )
}

fn step_patch(writes: &[(String, String, serde_json::Value)]) -> serde_json::Value {
    step_patch_on(writes, PUBLISH, OPEN_PR)
}

fn step_patch_on(
    writes: &[(String, String, serde_json::Value)],
    job: &str,
    step: &str,
) -> serde_json::Value {
    let path = format!("/api/jobs/{job}/steps/{step}/metadata");
    let found: Vec<_> = writes
        .iter()
        .filter(|(m, p, _)| m == "PATCH" && *p == path)
        .collect();
    assert_eq!(
        found.len(),
        1,
        "exactly one annotation on the open step: {writes:?}"
    );
    found[0].2.clone()
}

/// THE FAILED LEG — c98a782f. The step is annotated, never completed;
/// an urgent packet names the verb, the request and the line; and the
/// annotation names the packet, so the step points at its alert.
#[tokio::test]
async fn a_failed_publish_annotates_the_open_step_and_files_an_urgent_item() {
    let (base, writes) = mock_jobs(
        vec![request("1", FAILED_OUTPUT), publish("ready", json!({}))],
        vec![],
    )
    .await;
    handler(base)
        .invoke(&rule_args(), &ctx())
        .await
        .expect("runs");

    let w = writes.lock().unwrap().clone();
    assert!(
        !w.iter().any(|(m, _, _)| m == "PUT"),
        "a failed verb completes nothing: {w:?}"
    );

    let posts: Vec<_> = w.iter().filter(|(m, _, _)| m == "POST").collect();
    assert_eq!(posts.len(), 1, "one alert filed: {w:?}");
    let item = &posts[0].2;
    assert_eq!(item["kind"], "backlog-item");
    assert_eq!(item["priority"], "urgent");
    assert_eq!(
        item["owner_id"], OWNER,
        "filed to the platform owner the port answers"
    );
    let title = item["title"].as_str().unwrap_or("");
    assert!(
        title.contains("publish-github-pr") && title.contains("FAILED"),
        "the title names the verb and that it failed: {title}"
    );
    assert_eq!(
        item["metadata"]["for_request"], REQUEST,
        "the dedup key: the request judged"
    );
    assert_eq!(item["metadata"]["for_packet"], PUBLISH);
    assert_eq!(item["metadata"]["verb"], "publish-github-pr");
    assert_eq!(item["metadata"]["exit"], "1");
    assert_eq!(
        item["metadata"]["failed"], FAILED_LINE,
        "the line, verbatim"
    );
    let detail = item["metadata"]["detail"].as_str().unwrap_or("");
    assert!(
        detail.contains("dubious ownership") && detail.contains("open-pr"),
        "the detail carries the cause and names the step left open: {detail}"
    );

    let note = step_patch(&w);
    assert_eq!(note["failed"], FAILED_LINE);
    assert_eq!(note["failed_exit"], "1");
    assert_eq!(note["failed_source"], REQUEST);
    assert_eq!(
        note["alert"], MINTED,
        "the step points at the packet filed for it"
    );
    // The list lifts exactly these keys off a step for `failed_verbs=true`
    // (backlog ea80b5fd), so the note and that list are one fact held
    // equal here (CLAUDE.md §9a): a key added to the note and not the
    // list would never reach the receiving yard.
    let mut written: Vec<&str> = note
        .as_object()
        .map(|o| o.keys().map(String::as_str).collect())
        .unwrap_or_default();
    written.sort_unstable();
    let mut lifted = boss_jobs::http::FAILED_VERB_KEYS.to_vec();
    lifted.sort_unstable();
    assert_eq!(
        written, lifted,
        "the note's keys are the keys the list lifts"
    );
    assert!(
        !w.iter()
            .any(|(_, p, _)| p == &format!("/api/jobs/{REQUEST}/metadata")),
        "the failure is not a dead-link noop, so no obligation_noop note: {w:?}"
    );
}

/// THE OPENED LEG. The verb completes the step itself on its happy
/// path; when it has not (its own PUT failed after the PR opened, or a
/// redelivery races it), the rule completes open-pr with the url copied
/// from the verb's `opened` line.
#[tokio::test]
async fn an_opened_pr_completes_the_open_pr_step_with_its_url() {
    let (base, writes) = mock_jobs(
        vec![request("0", &opened_output()), publish("ready", json!({}))],
        vec![],
    )
    .await;
    handler(base)
        .invoke(&rule_args(), &ctx())
        .await
        .expect("runs");

    let w = writes.lock().unwrap().clone();
    let puts: Vec<_> = w.iter().filter(|(m, _, _)| m == "PUT").collect();
    assert_eq!(puts.len(), 1, "exactly the open-pr step completed: {w:?}");
    assert_eq!(puts[0].1, format!("/api/jobs/{PUBLISH}/steps/{OPEN_PR}"));
    assert_eq!(
        puts[0].2,
        json!({ "status": "completed" }),
        "the flip carries the status alone (e39a9d2a)"
    );
    // The fields ride the step merge door, before the flip.
    let body = step_patch(&w);
    let merge_path = format!("/api/jobs/{PUBLISH}/steps/{OPEN_PR}/metadata");
    let merge_at = w
        .iter()
        .position(|(m, p, _)| m == "PATCH" && *p == merge_path)
        .unwrap();
    let flip_at = w.iter().position(|(m, _, _)| m == "PUT").unwrap();
    assert!(merge_at < flip_at, "merge first, then flip: {w:?}");
    assert_eq!(body["pr_url"], PR_URL, "copied from the verb's line");
    // The head the PR stands on, from the same line: without it the
    // next machine step (read-publish-checks) refuses the packet, which
    // is how 8d7a3507 stalled after a hand completion (backlog 1f0aa60d).
    assert_eq!(
        body["snapshot_commit"], SNAPSHOT,
        "copied from the verb's line"
    );
    assert_eq!(body["published_by"]["car"], REQUEST);
    assert!(
        !w.iter().any(|(m, _, _)| m == "POST"),
        "nothing to alert: {w:?}"
    );
}

/// The reuse line completes the step the same way — url AND snapshot.
#[tokio::test]
async fn a_reused_pr_completes_the_open_pr_step_with_its_url_and_snapshot() {
    let (base, writes) = mock_jobs(
        vec![request("0", &reused_output()), publish("ready", json!({}))],
        vec![],
    )
    .await;
    handler(base)
        .invoke(&rule_args(), &ctx())
        .await
        .expect("runs");

    let w = writes.lock().unwrap().clone();
    let body = step_patch(&w);
    assert_eq!(body["pr_url"], PR_URL, "{w:?}");
    assert_eq!(body["snapshot_commit"], SNAPSHOT, "{w:?}");
    assert_eq!(
        w.iter().filter(|(m, _, _)| m == "PUT").count(),
        1,
        "the step completed: {w:?}"
    );
}

/// A redelivery finds the step already annotated from this request
/// and writes nothing more — no second alert, no second note.
#[tokio::test]
async fn a_failure_already_written_is_not_written_twice() {
    let (base, writes) = mock_jobs(
        vec![
            request("1", FAILED_OUTPUT),
            publish(
                "ready",
                json!({ "failed": FAILED_LINE, "failed_exit": "1",
                        "failed_source": REQUEST, "alert": MINTED }),
            ),
        ],
        vec![],
    )
    .await;
    handler(base)
        .invoke(&rule_args(), &ctx())
        .await
        .expect("runs");
    assert!(
        writes.lock().unwrap().is_empty(),
        "{:?}",
        writes.lock().unwrap()
    );
}

/// A redelivery after the alert was filed but before the annotation
/// landed (JetStream is at-least-once, and the two writes are two
/// calls) reuses the open alert instead of filing a twin.
#[tokio::test]
async fn a_failure_whose_alert_is_already_open_reuses_it() {
    let (base, writes) = mock_jobs(
        vec![request("1", FAILED_OUTPUT), publish("ready", json!({}))],
        vec![
            json!({ "id": MINTED, "kind": "backlog-item", "status": "open",
                     "metadata": { "for_request": REQUEST } }),
        ],
    )
    .await;
    handler(base)
        .invoke(&rule_args(), &ctx())
        .await
        .expect("runs");
    let w = writes.lock().unwrap().clone();
    assert!(!w.iter().any(|(m, _, _)| m == "POST"), "no twin: {w:?}");
    assert_eq!(step_patch(&w)["alert"], MINTED);
}

/// The publish already closed (superseded, declined) or its open-pr
/// step never opened: nothing to annotate, nothing to alert — the
/// step the failure would have troubled is not there.
#[tokio::test]
async fn a_failure_against_a_step_that_is_not_open_writes_nothing_on_it() {
    let mut closed = publish("pending", json!({}));
    closed["status"] = json!("closed");
    let (base, writes) = mock_jobs(vec![request("1", FAILED_OUTPUT), closed], vec![]).await;
    handler(base)
        .invoke(&rule_args(), &ctx())
        .await
        .expect("runs");
    assert!(
        writes.lock().unwrap().is_empty(),
        "{:?}",
        writes.lock().unwrap()
    );
}

/// A mode this handler does not know is rule authoring — the same on
/// every redelivery, so Permanent.
#[tokio::test]
async fn an_unknown_on_failure_mode_is_a_permanent_error() {
    let (base, _) = mock_jobs(
        vec![request("1", FAILED_OUTPUT), publish("ready", json!({}))],
        vec![],
    )
    .await;
    let mut a = rule_args();
    for (k, v) in a.iter_mut() {
        if k == "on_failure" {
            *v = Value::String("retry-forever".into());
        }
    }
    let err = handler(base)
        .invoke(&a, &ctx())
        .await
        .expect_err("an unknown mode cannot be retried into a known one");
    assert!(matches!(err, HandlerError::Permanent(_)), "{err:?}");
}

/// THE DEFAULT (v7). The mode was opt-in when it landed, so it covered
/// the two rules whose author had just been burned by the silence and
/// no other — a rule that asks for nothing still left its step ready
/// and said nothing. Here the publish rule's own args are handed over
/// with `on_failure` REMOVED, and the failed leg must answer exactly as
/// it does with the arg present: the annotation on the step, the urgent
/// packet, and no completion.
#[tokio::test]
async fn a_rule_that_asks_for_no_mode_still_troubles_the_step_it_left_open() {
    let (base, writes) = mock_jobs(
        vec![request("1", FAILED_OUTPUT), publish("ready", json!({}))],
        vec![],
    )
    .await;
    let args: Vec<(String, Value)> = rule_args()
        .into_iter()
        .filter(|(k, _)| k != "on_failure")
        .collect();
    assert!(
        !args.iter().any(|(k, _)| k == "on_failure"),
        "the claim is about a rule that does not name the mode"
    );
    handler(base).invoke(&args, &ctx()).await.expect("runs");

    let w = writes.lock().unwrap().clone();
    assert!(
        !w.iter().any(|(m, _, _)| m == "PUT"),
        "a failed verb completes nothing: {w:?}"
    );
    assert_eq!(
        w.iter().filter(|(m, _, _)| m == "POST").count(),
        1,
        "the alert is filed without the rule asking: {w:?}"
    );
    let note = step_patch(&w);
    assert_eq!(note["failed"], FAILED_LINE);
    assert_eq!(note["failed_source"], REQUEST);
}

/// The opt-out. A rule whose linked step is not troubled by its verb
/// failing says so in one word, and gets v5's dead-link note instead:
/// nothing on the step, no alert, the noop recorded on both ends.
#[tokio::test]
async fn a_rule_that_opts_out_keeps_the_dead_link_note() {
    let (base, writes) = mock_jobs(
        vec![request("1", FAILED_OUTPUT), publish("ready", json!({}))],
        vec![],
    )
    .await;
    let mut args = rule_args();
    for (k, v) in args.iter_mut() {
        if k == "on_failure" {
            *v = Value::String("note".into());
        }
    }
    handler(base).invoke(&args, &ctx()).await.expect("runs");

    let w = writes.lock().unwrap().clone();
    assert!(
        !w.iter().any(|(m, _, _)| m == "POST" || m == "PUT"),
        "no alert, no completion: {w:?}"
    );
    for id in [REQUEST, PUBLISH] {
        assert!(
            w.iter().any(|(m, p, b)| m == "PATCH"
                && p == &format!("/api/jobs/{id}/metadata")
                && b.get("obligation_noop").is_some()),
            "the noop is noted on {id}: {w:?}"
        );
    }
}

/// The release leg's ids and fixtures — a second verb, a second
/// protocol, one shape.
const RELEASE_RULE: &str = "complete-release-tag-on-tag-release-answered";
const TAG_REQUEST: &str = "3a7c1b95-2d4e-4f60-8a1b-7c6d5e4f3a2b";
const RELEASE: &str = "5f4e3d2c-1b0a-4998-8877-665544332211";
const TAG_STEP: &str = "8c7b6a59-4837-4261-95a4-b3c2d1e0f9a8";

/// An answered tag-release request whose verb exited 1, linked to the
/// release packet by the `release` edge its rule follows.
fn tag_request(output: &str) -> serde_json::Value {
    json!({
        "id": TAG_REQUEST, "kind": "ops-request", "status": "closed",
        "title": "tag-release v1.4.0 on forge",
        "subject": { "subject_kind": "custom", "id": "forge" },
        "metadata": { "host": "forge", "verb": "tag-release", "release": RELEASE,
                      "outcome": "answered" },
        "steps": [
            { "id": "t-execute", "spec_slug": "execute", "status": "completed",
              "metadata": { "disposition": "answered", "exit_code": "1",
                            "output": output, "runner_host": "forge" } },
        ],
    })
}

/// The release packet at its `tag` step, open and waiting.
fn release_packet() -> serde_json::Value {
    json!({
        "id": RELEASE, "kind": "cut-a-release", "status": "open",
        "title": "Cut release v1.4.0",
        "subject": { "subject_kind": "custom", "id": "algedonic" },
        "metadata": {},
        "steps": [
            { "id": TAG_STEP, "spec_slug": "tag", "status": "ready", "metadata": {} },
        ],
    })
}

/// THE SECOND VERB, AND THE POINT OF THE DEFAULT (v7). The release
/// rule was written before the failure mode existed and names none, so
/// a tag-release that ran and failed left the release packet's `tag`
/// step `ready` with nothing on it — the measured shape of f47861a5 on
/// another verb, another protocol and another step. Driven from the
/// release rule's own file, unedited.
#[tokio::test]
async fn a_failed_tag_release_troubles_the_release_it_was_filed_for() {
    let failed = "tag-release: forge: no tag v1.4.0 on remote origin\n\
                  tag-release: converged checkout: 7f3a1c2 resolves to 7f3a1c2d4e5f60718293a4b5c6d7e8f901a2b3c4\n\
                  tag-release: FAILED — pushing refs/tags/v1.4.0 to remote origin (as david): remote: the account may not write to this repository. The local tag was removed; the forge holds nothing\n";
    let (base, writes) = mock_jobs(vec![tag_request(failed), release_packet()], vec![]).await;
    handler(base)
        .invoke(
            &rule_args_of(RELEASE_RULE),
            &ctx_for(RELEASE_RULE, TAG_REQUEST),
        )
        .await
        .expect("runs");

    let w = writes.lock().unwrap().clone();
    assert!(
        !w.iter().any(|(m, _, _)| m == "PUT"),
        "the tag step is not completed — the forge holds nothing: {w:?}"
    );
    let posts: Vec<_> = w.iter().filter(|(m, _, _)| m == "POST").collect();
    assert_eq!(posts.len(), 1, "one urgent packet: {w:?}");
    let item = &posts[0].2;
    assert_eq!(item["priority"], "urgent");
    assert_eq!(item["metadata"]["verb"], "tag-release");
    assert_eq!(item["metadata"]["for_request"], TAG_REQUEST);
    assert_eq!(item["metadata"]["step"], "tag");
    let failed_line = item["metadata"]["failed"].as_str().unwrap_or("");
    assert!(
        failed_line.contains("may not write to this repository"),
        "the alert names what failed: {failed_line}"
    );
    let note = step_patch_on(&w, RELEASE, TAG_STEP);
    assert_eq!(note["failed_exit"], "1");
    assert_eq!(note["failed_source"], TAG_REQUEST);
}

/// A REFUSAL BY THE VERB IS A FAILURE OF THE REQUEST TOO — and its
/// verdict must name the refusal, not the epilogue. The forge verbs'
/// `refuse()` prints the reason and then `  Nothing was written.`, and
/// exits 1 like `fail()` does, so the runner records an answered
/// request whose step says exit 1. Picking the last non-empty line
/// would hand the alert `Nothing was written.` — true, and no verdict
/// at all (CLAUDE.md §Diagnosis: a verdict must name what failed).
#[tokio::test]
async fn a_refused_tag_release_is_alerted_by_its_reason_not_its_epilogue() {
    let refused = "tag-release: forge: no tag v1.4.0 on remote origin\n\
                   tag-release: REFUSED — sha e4d5d9816d34 is not the merge commit of any of the 57 closed pr-train packets read\n\
                   tag-release:   Nothing was written.\n";
    let (base, writes) = mock_jobs(vec![tag_request(refused), release_packet()], vec![]).await;
    handler(base)
        .invoke(
            &rule_args_of(RELEASE_RULE),
            &ctx_for(RELEASE_RULE, TAG_REQUEST),
        )
        .await
        .expect("runs");

    let w = writes.lock().unwrap().clone();
    let posts: Vec<_> = w.iter().filter(|(m, _, _)| m == "POST").collect();
    assert_eq!(posts.len(), 1, "one urgent packet: {w:?}");
    let failed_line = posts[0].2["metadata"]["failed"].as_str().unwrap_or("");
    assert!(
        failed_line.contains("is not the merge commit"),
        "the alert names the refusal: {failed_line}"
    );
    assert!(
        !failed_line.contains("Nothing was written"),
        "and not the epilogue after it: {failed_line}"
    );
    assert_eq!(step_patch_on(&w, RELEASE, TAG_STEP)["failed"], failed_line);
}

/// THE READ-CHECKS LEG, FROM THE CLOCK (backlog fd808d90, measured
/// 2026-09-27). `reread-publish-pr-every-15-minutes` files
/// read-publish-checks every quarter hour, and between 14:00Z and 15:45Z
/// eight of those runs refused publish 8d7a3507 (exit 2, its open-pr
/// step carries no `snapshot_commit`), closed `answered`, and reached
/// nothing: a timer names no packet, so the requests carried no
/// `for_publish`, and this rule follows only that edge. The verb now
/// writes the edge onto its own request before it refuses
/// (read_publish_checks_sh.rs pins that half); these cases pin what the
/// answer rule does with it — and that a refusal repeated every fifteen
/// minutes is ONE alert, not ninety-six a day.
const READ_RULE: &str = "complete-publish-read-checks-on-read-publish-checks-answered";
const READ_PUBLISH: &str = "8d7a3507-27c4-4e92-b308-6f8043201b03";
const READ_CHECKS: &str = "4c3b2a19-0817-4f6e-9d5c-4b3a29180716";
/// One of the eight silent re-reads, and the step-ready request whose
/// alert (487e67bf) was the only one filed.
const REREAD_REQUEST: &str = "6378fa4d-9e8f-4a7b-8c6d-5e4f3a2b1c0d";
const EARLIER_REQUEST: &str = "bd6aae0a-f748-42f3-b92c-11507a4ef313";
const EARLIER_ALERT: &str = "487e67bf-1a2b-4c3d-8e4f-5a6b7c8d9e0f";
/// The refusal, as the verb printed it on every one of the eight.
const REFUSED_LINE: &str = "read-publish-checks: REFUSED — packet 8d7a3507: the open-pr step recorded no head sha (metadata.snapshot_commit is 'empty') — publish-github-pr writes it when it pushes; nothing was read";

/// A clock-spawned read request as it closes now: the edge the verb
/// wrote, the refusal, exit 2.
fn reread_request() -> serde_json::Value {
    json!({
        "id": REREAD_REQUEST, "kind": "ops-request", "status": "closed",
        "title": "re-read the mirror PRs and their checks from the forge — what GitHub says of each publish PR",
        "subject": { "subject_kind": "custom", "id": "github-mirror-checks" },
        "metadata": { "host": "forge", "verb": "read-publish-checks", "area": "platform",
                      "for_publish": READ_PUBLISH, "outcome": "answered",
                      "spawned_by_rule": "reread-publish-pr-every-15-minutes" },
        "steps": [
            { "id": "rr-execute", "spec_slug": "execute", "status": "completed",
              "metadata": { "disposition": "answered", "exit_code": "2", "runner_host": "forge",
                            "output": format!(
                                "read-publish-checks: request 6378fa4d names publish {READ_PUBLISH} (for_publish)\n{REFUSED_LINE}\n") } },
        ],
    })
}

/// Publish 8d7a3507 at read-checks, the step carrying `read_checks`.
fn publish_at_read_checks(read_checks: serde_json::Value) -> serde_json::Value {
    json!({
        "id": READ_PUBLISH, "kind": "publish-to-github", "status": "open",
        "title": "Publish to the public GitHub mirror",
        "subject": { "subject_kind": "custom", "id": "github-mirror" },
        "metadata": { "snapshot_commit": SNAPSHOT },
        "steps": [
            { "id": OPEN_PR, "spec_slug": "open-pr", "status": "completed",
              "metadata": { "pr_url": PR_URL } },
            { "id": READ_CHECKS, "spec_slug": "read-checks", "status": "ready",
              "metadata": read_checks },
            { "id": "s-judge", "spec_slug": "judge-checks", "status": "pending", "metadata": {} },
        ],
    })
}

/// The alert the first refusal filed, still open.
fn open_alert(step: &str, verb: &str) -> serde_json::Value {
    json!({ "id": EARLIER_ALERT, "kind": "backlog-item", "status": "open",
            "metadata": { "for_request": EARLIER_REQUEST, "for_packet": READ_PUBLISH,
                          "step": step, "verb": verb } })
}

async fn answer_reread(
    open_items: Vec<serde_json::Value>,
    read_checks: serde_json::Value,
) -> Vec<(String, String, serde_json::Value)> {
    let (base, writes) = mock_jobs(
        vec![reread_request(), publish_at_read_checks(read_checks)],
        open_items,
    )
    .await;
    handler(base)
        .invoke(
            &rule_args_of(READ_RULE),
            &ctx_for(READ_RULE, REREAD_REQUEST),
        )
        .await
        .expect("runs");
    writes.lock().unwrap().clone()
}

#[tokio::test]
async fn a_clock_spawned_refusal_troubles_read_checks_and_files_the_alert() {
    let w = answer_reread(vec![], json!({ "ops_verb": "read-publish-checks" })).await;
    assert!(
        !w.iter().any(|(m, _, _)| m == "PUT"),
        "a refusal completes nothing: {w:?}"
    );
    let posts: Vec<_> = w.iter().filter(|(m, _, _)| m == "POST").collect();
    assert_eq!(posts.len(), 1, "one alert: {w:?}");
    assert_eq!(posts[0].2["metadata"]["for_packet"], READ_PUBLISH);
    assert_eq!(posts[0].2["metadata"]["step"], "read-checks");
    assert_eq!(posts[0].2["metadata"]["failed"], REFUSED_LINE);
    let note = step_patch_on(&w, READ_PUBLISH, READ_CHECKS);
    assert_eq!(note["failed"], REFUSED_LINE, "the refusal, on the step");
    assert_eq!(note["failed_exit"], "2");
    assert_eq!(note["failed_source"], REREAD_REQUEST);
    assert_eq!(note["alert"], MINTED);
}

/// The next quarter hour refuses again, from a NEW request. The alert
/// the first one filed is still open for this packet's step, so it is
/// the alert — a twin per firing would be ninety-six urgent items a day
/// for one stuck publish, the silence traded for noise.
#[tokio::test]
async fn a_refusal_repeated_on_the_same_step_reuses_the_open_alert() {
    let w = answer_reread(
        vec![open_alert("read-checks", "read-publish-checks")],
        json!({ "ops_verb": "read-publish-checks" }),
    )
    .await;
    assert!(!w.iter().any(|(m, _, _)| m == "POST"), "no twin: {w:?}");
    let note = step_patch_on(&w, READ_PUBLISH, READ_CHECKS);
    assert_eq!(
        note["alert"], EARLIER_ALERT,
        "the step points at the open alert"
    );
    assert_eq!(note["failed_source"], REREAD_REQUEST);
}

/// And when the step already says exactly this, under that alert, there
/// is nothing new to write: the request itself is the record that it
/// happened again.
#[tokio::test]
async fn a_refusal_the_step_already_carries_under_its_open_alert_writes_nothing() {
    let w = answer_reread(
        vec![open_alert("read-checks", "read-publish-checks")],
        json!({ "ops_verb": "read-publish-checks", "failed": REFUSED_LINE, "failed_exit": "2",
                "failed_source": EARLIER_REQUEST, "alert": EARLIER_ALERT }),
    )
    .await;
    assert!(w.is_empty(), "{w:?}");
}

/// An open alert about ANOTHER step or verb on the same packet is not
/// this failure's alert.
#[tokio::test]
async fn an_open_alert_for_another_step_on_the_packet_is_not_reused() {
    let w = answer_reread(
        vec![open_alert("open-pr", "publish-github-pr")],
        json!({ "ops_verb": "read-publish-checks" }),
    )
    .await;
    assert_eq!(
        w.iter().filter(|(m, _, _)| m == "POST").count(),
        1,
        "a new alert for read-checks: {w:?}"
    );
    assert_eq!(
        step_patch_on(&w, READ_PUBLISH, READ_CHECKS)["alert"],
        MINTED
    );
}
