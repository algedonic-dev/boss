//! Inactive capability: no rule or sweep posts returns in car 2c.
use super::*;
use axum::extract::Path;
use boss_core::job::JobId;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct ReturnRequest {
    ops_request: uuid::Uuid,
}

fn unread(error: impl std::fmt::Display) -> Response {
    (
        StatusCode::SERVICE_UNAVAILABLE,
        format!("stock return evidence unavailable: {error}"),
    )
        .into_response()
}
fn refused(error: impl std::fmt::Display) -> Response {
    (
        StatusCode::CONFLICT,
        format!("stock return refused: {error}"),
    )
        .into_response()
}
pub(super) async fn return_stock<R: JobsRepository + 'static, B: EventBus + 'static>(
    State(state): State<Arc<JobsApiState<R, B>>>,
    CurrentUser(user): CurrentUser,
    Path(acquisition): Path<uuid::Uuid>,
    Json(body): Json<ReturnRequest>,
) -> Response {
    let scope = match state.policy.ask(&user, controls::UPDATE_JOB).await {
        Ok(Decision::Allow { scope }) => scope,
        Ok(Decision::Deny { reason }) => return (StatusCode::FORBIDDEN, reason).into_response(),
        Err(e) => return e.into_response(),
    };
    let Some(actor) = user.ambient_actor() else {
        return (StatusCode::FORBIDDEN, "authenticated reclaimer required").into_response();
    };
    // Read-only preparation can be cancelled safely. The ledger write below
    // retains the existing adapter's own budget/COMMIT-uncertainty contract.
    let prepared = tokio::time::timeout(std::time::Duration::from_secs(5), async {
        let op_id = JobId::from_uuid(body.ops_request);
        let operation = state
            .jobs
            .get_job(&op_id)
            .await
            .map_err(unread)?
            .ok_or_else(|| refused("operation absent"))?;
        let steps = state.jobs.list_steps(&op_id).await.map_err(unread)?;
        let executes: Vec<_> = steps
            .iter()
            .filter(|s| s.spec_slug.as_deref() == Some("execute"))
            .collect();
        if executes.len() != 1 {
            return Err(refused("operation execute identity ambiguous"));
        }
        let name = crate::worker_teardown::operation_pool(executes[0]).map_err(refused)?;
        let obligations = state
            .jobs
            .stock_obligations(acquisition)
            .await
            .map_err(unread)?;
        let obligation = obligations
            .into_iter()
            .find(|o| o.allocation.pool == name)
            .ok_or_else(|| refused("obligation absent"))?;
        if user.id != obligation.reclaimer {
            return Err((StatusCode::FORBIDDEN, "stock reclaimer mismatch").into_response());
        }
        let job = state
            .jobs
            .get_job(&obligation.claim.job_id)
            .await
            .map_err(unread)?
            .ok_or_else(|| refused("original job absent"))?;
        if !scope_matches(&user, &scope, &job) {
            return Err((StatusCode::FORBIDDEN, "job is outside your scope").into_response());
        }
        if operation.partition != job.partition {
            return Err(refused("operation and original job partitions differ"));
        }
        let pool = state
            .jobs
            .resource_pool_version(&name, obligation.allocation.version)
            .await
            .map_err(unread)?
            .ok_or_else(|| refused("original pool version absent"))?;
        if pool.return_verifier.as_deref() != Some("worker-teardown") {
            return Err(refused(
                "no qualified return adapter on original pool version",
            ));
        }
        let workflows = state
            .kind_registry
            .clone()
            .ok_or_else(|| unread("pinned protocol reader not configured"))?;
        let protocol = workflows
            .get_version(&operation.kind, operation.workflow_version)
            .await
            .map_err(unread)?;
        crate::worker_teardown::qualify_operation(
            &obligation,
            &pool,
            &operation,
            executes[0],
            &protocol,
        )
        .map_err(refused)?;
        Ok((name, pool, workflows, job.partition))
    })
    .await;
    let (name, pool, workflows, partition) = match prepared {
        Ok(Ok(p)) => p,
        Ok(Err(e)) => return e,
        Err(_) => return unread("read preparation deadline exceeded"),
    };
    let verifier = crate::worker_teardown::WorkerTeardownVerifier {
        jobs: state.jobs.as_ref(),
        workflows: workflows.as_ref(),
        pool: &pool,
    };
    let stamp = state
        .publisher
        .stamp_with_actor(actor)
        .await
        .with_partition(partition);
    match state
        .jobs
        .discharge_stock_at(
            acquisition,
            &name,
            &user.id,
            body.ops_request,
            &verifier,
            &stamp,
        )
        .await
    {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(e) => refused(e),
    }
}
