//! Workflow registry handlers — author, version, publish, and retire
//! Workflow specs via `/api/workflows`.

use super::*;

use axum::extract::{Path, Query};

use crate::registry::WorkflowStatus;

#[allow(
    clippy::result_large_err,
    reason = "idiomatic axum Response error; crate-wide Box<Response> cleanup tracked separately"
)]
pub(super) fn kind_registry_or_503<R: JobsRepository, B: EventBus>(
    state: &JobsApiState<R, B>,
) -> Result<&Arc<dyn WorkflowRegistry>, Response> {
    state.kind_registry.as_ref().ok_or_else(|| {
        (
            StatusCode::SERVICE_UNAVAILABLE,
            "job kind registry not configured",
        )
            .into_response()
    })
}

/// The who + when every registry write hands to the adapter, which
/// builds and records the outbox event atomically with the row.
/// Actor per the Level-B stamping invariant: the authenticated
/// session identity, falling back to the platform automation for
/// header-less internal calls. `now` is clock-routed — never
/// wallclock — so sim-mode registry edits stamp sim time.
/// Shared with the step-plugin handlers (`plugins.rs`), whose
/// writes carry the same stamp contract.
pub(super) async fn write_stamp<R: JobsRepository, B: EventBus>(
    state: &JobsApiState<R, B>,
    user: &boss_policy_client::User,
) -> (boss_core::actor::ActorId, chrono::DateTime<chrono::Utc>) {
    let actor = user
        .ambient_actor()
        .unwrap_or_else(|| boss_core::actor::ActorId::Automation("platform".into()));
    let now = boss_clock_client::now_from(&state.clock).await;
    (actor, now)
}

pub(super) fn kind_err_response(err: WorkflowError) -> Response {
    match err {
        WorkflowError::CoverageUnavailable(msg) => {
            (StatusCode::SERVICE_UNAVAILABLE, msg).into_response()
        }
        WorkflowError::NotFound(msg) => (StatusCode::NOT_FOUND, msg).into_response(),
        WorkflowError::Conflict(msg) => (StatusCode::CONFLICT, msg).into_response(),
        WorkflowError::Invalid(msg) => (StatusCode::BAD_REQUEST, msg).into_response(),
        // 422, not 400: the spec parsed and is well-formed JSON — it
        // just describes work no Job could finish. Body is the same
        // `{ok, problems}` shape `_validate` returns so the editor
        // renders a refused publish exactly like a failed dry run.
        WorkflowError::Unviable(problems) => (
            StatusCode::UNPROCESSABLE_ENTITY,
            Json(lint_result_json(&problems)),
        )
            .into_response(),
        WorkflowError::Storage(msg) => (StatusCode::INTERNAL_SERVER_ERROR, msg).into_response(),
    }
}

/// The lint result body — `{ok, problems}`. One definition shared by
/// the author-time dry run (200) and the publish refusal (422).
pub(super) fn lint_result_json(
    problems: &[crate::workflow_lint::WorkflowLintError],
) -> serde_json::Value {
    serde_json::json!({
        "ok": problems.is_empty(),
        "problems": crate::workflow_lint::problems_json(problems),
    })
}

pub(super) async fn policy_check<R: JobsRepository, B: EventBus>(
    state: &JobsApiState<R, B>,
    user: &boss_policy_client::User,
    control: Pair,
) -> Result<(), Response> {
    match state.policy.ask(user, control).await {
        Ok(Decision::Allow { .. }) => Ok(()),
        Ok(Decision::Deny { reason }) => Err((StatusCode::FORBIDDEN, reason).into_response()),
        Err(e) => Err(e.into_response()),
    }
}

/// Whether `user` may read a DRAFT on the `workflow` resource — a
/// workflow version or a station version never published. `Ok(false)`
/// is a reader; `Err` is policy not answering (fail closed).
///
/// WHY (backlog 1a4a4d03, decided at its triage as the narrower
/// default). Workflow Read is the basic guest's shipped grant, justified
/// as "the operating model, already public in the repo" — true of the
/// protocols an install RUNS, not of the ones it is still writing. A
/// draft is an author's workspace: its metadata, entitlements and agent
/// blocks, a station predicate naming an assignee before anyone decided
/// to route that way. So a draft is read by whoever may author one —
/// Create, Update or Publish on `workflow` — while every version that
/// was published, active or retired, stays readable to Read: it is the
/// record of what ran, packets are pinned to retired versions, and a
/// step surface reads the version its packet runs under. A REJECTED
/// candidate (backlog 50ded63d) is read the same way: it was never
/// published, but packets ran it, so it is the record of what ran too.
///
/// THE FOURTH LEG (backlog f21804d8, design 600c07d7, decided
/// 2026-10-17). Read on `workflow-draft` reads a draft and authors
/// nothing. The movement observer captures the complete graph, and the
/// graph reads draft definitions before it filters them; with authoring
/// as the only way in, qualifying that read-only observer meant granting
/// it Create, Update or Publish. It is a grant of its own, held by
/// default only by the deploy superuser (who already reads drafts by
/// authoring) and never a shipped resource, so no read-only role
/// inherits it; neither an access tier nor workflow Read implies it.
pub(super) async fn may_read_drafts<R: JobsRepository, B: EventBus>(
    state: &JobsApiState<R, B>,
    user: &boss_policy_client::User,
) -> Result<bool, Response> {
    for control in [
        controls::CREATE_WORKFLOW,
        controls::UPDATE_WORKFLOW,
        controls::PUBLISH_WORKFLOW,
        controls::READ_WORKFLOW_DRAFT,
    ] {
        match state.policy.ask(user, control).await {
            Ok(Decision::Allow { .. }) => return Ok(true),
            Ok(Decision::Deny { .. }) => {}
            Err(e) => return Err(e.into_response()),
        }
    }
    Ok(false)
}

/// The refusal for a draft read by a caller who holds none of the four
/// legs of [`may_read_drafts`], naming the read-only one.
pub(super) fn draft_refusal(what: &str, user: &boss_policy_client::User) -> Response {
    (
        StatusCode::FORBIDDEN,
        format!(
            "{what} is an unpublished draft, read only by who may author one (Create, Update or \
             Publish on workflow) or holds Read on workflow-draft; {} (role {}) holds none of \
             those",
            user.id, user.role
        ),
    )
        .into_response()
}

#[derive(Deserialize)]
pub(super) struct ListKindsQuery {
    category: Option<String>,
}

pub(super) async fn list_kinds<R: JobsRepository + 'static, B: EventBus + 'static>(
    State(state): State<Arc<JobsApiState<R, B>>>,
    CurrentUser(user): CurrentUser,
    Query(q): Query<ListKindsQuery>,
) -> Response {
    let reg = match kind_registry_or_503(&state) {
        Ok(r) => r,
        Err(r) => return r,
    };
    // MACHINERY reads the active list by tier, not by grant (backlog
    // 47aed706, the follow-up of review 771c1308). The policy service's
    // coverage read signs as `automation:policy-coverage` at the
    // operator tier and judges every policy write by what this list
    // names; asked through policy, a rule narrowing its role's workflow
    // read — accepted once a second person holds the pair — blinded it,
    // and every guarded write then answered 503 until break-glass
    // restored the rule. People's roster admits the same reader the same
    // way (`boss-people` `grants::roster_scope`). Only active versions
    // are listed here, which the shipped basic guest already reads.
    if !crate::trust::is_trusted(&user)
        && let Err(r) = policy_check(&state, &user, controls::READ_WORKFLOW).await
    {
        return r;
    }
    match reg.list_active(q.category.as_deref()).await {
        Ok(kinds) => Json(kinds).into_response(),
        Err(e) => kind_err_response(e),
    }
}

/// The active read's query — and, by `deny_unknown_fields` on a struct
/// with no fields, the statement that it reads NONE. Until 2026-09-27
/// `GET /api/workflows/{kind}?version=2` answered the active row, HTTP
/// 200, without a word that `version` was never read: measured on
/// department-retro, `?version=2`, `?version=999` and `?no_such_param=1`
/// each answered v5 (backlog d57f6129). Any parameter is now a 400 that
/// names it (serde's "unknown field") and points at the route that does
/// answer one version — the jobs listing's posture (7f3e871a). Pinned in
/// tests/workflow_registry.rs.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct GetKindQuery {}

pub(super) async fn get_kind<R: JobsRepository + 'static, B: EventBus + 'static>(
    State(state): State<Arc<JobsApiState<R, B>>>,
    CurrentUser(user): CurrentUser,
    Path(kind): Path<String>,
    query: Result<Query<GetKindQuery>, axum::extract::rejection::QueryRejection>,
) -> Response {
    if let Err(rejection) = query {
        return (
            StatusCode::BAD_REQUEST,
            format!(
                "{} — GET /api/workflows/{kind} answers the ACTIVE version and reads no query \
                 parameter; one version is GET /api/workflows/{kind}/versions/{{version}}, \
                 every version is GET /api/workflows/{kind}/versions",
                rejection.body_text()
            ),
        )
            .into_response();
    }
    let reg = match kind_registry_or_503(&state) {
        Ok(r) => r,
        Err(r) => return r,
    };
    if let Err(r) = policy_check(&state, &user, controls::READ_WORKFLOW).await {
        return r;
    }
    match reg.get_active(&kind).await {
        Ok(spec) => Json(spec).into_response(),
        Err(e) => kind_err_response(e),
    }
}

pub(super) async fn get_kind_version<R: JobsRepository + 'static, B: EventBus + 'static>(
    State(state): State<Arc<JobsApiState<R, B>>>,
    CurrentUser(user): CurrentUser,
    Path((kind, version)): Path<(String, i32)>,
) -> Response {
    let reg = match kind_registry_or_503(&state) {
        Ok(r) => r,
        Err(r) => return r,
    };
    if let Err(r) = policy_check(&state, &user, controls::READ_WORKFLOW).await {
        return r;
    }
    match reg.get_version(&kind, version).await {
        Ok(spec) if spec.status == WorkflowStatus::Draft => {
            match may_read_drafts(&state, &user).await {
                Ok(true) => Json(spec).into_response(),
                Ok(false) => draft_refusal(&format!("workflow {kind} v{version}"), &user),
                Err(r) => r,
            }
        }
        Ok(spec) => Json(spec).into_response(),
        Err(e) => kind_err_response(e),
    }
}

pub(super) async fn list_kind_versions<R: JobsRepository + 'static, B: EventBus + 'static>(
    State(state): State<Arc<JobsApiState<R, B>>>,
    CurrentUser(user): CurrentUser,
    Path(kind): Path<String>,
) -> Response {
    let reg = match kind_registry_or_503(&state) {
        Ok(r) => r,
        Err(r) => return r,
    };
    if let Err(r) = policy_check(&state, &user, controls::READ_WORKFLOW).await {
        return r;
    }
    let drafts = match may_read_drafts(&state, &user).await {
        Ok(d) => d,
        Err(r) => return r,
    };
    match reg.list_versions(&kind).await {
        // A reader is served every version that was published and no
        // draft ([`may_read_drafts`]).
        Ok(versions) => Json(
            versions
                .into_iter()
                .filter(|v| drafts || v.status != WorkflowStatus::Draft)
                .collect::<Vec<_>>(),
        )
        .into_response(),
        Err(e) => kind_err_response(e),
    }
}

pub(super) async fn create_kind<R: JobsRepository + 'static, B: EventBus + 'static>(
    State(state): State<Arc<JobsApiState<R, B>>>,
    CurrentUser(user): CurrentUser,
    Json(spec): Json<WorkflowSpec>,
) -> Response {
    let reg = match kind_registry_or_503(&state) {
        Ok(r) => r,
        Err(r) => return r,
    };
    if let Err(r) = policy_check(&state, &user, controls::CREATE_WORKFLOW).await {
        return r;
    }
    let (actor, now) = write_stamp(&state, &user).await;
    match reg.create_draft(spec, &actor, now).await {
        Ok(stored) => (StatusCode::CREATED, Json(stored)).into_response(),
        Err(e) => kind_err_response(e),
    }
}

/// Body for the author-time dry run. Only the kind slug (for error
/// labels) and the step list are needed — the lint validates the graph,
/// not the heavyweight registry-row fields — so the editor doesn't have
/// to assemble a full `WorkflowSpec` on every keystroke.
#[derive(serde::Deserialize)]
pub(super) struct DraftLintRequest {
    #[serde(default)]
    pub kind: String,
    pub steps: Vec<crate::registry::StepSpec>,
}

/// Author-time dry run — lint a draft's steps WITHOUT persisting.
/// Calls `workflow_lint::gate_active`, the same function the publish
/// path enforces, so an editor showing "no problems" will publish
/// cleanly and a refused publish shows the same problem list.
/// Always returns 200 with a structured result; lint failures are
/// data, not an HTTP error — the editor renders them on the graph.
/// See architecture-decisions.md §Jobs, Workflows, Steps.
pub(super) async fn validate_kind<R: JobsRepository + 'static, B: EventBus + 'static>(
    State(state): State<Arc<JobsApiState<R, B>>>,
    CurrentUser(user): CurrentUser,
    Json(req): Json<DraftLintRequest>,
) -> Response {
    // Gated like create — the dry run is an authoring affordance.
    if let Err(r) = policy_check(&state, &user, controls::CREATE_WORKFLOW).await {
        return r;
    }
    let kind = if req.kind.is_empty() {
        "draft"
    } else {
        req.kind.as_str()
    };
    let spec = WorkflowSpec::platform_seed(kind, "draft", "draft", Vec::new(), req.steps);
    let errs = match crate::workflow_lint::gate_active(&spec) {
        Ok(()) => Vec::new(),
        Err(problems) => problems,
    };
    (StatusCode::OK, Json(lint_result_json(&errs))).into_response()
}

pub(super) async fn update_kind<R: JobsRepository + 'static, B: EventBus + 'static>(
    State(state): State<Arc<JobsApiState<R, B>>>,
    CurrentUser(user): CurrentUser,
    Path(kind): Path<String>,
    Json(mut spec): Json<WorkflowSpec>,
) -> Response {
    let reg = match kind_registry_or_503(&state) {
        Ok(r) => r,
        Err(r) => return r,
    };
    if let Err(r) = policy_check(&state, &user, controls::UPDATE_WORKFLOW).await {
        return r;
    }
    // Force kind match — a PUT for /kinds/foo always edits foo.
    spec.kind = kind;
    let (actor, now) = write_stamp(&state, &user).await;
    match reg.create_draft(spec, &actor, now).await {
        Ok(stored) => (StatusCode::CREATED, Json(stored)).into_response(),
        Err(e) => kind_err_response(e),
    }
}

/// Promote the latest draft of `kind` to ACTIVE.
///
/// The viability gate runs inside `WorkflowRegistry::publish`,
/// against the draft row the transaction actually promotes — not
/// against a copy re-read here, which could race a concurrent
/// author. An unviable draft comes back as `WorkflowError::Unviable`
/// and leaves as 422 + the problem list (2026-08-13: publish used to
/// accept anything, and the bad row surfaced as a dead API on the
/// next pod roll).
pub(super) async fn publish_kind<R: JobsRepository + 'static, B: EventBus + 'static>(
    State(state): State<Arc<JobsApiState<R, B>>>,
    CurrentUser(user): CurrentUser,
    Path(kind): Path<String>,
) -> Response {
    let reg = match kind_registry_or_503(&state) {
        Ok(r) => r,
        Err(r) => return r,
    };
    if let Err(r) = policy_check(&state, &user, controls::PUBLISH_WORKFLOW).await {
        return r;
    }
    let (actor, now) = write_stamp(&state, &user).await;
    match reg.publish(&kind, &actor, now).await {
        Ok(spec) => Json(spec).into_response(),
        Err(e) => kind_err_response(e),
    }
}

/// `DELETE /api/workflows/{kind}/versions/{version}` — discard a DRAFT
/// (ebd7bb70). The publish guard refuses a dirty registry and used to
/// demand "resolve that draft first" while no verb or route could;
/// this is that route. Draft-only: 409 for active/retired (history),
/// 404 for a version that never existed (a typo must not read as
/// success). Same policy gate as every registry write.
///
/// A draft a packet is pinned to is NOT pre-history — an experiment
/// admits packets to its draft candidate, and `boss job convert --to
/// vN` could move one onto a draft until it refused draft targets — so
/// the route refuses 409, naming
/// how many packets and one of them (backlog ce8b7d66). The registry
/// never reuses the discarded number either way, so this refusal
/// protects the packet's text, not only its number. This read answers
/// for every adapter; the Pg registry asks the same question again
/// INSIDE the discard's transaction, under a lock on the draft row, and
/// that answer is the one that decides — a packet committed between
/// this read and the delete is counted there, not orphaned. What it
/// cannot see is an admission that read the draft before the discard
/// and commits after it; the job insert takes no lock on the row it
/// pins to (recorded on backlog ce8b7d66).
pub(super) async fn discard_kind_version<R: JobsRepository + 'static, B: EventBus + 'static>(
    State(state): State<Arc<JobsApiState<R, B>>>,
    CurrentUser(user): CurrentUser,
    Path((kind, version)): Path<(String, i32)>,
) -> Response {
    let reg = match kind_registry_or_503(&state) {
        Ok(r) => r,
        Err(r) => return r,
    };
    if let Err(r) = policy_check(&state, &user, controls::UPDATE_WORKFLOW).await {
        return r;
    }
    match state.jobs.jobs_pinned_to_workflow(&kind, version).await {
        Ok(pinned) if pinned.count > 0 => {
            return (StatusCode::CONFLICT, pinned.discard_refusal(&kind, version)).into_response();
        }
        Ok(_) => {}
        Err(e) => return (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    }
    let (actor, now) = write_stamp(&state, &user).await;
    match reg.discard_draft(&kind, version, &actor, now).await {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(crate::registry::WorkflowError::NotFound(m)) => {
            (StatusCode::NOT_FOUND, m).into_response()
        }
        Err(crate::registry::WorkflowError::Conflict(m)) => {
            (StatusCode::CONFLICT, m).into_response()
        }
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    }
}

/// `POST /api/workflows/{kind}/versions/{version}/reject` — set a draft
/// that met traffic and lost to `rejected` (backlog 50ded63d). The
/// discard above refuses such a draft, rightly, and until this door
/// nothing else could move it, so a RETIRE verdict left the kind with
/// a draft publish would promote and `boss workflow publish` refused
/// to publish around. Rejected, it stays history for its pinned
/// packets and publish never sees it again.
///
/// Refuses rather than guesses. 409 while an OPEN experiment's split
/// still names the version on either arm — that experiment has not
/// concluded, and rejecting under it would silently send its share of
/// admissions back to the active version; 409 for an active or retired
/// row (history) and for a draft no packet is pinned to (it met no
/// traffic: discard it); 404 for a version that never existed; 503
/// when the open experiments cannot all be read. Retire on `workflow`,
/// the authority that takes a version out of service. 204 when the
/// row is rejected, including when it already was.
///
/// THE EXPERIMENT CHECK IS NOT ATOMIC WITH THE FLIP (the review of car
/// e3c4a326, N1). It reads the open experiments before, and outside,
/// the registry transaction, so an experiment write that puts a split
/// onto this version between the read and the flip still passes its own
/// check (the row reads Draft then) and the reject lands under an open
/// split. The window needs a concurrent experiment write; admission's
/// fail-safe sends that split's candidate share to the active version,
/// unstamped, so the cost is a skewed cohort, never a packet on a
/// rejected protocol.
pub(super) async fn reject_kind_version<R: JobsRepository + 'static, B: EventBus + 'static>(
    State(state): State<Arc<JobsApiState<R, B>>>,
    CurrentUser(user): CurrentUser,
    Path((kind, version)): Path<(String, i32)>,
) -> Response {
    let reg = match kind_registry_or_503(&state) {
        Ok(r) => r,
        Err(r) => return r,
    };
    if let Err(r) = policy_check(&state, &user, controls::RETIRE_WORKFLOW).await {
        return r;
    }
    let open_experiments = crate::port::JobFilter {
        kind: Some(crate::experiments::EXPERIMENT_KIND.to_string()),
        status: Some(boss_core::job::JobStatus::Open),
        ..Default::default()
    };
    // A limit is not a filter: every open experiment is read, or none
    // is trusted to have been.
    const PAGE: i64 = 500;
    match state.jobs.list_jobs(&open_experiments, PAGE, 0).await {
        Ok((rows, total)) if i64::try_from(rows.len()).unwrap_or(i64::MAX) < total => {
            return (
                StatusCode::SERVICE_UNAVAILABLE,
                format!(
                    "{total} open experiments and {} read — whether one still splits onto \
                     {kind} v{version} is unknown, so nothing was rejected",
                    rows.len()
                ),
            )
                .into_response();
        }
        Ok((rows, _)) => {
            if let Some(id) = crate::experiments::splitting_onto(&rows, &kind, version) {
                return (
                    StatusCode::CONFLICT,
                    format!(
                        "{kind} v{version} cannot be rejected: open experiment {id} still \
                         splits admissions onto it — conclude the experiment first"
                    ),
                )
                    .into_response();
            }
        }
        Err(e) => return (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    }
    let (actor, now) = write_stamp(&state, &user).await;
    match reg.reject_draft(&kind, version, &actor, now).await {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(e) => kind_err_response(e),
    }
}

pub(super) async fn retire_kind<R: JobsRepository + 'static, B: EventBus + 'static>(
    State(state): State<Arc<JobsApiState<R, B>>>,
    CurrentUser(user): CurrentUser,
    Path(kind): Path<String>,
) -> Response {
    let reg = match kind_registry_or_503(&state) {
        Ok(r) => r,
        Err(r) => return r,
    };
    if let Err(r) = policy_check(&state, &user, controls::RETIRE_WORKFLOW).await {
        return r;
    }
    let (actor, now) = write_stamp(&state, &user).await;
    match reg.retire(&kind, &actor, now).await {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(e) => kind_err_response(e),
    }
}

#[cfg(test)]
mod subject_kind_validator_tests {
    use super::*;
    use boss_core::job::Subject;
    use boss_subject_kinds_client::{FakeSubjectKindsClient, SubjectKindsClient};

    fn registry(client: FakeSubjectKindsClient) -> Option<Arc<dyn SubjectKindsClient>> {
        Some(Arc::new(client) as Arc<dyn SubjectKindsClient>)
    }

    #[tokio::test]
    async fn no_hardcoded_bypass_even_for_platform_named_kinds() {
        // The registry is the single source of truth — core no longer
        // fast-paths a baked-in vocabulary. A platform-named kind like
        // `account` absent from the registry is rejected like any other
        // unknown kind...
        let s = Subject::new("account", "acc-1");
        let reg = registry(FakeSubjectKindsClient::with(vec![]));
        assert!(
            check_custom_subject(reg.as_ref(), &s).await.is_err(),
            "an unknown-to-registry kind must 400, even a platform name"
        );
        // ...and passes once the registry lists it.
        let reg = registry(FakeSubjectKindsClient::with(vec!["account".into()]));
        check_custom_subject(reg.as_ref(), &s).await.unwrap();
    }

    #[tokio::test]
    async fn missing_registry_skips_check() {
        let s = Subject::new("anything", "x");
        check_custom_subject(None, &s).await.unwrap();
    }

    #[tokio::test]
    async fn known_custom_kind_passes() {
        let s = Subject::new("asset", "A-1");
        let reg = registry(FakeSubjectKindsClient::with(vec!["asset".into()]));
        check_custom_subject(reg.as_ref(), &s).await.unwrap();
    }

    #[tokio::test]
    async fn unknown_custom_kind_returns_400_with_actionable_message() {
        let s = Subject::new("made-up-kind", "x");
        let reg = registry(FakeSubjectKindsClient::with(vec!["asset".into()]));
        let resp = check_custom_subject(reg.as_ref(), &s).await.unwrap_err();
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    }
}
