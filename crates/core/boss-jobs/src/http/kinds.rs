//! JobKind registry handlers — author, version, publish, and retire
//! JobKind specs via `/api/jobs/kinds`.

use super::*;

use axum::extract::{Path, Query};

#[allow(
    clippy::result_large_err,
    reason = "idiomatic axum Response error; crate-wide Box<Response> cleanup tracked separately"
)]
pub(super) fn kind_registry_or_503<R: JobsRepository, B: EventBus>(
    state: &JobsApiState<R, B>,
) -> Result<&Arc<dyn JobKindRegistry>, Response> {
    state.kind_registry.as_ref().ok_or_else(|| {
        (
            StatusCode::SERVICE_UNAVAILABLE,
            "job kind registry not configured",
        )
            .into_response()
    })
}

pub(super) fn kind_err_response(err: JobKindError) -> Response {
    match err {
        JobKindError::NotFound(msg) => (StatusCode::NOT_FOUND, msg).into_response(),
        JobKindError::Conflict(msg) => (StatusCode::CONFLICT, msg).into_response(),
        JobKindError::Invalid(msg) => (StatusCode::BAD_REQUEST, msg).into_response(),
        JobKindError::Storage(msg) => (StatusCode::INTERNAL_SERVER_ERROR, msg).into_response(),
    }
}

pub(super) async fn policy_check<R: JobsRepository, B: EventBus>(
    state: &JobsApiState<R, B>,
    user: &boss_policy_client::User,
    action: Action,
) -> Result<(), Response> {
    match state.policy.check(user, action, Resource::job_kind()).await {
        Ok(Decision::Allow { .. }) => Ok(()),
        Ok(Decision::Deny { reason }) => Err((StatusCode::FORBIDDEN, reason).into_response()),
        Err(e) => Err((
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("policy check failed: {e}"),
        )
            .into_response()),
    }
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
    if let Err(r) = policy_check(&state, &user, Action::Read).await {
        return r;
    }
    match reg.list_active(q.category.as_deref()).await {
        Ok(kinds) => Json(kinds).into_response(),
        Err(e) => kind_err_response(e),
    }
}

pub(super) async fn get_kind<R: JobsRepository + 'static, B: EventBus + 'static>(
    State(state): State<Arc<JobsApiState<R, B>>>,
    CurrentUser(user): CurrentUser,
    Path(kind): Path<String>,
) -> Response {
    let reg = match kind_registry_or_503(&state) {
        Ok(r) => r,
        Err(r) => return r,
    };
    if let Err(r) = policy_check(&state, &user, Action::Read).await {
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
    if let Err(r) = policy_check(&state, &user, Action::Read).await {
        return r;
    }
    match reg.get_version(&kind, version).await {
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
    if let Err(r) = policy_check(&state, &user, Action::Read).await {
        return r;
    }
    match reg.list_versions(&kind).await {
        Ok(versions) => Json(versions).into_response(),
        Err(e) => kind_err_response(e),
    }
}

pub(super) async fn create_kind<R: JobsRepository + 'static, B: EventBus + 'static>(
    State(state): State<Arc<JobsApiState<R, B>>>,
    CurrentUser(user): CurrentUser,
    Json(spec): Json<JobKindSpec>,
) -> Response {
    let reg = match kind_registry_or_503(&state) {
        Ok(r) => r,
        Err(r) => return r,
    };
    if let Err(r) = policy_check(&state, &user, Action::Create).await {
        return r;
    }
    match reg.create_draft(spec).await {
        Ok(stored) => (StatusCode::CREATED, Json(stored)).into_response(),
        Err(e) => kind_err_response(e),
    }
}

/// Body for the author-time dry run. Only the kind slug (for error
/// labels) and the step list are needed — the lint validates the graph,
/// not the heavyweight registry-row fields — so the editor doesn't have
/// to assemble a full `JobKindSpec` on every keystroke.
#[derive(serde::Deserialize)]
pub(super) struct DraftLintRequest {
    #[serde(default)]
    pub kind: String,
    pub steps: Vec<crate::registry::StepSpec>,
}

/// Author-time dry run — lint a draft's steps WITHOUT persisting.
/// Runs the same `validate_job_kind` the publish path enforces, against
/// the same process-resident StepType registry, so an editor showing
/// "no problems" will publish cleanly (D5). Always returns 200 with a
/// structured result; lint failures are data, not an HTTP error — the
/// editor renders them on the graph. See docs/design/jobkind-authoring-ux.md.
pub(super) async fn validate_kind<R: JobsRepository + 'static, B: EventBus + 'static>(
    State(state): State<Arc<JobsApiState<R, B>>>,
    CurrentUser(user): CurrentUser,
    Json(req): Json<DraftLintRequest>,
) -> Response {
    // Gated like create — the dry run is an authoring affordance.
    if let Err(r) = policy_check(&state, &user, Action::Create).await {
        return r;
    }
    let kind = if req.kind.is_empty() {
        "draft"
    } else {
        req.kind.as_str()
    };
    let spec = JobKindSpec::platform_seed(kind, "draft", "draft", Vec::new(), req.steps);
    let registry = crate::step_registry::StepRegistry::v1();
    let errs = crate::job_kind_lint::validate_job_kind(&spec, &registry);
    let problems: Vec<serde_json::Value> = errs
        .iter()
        .map(|e| {
            serde_json::json!({
                "step": e.step,
                "reason": e.reason,
                "message": e.to_string(),
            })
        })
        .collect();
    (
        StatusCode::OK,
        Json(serde_json::json!({ "ok": errs.is_empty(), "problems": problems })),
    )
        .into_response()
}

pub(super) async fn update_kind<R: JobsRepository + 'static, B: EventBus + 'static>(
    State(state): State<Arc<JobsApiState<R, B>>>,
    CurrentUser(user): CurrentUser,
    Path(kind): Path<String>,
    Json(mut spec): Json<JobKindSpec>,
) -> Response {
    let reg = match kind_registry_or_503(&state) {
        Ok(r) => r,
        Err(r) => return r,
    };
    if let Err(r) = policy_check(&state, &user, Action::Update).await {
        return r;
    }
    // Force kind match — a PUT for /kinds/foo always edits foo.
    spec.kind = kind;
    match reg.create_draft(spec).await {
        Ok(stored) => (StatusCode::CREATED, Json(stored)).into_response(),
        Err(e) => kind_err_response(e),
    }
}

pub(super) async fn publish_kind<R: JobsRepository + 'static, B: EventBus + 'static>(
    State(state): State<Arc<JobsApiState<R, B>>>,
    CurrentUser(user): CurrentUser,
    Path(kind): Path<String>,
) -> Response {
    let reg = match kind_registry_or_503(&state) {
        Ok(r) => r,
        Err(r) => return r,
    };
    if let Err(r) = policy_check(&state, &user, Action::Publish).await {
        return r;
    }
    match reg.publish(&kind).await {
        Ok(spec) => Json(spec).into_response(),
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
    if let Err(r) = policy_check(&state, &user, Action::Retire).await {
        return r;
    }
    match reg.retire(&kind).await {
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
