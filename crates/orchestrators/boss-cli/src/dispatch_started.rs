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
    let before = api(reqwest::Method::GET, None)
        .await?
        .context("own run read returned no body")?;
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
}
