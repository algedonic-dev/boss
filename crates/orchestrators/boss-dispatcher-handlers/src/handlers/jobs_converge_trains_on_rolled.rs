//! `jobs.converge_trains_on_rolled` — the cluster-deploy-runner's report
//! that its rollout finished completes `converged` on the train whose
//! merge it rolled, the moment the report lands.
//!
//! THE GAP (backlog 090720c3, measured 2026-10-18 by run 73832267 over
//! 51 trains joined to the converge run that built their head): merge ->
//! converged p50 484 s = merge -> runner start 36 s (converge-on-merge
//! already starts it) + image build 330 s + push 2 s + rollout 53 s + 72 s
//! (p90 138 s) during which the cluster was ALREADY serving the merge and
//! nothing had read it. That remainder is the conductor's two-minute
//! `train-reconcile` pass (infra/platform/cadence/train-reconcile.toml):
//! a poll, and the only one left between a merge and an arrival. Every
//! train pays it, and while it waits the train holds the track.
//!
//! THE EVENT. `cluster-deploy-runner.sh` records `rolled = <full sha>` and
//! `rolled_at` on its own maintenance packet's `run` step the moment
//! `rollout status` of the source instance succeeds — through
//! `boss-step.sh`'s metadata-only door, mid-run, rather than in the
//! summary it hands its ExecStopPost when the WHOLE run ends (the other
//! instances, the verification and the orphan sweep come after the roll).
//! That merge emits `jobs.step.updated` carrying the step; the rule
//! `complete-train-converged-on-runner-rolled` selects it, and this
//! handler answers it.
//!
//! WHY THE DISPATCHER AND NOT THE CONDUCTOR. The conductor is a cadence
//! loop (`boss train cadence`, a 60 s tick) with no event subscription;
//! the dispatcher is the one process that reacts to an event. What the
//! conductor's `verify_convergence` judges for an IMAGE delivery is read
//! here exactly, from the same two sources, and written in the same
//! vocabulary — so the train records one kind of convergence whichever
//! of the two got there first. Reconcile is unchanged and stays the
//! safety net: a train this handler leaves alone (any refusal below, a
//! dark dispatcher, a report that never landed) converges on the next
//! pass, as every train did before.
//!
//! WHY NOT `jobs.complete_step_matching`. Its match is string equality and
//! a train's `merge_ref` is twelve characters where the runner's sha is
//! forty; it cannot leave a data-route train to the conductor, cannot ask
//! the cluster a second question, and renders no object, so the step's
//! required `delivery_evidence` could not carry the runner's receipt.
//!
//! WHAT IT JUDGES, in order — each refusal leaves the train to reconcile:
//!
//! 1. The event names a step whose packet, RE-READ, is the converge kind
//!    the rule names (`converge_kind`) and whose `run` step records a
//!    `rolled` that is a full commit (40 or 64 hex). A `rolled` key on any
//!    other packet completes nothing: the receipt is the runner's, or it
//!    is no receipt.
//! 2. The running cluster jobs API self-reports that commit
//!    (`/api/jobs/health` `capabilities.commit`, the conductor's proof).
//!    The runner's word is the trigger; the running binary is the second
//!    source, and both must agree (CLAUDE.md §Diagnosis: when a verdict
//!    has two sources, every consumer takes both). A cluster already
//!    rolled past it, or a pod not yet answering, is left to reconcile,
//!    which also asks git about ancestry — a question this handler,
//!    holding no clone, does not answer.
//! 3. Per open train (`train_kind`): `merged` is completed with a
//!    `merge_ref` naming that same commit (`boss_jobs::car_unland::
//!    same_commit`, the conductor's prefix rule); `converged` is open
//!    (ready or active — a pending one nothing has routed to is not this
//!    obligation's); and the train froze no DATA delivery plan
//!    (`metadata.delivery_plan.route = "data"`), whose channel obligations
//!    only the conductor's `delivery_plan::observe` can judge.
//!
//! WHAT IT WRITES — the conductor's image-basis completion, plus the
//! receipt: `cluster_commit` (the self-reported commit), `delivery_evidence`
//! `{schema 1, basis "image", arrived, merge_ref, health, rolled: {sha,
//! at, converge, step}}`, `verified` naming both sources, and
//! `completed_at` — the conductor's own stamp key, which the arrival
//! report and delivery timings read — taken from the event's instant,
//! never this process's clock (no-wallclock.sh, rule 3). Through the step
//! merge door, then the status alone (`common::complete_step`).
//!
//! IDEMPOTENT. JetStream is at-least-once and the runner's step is written
//! again by its ExecStopPost (the summary carries `rolled` too, so a
//! mid-run write that did not land is recovered there). A `converged`
//! already completed — by a first delivery or by the conductor — is not
//! open, and is not touched.

use std::sync::Arc;

use async_trait::async_trait;
use serde_json::{Map, Value, json};

use boss_dispatcher::rules::handler::{Handler, HandlerError, InvocationContext, arg_string};
use boss_jobs::car_unland::same_commit;

use super::common::{api_client, complete_step, get_json, open_jobs_of_kind};
use super::jobs_complete_linked_step::{is_open, step_by_slug};

/// The converge packet's step the runner records on.
pub(crate) const RUN_SLUG: &str = "run";
/// The keys `cluster-deploy-runner.sh` (`report_rolled`) writes there.
pub(crate) const ROLLED_KEY: &str = "rolled";
pub(crate) const ROLLED_AT_KEY: &str = "rolled_at";
/// The train's steps this reads and writes (infra/platform/workflows/pr-train.toml).
const MERGED_SLUG: &str = "merged";
const CONVERGED_SLUG: &str = "converged";

pub struct JobsConvergeTrainsOnRolled {
    client: boss_core::machine_token::Client,
    jobs_base: String,
}

impl JobsConvergeTrainsOnRolled {
    pub fn new(jobs_base: impl Into<String>) -> Arc<Self> {
        Self::with_client(api_client(), jobs_base)
    }

    /// Construct with a custom client (tests point it at a stand-in).
    pub fn with_client(
        client: boss_core::machine_token::Client,
        jobs_base: impl Into<String>,
    ) -> Arc<Self> {
        Arc::new(Self {
            client,
            jobs_base: jobs_base.into(),
        })
    }

    fn base(&self) -> &str {
        self.jobs_base.trim_end_matches('/')
    }
}

/// What the runner reported, read off its packet as it stands now.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Rolled {
    pub sha: String,
    pub at: Option<String>,
    pub converge: String,
    pub step: String,
}

/// PURE: the runner's receipt on a RE-READ converge packet, or why there
/// is none. Only a packet of `converge_kind` whose `run` step records a
/// full commit counts.
pub(crate) fn rolled_receipt(converge: &Value, converge_kind: &str) -> Result<Rolled, String> {
    let kind = converge.get("kind").and_then(Value::as_str).unwrap_or("");
    if kind != converge_kind {
        return Err(format!(
            "the packet is a {kind:?}, not a {converge_kind:?} — its `{ROLLED_KEY}` is no runner's receipt"
        ));
    }
    let run = step_by_slug(converge, RUN_SLUG)
        .ok_or_else(|| format!("the converge has no `{RUN_SLUG}` step"))?;
    let md = run.get("metadata");
    let sha = md
        .and_then(|m| m.get(ROLLED_KEY))
        .and_then(Value::as_str)
        .unwrap_or("");
    let full = matches!(sha.len(), 40 | 64) && sha.bytes().all(|b| b.is_ascii_hexdigit());
    if !full {
        return Err(format!(
            "`{RUN_SLUG}.{ROLLED_KEY}` is {sha:?}, not a full commit"
        ));
    }
    Ok(Rolled {
        sha: sha.to_string(),
        at: md
            .and_then(|m| m.get(ROLLED_AT_KEY))
            .and_then(Value::as_str)
            .map(str::to_string),
        converge: converge
            .get("id")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string(),
        step: run
            .get("id")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string(),
    })
}

/// What one open train is to this report.
#[derive(Debug, PartialEq)]
pub(crate) enum TrainVerdict<'a> {
    /// Complete this `converged` step: the train merged what rolled.
    Converge { step: &'a Value, merge_ref: &'a str },
    /// Not this report's train — a different merge, or not merged yet.
    NotThisMerge,
    /// This report's train, but not this handler's to complete; the
    /// reason is logged and reconcile judges it.
    LeftToReconcile(&'static str),
}

/// PURE: is `train` the one whose merge `sha` rolled, and may this
/// report complete its `converged`?
pub(crate) fn judge_train<'a>(train: &'a Value, sha: &str) -> TrainVerdict<'a> {
    let merge_ref = step_by_slug(train, MERGED_SLUG)
        .filter(|s| s.get("status").and_then(Value::as_str) == Some("completed"))
        .and_then(|s| s.pointer("/metadata/merge_ref"))
        .and_then(Value::as_str)
        .unwrap_or("");
    if !same_commit(merge_ref, sha) {
        return TrainVerdict::NotThisMerge;
    }
    if train
        .pointer("/metadata/delivery_plan/route")
        .and_then(Value::as_str)
        == Some("data")
    {
        return TrainVerdict::LeftToReconcile(
            "it froze a data delivery plan, whose channel obligations the conductor judges",
        );
    }
    let Some(step) = step_by_slug(train, CONVERGED_SLUG) else {
        return TrainVerdict::LeftToReconcile("it has no `converged` step");
    };
    if !is_open(step) {
        return TrainVerdict::LeftToReconcile("its `converged` step is not open");
    }
    TrainVerdict::Converge { step, merge_ref }
}

/// PURE: the fields `converged` is completed with — the conductor's
/// image-basis vocabulary, plus the runner's receipt.
pub(crate) fn converged_fields(
    rolled: &Rolled,
    merge_ref: &str,
    health: &Value,
    commit: &str,
    at: chrono::DateTime<chrono::Utc>,
    merged_at: Option<&str>,
) -> Map<String, Value> {
    let after = merged_at
        .and_then(|m| chrono::DateTime::parse_from_rfc3339(m).ok())
        .map(|m| {
            format!(
                "; verified {} min after merge",
                (at.fixed_offset() - m).num_minutes()
            )
        })
        .unwrap_or_default();
    let short = |s: &str| s.get(..12).unwrap_or(s).to_string();
    let mut fields = Map::new();
    fields.insert("cluster_commit".into(), json!(commit));
    fields.insert(
        "delivery_evidence".into(),
        json!({
            "schema": 1,
            "basis": "image",
            "arrived": true,
            "merge_ref": merge_ref,
            "health": health,
            "rolled": {
                "sha": rolled.sha,
                "at": rolled.at,
                "converge": rolled.converge,
                "step": rolled.step,
            },
        }),
    );
    fields.insert(
        "verified".into(),
        json!(format!(
            "the cluster-deploy-runner reported its rollout of {} complete at {} (maintenance \
             packet {}), and the running cluster jobs API self-reports build commit {} — \
             matches merge {merge_ref}{after}",
            short(&rolled.sha),
            rolled.at.as_deref().unwrap_or("an unrecorded time"),
            rolled.converge.get(..8).unwrap_or(&rolled.converge),
            short(commit),
        )),
    );
    fields.insert(
        "completed_at".into(),
        json!(at.to_rfc3339_opts(chrono::SecondsFormat::Secs, true)),
    );
    fields
}

#[async_trait]
impl Handler for JobsConvergeTrainsOnRolled {
    fn name(&self) -> &'static str {
        "jobs.converge_trains_on_rolled"
    }

    async fn invoke(
        &self,
        args: &[(String, boss_dispatcher::rules::expr::Value)],
        ctx: &InvocationContext,
    ) -> Result<(), HandlerError> {
        let train_kind = arg_string(args, "train_kind")?;
        let converge_kind = arg_string(args, "converge_kind")?;
        let rule = ctx.rule_name.as_str();
        let Some(job_id) = ctx.event_payload.get("job_id").and_then(Value::as_str) else {
            tracing::warn!(rule, topic = %ctx.triggering_topic,
                "the event names no packet (`job_id`) — completing nothing");
            return Ok(());
        };
        // The record as it stands, not as the event carried it: a later
        // write may have replaced it, and only the packet says whose it is.
        let converge = get_json(
            &self.client,
            &format!("{}/api/jobs/{job_id}", self.base()),
            rule,
        )
        .await?;
        let rolled = match rolled_receipt(&converge, converge_kind) {
            Ok(r) => r,
            Err(why) => {
                tracing::warn!(rule, packet = %job_id, "{why} — completing nothing");
                return Ok(());
            }
        };
        let health = get_json(
            &self.client,
            &format!("{}/api/jobs/health", self.base()),
            rule,
        )
        .await?;
        let commit = health
            .pointer("/capabilities/commit")
            .and_then(Value::as_str)
            .unwrap_or("");
        if !same_commit(commit, &rolled.sha) {
            tracing::info!(rule, rolled = %rolled.sha, cluster = %commit,
                "the runner reported a rollout the running jobs API does not self-report — \
                 leaving every train to reconcile");
            return Ok(());
        }
        let at = ctx.firing_instant()?;
        let trains = open_jobs_of_kind(&self.client, self.base(), train_kind, rule).await?;
        for train in &trains {
            let tid = train.get("id").and_then(Value::as_str).unwrap_or("");
            match judge_train(train, &rolled.sha) {
                TrainVerdict::NotThisMerge => {}
                TrainVerdict::LeftToReconcile(why) => {
                    tracing::info!(rule, train = %tid, "{why} — left to reconcile");
                }
                TrainVerdict::Converge { step, merge_ref } => {
                    let Some(sid) = step.get("id").and_then(Value::as_str) else {
                        continue;
                    };
                    let merged_at = step_by_slug(train, MERGED_SLUG)
                        .and_then(|s| s.pointer("/metadata/completed_at"))
                        .and_then(Value::as_str);
                    let fields =
                        converged_fields(&rolled, merge_ref, &health, commit, at, merged_at);
                    complete_step(&self.client, self.base(), tid, sid, fields, rule).await?;
                    tracing::info!(rule, train = %tid, rolled = %rolled.sha,
                        converge = %rolled.converge, "converged on the runner's report");
                }
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{Json, Router, extract::Path, extract::Query, routing::get};
    use std::collections::HashMap;
    use std::sync::Mutex;

    const SHA: &str = "9557a4d3fe62dfd3786b0d4f97cb3e3753f2bc0e";
    const OTHER: &str = "d9277c3edc463ffdc99dcd9dd40acdc04b9c624c";
    const CONVERGE: &str = "11111111-1111-1111-1111-111111111111";
    const RUN: &str = "11111111-0000-0000-0000-00000000000r";
    const TRAIN_A: &str = "22222222-2222-2222-2222-222222222222";
    const TRAIN_B: &str = "33333333-3333-3333-3333-333333333333";
    const AT: &str = "2026-10-19T02:24:40Z";

    fn args() -> Vec<(String, boss_dispatcher::rules::expr::Value)> {
        use boss_dispatcher::rules::expr::Value as V;
        vec![
            ("train_kind".into(), V::String("pr-train".into())),
            (
                "converge_kind".into(),
                V::String("maintenance-cluster-converge".into()),
            ),
        ]
    }

    /// The `jobs.step.updated` the runner's metadata-only write emits:
    /// the step row, `job_id` beside its own id.
    fn ctx(job_id: &str) -> InvocationContext {
        InvocationContext {
            rule_name: "complete-train-converged-on-runner-rolled".into(),
            triggering_event_id: "evt-rolled-1".into(),
            triggering_topic: "jobs.step.updated".into(),
            event_payload: json!({
                "id": RUN, "step_id": RUN, "job_id": job_id, "spec_slug": "run",
                "status": "ready", "metadata": { "rolled": SHA, "rolled_at": AT },
            }),
            event_timestamp: Some(
                chrono::DateTime::parse_from_rfc3339("2026-10-19T02:24:41Z")
                    .unwrap()
                    .with_timezone(&chrono::Utc),
            ),
        }
    }

    fn converge(kind: &str, rolled: &str) -> Value {
        json!({
            "id": CONVERGE, "kind": kind, "status": "open", "metadata": { "host": "forge" },
            "steps": [{ "id": RUN, "spec_slug": "run", "status": "ready",
                        "metadata": { "rolled": rolled, "rolled_at": AT } }],
        })
    }

    /// An open train: `merged` completed with a twelve-character
    /// `merge_ref` (as the conductor writes it), `converged` in
    /// `converged_status`, and the delivery plan route it froze.
    fn train(id: &str, merge_full: &str, converged_status: &str, route: &str) -> Value {
        json!({
            "id": id, "kind": "pr-train", "status": "open",
            "metadata": { "delivery_plan": { "route": route } },
            "steps": [
                { "id": format!("{id}-merged"), "spec_slug": "merged", "status": "completed",
                  "metadata": { "merge_ref": &merge_full[..12], "completed_at": "2026-10-19T02:21:47Z" } },
                { "id": format!("{id}-conv"), "spec_slug": "converged", "status": converged_status,
                  "metadata": { "audience": { "individual": "automation:train-conductor" } } },
            ],
        })
    }

    type Writes = Arc<Mutex<Vec<(String, String, Value)>>>;

    /// Stand-in for the jobs API: the open list, one packet by id, the
    /// health self-report, and every step write recorded and applied so a
    /// redelivery reads what the first delivery wrote.
    async fn mock(jobs: Vec<Value>, cluster_commit: &str) -> (String, Writes) {
        let writes: Writes = Arc::new(Mutex::new(Vec::new()));
        let by_id: Arc<Mutex<HashMap<String, Value>>> = Arc::new(Mutex::new(
            jobs.into_iter()
                .map(|j| (j["id"].as_str().unwrap().to_string(), j))
                .collect(),
        ));
        let health = json!({ "status": "ok", "capabilities": { "commit": cluster_commit } });
        let (list, one, put, patch) = (by_id.clone(), by_id.clone(), by_id.clone(), by_id);
        let (put_log, patch_log) = (writes.clone(), writes.clone());
        let app = Router::new()
            .route(
                "/api/jobs",
                get(move |Query(q): Query<HashMap<String, String>>| {
                    let list = list.clone();
                    async move {
                        let rows: Vec<Value> = list
                            .lock()
                            .unwrap()
                            .values()
                            .filter(|j| {
                                q.get("kind").is_none_or(|k| j["kind"] == json!(k))
                                    && q.get("status").is_none_or(|s| j["status"] == json!(s))
                            })
                            .cloned()
                            .collect();
                        Json(json!({ "data": rows, "total": rows.len() }))
                    }
                }),
            )
            .route(
                "/api/jobs/health",
                get(move || {
                    let health = health.clone();
                    async move { Json(health) }
                }),
            )
            .route(
                "/api/jobs/{id}",
                get(move |Path(id): Path<String>| {
                    let one = one.clone();
                    async move {
                        one.lock()
                            .unwrap()
                            .get(&id)
                            .cloned()
                            .map(Json)
                            .ok_or(axum::http::StatusCode::NOT_FOUND)
                    }
                }),
            )
            .route(
                "/api/jobs/{id}/steps/{sid}",
                axum::routing::put(
                    move |Path((id, sid)): Path<(String, String)>, Json(body): Json<Value>| {
                        let (put, log) = (put.clone(), put_log.clone());
                        async move {
                            log.lock()
                                .unwrap()
                                .push((id.clone(), sid.clone(), body.clone()));
                            apply(&put, &id, &sid, |s| s["status"] = body["status"].clone());
                            Json(json!({ "ok": true }))
                        }
                    },
                ),
            )
            .route(
                "/api/jobs/{id}/steps/{sid}/metadata",
                axum::routing::patch(
                    move |Path((id, sid)): Path<(String, String)>, Json(body): Json<Value>| {
                        let (patch, log) = (patch.clone(), patch_log.clone());
                        async move {
                            log.lock().unwrap().push((
                                id.clone(),
                                format!("{sid}/metadata"),
                                body.clone(),
                            ));
                            apply(&patch, &id, &sid, |s| {
                                for (k, v) in body.as_object().unwrap() {
                                    s["metadata"][k] = v.clone();
                                }
                            });
                            Json(json!({ "ok": true }))
                        }
                    },
                ),
            );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        (format!("http://{addr}"), writes)
    }

    fn apply(
        jobs: &Arc<Mutex<HashMap<String, Value>>>,
        id: &str,
        sid: &str,
        f: impl Fn(&mut Value),
    ) {
        if let Some(steps) = jobs
            .lock()
            .unwrap()
            .get_mut(id)
            .and_then(|j| j["steps"].as_array_mut())
        {
            steps
                .iter_mut()
                .filter(|s| s["id"] == json!(sid))
                .for_each(f);
        }
    }

    async fn deliver(base: &str, job_id: &str) {
        JobsConvergeTrainsOnRolled::with_client(api_client(), base)
            .invoke(&args(), &ctx(job_id))
            .await
            .expect("runs");
    }

    /// THE OBLIGATION: the runner's report completes `converged` on the
    /// ONE train whose merge it rolled — not on the train of another
    /// merge — carrying the receipt, both sources, and the conductor's
    /// own `completed_at` key stamped from the event, never a clock.
    #[tokio::test]
    async fn the_runners_report_converges_the_train_whose_merge_it_rolled() {
        let (base, writes) = mock(
            vec![
                converge("maintenance-cluster-converge", SHA),
                train(TRAIN_A, SHA, "ready", "software"),
                train(TRAIN_B, OTHER, "ready", "software"),
            ],
            SHA,
        )
        .await;
        deliver(&base, CONVERGE).await;
        let w = writes.lock().unwrap().clone();
        assert_eq!(w.len(), 2, "the merge, then the flip, on one train: {w:?}");
        let (job, step, md) = &w[0];
        assert_eq!(
            (job.as_str(), step.as_str()),
            (
                TRAIN_A,
                "22222222-2222-2222-2222-222222222222-conv/metadata"
            )
        );
        assert_eq!(
            md["cluster_commit"], SHA,
            "the commit the cluster self-reports"
        );
        let ev = &md["delivery_evidence"];
        assert_eq!(ev["basis"], "image");
        assert_eq!(ev["arrived"], true);
        assert_eq!(ev["merge_ref"], &SHA[..12]);
        assert_eq!(ev["health"]["capabilities"]["commit"], SHA);
        assert_eq!(
            ev["rolled"],
            json!({ "sha": SHA, "at": AT, "converge": CONVERGE, "step": RUN }),
            "the runner's receipt, copied — not a timestamp alone"
        );
        let verified = md["verified"].as_str().unwrap();
        assert!(
            verified.contains("cluster-deploy-runner reported its rollout of 9557a4d3fe62"),
            "{verified}"
        );
        assert!(
            verified.contains("self-reports build commit 9557a4d3fe62"),
            "{verified}"
        );
        assert!(
            verified.contains("verified 2 min after merge"),
            "{verified}"
        );
        assert_eq!(
            md["completed_at"], "2026-10-19T02:24:41Z",
            "the event's instant"
        );
        assert_eq!(w[1].2, json!({ "status": "completed" }), "the flip alone");
    }

    /// A redelivery — or the conductor having got there first — finds
    /// `converged` completed and writes nothing.
    #[tokio::test]
    async fn a_redelivery_writes_nothing_more() {
        let (base, writes) = mock(
            vec![
                converge("maintenance-cluster-converge", SHA),
                train(TRAIN_A, SHA, "ready", "software"),
            ],
            SHA,
        )
        .await;
        deliver(&base, CONVERGE).await;
        deliver(&base, CONVERGE).await;
        assert_eq!(writes.lock().unwrap().len(), 2, "one completion, ever");
    }

    /// The runner's word alone is not convergence: a cluster that does
    /// not self-report the rolled commit leaves every train to reconcile.
    #[tokio::test]
    async fn a_cluster_that_does_not_self_report_the_rolled_commit_converges_nothing() {
        let (base, writes) = mock(
            vec![
                converge("maintenance-cluster-converge", SHA),
                train(TRAIN_A, SHA, "ready", "software"),
            ],
            OTHER,
        )
        .await;
        deliver(&base, CONVERGE).await;
        assert!(writes.lock().unwrap().is_empty());
    }

    /// A `rolled` key on a packet that is not the runner's is no receipt.
    #[tokio::test]
    async fn a_rolled_key_on_another_kind_of_packet_converges_nothing() {
        let (base, writes) = mock(
            vec![
                converge("maintenance-backup", SHA),
                train(TRAIN_A, SHA, "ready", "software"),
            ],
            SHA,
        )
        .await;
        deliver(&base, CONVERGE).await;
        assert!(writes.lock().unwrap().is_empty());
    }

    /// A data-route train's obligations are the conductor's to observe;
    /// a train whose `converged` nothing has routed to is not this one's.
    #[tokio::test]
    async fn a_data_plan_or_an_unopened_step_is_left_to_reconcile() {
        let (base, writes) = mock(
            vec![
                converge("maintenance-cluster-converge", SHA),
                train(TRAIN_A, SHA, "ready", "data"),
                train(TRAIN_B, SHA, "pending", "software"),
            ],
            SHA,
        )
        .await;
        deliver(&base, CONVERGE).await;
        assert!(writes.lock().unwrap().is_empty());
    }

    #[test]
    fn only_a_full_commit_is_a_receipt() {
        for bad in [
            "",
            "9557a4d",
            &SHA[..12],
            "z557a4d3fe62dfd3786b0d4f97cb3e3753f2bc0e",
        ] {
            assert!(
                rolled_receipt(
                    &converge("maintenance-cluster-converge", bad),
                    "maintenance-cluster-converge"
                )
                .is_err(),
                "{bad:?}"
            );
        }
        let r = rolled_receipt(
            &converge("maintenance-cluster-converge", SHA),
            "maintenance-cluster-converge",
        )
        .unwrap();
        assert_eq!(
            (r.sha.as_str(), r.at.as_deref(), r.converge.as_str()),
            (SHA, Some(AT), CONVERGE)
        );
    }
}
