use base64::{Engine, engine::general_purpose::STANDARD};
use boss_core::{
    actor::ActorId,
    job::{Job, Priority, Step, StepStatus, Subject},
};
use boss_jobs::{pools::PoolSpec, registry::WorkflowSpec, resource_leases::StockObligation};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

fn stream(raw: &[u8]) -> Value {
    json!({"base64":STANDARD.encode(raw),"bytes":raw.len(),"retained_bytes":raw.len(),
           "sha256":hex::encode(Sha256::digest(raw)),"complete":true})
}
fn fixture() -> (StockObligation, PoolSpec, Job, Step, WorkflowSpec) {
    let id = uuid::Uuid::new_v4();
    let obligation: StockObligation = serde_json::from_value(json!({
        "claim":{"version":1,"id":id,"step_id":id,"job_id":id,"generation":1,
          "holder":"emp-fixture","workflow_kind":"test","workflow_version":1,
          "bill":{"disk_gb":"1"},"allocations":[{"pool":"disk:ci-workspaces","version":2,"amount":"1"}],
          "observed":[]},
        "allocation":{"pool":"disk:ci-workspaces","version":2,"amount":"1"},
        "reclaimer":"automation:reap-dead-ci-jobs",
        "identity":{"namespace":"boss-dev","job_uid":uuid::Uuid::new_v4(),"pvc_uid":uuid::Uuid::new_v4(),"run_id":uuid::Uuid::new_v4()},
        "returned_by":null
    })).unwrap();
    let pool: PoolSpec = serde_json::from_value(
        json!({"name":"disk:ci-workspaces","version":2,"status":"active",
        "unit":"GiB","kind":"stock","capacity":{"state":"unknown"},"reclaimer":obligation.reclaimer,
        "program":"infra/forge/reap-dead-ci-jobs.sh","return_verifier":"worker-teardown"}),
    )
    .unwrap();
    let mut job = Job::new(
        "ops-request",
        Subject::new("custom", "forge"),
        "fixture",
        "emp-owner",
        Priority::Standard,
        chrono::Utc::now().date_naive(),
    );
    job.workflow_version = 2;
    let mut step = Step::new(job.id, "task", "execute", 0);
    step.spec_slug = Some("execute".into());
    step.status = StepStatus::Completed;
    step.completed_by = Some(ActorId::automation("ops-runner"));
    let protocol: WorkflowSpec = serde_json::from_value(json!({"kind":"ops-request","label":"Ops","version":2,"status":"active","created_at":chrono::Utc::now(),
        "category":"test","subject_kinds":["custom"],"owning_team":"platform",
        "steps":[{"title":"execute","kind":"task","executor":"runner:ops","ready_when":"true","fields":[
            {"name":"disposition","field_type":"string","writer":"runner:ops"},
            {"name":"output","field_type":"string","writer":"runner:ops"},
            {"name":"exit_code","field_type":"string","writer":"runner:ops"},
            {"name":"executed","field_type":"object","writer":"runner:ops"}]}]})).unwrap();
    let target = boss_jobs::worker_teardown::target(&obligation).unwrap();
    let output = json!({"schema":1,"claim":"worker-teardown-only","state":"absent","target":target,
        "request":job.id,"observed_at":chrono::Utc::now().to_rfc3339(),"errors":[],
        "controls":{"namespace":uuid::Uuid::new_v4(),"service_account":uuid::Uuid::new_v4()},
        "inventories":{"jobs":{"complete":true,"count":0,"pages":1,"resource_version":"7"},
            "pvcs":{"complete":true,"count":0,"pages":1,"resource_version":"8"}},
        "source":boss_jobs::worker_teardown::source_hashes(),
        "manifest_attachment_receipt":"fixture durable native receipt", "retained_original":"fixture-only"});
    let raw = output.to_string();
    step.metadata = json!({"disposition":"answered","exit_code":"0","runner_host":"forge",
        "executed":{"verb":"worker-teardown-witness","host":"forge",
            "verb_file_sha256":hex::encode(Sha256::digest(include_bytes!("../../../../infra/ops/verbs/worker-teardown-witness.json"))),
            "argv":boss_jobs::worker_teardown::argv(&obligation).unwrap()},
        "output":raw,"streams":{"stdout":stream(raw.as_bytes()),"stderr":stream(b"")}});
    (obligation, pool, job, step, protocol)
}

#[test]
fn old_pool_bytes_and_authority_are_not_upgraded_by_a_new_adapter() {
    let (mut obligation, _, job, step, protocol) = fixture();
    let old_wire = json!({"name":"disk:ci-workspaces","version":1,"status":"retired",
        "unit":"GiB","kind":"stock","capacity":{"state":"unknown"},
        "reclaimer":"infra/forge/reap-dead-ci-jobs.sh"});
    let old: PoolSpec = serde_json::from_value(old_wire.clone()).unwrap();
    let encoded = serde_json::to_value(&old).unwrap();
    for key in ["program", "return_verifier", "identity_pin_unenforced"] {
        assert!(encoded.get(key).is_none(), "old declaration acquired {key}");
    }
    assert_eq!(encoded["reclaimer"], old_wire["reclaimer"]);
    obligation.allocation.version = 1;
    obligation.claim.allocations[0].version = 1;
    obligation.reclaimer = old.reclaimer.clone().unwrap();
    let before = serde_json::to_value(&obligation).unwrap();
    assert!(
        boss_jobs::worker_teardown::qualify_operation(&obligation, &old, &job, &step, &protocol)
            .is_err()
    );
    assert_eq!(serde_json::to_value(&obligation).unwrap(), before);
}

#[test]
fn only_the_exact_protected_completed_operation_qualifies() {
    let (o, p, j, s, w) = fixture();
    assert!(boss_jobs::worker_teardown::qualify_operation(&o, &p, &j, &s, &w).is_ok());
    for fault in [
        "old-protocol",
        "unprotected",
        "wrong-executor",
        "incomplete",
        "wrong-actor",
        "failed",
        "truncated",
        "stream-hash",
        "wrong-argv",
        "wrong-file",
        "wrong-host",
        "wrong-lineage",
        "old-authority",
        "unpaid-identity",
    ] {
        let (o, mut p, j, mut s, mut w) = fixture();
        match fault {
            "old-protocol" => w.version = 1,
            "unprotected" => w.steps[0].fields.retain(|f| f.name != "executed"),
            "wrong-executor" => w.steps[0].executor = None,
            "incomplete" => s.status = StepStatus::Active,
            "wrong-actor" => s.completed_by = Some(ActorId::automation("forged")),
            "failed" => s.metadata["exit_code"] = json!("1"),
            "truncated" => s.metadata["streams"]["stdout"]["complete"] = json!(false),
            "stream-hash" => s.metadata["streams"]["stdout"]["sha256"] = json!("wrong"),
            "wrong-argv" => s.metadata["executed"]["argv"][2] = json!("foreign"),
            "wrong-file" => s.metadata["executed"]["verb_file_sha256"] = json!("wrong"),
            "wrong-host" => s.metadata["executed"]["host"] = json!("foreign"),
            "wrong-lineage" => p.version += 1,
            "old-authority" => p.reclaimer = Some("infra/forge/reap-dead-ci-jobs.sh".into()),
            "unpaid-identity" => p.identity_pin_unenforced = Some("activation debt".into()),
            _ => unreachable!(),
        }
        assert!(
            boss_jobs::worker_teardown::qualify_operation(&o, &p, &j, &s, &w).is_err(),
            "accepted {fault}"
        );
    }
}

#[test]
fn a_successful_exit_cannot_substitute_another_target_or_incomplete_inventory() {
    for fault in [
        "job_uid",
        "pvc_uid",
        "run_id",
        "acquisition",
        "generation",
        "pool_version",
        "amount",
        "reclaimer",
        "namespace",
        "pool",
        "state",
        "coverage",
        "attachment",
        "source",
        "control",
    ] {
        let (o, p, j, mut s, w) = fixture();
        let mut value: Value =
            serde_json::from_str(s.metadata["output"].as_str().unwrap()).unwrap();
        match fault {
            "generation" | "pool_version" => value["target"][fault] = json!(99),
            "state" => value["state"] = json!("unavailable"),
            "coverage" => value["inventories"]["jobs"]["complete"] = json!(false),
            "attachment" => value["manifest_attachment_receipt"] = json!(""),
            "source" => value["source"]["worker-teardown-witness.py"] = json!("wrong"),
            "control" => value["controls"]["namespace"] = json!(null),
            _ => value["target"][fault] = json!("different"),
        }
        let raw = value.to_string();
        s.metadata["output"] = json!(raw);
        s.metadata["streams"]["stdout"] = stream(raw.as_bytes());
        assert!(
            boss_jobs::worker_teardown::qualify_operation(&o, &p, &j, &s, &w).is_err(),
            "accepted {fault}"
        );
    }
    // A later pool declaration cannot silently re-authorize old obligations.
    let (mut o, p, j, s, w) = fixture();
    o.allocation.version = 1;
    o.reclaimer = "infra/forge/reap-dead-ci-jobs.sh".into();
    assert!(boss_jobs::worker_teardown::qualify_operation(&o, &p, &j, &s, &w).is_err());
}
