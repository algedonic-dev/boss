//! `GET /api/jobs/ops-verbs` and `POST /api/jobs/file-an-ops-request` —
//! a person files an ops request from the verb list, on the website
//! (backlog 7aef2405; David 2026-11-07 ~15:05Z: "In general, I just want
//! to be using the website UI for my controls").
//!
//! WHY. Car 72e7a790 gave a rendered plan's page its "File the request"
//! (`file_the_request.rs`), and on 2026-11-07 the gate volume move was
//! still filed from the pod shell line by line: a plan page files only
//! the write of a plan someone already filed, and filing the plan — or
//! any verb at all — still took `boss ops` in a terminal. This is the
//! reshape's other half, "from the verb list": the website lists the
//! allowlist and files a call to any verb on it.
//!
//! THE SAME RULES, THE SAME PACKET. The listing is the compiled-in
//! allowlist ([`crate::ops_verbs::catalog`]); a call is judged by
//! [`crate::ops_verbs::validate`], the rules `boss ops` applies; and the
//! packet is [`crate::ops_verbs::request_envelope`], the body `boss ops`
//! sends, through the one create door ([`create_job`]), so the policy's
//! Create on `job:ops-request` decides and `opened_by` is the caller. A
//! refusal is the allowlist's reason in no terminal's words (Codex
//! review of car 72e7a790). The body is `{host, verb, args}` and nothing
//! else: an owner, an approval flag or a `plan_sha256` is refused, since
//! the runner appends the SIGNED plan's hash itself (design 17835005).
//!
//! A PERSON'S DOOR. The caller must be a person by the one test the
//! human-only steps use ([`crate::human_only::person_check`]): an agent
//! or automation id is refused here even with the role that may open a
//! packet, so no agent gains a filing path through the website's surface.
//! `boss ops` stays the dark-API fallback, unchanged. The machine door
//! believes the id it is handed, so this refuses an agent that SAYS it
//! is one — not a forged person, which no create door can tell either.
//!
//! FILING IS NOT APPROVING. The packet's approve step is ready and
//! unsigned, exactly as one `boss ops` files; the passkey stamp on it is
//! still the approval, and the runner still re-checks every rule.

use super::*;

use axum::extract::Query;

use crate::ops_verbs;

/// The listing: every allowlisted verb, as a filer fills it in. It is
/// operator reference data — which hosts take which verbs, who approves
/// them — so it asks the policy's Read on job like every other jobs read,
/// and an unnamed caller is refused 401 before that, as the POST refuses
/// one (gate af039e44: every_jobs_read_asks_policy_or_is_named_public).
pub(super) async fn list_ops_verbs<R: JobsRepository + 'static, B: EventBus + 'static>(
    State(state): State<Arc<JobsApiState<R, B>>>,
    CurrentUser(user): CurrentUser,
) -> Response {
    if let Err(refusal) = named_caller(&user) {
        return refusal;
    }
    if let Err(refusal) = job_read_scope(&state, &user).await {
        return refusal;
    }
    match ops_verbs::shipped_allowlist() {
        Ok(verbs) => {
            Json(serde_json::json!({ "verbs": ops_verbs::catalog(&verbs) })).into_response()
        }
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, e).into_response(),
    }
}

/// A call, and only a call: `deny_unknown_fields` is what refuses an
/// owner, a routing flag or a hash riding beside it.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Call {
    host: String,
    verb: String,
    #[serde(default)]
    args: Vec<String>,
}

fn refused(status: StatusCode, why: String) -> Response {
    (status, Json(serde_json::json!({ "error": why }))).into_response()
}

/// Both doors' first question: a caller that sent no identity is refused
/// 401, before anything is read.
#[allow(
    clippy::result_large_err,
    reason = "idiomatic axum Response error, as readable_path_job"
)]
fn named_caller(user: &boss_policy_client::User) -> Result<(), Response> {
    if user.id.trim().is_empty() || user.id == boss_policy_client::User::ANONYMOUS_ID {
        return Err(refused(
            StatusCode::UNAUTHORIZED,
            "an unnamed caller is refused: the verb list and a filing are read and written \
             by a named person"
                .into(),
        ));
    }
    Ok(())
}

pub(super) async fn file_an_ops_request<R: JobsRepository + 'static, B: EventBus + 'static>(
    State(state): State<Arc<JobsApiState<R, B>>>,
    CurrentUser(user): CurrentUser,
    caller: Option<axum::Extension<crate::field_writer::CredentialedCaller>>,
    body: axum::body::Bytes,
) -> Response {
    // Who files is the record's first fact; an unnamed filing is refused
    // before anything is read, as file-the-request refuses one.
    if let Err(refusal) = named_caller(&user) {
        return refusal;
    }
    if let Err(why) = crate::human_only::person_check(state.roster.as_deref(), &user.id).await {
        return refused(
            StatusCode::FORBIDDEN,
            format!(
                "file-an-ops-request is a person's door, and `{}` is not a person: {why}. No \
                 agent or automation files a request through the website (backlog 7aef2405).",
                user.id
            ),
        );
    }
    let call: Call = match serde_json::from_slice(&body) {
        Ok(c) => c,
        Err(e) => {
            return refused(
                StatusCode::UNPROCESSABLE_ENTITY,
                format!(
                    "a request is {{host, verb, args}} and nothing else: {e}. The owner is \
                     whoever signs; requires_approval and requires_review are the verb file's; \
                     plan_sha256 is appended by the host's runner from the SIGNED plan (design \
                     17835005), so no filer supplies any of them"
                ),
            );
        }
    };
    let verbs = match ops_verbs::shipped_allowlist() {
        Ok(v) => v,
        Err(e) => return (StatusCode::INTERNAL_SERVER_ERROR, e).into_response(),
    };
    let v = match ops_verbs::validate(&verbs, &call.host, &call.verb, &call.args) {
        Ok(v) => v,
        Err(r) => {
            return refused(
                StatusCode::UNPROCESSABLE_ENTITY,
                r.reason_with("a plan_sha256 in the arguments"),
            );
        }
    };
    let request = ops_verbs::request_envelope(&v, &user.id);
    create_job(
        State(state),
        CurrentUser(user),
        Query(CreateJobQuery::admitting_steps()),
        caller,
        Json(request),
    )
    .await
}
