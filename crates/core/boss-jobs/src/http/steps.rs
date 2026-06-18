//! Step handlers — list, add, update (the readiness/sign-off/dispatch
//! engine), and sign-off stamping.

use super::*;

use axum::extract::Path;

pub(super) async fn list_steps<R: JobsRepository + 'static, B: EventBus + 'static>(
    State(state): State<Arc<JobsApiState<R, B>>>,
    Path(id): Path<String>,
) -> Response {
    let job_id = match parse_job_id(&id) {
        Some(id) => id,
        None => return (StatusCode::BAD_REQUEST, "invalid job id").into_response(),
    };

    match state.jobs.list_steps(&job_id).await {
        Ok(steps) => Json(steps).into_response(),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    }
}

pub(super) async fn add_step<R: JobsRepository + 'static, B: EventBus + 'static>(
    State(state): State<Arc<JobsApiState<R, B>>>,
    CurrentUser(user): CurrentUser,
    Path(id): Path<String>,
    Json(mut step): Json<Step>,
) -> Response {
    let job_id = match parse_job_id(&id) {
        Some(id) => id,
        None => return (StatusCode::BAD_REQUEST, "invalid job id").into_response(),
    };

    // Ensure the step belongs to this job.
    step.job_id = job_id;

    // Schema validation runs only when the step is being marked done —
    // required fields represent what must be true for the work to count
    // as complete, not what must be true for it to exist. A brand-new
    // scheduling step can have no `scheduled_at`; it gets filled in by
    // the person doing the work.
    if step.status == StepStatus::Completed
        && let Err(errors) = state
            .step_registry
            .validate_metadata(&step.kind, &step.metadata)
            .and_then(|()| {
                // Inline authoring: the completion contract is
                // the union of the kind bundle's fields and the step's
                // own authored fields.
                crate::step_registry::StepRegistry::validate_authored_fields(
                    &step.fields,
                    &step.metadata,
                )
            })
    {
        let msg = errors
            .iter()
            .map(|e| e.to_string())
            .collect::<Vec<_>>()
            .join("; ");
        return (
            StatusCode::BAD_REQUEST,
            format!("invalid step metadata: {msg}"),
        )
            .into_response();
    }

    let now = boss_clock_client::now_from(&state.clock).await;
    if let Err(e) = state.jobs.add_step_at(&step, now).await {
        return (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response();
    }

    // State event — full row state, what the rebuild consumes. The
    // actor is stamped from the authenticated session per the Level-B
    // actor-stamping invariant. Sim / runner back-channel paths
    // (role=system-sim|system, or an `automation:`/`rule:` id) include
    // an `assignee_id` on the Step body that names the real Employee
    // taking on the work; we honor that as the audit actor so
    // step.created rows attribute to a person, not a process. Otherwise
    // the actor is the session's own identity — a human operator, or
    // the named automation (`automation:<authority>`); never anonymous.
    let is_automation = user.id == "anonymous"
        || user.id.starts_with("automation:")
        || user.id.starts_with("rule:")
        || user.id.ends_with("-sim")
        || user.id.ends_with("-runner")
        || user.role == "system-sim"
        || user.role == "system";
    let actor = match (is_automation, step.assignee_id.as_deref()) {
        (true, Some(emp_id)) if !emp_id.is_empty() => {
            boss_core::actor::ActorId::Human(emp_id.to_string())
        }
        _ => user
            .ambient_actor()
            .unwrap_or_else(|| boss_core::actor::ActorId::Automation("platform".into())),
    };
    state
        .publisher
        .emit_with_actor_at(
            events::STEP_CREATED,
            actor,
            serde_json::to_value(&step).unwrap_or_default(),
            now,
        )
        .await;

    (
        StatusCode::CREATED,
        Json(serde_json::json!({ "id": step.id.to_string() })),
    )
        .into_response()
}

/// Dispatch path for the `job-kind-publish` StepType — the
/// terminal step of every `job-kind-design` Job. Reads
/// `job_kind_spec` out of the step metadata, validates it, and
/// calls `JobKindRegistry::publish_authored`. Returns the
/// published spec on success or a (status, message) pair the
/// caller can short-circuit with.
///
/// Validation = `boss_jobs::job_kind_lint::validate_all` —
/// catches required-field mismatches, unknown step kinds, and the
/// other static guarantees a published spec needs. The lint
/// failure is surfaced as 400 so the SPA can render the offender.
async fn dispatch_job_kind_publish(
    registry: &dyn crate::registry::JobKindRegistry,
    step: &boss_core::job::Step,
    job_id: boss_core::job::JobId,
) -> Result<crate::registry::JobKindSpec, (StatusCode, String)> {
    use crate::job_kind_lint::validate_all;

    let spec_value = step.metadata.get("job_kind_spec").ok_or((
        StatusCode::BAD_REQUEST,
        "job-kind-publish step missing required metadata field `job_kind_spec`".to_string(),
    ))?;

    let spec: crate::registry::JobKindSpec =
        serde_json::from_value(spec_value.clone()).map_err(|e| {
            (
                StatusCode::BAD_REQUEST,
                format!("`job_kind_spec` did not deserialize as JobKindSpec: {e}"),
            )
        })?;

    let registry_v1 = crate::step_registry::StepRegistry::v1();
    let lint_errs = validate_all(std::slice::from_ref(&spec), &registry_v1);
    if !lint_errs.is_empty() {
        let mut msg = String::from("job-kind-publish: spec failed validate_all:");
        for e in &lint_errs {
            msg.push_str(&format!("\n  {e}"));
        }
        return Err((StatusCode::BAD_REQUEST, msg));
    }

    registry.publish_authored(spec, job_id).await.map_err(|e| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("publish_authored failed: {e}"),
        )
    })
}

pub(super) async fn update_step<R: JobsRepository + 'static, B: EventBus + 'static>(
    State(state): State<Arc<JobsApiState<R, B>>>,
    Path((id, step_id_str)): Path<(String, String)>,
    CurrentUser(user): CurrentUser,
    Json(body): Json<serde_json::Value>,
) -> Response {
    let job_id = match parse_job_id(&id) {
        Some(id) => id,
        None => return (StatusCode::BAD_REQUEST, "invalid job id").into_response(),
    };
    let step_id = match parse_step_id(&step_id_str) {
        Some(id) => id,
        None => return (StatusCode::BAD_REQUEST, "invalid step id").into_response(),
    };

    // Audit-write authorization. Every step PUT emits at least a
    // STEP_UPDATED row below, so the write is gated here on a coarse
    // (Update, step) decision — the caller's role must be permitted to
    // update steps at all. The sign-off transition adds the role-scoped
    // `step-signoff:<role>` authority on top (see further down).
    // Simulator traffic is allowed by the SimBypassPolicyClient (trusted
    // box; the write is still stamped `_simulated`), so this gate never
    // stalls a regen.
    match state
        .policy
        .check(&user, Action::Update, Resource::step())
        .await
    {
        Ok(Decision::Deny { reason }) => {
            return (StatusCode::FORBIDDEN, reason).into_response();
        }
        Ok(_) => {}
        Err(e) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("policy check failed: {e}"),
            )
                .into_response();
        }
    }

    // PATCH semantics: fetch the current step, then overlay the caller's
    // body on top. Any field the caller omits keeps its current value,
    // so clients can send `{"status": "done"}` without having to round-
    // trip the whole Step. Full replacements still work — a body that
    // includes every field just overwrites everything.
    let old = match state.jobs.get_step(&step_id).await {
        Ok(Some(s)) => s,
        Ok(None) => return (StatusCode::NOT_FOUND, "step not found").into_response(),
        Err(e) => return (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    };

    let mut merged = match serde_json::to_value(&old) {
        Ok(v) => v,
        Err(e) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("serialize old step: {e}"),
            )
                .into_response();
        }
    };
    let merged_obj = match merged.as_object_mut() {
        Some(obj) => obj,
        None => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                "old step did not serialize to an object",
            )
                .into_response();
        }
    };
    let body_obj = match body.as_object() {
        Some(obj) => obj,
        None => return (StatusCode::BAD_REQUEST, "body must be a JSON object").into_response(),
    };
    for (k, v) in body_obj {
        merged_obj.insert(k.clone(), v.clone());
    }

    let mut step: Step = match serde_json::from_value(merged) {
        Ok(s) => s,
        Err(e) => {
            return (StatusCode::BAD_REQUEST, format!("invalid step fields: {e}")).into_response();
        }
    };
    // Path params are authoritative — reject body-driven ID swaps.
    step.job_id = job_id;
    step.id = step_id;

    // Stamps are server-minted (POST .../sign-offs) and requirements
    // are materialization data — a PUT body controls neither.
    step.sign_offs = old.sign_offs.clone();
    step.sign_offs_required = old.sign_offs_required.clone();

    // `authority_role` is immutable across PUTs. Carry the persisted
    // value forward so a body can neither raise nor lower the required
    // sign-off authority — the sign-off gate above reads `old.metadata`
    // for its decision, and this keeps the stored row consistent with
    // that decision (a caller can't change it in a prior PUT either).
    if let Some(old_obj) = old.metadata.as_object()
        && let Some(auth) = old_obj.get("authority_role").cloned()
        && let Some(obj) = step.metadata.as_object_mut()
    {
        obj.insert("authority_role".into(), auth);
    }

    // Auto-stamp completed_on on the done-transition if the caller
    // didn't send one. The simulator's LiveApiOutput sends the
    // sim-day explicitly; SPA-driven step completion ("Mark done"
    // button) doesn't, and falling through with NULL leaves the
    // step undated → dispatcher rule handlers stamp wall-clock
    // NOW() on every downstream row. Wall-clock is the right
    // default *here* because the operator pressing the button
    // really is acting in real time, but we let an explicit body
    // value win.
    let is_flipping_to_done =
        old.status != StepStatus::Completed && step.status == StepStatus::Completed;
    if is_flipping_to_done && step.completed_on.is_none() {
        step.completed_on = Some(boss_clock_client::now_from(&state.clock).await.date_naive());
    }

    // Sign-off contract: a step completes only when every required role has
    // stamped its *current* shape. Stale stamps (edits after stamping)
    // don't count.
    if is_flipping_to_done && !step.sign_offs_satisfied() {
        let current = boss_core::job::step_shape_hash(&step.title, &step.metadata);
        let missing: Vec<&str> = step
            .sign_offs_required
            .iter()
            .filter(|role| {
                !step
                    .sign_offs
                    .iter()
                    .any(|st| &&st.role == role && st.shape_hash == current)
            })
            .map(|r| r.as_str())
            .collect();
        return (
            StatusCode::CONFLICT,
            Json(serde_json::json!({
                "error": "sign-offs incomplete",
                "missing_or_stale_roles": missing,
            })),
        )
            .into_response();
    }

    // Validate metadata only when the step is done (see add_step for
    // rationale). In-progress updates can still carry thin metadata.
    if step.status == StepStatus::Completed
        && let Err(errors) = state
            .step_registry
            .validate_metadata(&step.kind, &step.metadata)
            .and_then(|()| {
                // Inline authoring: the completion contract is
                // the union of the kind bundle's fields and the step's
                // own authored fields.
                crate::step_registry::StepRegistry::validate_authored_fields(
                    &step.fields,
                    &step.metadata,
                )
            })
    {
        let msg = errors
            .iter()
            .map(|e| e.to_string())
            .collect::<Vec<_>>()
            .join("; ");
        return (
            StatusCode::BAD_REQUEST,
            format!("invalid step metadata: {msg}"),
        )
            .into_response();
    }

    // Blocker gate (invariant I-4 — preconditions enforced). When the
    // caller is flipping this step to `done`, every step in
    // `blocked_by` must already be in a terminal state. Otherwise the
    // machine is firing a transition whose upstream data dependencies
    // aren't satisfied. Moving a step to `active` or any other
    // non-terminal state is still fine even with open blockers (a tech
    // may start prep work before a sign-off lands); the gate only fires
    // at `done`.
    //
    // The terminal set is `Completed | Skipped`. A Skipped blocker
    // means that branch was provably not-taken (its ready_when is
    // false-forever) — for an OR-predicate dependent (`steps.a.done OR
    // steps.b.done`) where `a` completed and `b` skipped, the dependent
    // is legitimately Ready and must be completable, so a Skipped
    // blocker clears the gate. Only Pending/Ready/Active (work
    // genuinely still outstanding) or a missing blocker hold it. The
    // re-evaluator is the readiness authority and won't promote a
    // dependent whose predicate is unsatisfiable; a Skipped upstream is
    // a resolved branch, not a broken hand-off.
    let is_flipping_to_done =
        old.status != StepStatus::Completed && step.status == StepStatus::Completed;
    if is_flipping_to_done && !step.blocked_by.is_empty() {
        match state.jobs.resolve_blockers(&step.blocked_by).await {
            Ok(statuses) => {
                // Missing blockers (returned-length < asked-length) are
                // treated as unresolved — a step we can't find is
                // definitely not terminal.
                let resolved_by_id: std::collections::HashMap<_, _> =
                    statuses.into_iter().collect();
                let unresolved: Vec<String> = step
                    .blocked_by
                    .iter()
                    .filter_map(|id| match resolved_by_id.get(id) {
                        Some(StepStatus::Completed | StepStatus::Skipped) => None,
                        Some(s) => Some(format!("{id}={s:?}").to_lowercase()),
                        None => Some(format!("{id}=missing")),
                    })
                    .collect();
                if !unresolved.is_empty() {
                    return (
                        StatusCode::CONFLICT,
                        Json(serde_json::json!({
                            "error": "step has unresolved blockers",
                            "step_id": step.id.to_string(),
                            "unresolved_blockers": unresolved,
                        })),
                    )
                        .into_response();
                }
            }
            Err(e) => {
                return (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    format!("blocker check failed: {e}"),
                )
                    .into_response();
            }
        }
    }

    // Loud invalidation: an edit that changes the step's
    // completion-relevant shape makes existing stamps stale — they
    // attested different content. Stamps stay recorded (provenance);
    // the event tells the surface who must re-sign. Stamping itself
    // moved to POST .../sign-offs.
    let stamps_invalidated = !old.sign_offs.is_empty()
        && boss_core::job::step_shape_hash(&old.title, &old.metadata)
            != boss_core::job::step_shape_hash(&step.title, &step.metadata);

    // Calendar reservation hook — runs BEFORE the persistence
    // write so a hard-conflict 409 doesn't leave the step in the
    // new in-progress state without a reservation. The hook is a
    // no-op when calendar isn't configured or the step lacks the
    // scheduling metadata.
    match crate::calendar_hook::apply_step_transition(
        state.calendar.as_ref(),
        &old,
        &step,
        &user.id,
    )
    .await
    {
        Ok(crate::calendar_hook::HookOutcome::Conflict { existing_rows }) => {
            return (
                StatusCode::CONFLICT,
                Json(serde_json::json!({
                    "error": "calendar conflict",
                    "step_id": step.id.to_string(),
                    "existing": existing_rows,
                })),
            )
                .into_response();
        }
        Ok(_) => {}
        Err(e) => {
            tracing::warn!(error = %e, "calendar hook errored; proceeding with step update");
        }
    }

    // In-process dispatch for the `job-kind-publish` StepType. When a
    // step of this kind flips to Done, read `job_kind_spec` from
    // metadata, lint it via `validate_all`, and call
    // `JobKindRegistry::publish_authored` so the meta-Job's authoring
    // closes by writing a real registry row.
    //
    // Registry-write-first: if publish_authored fails, `update_step_at`
    // is never called and no STEP_UPDATED accumulates in audit_log for
    // a step whose side effect couldn't fire — keeping audit_log
    // integrity on partial failure.
    let mut published_kind: Option<crate::registry::JobKindSpec> = None;
    if is_flipping_to_done && step.kind == "job-kind-publish" {
        let Some(reg) = &state.kind_registry else {
            return (
                StatusCode::SERVICE_UNAVAILABLE,
                "JobKind registry unavailable for job-kind-publish dispatch",
            )
                .into_response();
        };
        match dispatch_job_kind_publish(reg.as_ref(), &step, job_id).await {
            Ok(spec) => published_kind = Some(spec),
            Err((status, msg)) => return (status, msg).into_response(),
        }
    }

    let now = boss_clock_client::now_from(&state.clock).await;
    if let Err(e) = state.jobs.update_step_at(&step, now).await {
        return (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response();
    }

    // State event + actor stamping per the Level-B invariant. The
    // actor here is the session user who PUT the step — typically a
    // human (the assignee or a manager signing off). Sim / dispatcher
    // back-channel paths set x-boss-user to a synthetic slug
    // (`brewery-sim`, `rule:<name>`) AND include a
    // `completed_by` field in the body that names the real
    // Employee whose work the step represents. We honor that
    // override when the calling identity is an automation slug
    // so the audit_log row attributes work to a person, not a
    // process — preserves the "human-powered state machine" frame
    // for sim-driven traffic.
    let body_completed_by = body
        .get("completed_by")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
        .map(str::to_string);
    let is_automation = user.id == "anonymous"
        || user.id.starts_with("automation:")
        || user.id.starts_with("rule:")
        || user.id.ends_with("-sim")
        || user.id.ends_with("-runner")
        || user.role == "system-sim"
        || user.role == "system";
    let actor = match (is_automation, body_completed_by.as_deref()) {
        (true, Some(emp_id)) => boss_core::actor::ActorId::Human(emp_id.to_string()),
        _ => user
            .ambient_actor()
            .unwrap_or_else(|| boss_core::actor::ActorId::Automation("platform".into())),
    };
    state
        .publisher
        .emit_with_actor_at(
            events::STEP_UPDATED,
            actor.clone(),
            serde_json::to_value(&step).unwrap_or_default(),
            now,
        )
        .await;

    // The `job-kind-publish` dispatch produces an audit-bearing
    // event with the full published spec — what `rebuild_job_kinds`
    // reads to reconstruct the registry from audit_log.
    if let Some(spec) = &published_kind {
        state
            .publisher
            .emit_with_actor_at(
                events::JOB_KIND_PUBLISHED,
                actor.clone(),
                serde_json::to_value(spec).unwrap_or_default(),
                now,
            )
            .await;
    }

    // Marker events for downstream consumers — informational
    // duplicates of state already in STEP_UPDATED. Rebuild ignores.
    if old.status != StepStatus::Completed && step.status == StepStatus::Completed {
        state
            .publisher
            .emit_with_actor_at(
                events::STEP_COMPLETED,
                actor.clone(),
                serde_json::json!({
                    "job_id": job_id.to_string(),
                    "step_id": step_id.to_string(),
                }),
                now,
            )
            .await;

        // Dispatcher routing: rules in
        // infra/dispatcher/rules.toml listen on
        // `step.done.<kind>` so each StepType's side effects can
        // be declared as a rule without a giant `match` in the
        // subscriber. Payload mirrors the simulator's in-process
        // SimEventBus shape so handlers don't fork by source —
        // subject_kind / subject_id come from the parent Job so
        // every handler has the Subject identity without an
        // extra fetch.
        if !step.kind.is_empty() {
            let (subject_kind, subject_id) =
                if let Ok(Some(job)) = state.jobs.get_job(&job_id).await {
                    (
                        boss_core::primitives::Subject::kind(&job.subject).to_string(),
                        boss_core::primitives::Subject::id(&job.subject).to_string(),
                    )
                } else {
                    (String::new(), String::new())
                };
            state
                .publisher
                .emit_with_actor_at(
                    &format!("step.done.{}", step.kind),
                    actor.clone(),
                    serde_json::json!({
                        "job_id": job_id.to_string(),
                        "step_id": step_id.to_string(),
                        "kind": step.kind,
                        "subject_kind": subject_kind,
                        "subject_id": subject_id,
                        "completed_on": step.completed_on,
                        "metadata": step.metadata,
                    }),
                    now,
                )
                .await;
        }
    }

    if stamps_invalidated {
        let stale_roles: Vec<String> = step.sign_offs.iter().map(|st| st.role.clone()).collect();
        state
            .publisher
            .emit_with_actor_at(
                events::STEP_STAMPS_INVALIDATED,
                actor.clone(),
                serde_json::json!({
                    "job_id": job_id.to_string(),
                    "step_id": step_id.to_string(),
                    "stale_roles": stale_roles,
                    "required_roles": step.sign_offs_required,
                }),
                now,
            )
            .await;
    }

    // Re-evaluate readiness: the just-updated step's status change may
    // make a downstream step's `ready_when` predicate flip (Pending →
    // Ready) or rule a branch out (Pending → Skipped). The re-evaluator
    // is the single readiness engine, driven off the active
    // JobKindSpec's predicates rather than denormalized edges.
    if let Some(reg) = &state.kind_registry {
        let job_for_reeval = match state.jobs.get_job(&job_id).await {
            Ok(Some(j)) => Some(j),
            _ => None,
        };
        if let Some(job) = job_for_reeval {
            match reg.get_active(&job.kind).await {
                Ok(spec) => {
                    if let Ok(mut steps) = state.jobs.list_steps(&job_id).await {
                        // `reevaluate` requires steps in spec order
                        // (sort_order == index); list_steps returns them
                        // sorted by sort_order, so the invariant holds.
                        // Invariant (expose, don't swallow): a Job's live
                        // step set must match its active JobKind spec, or
                        // `reevaluate`'s length-guard bails and the Job can
                        // no longer advance. With atomic materialization
                        // this only fires on a genuine mid-flight republish
                        // that changed the step count. Surface it loudly
                        // instead of silently stalling the Job.
                        if spec.steps.len() != steps.len() {
                            tracing::warn!(
                                job_id = %job_id,
                                kind = %job.kind,
                                spec_len = spec.steps.len(),
                                steps_len = steps.len(),
                                "re-eval: live step count != active JobKind spec — \
                                 readiness cannot advance this Job (its step graph \
                                 is inconsistent with its JobKind)"
                            );
                        }
                        let changed = crate::registry::reevaluate(
                            &spec,
                            &mut steps,
                            &job.subject,
                            &job.metadata,
                        );
                        for idx in changed {
                            let changed_step = &steps[idx];
                            if let Err(e) = state.jobs.update_step_at(changed_step, now).await {
                                tracing::warn!(
                                    job_id = %job_id,
                                    step_id = %changed_step.id,
                                    error = %e,
                                    "re-eval: failed to persist promoted step",
                                );
                                continue;
                            }
                            state
                                .publisher
                                .emit_with_actor_at(
                                    events::STEP_UPDATED,
                                    actor.clone(),
                                    serde_json::to_value(changed_step).unwrap_or_default(),
                                    now,
                                )
                                .await;
                            // D6 ready marker: when a step the re-eval
                            // just promoted lands in `Ready`, fire
                            // `step.ready.<kind>` so dispatcher rules
                            // can react to a step *becoming eligible*
                            // (the delegate-subjob spawn fork, D7) the
                            // same way `step.done.<kind>` lets them react
                            // to completion. Payload mirrors the
                            // `step.done` marker shape (subject pulled
                            // from the parent Job already in hand).
                            if changed_step.status == StepStatus::Ready
                                && !changed_step.kind.is_empty()
                            {
                                emit_step_ready(&state, &job, changed_step, &actor, now).await;
                            }
                        }

                        // If the step we just completed is a declared
                        // terminal, close the Job with that outcome and
                        // skip every still-non-terminal step. Pair the
                        // live Step back to its StepSpec by index (==
                        // sort_order, the materializer's contract).
                        let just_completed = old.status != StepStatus::Completed
                            && step.status == StepStatus::Completed;
                        let terminal_outcome = spec
                            .steps
                            .get(step.sort_order as usize)
                            .and_then(|spec_step| spec_step.terminal.as_ref())
                            .map(|t| t.outcome.clone());
                        if just_completed && let Some(outcome) = terminal_outcome {
                            close_job_on_terminal(&state, &job_id, &outcome, &actor, now).await;
                        }
                    }
                }
                Err(crate::registry::JobKindError::NotFound(_)) => {
                    // No active spec (ad-hoc / registry-less kind):
                    // nothing to re-evaluate. The compute_job_status
                    // auto-close below still handles the
                    // all-steps-terminal case.
                }
                Err(e) => {
                    tracing::warn!(error = %e, job_id = %job_id, "re-eval: get_active failed");
                }
            }
        }
    }

    // Auto-transition job status based on step states. Acts as the
    // catch-all close path: a Job whose every step reached a terminal
    // state (Completed / Skipped) closes here even when no *declared*
    // terminal step fired (the declared-terminal close above already
    // handled that case and left the Job Closed, so this no-ops then).
    if let Ok(steps) = state.jobs.list_steps(&job_id).await {
        let new_status = compute_job_status(&steps);
        if let Ok(Some(mut job)) = state.jobs.get_job(&job_id).await
            && job.status != new_status
            && job.status != JobStatus::Cancelled
            && job.status != JobStatus::Draft
            // A Closed Job is terminal — never reopen / re-transition it
            // (the declared-terminal close above may have already closed
            // it, force-skipping a still-pending sign-off step).
            && job.status != JobStatus::Closed
        {
            let old_status = job.status;
            job.status = new_status;
            if new_status == JobStatus::Closed {
                // Same sim-day-vs-wall-clock contract as step.completed_on:
                // the closing transition is the step we just wrote, so its
                // completed_on (sim-day when the LiveApiOutput sent it,
                // wall-clock-NOW filled in above otherwise) is the right
                // anchor for the Job's closed_on too. Falls through to
                // wall-clock only if the step somehow lacks a date.
                let job_now = boss_clock_client::now_from(&state.clock).await;
                job.closed_on = step.completed_on.or(Some(job_now.date_naive()));
                let _ = (job_now,); // keep job_now hoisted for the update_job_at call below
            }
            let job_now = boss_clock_client::now_from(&state.clock).await;
            let _ = state.jobs.update_job_at(&job, job_now).await;

            // State event — full row state for the rebuild. The
            // actor is inherited from the step transition that
            // triggered this auto-close: the operator (or sim slug)
            // who flipped the terminal step is the responsible CPU
            // for the resulting Job state change too.
            state
                .publisher
                .emit_with_actor_at(
                    events::JOB_UPDATED,
                    actor.clone(),
                    serde_json::to_value(&job).unwrap_or_default(),
                    job_now,
                )
                .await;

            state
                .publisher
                .emit_with_actor_at(
                    events::JOB_STATUS_CHANGED,
                    actor.clone(),
                    serde_json::json!({
                        "id": job.id.to_string(),
                        "old_status": old_status,
                        "new_status": new_status,
                    }),
                    job_now,
                )
                .await;

            if new_status == JobStatus::Closed {
                state
                    .publisher
                    .emit_with_actor_at(
                        events::JOB_CLOSED,
                        actor.clone(),
                        serde_json::json!({
                            "id": job.id.to_string(),
                            "closed_on": job.closed_on,
                            // D7: same delegate-subjob back-link as the
                            // terminal-close path, so a child Job that
                            // closes via the all-steps-terminal catch-all
                            // (no declared `outcome` step) still triggers
                            // the parent resolve. Null when absent.
                            "parent_step_id": job.metadata.get("parent_step_id"),
                        }),
                        job_now,
                    )
                    .await;
            }
        }
    }

    StatusCode::NO_CONTENT.into_response()
}

#[derive(Deserialize)]
pub(super) struct SignOffBody {
    /// Which required role this stamp satisfies.
    role: String,
}

/// POST /api/jobs/{id}/steps/{step_id}/sign-offs — stamp a step in
/// its current shape. Policy decides who may sign via
/// the role-scoped resource `step-signoff:<role>`.
/// Idempotent per (role, current shape): re-stamping unchanged
/// content returns the step unchanged.
pub(super) async fn post_step_sign_off<R: JobsRepository + 'static, B: EventBus + 'static>(
    State(state): State<Arc<JobsApiState<R, B>>>,
    Path((id, step_id_str)): Path<(String, String)>,
    CurrentUser(user): CurrentUser,
    Json(body): Json<SignOffBody>,
) -> Response {
    let job_id = match parse_job_id(&id) {
        Some(v) => v,
        None => return (StatusCode::BAD_REQUEST, "invalid job id").into_response(),
    };
    let step_id = match parse_step_id(&step_id_str) {
        Some(v) => v,
        None => return (StatusCode::BAD_REQUEST, "invalid step id").into_response(),
    };
    let mut step = match state.jobs.get_step(&step_id).await {
        Ok(Some(s)) => s,
        Ok(None) => return (StatusCode::NOT_FOUND, "no such step").into_response(),
        Err(e) => return (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    };
    let role = body.role;
    if !step.sign_offs_required.iter().any(|r| r == &role) {
        return (
            StatusCode::UNPROCESSABLE_ENTITY,
            format!("role {role} is not required on this step"),
        )
            .into_response();
    }
    let decision = match state
        .policy
        .check(
            &user,
            Action::SignOff,
            Resource::new(format!("step-signoff:{role}")),
        )
        .await
    {
        Ok(d) => d,
        Err(e) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("policy check failed: {e}"),
            )
                .into_response();
        }
    };
    if let Decision::Deny { reason } = decision {
        return (StatusCode::FORBIDDEN, reason).into_response();
    }

    let shape = boss_core::job::step_shape_hash(&step.title, &step.metadata);
    if step
        .sign_offs
        .iter()
        .any(|st| st.role == role && st.shape_hash == shape)
    {
        return Json(step).into_response(); // idempotent re-stamp
    }
    let now = boss_clock_client::now_from(&state.clock).await;
    let stamp = boss_core::job::SignOffStamp {
        authority_id: user.id.clone(),
        role: role.clone(),
        stamped_at: now,
        shape_hash: shape.clone(),
    };
    if let Err(e) = state.jobs.append_sign_off(&step_id, &stamp, now).await {
        return (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response();
    }
    step.sign_offs.push(stamp);
    let actor = boss_core::actor::ActorId::human(&user.id);
    state
        .publisher
        .emit_with_actor_at(
            events::STEP_SIGNED_OFF,
            actor,
            serde_json::json!({
                "job_id": job_id.to_string(),
                "step_id": step_id.to_string(),
                "role": role,
                "authority_id": user.id,
                "shape_hash": shape,
            }),
            now,
        )
        .await;
    Json(step).into_response()
}

/// Close a Job because a *declared terminal* step reached
/// `Completed`. Mirrors the `compute_job_status`-driven close (same
/// JOB_UPDATED / JOB_STATUS_CHANGED / JOB_CLOSED events, same
/// closed_on anchoring) but is outcome-aware: it stamps
/// `metadata.outcome` and marks every still-non-terminal step
/// (`Pending` / `Ready` / `Active`) `Skipped` so the closed Job has no
/// dangling open work. No-ops if the Job is already terminal
/// (Cancelled / Draft) or already Closed.
async fn close_job_on_terminal<R: JobsRepository + 'static, B: EventBus + 'static>(
    state: &Arc<JobsApiState<R, B>>,
    job_id: &boss_core::job::JobId,
    outcome: &str,
    actor: &boss_core::actor::ActorId,
    now: chrono::DateTime<chrono::Utc>,
) {
    let Ok(Some(mut job)) = state.jobs.get_job(job_id).await else {
        return;
    };
    if matches!(
        job.status,
        JobStatus::Closed | JobStatus::Cancelled | JobStatus::Draft
    ) {
        // Already terminal / not-yet-open — nothing to close.
        return;
    }

    // Skip every still-non-terminal step. The Job is closing on its
    // terminal outcome; any Pending/Ready/Active step is now moot.
    if let Ok(steps) = state.jobs.list_steps(job_id).await {
        for mut s in steps {
            if matches!(
                s.status,
                StepStatus::Pending | StepStatus::Ready | StepStatus::Active
            ) {
                s.status = StepStatus::Skipped;
                if let Err(e) = state.jobs.update_step_at(&s, now).await {
                    tracing::warn!(
                        job_id = %job_id,
                        step_id = %s.id,
                        error = %e,
                        "terminal close: failed to skip non-terminal step",
                    );
                    continue;
                }
                state
                    .publisher
                    .emit_with_actor_at(
                        events::STEP_UPDATED,
                        actor.clone(),
                        serde_json::to_value(&s).unwrap_or_default(),
                        now,
                    )
                    .await;
            }
        }
    }

    let old_status = job.status;
    job.status = JobStatus::Closed;
    job.closed_on = Some(now.date_naive());
    // Stamp the terminal outcome onto the Job metadata so projections
    // / the SPA can render *why* the Job closed.
    if let serde_json::Value::Object(map) = &mut job.metadata {
        map.insert(
            "outcome".to_string(),
            serde_json::Value::String(outcome.to_string()),
        );
    } else {
        job.metadata = serde_json::json!({ "outcome": outcome });
    }

    if let Err(e) = state.jobs.update_job_at(&job, now).await {
        tracing::warn!(job_id = %job_id, error = %e, "terminal close: failed to persist closed Job");
        return;
    }

    state
        .publisher
        .emit_with_actor_at(
            events::JOB_UPDATED,
            actor.clone(),
            serde_json::to_value(&job).unwrap_or_default(),
            now,
        )
        .await;
    state
        .publisher
        .emit_with_actor_at(
            events::JOB_STATUS_CHANGED,
            actor.clone(),
            serde_json::json!({
                "id": job.id.to_string(),
                "old_status": old_status,
                "new_status": JobStatus::Closed,
            }),
            now,
        )
        .await;
    state
        .publisher
        .emit_with_actor_at(
            events::JOB_CLOSED,
            actor.clone(),
            serde_json::json!({
                "id": job.id.to_string(),
                "closed_on": job.closed_on,
                "outcome": outcome,
                // D7: surface the delegate-subjob back-link (if any) on
                // the close marker so the jobs.subjob_resolve rule can
                // gate `when` on it without fetching the Job. Null for
                // an ordinary (non-delegated) Job.
                "parent_step_id": job.metadata.get("parent_step_id"),
            }),
            now,
        )
        .await;
}

/// D6 ready marker — emit `step.ready.<kind>` for a step that just
/// transitioned into `Ready`. Mirrors the `step.done.<kind>` marker
/// (informational duplicate of state already in STEP_UPDATED /
/// STEP_CREATED; the rebuilder ignores it). The dispatcher rule
/// registry subscribes to it the same way it subscribes to
/// `step.done.<kind>`; the D7 delegate-subjob spawn fork is its first
/// consumer.
///
/// The payload is shape-compatible with the `step.done` marker — same
/// `job_id` / `step_id` / `kind` / `subject_kind` / `subject_id` /
/// `metadata` keys — so handlers reuse the same `StepEvent` view. The
/// Subject identity comes from the parent `job` the caller already
/// holds, so no extra fetch.
pub(super) async fn emit_step_ready<R: JobsRepository + 'static, B: EventBus + 'static>(
    state: &Arc<JobsApiState<R, B>>,
    job: &Job,
    step: &Step,
    actor: &boss_core::actor::ActorId,
    now: chrono::DateTime<chrono::Utc>,
) {
    let subject_kind = boss_core::primitives::Subject::kind(&job.subject).to_string();
    let subject_id = boss_core::primitives::Subject::id(&job.subject).to_string();
    state
        .publisher
        .emit_with_actor_at(
            &format!("step.ready.{}", step.kind),
            actor.clone(),
            serde_json::json!({
                "job_id": step.job_id.to_string(),
                "step_id": step.id.to_string(),
                "kind": step.kind,
                "subject_kind": subject_kind,
                "subject_id": subject_id,
                "metadata": step.metadata,
            }),
            now,
        )
        .await;
}
