//! A SIGNED protocol publish, for tests that publish over a live
//! passkey-protected row (backlog a4b57aca).
//!
//! The server publishes such a row only when the request names a
//! publish-a-protocol packet whose approve step a named approver signed
//! with a passkey for the exact bytes, over the live version the publish
//! replaces. ONE DEFINITION of that packet for every test file that
//! needs one (CLAUDE.md §9a): the rule's own pins
//! (a_passkey_guards_only_passkey_protocols.rs) and the ops-request
//! rehearsals whose road back publishes over the presence-carrying
//! ops-request row.
#![allow(dead_code)]

use boss_core::job::{
    Assurance, Job, JobStatus, Priority, SignOffStamp, Step, StepStatus, Subject, step_shape_hash,
};
use boss_jobs::InMemoryJobs;
use boss_jobs::port::JobsRepository;
use boss_jobs::registry::WorkflowSpec;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

pub fn sha(text: &str) -> String {
    hex::encode(Sha256::digest(text.as_bytes()))
}

/// A publish-a-protocol packet whose approve step emp-david signed with
/// a passkey for `text` at `path` over live `live_version`.
pub async fn signed_packet(
    jobs: &InMemoryJobs,
    kind: &str,
    path: &str,
    text: &str,
    live_version: &str,
) -> uuid::Uuid {
    let mut job = Job::new(
        "publish-a-protocol",
        Subject::new("custom", kind),
        format!("Publish {kind}"),
        "emp-david",
        Priority::Standard,
        chrono::NaiveDate::from_ymd_opt(2026, 11, 8).unwrap(),
    )
    .with_metadata(json!({"registry": "workflow", "row_name": kind}));
    job.status = JobStatus::Open;
    jobs.create_job(&job).await.unwrap();
    let mut approve = Step::new(job.id, "sign-off", "Approve", 1)
        .with_sign_offs_required(vec!["platform-admin".into()]);
    approve.spec_slug = Some("approve".into());
    approve.status = StepStatus::Completed;
    approve.assurance_required = Some(Assurance::Presence);
    approve.metadata = json!({
        "decision": "approved", "registry": "workflow", "row_name": kind,
        "spec_path": path, "spec_sha256": sha(text), "live_version": live_version,
    });
    approve.sign_offs = vec![SignOffStamp {
        authority_id: "emp-david".into(),
        role: "platform-admin".into(),
        stamped_at: chrono::Utc::now(),
        shape_hash: step_shape_hash(&approve.title, &approve.metadata),
        assurance: Assurance::Presence,
        presence_nonce: Some("nonce-1".into()),
        voided_at: None,
        voided_by_event: None,
    }];
    jobs.add_step(&approve).await.unwrap();
    let mut publish = Step::new(job.id, "task", "Publish", 2);
    publish.spec_slug = Some("publish".into());
    publish.status = StepStatus::Ready;
    jobs.add_step(&publish).await.unwrap();
    *job.id.inner().as_uuid()
}

/// The fields a publish authors — what the server compares between the
/// draft it promotes and the signed bytes.
fn authored(spec: &WorkflowSpec) -> Value {
    let mut v = serde_json::to_value(spec).unwrap();
    let o = v.as_object_mut().unwrap();
    for k in ["version", "status", "created_at", "authoring_job_id"] {
        o.remove(k);
    }
    v
}

/// `spec` as the platform bundle's TOML — the bytes a person signs —
/// checked to read back, through the seed's own loader, as `spec`. A
/// row a test builds in code (a field taken off, a writer removed) has
/// no file in the tree, so its signed bytes are rendered here.
pub fn spec_toml(spec: &WorkflowSpec) -> String {
    let mut row = serde_json::to_value(spec).unwrap();
    let o = row.as_object_mut().unwrap();
    for k in ["version", "status", "created_at", "authoring_job_id"] {
        o.remove(k);
    }
    o.retain(|_, v| !v.is_null());
    let mut steps = o.remove("steps").unwrap_or_else(|| json!([]));
    for step in steps.as_array_mut().unwrap() {
        step.as_object_mut().unwrap().retain(|_, v| !v.is_null());
    }
    o.insert("step".into(), steps);
    let text = toml::to_string(&json!({ "workflow": [row] })).unwrap();
    let back = boss_jobs::seed_loader::parse_workflows(&text, &spec.owning_team, "signed.toml")
        .unwrap_or_else(|e| panic!("the rendered row reads back: {e}\n{text}"))
        .remove(0);
    assert_eq!(
        authored(&back),
        authored(spec),
        "the rendered row reads back as the row it renders"
    );
    text
}

/// The body of a SIGNED publish of `spec` over live `live_version`: a
/// publish-a-protocol packet, filed in `jobs`, that signs `spec`'s
/// bytes — and the bytes.
pub async fn signed_body(jobs: &InMemoryJobs, spec: &WorkflowSpec, live_version: i64) -> Value {
    let path = format!("infra/platform/workflows/{}.toml", spec.kind);
    let text = spec_toml(spec);
    let packet = signed_packet(jobs, &spec.kind, &path, &text, &live_version.to_string()).await;
    json!({"approval": packet, "spec_path": path, "spec_toml": text})
}
