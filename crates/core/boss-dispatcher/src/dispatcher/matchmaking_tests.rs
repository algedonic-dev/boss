//! Whole-event controls for d3cfa247; child processes own their env.
use super::*;

#[test]
fn resolved_contract_and_environment_matchmaking() {
    const JOB: &str = "059a57cf-30d1-4d8d-a3e0-f065f364a066";
    const STEP: &str = "a991b884-8f92-4a96-b60e-06fa9f9d0421";
    const CASE: &str = "BOSS_MATCHMAKING_FIXTURE_CASE";
    let Ok(case) = std::env::var(CASE) else {
        let mut failures = Vec::new();
        for case in [
            "workflow",
            "workflow-available",
            "workflow-unread",
            "workflow-retired",
            "workflow-transport",
            "workflow-forbidden",
            "skew",
            "human",
            "model",
            "alias",
            "alias-unread",
            "legacy-alias",
            "released",
            "steps-unread",
            "steps-missing",
            "steps-duplicate",
            "steps-wrong-job",
            "steps-wrong-slug",
            "steps-fields-skew",
            "workflow-no-event-fields",
            "event-metadata-authority",
            "stored-no-reads",
        ] {
            let out = std::process::Command::new(std::env::current_exe().unwrap())
                .args([
                    "--exact",
                    "dispatcher::matchmaking_tests::resolved_contract_and_environment_matchmaking",
                    "--nocapture",
                ])
                .env(CASE, case)
                .env(
                    "BOSS_DISPATCH_EXECUTOR_ID",
                    if case.starts_with("alias") || case == "legacy-alias" {
                        "builder@example.invalid"
                    } else if case.starts_with("workflow") || case == "skew" {
                        "emp-human"
                    } else {
                        "agent-builder"
                    },
                )
                .env(
                    "BOSS_DISPATCH_EXECUTOR_ROLES",
                    if case == "human" {
                        ""
                    } else {
                        "platform-admin"
                    },
                )
                .output()
                .unwrap();
            if !out.status.success() {
                failures.push(format!(
                    "{case}: {}\n{}",
                    String::from_utf8_lossy(&out.stdout),
                    String::from_utf8_lossy(&out.stderr)
                ));
            }
        }
        assert!(failures.is_empty(), "{}", failures.join("\n"));
        return;
    };
    tokio::runtime::Runtime::new().unwrap().block_on(async move {
        use axum::{Json, Router, response::IntoResponse, routing::{get, put}};
        use std::sync::atomic::{AtomicUsize, Ordering};
        let assigned = Arc::new(std::sync::Mutex::new(Vec::<String>::new()));
        let put_assigned = assigned.clone();
        let reads = Arc::new(AtomicUsize::new(0));
        let roster_reads = reads.clone();
        let unread_alias = case == "alias-unread";
        let released = case == "released";
        let available = case == "workflow-available";
        let fallback = case.starts_with("workflow") || case.starts_with("steps-") || case == "skew" || case == "event-metadata-authority";
        let model = if fallback || case == "model" || case.starts_with("alias") { "review-model" } else { "builder-model" };
        let spec = boss_jobs::agent_spec::AgentSpec { profile:"reviewer".into(), model:model.into(), budget_usd:3.0,
            effort:boss_jobs::agent_spec::Effort::High, executor_provenance:Default::default() };
        let mut row = boss_jobs::registry::WorkflowSpec::platform_seed("fixture", "Fixture", "engineering", vec![],
            vec![boss_jobs::registry::StepSpec { title:"review".into(),kind:"task".into(),ready_when:"true".into(),agent:Some(spec.clone()),..Default::default() }]);
        if case == "skew" { row.steps[0].fields.push(serde_json::from_value(serde_json::json!({"name":"evidence","field_type":"text","required":true,"filled_by":"executor"})).unwrap()); }
        let unread_workflow = case == "workflow-unread";
        let retired_workflow = case == "workflow-retired";
        let broken_workflow_transport = case == "workflow-transport";
        let forbidden_workflow = case == "workflow-forbidden";
        let step_reads = Arc::new(AtomicUsize::new(0));
        let observed_step_reads = step_reads.clone();
        let mut pinned = boss_core::job::Step::new(serde_json::from_value(serde_json::json!(JOB)).unwrap(), "task", "Review", 0);
        pinned.id = serde_json::from_value(serde_json::json!(STEP)).unwrap();
        pinned.spec_slug = Some("review".into());
        pinned.metadata = serde_json::json!({"authority_role":"platform-admin"});
        if case == "steps-wrong-job" { pinned.job_id = serde_json::from_value(serde_json::json!("16e75cda-2be0-4f18-a8e8-a897086756ab")).unwrap(); }
        if case == "steps-wrong-slug" { pinned.spec_slug = Some("different".into()); }
        if case == "steps-fields-skew" { pinned.fields.push(serde_json::from_value(serde_json::json!({"name":"later","field_type":"text","required":true,"filled_by":"executor"})).unwrap()); }
        if case == "event-metadata-authority" {
            let mut later = spec.clone(); later.model = "builder-model".into();
            for (key,value) in boss_jobs::agent_spec::projection(&later) { pinned.metadata[key] = value; }
        }
        let step_case = case.clone();
        let app = Router::new()
            .route("/api/people", get(|| async { Json(serde_json::json!([{"id":"emp-human","role":"platform-admin","status":"active"}])) }))
            .route("/api/agents", get(move || { let reads=roster_reads.clone(); async move {
                if unread_alias && reads.fetch_add(1, Ordering::SeqCst) < 2 {
                    return axum::http::StatusCode::SERVICE_UNAVAILABLE.into_response();
                }
                let mut agents=vec![serde_json::json!({"id":"agent-builder","display_name":"Builder","default_model":"builder-model","role":"platform-admin","department":null,"hourly_budget_usd_micros":null,"max_concurrent_runs":null,"aliases":["builder@example.invalid"]})];
                if available { agents[0]["default_model"] = serde_json::json!("review-model"); }
                if released { agents.push(serde_json::json!({"id":"agent-other","display_name":"Other","default_model":"builder-model","role":"platform-admin","department":null,"hourly_budget_usd_micros":null,"max_concurrent_runs":null,"aliases":[]})); }
                Json(serde_json::json!({"total":agents.len(),"data":agents})).into_response()
            }}))
            .route("/api/jobs", get(|| async { Json(serde_json::json!({"total":2,"data":[{"kind":"work-session","status":"open","metadata":{"actor":"agent-builder"}},{"kind":"work-session","status":"open","metadata":{"actor":"agent-other"}}]})) }))
            .route(&format!("/api/jobs/{JOB}"), get(|| async { Json(serde_json::json!({"kind":"fixture"})) }))
            .route(&format!("/api/jobs/{JOB}/steps"), get(move || { let pinned=pinned.clone();let case=step_case.clone();let reads=step_reads.clone();async move {
                reads.fetch_add(1,Ordering::SeqCst);
                match case.as_str() {
                    "steps-unread"|"stored-no-reads" => axum::http::StatusCode::SERVICE_UNAVAILABLE.into_response(),
                    "steps-missing" => Json(Vec::<boss_core::job::Step>::new()).into_response(),
                    "steps-duplicate" => Json(vec![pinned.clone(),pinned]).into_response(),
                    _ => Json(vec![pinned]).into_response(),
                }
            }}))
            .route("/api/workflows/fixture", get(move || {let row=row.clone();async move {
                if retired_workflow { axum::http::StatusCode::NOT_FOUND.into_response() }
                else if forbidden_workflow { axum::http::StatusCode::FORBIDDEN.into_response() }
                else if broken_workflow_transport {
                    axum::response::Response::new(axum::body::Body::from_stream(futures::stream::once(async {
                        Err::<axum::body::Bytes, _>(std::io::Error::new(std::io::ErrorKind::ConnectionReset, "workflow response transport reset"))
                    })))
                }
                else if unread_workflow {axum::http::StatusCode::SERVICE_UNAVAILABLE.into_response()} else {Json(row).into_response()}
            }}))
            .route(&format!("/api/jobs/{JOB}/steps/{STEP}"), put(move |Json(body):Json<Value>| {let assigned=put_assigned.clone();async move {
                assigned.lock().unwrap().push(body["assignee_id"].as_str().unwrap().into()); axum::http::StatusCode::NO_CONTENT
            }}));
        let listener=tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base=format!("http://{}",listener.local_addr().unwrap());
        let server=tokio::spawn(async move {axum::serve(listener,app).await.unwrap()});
        let ctx=DispatcherCtx::new(base.clone(),base,AssignmentStrategy::LowestId);
        let mut event=serde_json::json!({"id":STEP,"job_id":JOB,"status":"ready","kind":"task","metadata":{"authority_role":"platform-admin"}});
        if fallback {event["spec_slug"]=serde_json::json!("review");if case!="workflow-no-event-fields" {event["fields"]=serde_json::json!([]);}}
        else if case!="human" && case!="legacy-alias" {
            for (key,value) in boss_jobs::agent_spec::projection(&spec) {event["metadata"][key]=value;}
        }
        if case=="human" {event["kind"]=serde_json::json!("sign-off");}
        if released {event["metadata"]["released"]=serde_json::json!({"from":"agent-builder"});}
        if case=="stored-no-reads" {event["spec_slug"]=serde_json::json!("review");}
        let result=handle_event(&ctx,"jobs.step.updated",&event,Partition::Real).await;
        server.abort();
        if unread_alias || unread_workflow || broken_workflow_transport || forbidden_workflow || case.starts_with("steps-") {assert!(result.is_err(), "unread or mismatched pinned eligibility must NAK, not ACK silence: {case}");}
        else {result.unwrap();}
        let expected=match case.as_str() {"human"|"skew"|"workflow-retired"=>vec!["emp-human"],"legacy-alias"|"workflow-available"|"stored-no-reads"=>vec!["agent-builder"],"released"=>vec!["agent-other"],_=>vec![]};
        assert_eq!(*assigned.lock().unwrap(),expected,"whole-event {case}");
        if case=="stored-no-reads" { assert_eq!(observed_step_reads.load(Ordering::SeqCst),0,"stored projection must not fetch the live pinned step"); }
    });
}
