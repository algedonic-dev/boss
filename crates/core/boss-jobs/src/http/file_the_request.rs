//! `GET|POST /api/jobs/{plan}/file-the-request` — a rendered plan files
//! its mutating request (backlog 7aef2405, David's request 2026-11-05).
//!
//! WHY. A requires_approval ops verb already renders its plan onto the
//! request's approve step and the passkey signs those bytes. What was
//! missing was the FILING: every act listed for David — the disk_tight
//! alarm's replica rebuild, three tenant merges whose plans were already
//! rendered — meant typing `boss ops forge <verb> …` in a terminal, the
//! website had no surface that files a planned verb, and the agents'
//! harnesses refuse the mutating filing. A plan-* preview packet that
//! rendered clean is the whole request already: this door files it.
//!
//! WHAT IS FILED IS READ OFF THE RECORD, NEVER THE BODY. The write is the
//! verb whose `plan_verb` the plan packet ran; its host and args are the
//! plan's; the flags are the verb file's; all of it judged by
//! [`crate::ops_verbs`], the rules `boss ops` applies
//! ([`crate::ops_verbs::read_plan`]). The POST takes no body — a body
//! with any key is refused 422 — so neither a `plan_sha256` (the runner
//! appends the SIGNED plan's hash, design 17835005) nor an args override
//! nor a routing flag can ride in.
//!
//! FILING IS NOT APPROVING. The request is created through the one create
//! door, [`create_job`], as the caller that signed this POST: the policy's
//! Create on `job:ops-request` decides (crate::open_authority), the packet
//! carries `opened_by` = that caller, and its approve step is ready and
//! unsigned, exactly as one `boss ops` files. Nothing here signs, claims
//! or completes a step.
//!
//! ONCE WHILE IT IS OPEN. A request filed from a plan carries
//! `filed_from_plan`; while one is open a second POST is refused naming
//! it, so a double press cannot put two approvals of one plan in front of
//! a passkey. One that has closed (refused, answered, its approval gone
//! stale) does not block a new one. The check and the create are two
//! steps, so the gap between them is closed by the id: the request's id
//! is derived from the plan and the count of requests already filed from
//! it ([`request_id`]), so two presses racing past the check send the
//! same id, and the create door admits an id once (backlog 558396ff) —
//! the second is answered the packet the first created.
//!
//! The GET is the same reading without the write, for the page: `{plan:
//! false, why}` for a packet that is no plan, `{plan: true, fileable:
//! false, why, open_request}` for one whose write cannot be filed now,
//! `{plan: true, fileable: true, request, open_request: null}` otherwise.

use super::*;

use axum::body::Bytes;
use axum::extract::Path;
use axum::extract::Query;

use crate::ops_verbs::{self, FILED_FROM_PLAN, PlanReading, Validated};

/// The caller-independent reading of a plan packet, plus any request
/// already filed from it that is still open, and how many were ever
/// filed from it.
struct Reading {
    plan: boss_core::job::JobId,
    reading: PlanReading,
    open_request: Option<String>,
    filed_before: i64,
}

/// The id of the `n`th request filed from `plan` (0 for the first). The
/// same plan and count always name the same packet, so a filing raced
/// past the open-request check by a second press is admitted once.
fn request_id(plan: &boss_core::job::JobId, n: i64) -> String {
    uuid::Uuid::new_v5(
        &uuid::Uuid::NAMESPACE_OID,
        format!("boss:file-the-request:{plan}:{n}").as_bytes(),
    )
    .to_string()
}

#[allow(
    clippy::result_large_err,
    reason = "idiomatic axum Response error, as readable_path_job"
)]
async fn read<R: JobsRepository + 'static, B: EventBus + 'static>(
    state: &JobsApiState<R, B>,
    user: &boss_policy_client::User,
    id: &str,
) -> Result<Reading, Response> {
    let scope = job_read_scope(state, user).await?;
    let job = readable_path_job(state, user, &scope, id).await?;
    let plan = job.id;
    let packet = packet_value(state, job)
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e).into_response())?;
    let verbs = ops_verbs::shipped_allowlist()
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e).into_response())?;
    let reading = ops_verbs::read_plan(&verbs, &packet);
    let (open_request, filed_before) = if matches!(reading, PlanReading::NotAPlan(_)) {
        (None, 0)
    } else {
        // Asked of every packet, not the caller's scope: a twin the caller
        // cannot read still sits in front of a passkey.
        let filed_from = JobFilter {
            kind: Some("ops-request".into()),
            metadata_contains: Some(serde_json::json!({ FILED_FROM_PLAN: plan.to_string() })),
            ..Default::default()
        };
        let open = JobFilter {
            status: Some(JobStatus::Open),
            ..filed_from.clone()
        };
        let (_, ever) = state
            .jobs
            .list_jobs(&filed_from, 1, 0)
            .await
            .map_err(persist_error_response)?;
        let (rows, _) = state
            .jobs
            .list_jobs(&open, 1, 0)
            .await
            .map_err(persist_error_response)?;
        (rows.first().map(|j| j.id.to_string()), ever)
    };
    Ok(Reading {
        plan,
        reading,
        open_request,
        filed_before,
    })
}

fn request_json(v: &Validated) -> serde_json::Value {
    serde_json::json!({
        "host": v.host,
        "verb": v.verb,
        "args": v.args,
        "title": v.title,
        "mutating": v.mutating,
        "requires_approval": v.requires_approval,
        "requires_review": v.requires_review,
        "approvers": v.approvers,
    })
}

fn twin_refusal(open: &str) -> String {
    format!(
        "a request filed from this plan is still open: {open}. It waits on its own approve \
         step; a second would put two approvals of one plan in front of a passkey."
    )
}

pub(super) async fn preview_file_the_request<R: JobsRepository + 'static, B: EventBus + 'static>(
    State(state): State<Arc<JobsApiState<R, B>>>,
    Path(id): Path<String>,
    CurrentUser(user): CurrentUser,
) -> Response {
    let r = match read(&state, &user, &id).await {
        Ok(r) => r,
        Err(refusal) => return refusal,
    };
    let body = match (&r.reading, &r.open_request) {
        (PlanReading::NotAPlan(why), _) => serde_json::json!({ "plan": false, "why": why }),
        (PlanReading::Refused { write, why }, open) => serde_json::json!({
            "plan": true, "fileable": false, "write": write, "why": why, "open_request": open,
        }),
        (PlanReading::Fileable(v), Some(open)) => serde_json::json!({
            "plan": true, "fileable": false, "write": v.verb, "why": twin_refusal(open),
            "request": request_json(v), "open_request": open,
        }),
        (PlanReading::Fileable(v), None) => serde_json::json!({
            "plan": true, "fileable": true, "write": v.verb,
            "request": request_json(v), "open_request": null,
        }),
    };
    Json(body).into_response()
}

pub(super) async fn file_the_request<R: JobsRepository + 'static, B: EventBus + 'static>(
    State(state): State<Arc<JobsApiState<R, B>>>,
    Path(id): Path<String>,
    CurrentUser(user): CurrentUser,
    caller: Option<axum::Extension<crate::field_writer::CredentialedCaller>>,
    body: Bytes,
) -> Response {
    // THE BODY CARRIES NOTHING. An empty body, or an empty object, is the
    // only one admitted: every value the request holds is read off the
    // plan packet and the verb file.
    if !body.iter().all(u8::is_ascii_whitespace) {
        let sent: Option<serde_json::Value> = serde_json::from_slice(&body).ok();
        let empty = sent
            .as_ref()
            .and_then(serde_json::Value::as_object)
            .is_some_and(serde_json::Map::is_empty);
        if !empty {
            let keys: Vec<String> = sent
                .as_ref()
                .and_then(serde_json::Value::as_object)
                .map(|m| m.keys().cloned().collect())
                .unwrap_or_default();
            return (
                StatusCode::UNPROCESSABLE_ENTITY,
                Json(serde_json::json!({
                    "error": "file-the-request takes no body: the request is read off the plan \
                              packet and the verb file, never supplied by its filer",
                    "refused_keys": keys,
                    "hint": "the write's args are the plan's; requires_approval and \
                             requires_review are the verb file's; plan_sha256 is appended by \
                             the host's runner from the SIGNED plan (design 17835005), so no \
                             filer supplies it",
                })),
            )
                .into_response();
        }
    }
    // Who filed it is the record's first fact; an unnamed filing is
    // refused before anything is read, as the release door refuses one.
    if user.id.trim().is_empty() || user.id == boss_policy_client::User::ANONYMOUS_ID {
        return (
            StatusCode::UNAUTHORIZED,
            "an unnamed filing is refused: the record names who filed the request",
        )
            .into_response();
    }
    let r = match read(&state, &user, &id).await {
        Ok(r) => r,
        Err(refusal) => return refusal,
    };
    let v = match r.reading {
        PlanReading::NotAPlan(why) => {
            return (
                StatusCode::CONFLICT,
                format!("file-the-request: REFUSED — this packet is not a plan: {why}"),
            )
                .into_response();
        }
        PlanReading::Refused { write, why } => {
            return (
                StatusCode::CONFLICT,
                format!("file-the-request: {write} is not filed — {why}"),
            )
                .into_response();
        }
        PlanReading::Fileable(v) => v,
    };
    if let Some(open) = r.open_request {
        return (
            StatusCode::CONFLICT,
            format!("file-the-request: REFUSED — {}", twin_refusal(&open)),
        )
            .into_response();
    }
    // The envelope `boss ops` sends (ops_verbs::request_envelope, the one
    // copy), owned by the caller: the create door resolves the owner and
    // stamps the filer. This door adds the plan's id and its own.
    let mut request = ops_verbs::request_envelope(&v, &user.id);
    request["id"] = serde_json::json!(request_id(&r.plan, r.filed_before));
    request["metadata"][FILED_FROM_PLAN] = serde_json::json!(r.plan.to_string());
    create_job(
        State(state),
        CurrentUser(user),
        Query(CreateJobQuery::admitting_steps()),
        caller,
        Json(request),
    )
    .await
}
