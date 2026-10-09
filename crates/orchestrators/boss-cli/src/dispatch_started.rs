//! An own-run receipt records that the worker read START, not physical
//! execution or successful delivery. The metadata merge's audit event
//! supplies server provenance; the receipt carries no client clock.

use anyhow::{Context, Result, bail};
use serde_json::{Value, json};

pub(crate) const EXPECTATION_KEY: &str = "worker_start_expectation";
pub(crate) const RECEIPT_KEY: &str = "worker_started";

pub(crate) fn expectation(job: &Value) -> Option<&Value> {
    crate::envelope::steps(job)
        .into_iter()
        .find(|s| s.get("spec_slug").and_then(Value::as_str) == Some("building"))
        .and_then(|s| s.get("metadata"))
        .and_then(|m| m.get(EXPECTATION_KEY))
}

fn receipt(job: &Value, run: &str, actor: &str, owned: Option<&str>) -> Result<Value> {
    let job = job.get("data").unwrap_or(job);
    if owned != Some(run) {
        bail!("--started requires BOSS_AGENT_RUN to name this exact own run");
    }
    if job.get("id").and_then(Value::as_str) != Some(run)
        || job.get("kind").and_then(Value::as_str) != Some("agent-run")
        || job.get("status").and_then(Value::as_str) != Some("open")
    {
        bail!("--started requires the exact open native agent-run");
    }
    let step = crate::envelope::steps(job)
        .into_iter()
        .find(|s| s.get("spec_slug").and_then(Value::as_str) == Some("building"))
        .context("--started found no building step")?;
    if step.get("status").and_then(Value::as_str) != Some("active")
        || step.get("assignee_id").and_then(Value::as_str) != Some(actor)
    {
        bail!("--started requires this actor's active assigned building step");
    }
    // metadata.agent is the dispatch login (which can be a registry alias),
    // not the actor assigned to this step. The signed caller must own the step.
    let expected =
        expectation(job).context("this legacy or unmarked run declares no worker receipt")?;
    if expected.get("schema").and_then(Value::as_u64) != Some(1)
        || expected
            .get("minutes")
            .and_then(Value::as_i64)
            .is_none_or(|n| n <= 0)
    {
        bail!("worker receipt expectation is unsupported");
    }
    Ok(json!({"schema":1,"run":run,"actor":actor}))
}

/// The id of the open run's `building` step when it is READY and
/// nominated to `actor` — the one state [`started_at`] claims from.
fn own_ready_nomination(job: &Value, actor: &str) -> Option<String> {
    let job = job.get("data").unwrap_or(job);
    if job.get("kind").and_then(Value::as_str) != Some("agent-run")
        || job.get("status").and_then(Value::as_str) != Some("open")
    {
        return None;
    }
    crate::envelope::steps(job)
        .into_iter()
        .find(|s| s.get("spec_slug").and_then(Value::as_str) == Some("building"))
        .filter(|s| {
            s.get("status").and_then(Value::as_str) == Some("ready")
                && s.get("assignee_id").and_then(Value::as_str) == Some(actor)
        })
        .and_then(|s| s.get("id").and_then(Value::as_str))
        .map(str::to_string)
}

pub(crate) async fn started_at(
    http: &boss_core::machine_token::Client,
    base: &str,
    run: &str,
    actor: &str,
    owned: Option<&str>,
) -> Result<()> {
    let registry = crate::gate::api_at_signed(
        http,
        base,
        reqwest::Method::GET,
        "/api/agents",
        None,
        crate::identity::Signature::As(actor.to_string()),
    )
    .await?
    .context("agent registry returned no body")?;
    let total = registry
        .get("total")
        .and_then(Value::as_u64)
        .context("agent registry returned no complete count")?;
    let agents = crate::train::rows(Some(registry))?;
    if total != agents.len() as u64 {
        bail!("agent registry is incomplete; no worker receipt written");
    }
    let matches: Vec<_> = agents
        .iter()
        .filter_map(|row| crate::dispatch::resolve_agent(std::slice::from_ref(row), actor))
        .collect();
    if matches.len() != 1 {
        bail!("running principal has no unique registered actor; no worker receipt written");
    }
    let registered = &matches[0];
    if registered.trim().is_empty() {
        bail!("registered actor is blank; no worker receipt written");
    }
    let api = |method: reqwest::Method, body: Option<Value>| async move {
        let path = if method == reqwest::Method::PATCH {
            format!("/api/jobs/{run}/metadata")
        } else {
            format!("/api/jobs/{run}")
        };
        crate::gate::api_at_signed(
            http,
            base,
            method,
            &path,
            body,
            crate::identity::Signature::As(actor.to_string()),
        )
        .await
    };
    let mut before = api(reqwest::Method::GET, None)
        .await?
        .context("own run read returned no body")?;
    // THE WORKER TAKES ITS OWN NOMINATION (backlog 2f7b8c00, 2026-10-07).
    // Nothing in the protocol claimed a run's Building: dispatch claims
    // the SOURCE step, the dispatcher service nominates Building, and the
    // step then sat READY — so this verb refused every worker that had
    // not claimed by hand (every Claude subagent that day; Codex sessions
    // claimed by hand first). The claim door is the one act that makes a
    // nomination a holding, it compares the signer against the nominee,
    // and it is asked only for a step already nominated to THIS actor: an
    // unassigned Building is one the door would grant to anyone, so it is
    // left for the refusal below.
    if owned == Some(run)
        && let Some(sid) = own_ready_nomination(&before, registered)
    {
        // NO WRITE BEFORE A REFUSAL (review f7f0b689, N5): a run that
        // declares no worker receipt is refused below whatever its step
        // is, so it is refused HERE, before the claim would have left
        // the step ACTIVE behind that refusal.
        if expectation(before.get("data").unwrap_or(&before)).is_none() {
            bail!("this legacy or unmarked run declares no worker receipt");
        }
        crate::gate::api_at_signed(
            http,
            base,
            reqwest::Method::POST,
            &format!("/api/jobs/{run}/steps/{sid}/claim"),
            None,
            crate::identity::Signature::As(actor.to_string()),
        )
        .await
        .context("claiming this run's own building step")?;
        before = api(reqwest::Method::GET, None)
            .await?
            .context("own run read returned no body")?;
    }
    let expected = receipt(&before, run, registered, owned)?;
    let before = before.get("data").unwrap_or(&before);
    match before.get("metadata").and_then(|m| m.get(RECEIPT_KEY)) {
        Some(existing) if existing != &expected => {
            bail!("own run already carries a different or unsupported worker receipt")
        }
        Some(_) => {}
        None => {
            api(reqwest::Method::PATCH, Some(json!({RECEIPT_KEY:expected}))).await?;
        }
    }
    let after = api(reqwest::Method::GET, None)
        .await?
        .context("receipt readback returned no body")?;
    // A concurrent close or reassignment makes this a failed acknowledgment;
    // the observer validates current identity again, never inferring death.
    receipt(&after, run, registered, owned)?;
    let after = after.get("data").unwrap_or(&after);
    if after.get("metadata").and_then(|m| m.get(RECEIPT_KEY)) != Some(&expected) {
        bail!("worker receipt did not read back; no acknowledgment is claimed");
    }
    println!("worker receipt recorded for {run} as {registered}, signed by {actor}");
    Ok(())
}

pub(crate) async fn started(run: String) -> Result<()> {
    // This is deliberately not a prefix resolver: own-run identity is exact.
    let id = uuid::Uuid::parse_str(&run).context("--started requires the full native run UUID")?;
    if id.to_string() != run {
        bail!("--started requires the canonical native run UUID");
    }
    let actor = crate::identity::sign(&reqwest::Method::PATCH, "/api/jobs")?;
    let owned = std::env::var("BOSS_AGENT_RUN").ok();
    started_at(
        &crate::gate::machine_client()?,
        &crate::gate::resolve_jobs_base(None)?,
        &run,
        &actor,
        owned.as_deref(),
    )
    .await
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn active() -> serde_json::Value {
        json!({"id":"own-run","kind":"agent-run","status":"open",
            "metadata":{"agent":"codex@algedonic.dev","unrelated":"kept"},
            "steps":[{"spec_slug":"building","status":"active","assignee_id":"agent-codex",
                "metadata":{"worker_start_expectation":{"schema":1,"minutes":15}}}]})
    }

    #[test]
    fn a_worker_receipt_names_its_own_run_actor_and_schema_without_a_clock() {
        assert_eq!(
            receipt(&active(), "own-run", "agent-codex", Some("own-run")).unwrap(),
            json!({"schema":1,"run":"own-run","actor":"agent-codex"})
        );
    }

    #[test]
    fn coordinator_metadata_and_another_run_or_actor_cannot_record_worker_receipt() {
        assert!(receipt(&active(), "own-run", "agent-codex", None).is_err());
        assert!(receipt(&active(), "own-run", "agent-codex", Some("other-run")).is_err());
        assert!(receipt(&active(), "own-run", "emp-david", Some("own-run")).is_err());
        let mut job = active();
        job["steps"][0]["assignee_id"] = json!("other-actor");
        assert!(receipt(&job, "own-run", "agent-codex", Some("own-run")).is_err());
        job = active();
        job["steps"][0]["status"] = json!("ready");
        assert!(receipt(&job, "own-run", "agent-codex", Some("own-run")).is_err());
    }

    #[test]
    fn legacy_and_terminal_runs_refuse_receipt_without_a_repin_or_resurrection() {
        let mut job = active();
        job["status"] = json!("closed");
        assert!(receipt(&job, "own-run", "agent-codex", Some("own-run")).is_err());
        job = active();
        job["steps"][0]["metadata"] = json!({});
        assert!(receipt(&job, "own-run", "agent-codex", Some("own-run")).is_err());
    }

    async fn stub(
        drop_receipt: bool,
        close_after_write: bool,
    ) -> (
        String,
        std::sync::Arc<std::sync::Mutex<(Value, Vec<Value>)>>,
    ) {
        stub_registry(
            drop_receipt,
            close_after_write,
            json!({"data":[
            {"id":"agent-codex","aliases":["codex@algedonic.dev"]}
        ],"total":1}),
        )
        .await
    }

    async fn stub_registry(
        drop_receipt: bool,
        close_after_write: bool,
        registry: Value,
    ) -> (
        String,
        std::sync::Arc<std::sync::Mutex<(Value, Vec<Value>)>>,
    ) {
        use axum::{
            Json, Router,
            routing::{get, patch},
        };
        let state = std::sync::Arc::new(std::sync::Mutex::new((active(), Vec::new())));
        let read = state.clone();
        let write = state.clone();
        let app = Router::new()
            .route(
                "/api/agents",
                get(move || {
                    let registry = registry.clone();
                    async move { Json(registry) }
                }),
            )
            .route(
                "/api/jobs/own-run",
                get(move || {
                    let read = read.clone();
                    async move { Json(read.lock().unwrap().0.clone()) }
                }),
            )
            .route(
                "/api/jobs/own-run/metadata",
                patch(move |Json(body): Json<Value>| {
                    let write = write.clone();
                    async move {
                        let mut s = write.lock().unwrap();
                        s.1.push(body.clone());
                        if !drop_receipt {
                            for (key, value) in body.as_object().unwrap() {
                                s.0["metadata"][key] = value.clone();
                            }
                        }
                        if close_after_write {
                            s.0["status"] = json!("closed");
                        }
                        axum::http::StatusCode::NO_CONTENT
                    }
                }),
            );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        (format!("http://{address}"), state)
    }

    #[tokio::test]
    async fn the_supported_door_merges_only_its_receipt_and_retries_without_a_second_write() {
        let (base, state) = stub(false, false).await;
        let client = crate::gate::machine_client().unwrap();
        for _ in 0..2 {
            started_at(
                &client,
                &base,
                "own-run",
                "codex@algedonic.dev",
                Some("own-run"),
            )
            .await
            .unwrap();
        }
        let s = state.lock().unwrap();
        assert_eq!(s.1.len(), 1);
        assert_eq!(
            s.1[0],
            json!({"worker_started":{"schema":1,"run":"own-run","actor":"agent-codex"}})
        );
        assert_eq!(s.0["metadata"]["unrelated"], "kept");
        assert_eq!(s.0["steps"][0]["status"], "active");
    }

    #[tokio::test]
    async fn a_lost_receipt_or_concurrent_close_is_never_reported_as_acknowledged() {
        let client = crate::gate::machine_client().unwrap();
        for (drop, close) in [(true, false), (false, true)] {
            let (base, state) = stub(drop, close).await;
            assert!(
                started_at(&client, &base, "own-run", "agent-codex", Some("own-run"))
                    .await
                    .is_err()
            );
            assert_eq!(state.lock().unwrap().1.len(), 1);
        }
        let (base, state) = stub(false, false).await;
        assert!(
            started_at(&client, &base, "own-run", "agent-codex", Some("other-run"))
                .await
                .is_err()
        );
        assert!(state.lock().unwrap().1.is_empty());
    }

    #[test]
    fn started_is_a_separate_clap_mode_and_cannot_override_dispatch_settings() {
        use clap::Parser;
        assert!(crate::Cli::try_parse_from(["boss", "dispatch", "own-run", "--started"]).is_ok());
        for flag in ["--from-hook", "--next", "--force"] {
            assert!(
                crate::Cli::try_parse_from(["boss", "dispatch", "own-run", "--started", flag])
                    .is_err()
            );
        }
        assert!(
            crate::Cli::try_parse_from([
                "boss",
                "dispatch",
                "own-run",
                "--started",
                "--model",
                "invented"
            ])
            .is_err()
        );
    }

    #[tokio::test]
    async fn an_unknown_ambiguous_or_unavailable_principal_never_writes_a_receipt() {
        let client = crate::gate::machine_client().unwrap();
        for registry in [
            json!({"data":[],"total":0}),
            json!({"data":[{"aliases":["codex@algedonic.dev"]}],"total":1}),
            json!({"data":[{"id":"agent-codex","aliases":["codex@algedonic.dev"]},
                {"id":"other","aliases":["codex@algedonic.dev"]}],"total":2}),
            json!({"data":[{"id":"agent-codex","aliases":["codex@algedonic.dev"]}],"total":2}),
            json!({"error":"unavailable"}),
        ] {
            let (base, state) = stub_registry(false, false, registry).await;
            assert!(
                started_at(
                    &client,
                    &base,
                    "own-run",
                    "codex@algedonic.dev",
                    Some("own-run")
                )
                .await
                .is_err()
            );
            assert!(state.lock().unwrap().1.is_empty());
        }
        let (base, state) = stub(false, false).await;
        assert!(
            started_at(
                &client,
                &base,
                "own-run",
                "stranger@example.test",
                Some("own-run")
            )
            .await
            .is_err()
        );
        assert!(state.lock().unwrap().1.is_empty());
    }

    #[tokio::test]
    async fn a_malformed_registry_id_cannot_become_a_recorded_worker_actor() {
        let client = crate::gate::machine_client().unwrap();
        for id in [json!(""), json!("   "), json!(null), json!(17)] {
            let registry = json!({"data":[{"id":id,"aliases":["codex@algedonic.dev"]}],"total":1});
            let (base, state) = stub_registry(false, false, registry).await;
            // A malformed native assignment cannot make a malformed row an actor.
            state.lock().unwrap().0["steps"][0]["assignee_id"] = id;
            assert!(
                started_at(
                    &client,
                    &base,
                    "own-run",
                    "codex@algedonic.dev",
                    Some("own-run")
                )
                .await
                .is_err()
            );
            assert!(state.lock().unwrap().1.is_empty());
        }
    }

    /// The claim door as the jobs API answers it for a READY step: the
    /// caller's own nomination becomes ACTIVE, anyone else's is a 409
    /// naming the holder. The stub records who asked.
    async fn stub_with_claim(
        holder: &str,
    ) -> (
        String,
        std::sync::Arc<std::sync::Mutex<(Value, Vec<Value>)>>,
    ) {
        use axum::{Json, routing::post};
        let (base, state) = stub(false, false).await;
        {
            let mut s = state.lock().unwrap();
            s.0["steps"][0]["id"] = json!("s-building");
            s.0["steps"][0]["status"] = json!("ready");
            s.0["steps"][0]["assignee_id"] = json!(holder);
        }
        let claims = state.clone();
        let app = axum::Router::new()
            .route(
                "/api/jobs/own-run/steps/s-building/claim",
                post(move |headers: axum::http::HeaderMap| {
                    let claims = claims.clone();
                    async move {
                        let signer = headers
                            .get("x-boss-user")
                            .and_then(|v| v.to_str().ok())
                            .and_then(|v| serde_json::from_str::<Value>(v).ok())
                            .map(|v| v["id"].clone())
                            .unwrap_or(Value::Null);
                        let mut s = claims.lock().unwrap();
                        s.1.push(json!({"claim_by": signer}));
                        // The login door: an alias signs as its registered id.
                        if s.0["steps"][0]["assignee_id"] == "agent-codex"
                            && signer == "codex@algedonic.dev"
                        {
                            s.0["steps"][0]["status"] = json!("active");
                            (axum::http::StatusCode::OK, Json(json!({"status":"active"})))
                        } else {
                            (
                                axum::http::StatusCode::CONFLICT,
                                Json(
                                    json!({"error":"held","holder":s.0["steps"][0]["assignee_id"]}),
                                ),
                            )
                        }
                    }
                }),
            )
            .fallback(move |req: axum::extract::Request| {
                let base = base.clone();
                async move {
                    let client = reqwest::Client::new();
                    let url = format!("{base}{}", req.uri().path());
                    let method = req.method().clone();
                    let body = axum::body::to_bytes(req.into_body(), 1 << 20)
                        .await
                        .unwrap();
                    let answer = client
                        .request(method, url)
                        .header("content-type", "application/json")
                        .body(body)
                        .send()
                        .await
                        .unwrap();
                    let status =
                        axum::http::StatusCode::from_u16(answer.status().as_u16()).unwrap();
                    (status, answer.bytes().await.unwrap())
                }
            });
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        (format!("http://{address}"), state)
    }

    /// THE SHARED CAUSE (backlog 2f7b8c00, run f7c7891b, 2026-10-07).
    /// Every Claude subagent that day had `--started` refused "requires
    /// this actor's active assigned building step": its Building was
    /// READY and nominated to it, and nothing in the protocol claims a
    /// run's Building — Codex sessions did it by hand. The verb now takes
    /// its own nomination through the claim door first.
    #[tokio::test]
    async fn started_claims_its_own_ready_nomination_before_the_receipt() {
        let (base, state) = stub_with_claim("agent-codex").await;
        let client = crate::gate::machine_client().unwrap();
        started_at(
            &client,
            &base,
            "own-run",
            "codex@algedonic.dev",
            Some("own-run"),
        )
        .await
        .expect("a ready step nominated to the caller is claimed, then acknowledged");
        let s = state.lock().unwrap();
        assert_eq!(s.0["steps"][0]["status"], "active");
        assert_eq!(
            s.1,
            vec![
                json!({"claim_by":"codex@algedonic.dev"}),
                json!({"worker_started":{"schema":1,"run":"own-run","actor":"agent-codex"}}),
            ],
            "one claim signed by the caller, then the one receipt"
        );
    }

    /// NO WRITE BEFORE A REFUSAL (review f7f0b689, N5). A legacy run
    /// declares no worker receipt, so `--started` refuses it — and it
    /// used to claim the step first, leaving it ACTIVE behind a refusal.
    #[tokio::test]
    async fn a_run_that_declares_no_worker_receipt_is_refused_before_any_claim() {
        let (base, state) = stub_with_claim("agent-codex").await;
        state.lock().unwrap().0["steps"][0]["metadata"] = json!({});
        let client = crate::gate::machine_client().unwrap();
        let refused = started_at(
            &client,
            &base,
            "own-run",
            "codex@algedonic.dev",
            Some("own-run"),
        )
        .await
        .unwrap_err()
        .to_string();
        assert!(refused.contains("declares no worker receipt"), "{refused}");
        let s = state.lock().unwrap();
        assert!(s.1.is_empty(), "no claim and no receipt: {:?}", s.1);
        assert_eq!(s.0["steps"][0]["status"], "ready");
    }

    /// The claim is the caller's own or it is not made: a Building
    /// nominated to another actor, or to nobody, is reported and no claim
    /// and no receipt is sent. The door would refuse the first anyway;
    /// the second it would GRANT, and a worker that takes an unassigned
    /// run is the identity slip this verb exists to refuse.
    #[tokio::test]
    async fn started_never_claims_a_building_step_nominated_to_someone_else_or_to_nobody() {
        let client = crate::gate::machine_client().unwrap();
        for holder in [json!("agent-claude"), Value::Null] {
            let (base, state) = stub_with_claim("agent-claude").await;
            state.lock().unwrap().0["steps"][0]["assignee_id"] = holder;
            let refused = started_at(
                &client,
                &base,
                "own-run",
                "codex@algedonic.dev",
                Some("own-run"),
            )
            .await
            .unwrap_err()
            .to_string();
            assert!(
                refused.contains("active assigned building step"),
                "{refused}"
            );
            assert!(state.lock().unwrap().1.is_empty(), "no claim, no receipt");
        }
    }
}
