//! Station read surfaces — the registry rows and the evaluated
//! queues (docs/design/station-tree.md).
//!
//! Reads pass through the SAME CurrentUser/policy path as the job
//! lists: the caller's read-scope Predicate on the `job` resource is
//! computed once and pushed into the packet query, so a station
//! queue can never show a caller a packet /api/jobs would hide. A
//! denied caller is REFUSED (403), and a read that could not be made
//! fails the answer (500 naming it) — neither is ever an empty 200,
//! which is what an idle network says (backlogs 8dcd28ce, c11e9d3c).

use super::*;

use std::collections::BTreeMap;

use axum::extract::Path;

use crate::JobsError;
use crate::station_projection::derived_stations;
use crate::station_queue::evaluate_station;
use crate::stations::{StationError, StationRegistry, StationSpec};

// This macro is used only by the two native read-preparation surfaces.
// It preserves each port's ordinary result and never wraps COMMIT.
macro_rules! prepare_selection {
    ($ingress:expr, $read:expr) => {{
        match $ingress {
            Some(ingress) => match {
                #[cfg(all(test, feature = "postgres"))]
                let mut trace = crate::native_selection_store::SelectionPhase::start(
                    stringify!($read).to_string(),
                    ingress.started(),
                );
                let result = ingress.prepare($read).await;
                #[cfg(all(test, feature = "postgres"))]
                trace.finish(if result.is_ok() {
                    "returned"
                } else {
                    "deadline"
                });
                result
            } {
                Ok(result) => result,
                Err(error) => {
                    return (axum::http::StatusCode::CONFLICT, error.to_string()).into_response();
                }
            },
            None => $read.await,
        }
    }};
}
pub(super) use prepare_selection;

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct ClaimNextRequest {
    request_id: uuid::Uuid,
}

pub(super) async fn claim_next_at_station<R: JobsRepository + 'static, B: EventBus + 'static>(
    State(state): State<Arc<JobsApiState<R, B>>>,
    Path(name): Path<String>,
    CurrentUser(mut user): CurrentUser,
    caller: Option<axum::Extension<crate::field_writer::CredentialedCaller>>,
    ingress: Option<axum::Extension<crate::station_claim::AuthenticatedSelectionIngress>>,
    estate: Option<axum::Extension<boss_core::machine_gate::VerifiedEstate>>,
    mode: Option<axum::Extension<boss_core::machine_gate::RequestModeObservation>>,
    Json(request): Json<ClaimNextRequest>,
) -> Response {
    // A resolved runner keeps its credential-bound identity. Otherwise the
    // estate gate's actual match admits the named machine caller, whose
    // registry row and policy are consumed by the native owner below.
    let ingress = ingress.or_else(|| {
        let estate = estate.as_ref()?;
        let mode = mode.as_ref()?;
        crate::station_claim::AuthenticatedSelectionIngress::from_machine(&user, &estate.0, mode.0)
            .map(axum::Extension)
    });
    if state.jobs.native_selection_requires_ingress() && ingress.is_none() {
        return (
            StatusCode::UNAUTHORIZED,
            "native selection requires server-authenticated ingress",
        )
            .into_response();
    }
    let admitted_ingress = ingress.as_ref().map(|extension| extension.0.clone());
    let preparation = admitted_ingress.as_ref();
    let native_read = match ingress.as_ref() {
        Some(ingress) => match state
            .jobs
            .native_selection_read(&ingress.0, request.request_id, &name)
            .await
        {
            Ok(read) => read,
            Err(error) => return (StatusCode::CONFLICT, error.to_string()).into_response(),
        },
        None => None,
    };
    if let Some(read) = &native_read {
        user = read.user.clone();
        if let Some(original) = &read.original {
            return selection_receipt_response(original.clone());
        }
    }
    let scope = if let Some(read) = &native_read {
        read.scope.clone()
    } else {
        let predicate = match readable_predicate(&state, &user).await {
            Ok(predicate) => predicate,
            Err(response) => return response,
        };
        JobScope::from_predicate(&user, &predicate)
    };
    let original = if native_read.is_some() {
        Ok(None)
    } else {
        state.jobs.station_claim_receipt(request.request_id).await
    };
    match original {
        Ok(Some(receipt)) => {
            if let Err(error) =
                crate::station_claim::replay(&receipt, &user.id, &name, request.request_id)
            {
                return (StatusCode::CONFLICT, error.to_string()).into_response();
            }
            let id = receipt
                .pointer("/selected/job_id")
                .and_then(serde_json::Value::as_str)
                .and_then(parse_job_id);
            let Some(id) = id else {
                return (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "selection receipt lacks its packet identity",
                )
                    .into_response();
            };
            return match state.jobs.get_job(&id).await {
                Ok(Some(job)) if scope.admits(&job) => selection_receipt_response(receipt),
                Ok(_) => (
                    StatusCode::FORBIDDEN,
                    "selection receipt packet is outside current read scope",
                )
                    .into_response(),
                Err(error) => {
                    (StatusCode::INTERNAL_SERVER_ERROR, error.to_string()).into_response()
                }
            };
        }
        Ok(None) => {}
        Err(error) => {
            return (StatusCode::INTERNAL_SERVER_ERROR, error.to_string()).into_response();
        }
    }
    let native_authority = match ingress {
        Some(ingress) if native_read.is_some() => match state
            .jobs
            .prepare_native_selection(ingress.0, request.request_id, &name)
            .await
        {
            Ok(Some(observation)) if observation.user == user => Some(Arc::new(observation)),
            Ok(_) => {
                return (
                    StatusCode::CONFLICT,
                    "native selection preparation changed its read identity",
                )
                    .into_response();
            }
            Err(error) => return (StatusCode::CONFLICT, error.to_string()).into_response(),
        },
        _ => None,
    };
    let prepared_policy = if native_authority.is_some() {
        None
    } else {
        Some(
            match crate::station_claim::PolicySelectionObservation::prepare(
                state.policy.clone(),
                user.clone(),
                request.request_id,
            ) {
                Ok(observation) => std::sync::Arc::new(observation),
                Err(error) => return (StatusCode::CONFLICT, error.to_string()).into_response(),
            },
        )
    };
    let registry = match stations_or_503(&state) {
        Ok(registry) => registry,
        Err(response) => return response,
    };
    let row = match prepare_selection!(preparation, station_by_name(&state, registry, &name)) {
        Ok(row) => row,
        Err(response) => return response,
    };
    let Some(station) = row.bind_self(self_id(&user)) else {
        return (StatusCode::UNAUTHORIZED, "selection requires a bound actor").into_response();
    };
    let active = match &state.kind_registry {
        Some(registry) => match prepare_selection!(preparation, registry.list_active(None)) {
            Ok(rows) => rows
                .into_iter()
                .map(|row| (row.kind.clone(), row))
                .collect(),
            Err(error) => {
                return (
                    StatusCode::SERVICE_UNAVAILABLE,
                    format!("selection protocol registry unreadable: {error}"),
                )
                    .into_response();
            }
        },
        None => {
            return (
                StatusCode::SERVICE_UNAVAILABLE,
                "selection protocol registry not configured",
            )
                .into_response();
        }
    };
    let packets = match prepare_selection!(
        preparation,
        resolved_open_packets(&state, scope.clone(), &std::collections::BTreeMap::new(),)
    ) {
        Ok(packets) => packets,
        Err(error) => {
            return (StatusCode::INTERNAL_SERVER_ERROR, error.to_string()).into_response();
        }
    };
    let tree_selection = if station.id.is_some() {
        let prepared = async {
            let stations = registry
                .list_active()
                .await
                .map_err(|error| JobsError::SelectionConflict(error.to_string()))?;
            let kinds = state.kind_registry.as_ref().ok_or_else(|| {
                JobsError::SelectionConflict("tree selection protocol owner unavailable".into())
            })?;
            let pins: std::collections::BTreeSet<_> = packets
                .iter()
                .map(|(job, _)| (job.kind.clone(), job.workflow_version))
                .collect();
            let mut protocols = Vec::with_capacity(pins.len());
            for (kind, version) in pins {
                protocols.push(
                    kinds
                        .get_version(&kind, version)
                        .await
                        .map_err(|error| JobsError::SelectionConflict(error.to_string()))?,
                );
            }
            let executor = if let Some(observation) = &native_authority {
                observation.pull_executor()
            } else {
                let owner = state.agent_budget.as_ref().ok_or_else(|| {
                    JobsError::SelectionConflict("tree selection actor owner unavailable".into())
                })?;
                let agents = owner
                    .agents
                    .list()
                    .await
                    .map_err(|error| JobsError::SelectionConflict(error.to_string()))?;
                crate::station_claim::PullExecutor::from_user(
                    &user,
                    agents
                        .iter()
                        .find(|row| row.id == user.id || row.aliases.contains(&user.id)),
                )
            };
            Ok::<_, JobsError>(crate::station_claim::TreeSelection {
                inputs: crate::station_claim::TreePullInputs {
                    stations,
                    protocols,
                },
                executor,
            })
        };
        match prepare_selection!(preparation, prepared) {
            Ok(tree) => Some(tree),
            Err(error) => return (StatusCode::CONFLICT, error.to_string()).into_response(),
        }
    } else {
        None
    };
    let selected = match &tree_selection {
        Some(tree) => crate::station_claim::head_in_tree(
            &station,
            &scope,
            &active,
            &packets,
            &tree.inputs,
            &tree.executor,
        ),
        None => crate::station_claim::head(&station, &scope, &active, &packets),
    };
    let candidate = match selected {
        Ok(Some(candidate)) => candidate,
        Ok(None) => return StatusCode::NO_CONTENT.into_response(),
        Err(error) => return (StatusCode::CONFLICT, error.to_string()).into_response(),
    };
    let context = crate::station_claim::SelectionContext {
        native_authority,
        actor_observation: None,
        prepared_policy,
        definition_fence: None,
        request_id: request.request_id,
        station,
        scope,
        active_workflows: active,
        tree_selection,
        validated_job: candidate.0,
        validated_step: candidate.1,
    };
    super::steps::claim_step_with_selection(
        State(state),
        Path((
            context.validated_job.id.to_string(),
            context.validated_step.id.to_string(),
        )),
        CurrentUser(user),
        caller,
        axum::extract::Query(super::steps::ClaimQuery::from_station(name)),
        Some(context),
    )
    .await
}

/// THE RECEIPT DOOR — what the store holds for one station pull's request
/// id, read and nothing else (backlog 614c2bb7, decided 2026-11-18). The
/// CLI journals each pull's request id and replays it until the server
/// answers for it, because a claim whose answer was lost may have
/// committed. A refused `claim-next` cannot settle that: the native read
/// expires "before commit dispatch" ahead of ever reading the store, so
/// the refusal says nothing about a receipt. And claim-next cannot read
/// the store after its refusal either — that is a second port read past
/// its ingress cut, which the native deadline tests forbid (car ebd06d09,
/// gate-run e8545c67). So the read is its own request.
///
/// 200 is the stored original, under the replay's identity rule (the
/// receipt's actor is the caller) and the caller's current read scope on
/// its packet. 404 says the store holds nothing for the id, and names the
/// id in a JSON body, so a server without this route — whose 404 is bare —
/// can never be read as "no receipt". A failed read is a 500. A committed
/// selection stores its receipt in its claim's transaction, so a 404
/// means nothing committed under the id as of this read.
pub(super) async fn station_claim_receipt_read<R: JobsRepository, B: EventBus>(
    State(state): State<Arc<JobsApiState<R, B>>>,
    Path(raw): Path<String>,
    CurrentUser(user): CurrentUser,
) -> Response {
    // Policy first, then the id: a typed path rejected a malformed id with
    // a 400 before policy was ever asked, and every jobs read asks policy
    // (every_jobs_read_asks_policy_or_is_named_public, gate-run 7f7bb6b5).
    let predicate = match readable_predicate(&state, &user).await {
        Ok(predicate) => predicate,
        Err(response) => return response,
    };
    let Ok(request_id) = uuid::Uuid::parse_str(&raw) else {
        return (
            StatusCode::BAD_REQUEST,
            format!("`{raw}` is not a selection request id"),
        )
            .into_response();
    };
    let receipt = match state.jobs.station_claim_receipt(request_id).await {
        Ok(Some(receipt)) => receipt,
        Ok(None) => {
            return (
                StatusCode::NOT_FOUND,
                Json(serde_json::json!({"request_id": request_id, "receipt": null})),
            )
                .into_response();
        }
        Err(error) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("reading the selection receipt for {request_id}: {error}"),
            )
                .into_response();
        }
    };
    if receipt.get("actor").and_then(serde_json::Value::as_str) != Some(user.id.as_str()) {
        return (
            StatusCode::FORBIDDEN,
            "selection receipt belongs to another actor",
        )
            .into_response();
    }
    let Some(id) = receipt
        .pointer("/selected/job_id")
        .and_then(serde_json::Value::as_str)
        .and_then(parse_job_id)
    else {
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            "selection receipt lacks its packet identity",
        )
            .into_response();
    };
    let scope = JobScope::from_predicate(&user, &predicate);
    match state.jobs.get_job(&id).await {
        Ok(Some(job)) if scope.admits(&job) => (
            StatusCode::OK,
            [(axum::http::header::CONTENT_TYPE, "application/json")],
            receipt.into_json(),
        )
            .into_response(),
        Ok(_) => (
            StatusCode::FORBIDDEN,
            "selection receipt packet is outside current read scope",
        )
            .into_response(),
        Err(error) => (StatusCode::INTERNAL_SERVER_ERROR, error.to_string()).into_response(),
    }
}

pub(super) fn selection_receipt_response(
    receipt: crate::station_claim::SelectionReceipt,
) -> Response {
    (
        if receipt["outcome"] == "resource-refused" {
            StatusCode::CONFLICT
        } else {
            StatusCode::CREATED
        },
        [(axum::http::header::CONTENT_TYPE, "application/json")],
        receipt.into_json(),
    )
        .into_response()
}

#[allow(
    clippy::result_large_err,
    reason = "idiomatic axum Response error; crate-wide Box<Response> cleanup tracked separately"
)]
pub(super) fn stations_or_503<R: JobsRepository, B: EventBus>(
    state: &JobsApiState<R, B>,
) -> Result<&Arc<dyn StationRegistry>, Response> {
    state.stations.as_ref().ok_or_else(|| {
        (
            StatusCode::SERVICE_UNAVAILABLE,
            "station registry not configured",
        )
            .into_response()
    })
}

/// The ACTIVE Workflow row per kind — what a step's agent block
/// resolves against ([`crate::agent_spec::resolved`]) and what
/// `station_reach` measures drift from.
///
/// Best-effort, and the degraded answer is the pre-51aef4dd one: with
/// no registry wired, or a read that fails, nothing resolves and each
/// station answers from the packets' own projections, as it did before
/// — a queue that still holds everything it held, rather than a
/// refusal that holds nothing.
pub(super) async fn active_rows<R: JobsRepository, B: EventBus>(
    state: &JobsApiState<R, B>,
) -> BTreeMap<String, crate::registry::WorkflowSpec> {
    match &state.kind_registry {
        Some(reg) => reg
            .list_active(None)
            .await
            .unwrap_or_default()
            .into_iter()
            .map(|row| (row.kind.clone(), row))
            .collect(),
        None => BTreeMap::new(),
    }
}

/// The open packets `scope` reaches, each with its steps RESOLVED
/// against its kind's ACTIVE row — the one set every reader that
/// counts a station's members evaluates the predicate over. The load
/// and the yard's marshalling read (which the regions map and the
/// borders both take) each listed and resolved their own copy until
/// 2026-09-23, and the yard's copy never resolved: the agent station
/// read 213 in the load and 170 on the map (backlog 6c06ef65). One
/// definition, so the two cannot disagree again (CLAUDE.md §9a).
///
/// A step read that fails is an error, never an empty step list: a
/// packet with no steps matches no step clause, so it would drop out
/// of the count instead of failing the read.
pub(super) async fn resolved_open_packets<R: JobsRepository, B: EventBus>(
    state: &JobsApiState<R, B>,
    scope: crate::port::JobScope,
    active: &BTreeMap<String, crate::registry::WorkflowSpec>,
) -> Result<Vec<(boss_core::job::Job, Vec<boss_core::job::Step>)>, crate::port::JobsError> {
    let filter = JobFilter {
        status: Some(JobStatus::Open),
        scope,
        ..Default::default()
    };
    // Every open packet, never one page of them: this bound its total
    // as `_total` and read one MAX_LIMIT page, so past 1000 open
    // packets every depth was a floor that did not say it was one
    // (backlog f71d1e81).
    let jobs = crate::list_every::list_every(state.jobs.as_ref(), &filter, MAX_LIMIT).await?;
    // Keep the whole scoped population, but read its steps through the
    // batch port: one PostgreSQL round trip instead of one per packet
    // inside the selection's shared ingress deadline (f999363f).
    let ids: Vec<_> = jobs.iter().map(|job| job.id).collect();
    let mut by_job: BTreeMap<_, Vec<_>> = BTreeMap::new();
    for step in state.jobs.list_steps_for_jobs(&ids).await? {
        by_job
            .entry(*step.job_id.inner().as_uuid())
            .or_default()
            .push(step);
    }
    let mut packets = Vec::with_capacity(jobs.len());
    for job in jobs {
        let steps = by_job.remove(job.id.inner().as_uuid()).unwrap_or_default();
        let steps = crate::agent_spec::resolved_steps(&steps, active.get(&job.kind));
        packets.push((job, steps));
    }
    Ok(packets)
}

fn station_err_response(err: StationError) -> Response {
    match err {
        StationError::NotFound(msg) => (StatusCode::NOT_FOUND, msg).into_response(),
        StationError::Conflict(msg) => (StatusCode::CONFLICT, msg).into_response(),
        StationError::Invalid(msg) => (StatusCode::BAD_REQUEST, msg).into_response(),
        // 422, not 400: the spec parsed and is well-formed JSON — it
        // just describes a queue that cannot behave as declared. Body
        // is the same `{ok, problems}` shape `_validate` returns, so
        // the editor renders a refused publish exactly like a failed
        // dry run.
        StationError::Unviable(problems) => (
            StatusCode::UNPROCESSABLE_ENTITY,
            Json(lint_result_json(&problems)),
        )
            .into_response(),
        StationError::Storage(msg) => (StatusCode::INTERNAL_SERVER_ERROR, msg).into_response(),
    }
}

/// The lint result body — `{ok, problems}`. One definition shared by
/// the author-time dry run (200) and the publish refusal (422).
fn lint_result_json(problems: &[crate::station_lint::StationLintError]) -> serde_json::Value {
    serde_json::json!({
        "ok": problems.is_empty(),
        "problems": crate::station_lint::problems_json(problems),
    })
}

/// The caller's packet-read predicate, or the refusal every station
/// read owes a caller who may read no packets.
///
/// One definition for the four read surfaces, because the posture is
/// one decision. Until 2026-09-23 each answered a denied scope with
/// `{"data": [], "total": 0}` and a 200 — the same bytes an idle
/// network sends — so a caller who could read nothing was told there
/// was nothing, and a board rendered it as calm (backlog 8dcd28ce).
/// A refused scope refuses: 403, naming the caller.
///
/// Refused on the TRANSLATED scope, not only `Predicate::None`: a
/// `DepartmentIs` grant for another department also translates to
/// [`JobScope::None`], and the queue it produced was the same empty
/// 200 for the same reason.
#[allow(
    clippy::result_large_err,
    reason = "idiomatic axum Response error; crate-wide Box<Response> cleanup tracked separately"
)]
async fn readable_predicate<R: JobsRepository, B: EventBus>(
    state: &JobsApiState<R, B>,
    user: &boss_policy_client::User,
) -> Result<boss_policy_client::Predicate, Response> {
    let predicate = state
        .policy
        .scope_of(user, controls::READ_JOB)
        .await
        .map_err(|e| e.into_response())?;
    match JobScope::from_predicate(user, &predicate) {
        JobScope::None => Err((
            StatusCode::FORBIDDEN,
            format!(
                "{} (role {}) may read no packets, so no station can be evaluated for them",
                user.id, user.role
            ),
        )
            .into_response()),
        _ => Ok(predicate),
    }
}

/// Station authoring is a network-configuration change, so it is
/// gated on the `workflow` resource — the same privilege that governs
/// the other registries a protocol is assembled from. A reader who
/// may see queues still cannot redraw them.
async fn station_policy_check<R: JobsRepository, B: EventBus>(
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

/// Q's effective registry: the authored rows PLUS the stations the
/// active protocol set requires.
///
/// Two sources, one list, authored wins — the merge rule lives in
/// [`derived_stations`], which drops a projected row whose name is
/// already authored. Reading them together HERE rather than
/// materialising derived rows into the table is deliberate: a
/// projection persisted is a second copy that can go stale the moment
/// a protocol is published, and the whole reason the constraint
/// stations were never hand-authored is that keeping fifty-one rows in
/// step with the protocols is work nobody does.
///
/// A protocol-set read failure is an ERROR, not a fallback to the
/// authored rows. Serving four stations where there should be
/// fifty-five, with a 200 and no explanation, is the exact shape of
/// defect this whole change exists to remove.
pub(super) async fn effective_stations<R: JobsRepository, B: EventBus>(
    state: &JobsApiState<R, B>,
    reg: &Arc<dyn StationRegistry>,
) -> Result<Vec<StationSpec>, Response> {
    let authored = reg.list_active().await.map_err(station_err_response)?;

    // No workflow registry wired: the same explicit seam every other
    // optional adapter keeps. There are no protocols to project from,
    // so the authored rows ARE the registry — not a degraded view of it.
    let Some(kinds) = state.kind_registry.as_ref() else {
        return Ok(authored);
    };
    let workflows = kinds.list_active(None).await.map_err(|e| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("cannot project stations — protocol set unreadable: {e}"),
        )
            .into_response()
    })?;

    let names: Vec<String> = authored.iter().map(|s| s.name.clone()).collect();
    let now = boss_clock_client::now_from(&state.clock).await;
    let mut all = authored;
    all.extend(derived_stations(&workflows, &names, now));
    all.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(all)
}

/// One station by name, resolved the way EVERY reader of a station
/// must resolve one: the authored row first, then the projection the
/// listing serves — the same two sources in the same order.
///
/// It is a function because it was two (923b6571, 2026-09-19). The
/// queue read fell back to the projection and the CLAIM did not, so a
/// claim naming a DERIVED station answered 404 — and since every
/// `(role, model)` agent inbox is derived (nothing authors
/// `a.platform-admin.opus-5-1m`; it is projected from the protocols'
/// agent blocks), no agent could claim from its own queue. A queue
/// that renders but cannot be claimed from is a display, not a
/// station. §9a: one definition, because two lookups of the same fact
/// drifted.
pub(super) async fn station_by_name<R: JobsRepository, B: EventBus>(
    state: &JobsApiState<R, B>,
    reg: &Arc<dyn StationRegistry>,
    name: &str,
) -> Result<StationSpec, Response> {
    match reg.get_active(name).await {
        Ok(s) => Ok(s),
        Err(StationError::NotFound(msg)) => effective_stations(state, reg)
            .await?
            .into_iter()
            .find(|s| s.name == name)
            .ok_or_else(|| station_err_response(StationError::NotFound(msg))),
        Err(e) => Err(station_err_response(e)),
    }
}

/// `GET /api/stations` — every active station row. The registry rows
/// themselves carry no packet data; the policy gate is the one every
/// station read shares ([`readable_predicate`]: a caller who may read
/// no packets is refused, not shown an empty registry).
///
/// `?under=<id or name>` is THE TREE READ (T2+T5 of design 4b27a65a,
/// backlog 3f58c823): see [`station_tree_read`].
pub(super) async fn list_stations<R: JobsRepository + 'static, B: EventBus + 'static>(
    State(state): State<Arc<JobsApiState<R, B>>>,
    CurrentUser(user): CurrentUser,
    axum::extract::Query(query): axum::extract::Query<StationListQuery>,
) -> Response {
    let reg = match stations_or_503(&state) {
        Ok(r) => r,
        Err(r) => return r,
    };
    if let Err(r) = readable_predicate(&state, &user).await {
        return r;
    }
    match effective_stations(&state, reg).await {
        Ok(rows) => match query.under {
            Some(under) => station_tree_read(&state, &user, &rows, &under).await,
            None => {
                let total = rows.len();
                Json(serde_json::json!({ "data": rows, "total": total })).into_response()
            }
        },
        Err(r) => r,
    }
}

#[derive(Debug, serde::Deserialize)]
pub(super) struct StationListQuery {
    under: Option<String>,
}

/// `GET /api/stations?under=<id or name>` — the subtree under one
/// station, root first then depth-first, each row with its ADDRESS
/// derived from the tree as the registry holds it now (never stored), its
/// depth and its children; and beside it the pools the subtree holds
/// from — every active pool when `under` is the root, which holds them
/// all — each `host:<node>` pool joined to its node read LIVE from the
/// estate registry, so the read serves the real boxes, not a copy.
///
/// Every gap is named, never filled: a pool list that cannot be read is
/// `pools_unavailable`, a host pool no estate row answers is
/// `host_unavailable` on that pool, an estate the caller may not read is
/// `hosts_unavailable`, and a live node no host pool declares is listed
/// in `hosts_without_pool`. `outside_tree` names the active rows (authored
/// or projected) that declare no `id`. 404 when no tree row is `under`.
async fn station_tree_read<R: JobsRepository + 'static, B: EventBus + 'static>(
    state: &Arc<JobsApiState<R, B>>,
    user: &boss_policy_client::User,
    rows: &[StationSpec],
    under: &str,
) -> Response {
    let nodes = match crate::station_tree::subtree(rows, under) {
        Ok(nodes) => nodes,
        Err(why) => return (StatusCode::NOT_FOUND, why).into_response(),
    };
    let is_root = nodes.first().is_some_and(|n| n.station.parent.is_none());
    let held: std::collections::BTreeSet<&str> = nodes
        .iter()
        .flat_map(|n| n.station.allocation.keys().map(String::as_str))
        .collect();
    let (pools, pools_unavailable) = match state.jobs.active_resource_pools().await {
        Ok(all) => (
            all.into_iter()
                .filter(|p| is_root || held.contains(p.name.as_str()))
                .collect::<Vec<_>>(),
            None,
        ),
        Err(e) => (
            Vec::new(),
            Some(format!("the pool registry could not be read: {e}")),
        ),
    };
    let (estate, hosts_unavailable) =
        match super::jobs::estate_read_refusal(state, user, "the estate registry").await {
            Some(_) => (
                None,
                Some("this caller may not read the estate registry".to_string()),
            ),
            None => match state.jobs.list_estate_nodes().await {
                Ok(nodes) => (Some(nodes), None),
                Err(e) => (
                    None,
                    Some(format!("the estate registry could not be read: {e}")),
                ),
            },
        };
    let pool_rows: Vec<serde_json::Value> = pools
        .iter()
        .map(|p| {
            let mut row = serde_json::json!({ "pool": p });
            if let Some(node) = crate::station_tree::host_of(&p.name) {
                match estate.as_ref().map(|e| e.iter().find(|n| n.id == node)) {
                    Some(Some(found)) => row["host"] = serde_json::json!(found),
                    Some(None) => {
                        row["host_unavailable"] =
                            serde_json::json!(format!("no estate node `{node}` is declared"))
                    }
                    None => row["host_unavailable"] = serde_json::json!(hosts_unavailable),
                }
            }
            row
        })
        .collect();
    let hosts_without_pool: Vec<&str> = if is_root {
        estate
            .iter()
            .flatten()
            .filter(|n| !n.retired)
            .filter(|n| {
                !pools
                    .iter()
                    .any(|p| crate::station_tree::host_of(&p.name) == Some(n.id.as_str()))
            })
            .map(|n| n.id.as_str())
            .collect()
    } else {
        Vec::new()
    };
    let outside_tree: Vec<&str> = rows
        .iter()
        .filter(|s| s.id.is_none())
        .map(|s| s.name.as_str())
        .collect();
    let total = nodes.len();
    Json(serde_json::json!({
        "under": nodes.first().map(|n| &n.station.name),
        "data": nodes,
        "total": total,
        "pools": pool_rows,
        "pools_unavailable": pools_unavailable,
        "hosts_unavailable": hosts_unavailable,
        "hosts_without_pool": hosts_without_pool,
        "outside_tree": outside_tree,
    }))
    .into_response()
}

/// `GET /api/stations/load` — every station's depth and the age of
/// the oldest packet waiting in it, in one call.
///
/// WHY IT EXISTS. Reading congestion across the network meant calling
/// each station's queue in turn: on 2026-08-17 that was 55 round
/// trips, each re-fetching an overlapping packet set. The flow board
/// (Q1: "the station is the unit") and M's matchmaking both want the
/// same answer, so it is one surface rather than two.
///
/// AGE, NOT DEPTH, IS THE SIGNAL (flow-board Q2). Depth is close to
/// meaningless without a drain rate — ten role queues each read
/// exactly 48 the day this was written and none was a bottleneck, they
/// were a bug. Age needs no rate to interpret. So `oldest_age_days` is
/// what the board sorts by; depth rides along because a reader wants both.
///
/// WHAT `oldest_age_days` ACTUALLY MEASURES, stated rather than
/// implied: the age of the oldest MEMBER PACKET, from its `opened_on`.
/// That is packet age, not time-in-this-queue — a packet that spent
/// eight days in review before arriving here reads as eight days old
/// on arrival. Station membership is a packet-level predicate, so a
/// packet-level age is the honest per-station figure; it over-reports
/// rather than under-reports, which is the safer direction for a
/// congestion signal. The STEP-level answer — how long has this
/// obligation waited, from the `became_ready_at` stamp — is the
/// queue-age lens, `GET /api/jobs/queue-age` (2a0b034e).
///
/// OPEN PACKETS ONLY. Stations with a `terminal_window_days` also hold
/// recently-departed packets so a filer can see an outcome; those are
/// not congestion and are excluded here, so a load figure can be lower
/// than the same station's queue length.
///
/// Cost: paged packet reads plus one batched step read, shared across
/// every station's predicate evaluation.
pub(super) async fn stations_load<R: JobsRepository + 'static, B: EventBus + 'static>(
    State(state): State<Arc<JobsApiState<R, B>>>,
    CurrentUser(user): CurrentUser,
) -> Response {
    let reg = match stations_or_503(&state) {
        Ok(r) => r,
        Err(r) => return r,
    };
    // A caller who may read no packets is refused (403), not shown a
    // network with every depth at zero (backlog 8dcd28ce).
    let predicate = match readable_predicate(&state, &user).await {
        Ok(p) => p,
        Err(r) => return r,
    };
    let stations = match effective_stations(&state, reg).await {
        Ok(s) => s,
        Err(r) => return r,
    };

    let scope = JobScope::from_predicate(&user, &predicate);

    // The ACTIVE protocol per kind, read ONCE — the second half of the
    // omission question `station_reach` answers, and the row each
    // packet's agent block resolves against (backlog 51aef4dd), so the
    // depth here and the queue an agent reads count the same members.
    let active = active_rows(&state).await;

    // Fetched ONCE and shared. Every constraint station matches on a
    // step, so per-station fetching would re-read the same rows 55
    // times.
    let packets = match resolved_open_packets(&state, scope, &active).await {
        Ok(p) => p,
        Err(e) => return (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    };

    let today = boss_clock_client::now_from(&state.clock).await.date_naive();
    // Membership first, rows second: a row's `also_elsewhere` needs every
    // station's members before any row can be written.
    let memberships: Vec<(
        StationSpec,
        Vec<&(boss_core::job::Job, Vec<boss_core::job::Step>)>,
    )> = stations
        .iter()
        // A per-actor station binds to the caller, exactly as its own
        // queue endpoint does; an unbindable one holds nothing rather
        // than everything.
        .filter_map(|spec| spec.bind_self(self_id(&user)))
        .map(|bound| {
            let members = packets
                .iter()
                .filter(|(job, steps)| bound.predicate.matches(job, steps))
                .collect();
            (bound, members)
        })
        .collect();
    // STATIONS OVERLAP, so the depths do not add up to the work. A
    // packet stands at every station whose predicate it matches: on
    // 2026-09-23 the marshalling sidings summed to 517 over 303 distinct
    // packets — all 213 in the agent station also stood in
    // q.platform-admin.task — and nothing on the board said so (backlog
    // 140a2222). How many stations each packet stands at, counted over
    // the rows this read returns, so `distinct_packets` and each row's
    // `also_elsewhere` are figures the server counted, not ones a reader
    // has to derive by joining N queue reads by id.
    let standings: BTreeMap<String, usize> = memberships
        .iter()
        .flat_map(|(_, members)| members.iter().map(|(job, _)| job.id.to_string()))
        .fold(BTreeMap::new(), |mut acc, id| {
            *acc.entry(id).or_insert(0) += 1;
            acc
        });
    let mut rows: Vec<serde_json::Value> = Vec::with_capacity(memberships.len());
    for (bound, members) in &memberships {
        let depth = members.len();
        let also_elsewhere = members
            .iter()
            .filter(|(job, _)| standings.get(&job.id.to_string()).is_some_and(|n| *n > 1))
            .count();
        let oldest = members.iter().map(|(j, _)| j.opened_on).min();
        rows.push(serde_json::json!({
            "station": bound.name,
            "kind": bound.kind,
            // The Workflow kind the predicate holds, beside the STATION
            // kind above — what the Department Map links to the per-kind
            // drill-down (/it/operate/bottlenecks?kind=…). Null when the
            // predicate spans kinds (backlog c7c5c1de).
            "workflow_kind": bound.predicate.kind,
            "depth": depth,
            "oldest_opened_on": oldest,
            "oldest_age_days": oldest.map(|d| (today - d).num_days()),
            // Packets this station's own predicate CANNOT see — absent,
            // not deprioritised, because a projected key never reached
            // their step (backlog abda9ab4). `depth + unreachable` is
            // what depth should have been; zero on a network with no
            // drift, and zero for the stations that read no projected
            // key at all.
            "unreachable": crate::station_reach::unreachable_packets(bound, &packets, &active),
            // Members that also stand at another station in `data`.
            "also_elsewhere": also_elsewhere,
        }));
    }
    let total = rows.len();
    Json(serde_json::json!({
        "data": rows,
        "total": total,
        // Distinct packets across every row's members — what the depth
        // column sums to once each packet is counted once.
        "distinct_packets": standings.len(),
    }))
    .into_response()
}

/// The window `GET /api/stations/flow` counts over when the caller
/// names none. A day is the shortest window in which every queue on
/// this network has had a chance to be worked at least once, so a
/// shorter default would report "not draining" about queues that were
/// merely quiet overnight.
const DEFAULT_FLOW_WINDOW_HOURS: i64 = 24;
/// Thirty days. The window is a filter over the log, not a page, so
/// the cost of a wide one is a wider scan; this caps it.
const MAX_FLOW_WINDOW_HOURS: i64 = 720;

/// The default basis of `GET /api/stations/flow`: each station counted
/// over the step record's own two stamps ([`crate::station_window`]).
pub(super) const STEP_WINDOW_BASIS: &str = "step-window-v1";

#[derive(Debug, Deserialize, Default)]
pub(super) struct FlowQuery {
    window_hours: Option<i64>,
    basis: Option<String>,
}

/// The window's packets with every step resolved against its kind's
/// ACTIVE row, as [`resolved_open_packets`] resolves the open ones — one
/// definition of what a station holds, so its flow and its members are
/// counted over the same steps (backlog 6c06ef65). This door and the
/// IT map's marshalling read both count through it.
pub(super) fn resolved_window(
    packets: Vec<crate::station_window::WindowPacket>,
    active: &BTreeMap<String, crate::registry::WorkflowSpec>,
) -> Vec<crate::station_window::WindowPacket> {
    packets
        .into_iter()
        .map(|packet| {
            let protocol = active.get(&packet.job.kind);
            crate::station_window::WindowPacket {
                steps: packet
                    .steps
                    .into_iter()
                    .map(|s| crate::station_window::StampedStep {
                        step: crate::agent_spec::resolved(&s.step, protocol),
                        ready_at: s.ready_at,
                    })
                    .collect(),
                job: packet.job,
            }
        })
        .collect()
}

/// `GET /api/stations/flow` — arrivals and departures per station over
/// a window.
///
/// The default `step-window-v1` basis counts each station over the step
/// record, as the IT map's marshalling read counts it (design 7b8ed079,
/// "Station membership is read, not replayed", car 1): an ARRIVAL is a
/// step that became ready at the station in the window, a DEPARTURE a
/// step that completed there in it (`completed_at`), and what departed
/// is what was served — a skipped step records no completion and is
/// neither. The station's own predicate decides "there", bound to the
/// caller, over the caller's policy-scoped packets. Until this car the
/// default replayed the audit log into packet-membership spans, a
/// projection stale since 2026-10-30 (b3572a26) whose every station read
/// unavailable; nothing here reads it now.
///
/// The window is the clock port's, the instant the step stamps are
/// written on (`completed_at` and the ready flip both take the clock
/// port's `now`) and the one the map counts against.
///
/// `?basis=step-events` retains the distinct ready/done obligation cube
/// as a separate unit. Its global aggregate is unavailable to a narrow
/// grant; it never supplies fallback counts.
pub(super) async fn stations_flow<R: JobsRepository + 'static, B: EventBus + 'static>(
    State(state): State<Arc<JobsApiState<R, B>>>,
    CurrentUser(user): CurrentUser,
    axum::extract::Query(q): axum::extract::Query<FlowQuery>,
) -> Response {
    let reg = match stations_or_503(&state) {
        Ok(r) => r,
        Err(r) => return r,
    };
    // Same read gate as every other station surface: a caller who may
    // read no packets is refused (403), never shown a calm network.
    let predicate = match readable_predicate(&state, &user).await {
        Ok(predicate) => predicate,
        Err(response) => return response,
    };
    let stations = match effective_stations(&state, reg).await {
        Ok(s) => s,
        Err(r) => return r,
    };

    let window_hours = q
        .window_hours
        .unwrap_or(DEFAULT_FLOW_WINDOW_HOURS)
        .clamp(1, MAX_FLOW_WINDOW_HOURS);
    if q.basis
        .as_deref()
        .is_some_and(|basis| basis != STEP_WINDOW_BASIS && basis != "step-events")
    {
        return (
            StatusCode::BAD_REQUEST,
            format!("flow basis must be {STEP_WINDOW_BASIS} or step-events"),
        )
            .into_response();
    }
    let scope = JobScope::from_predicate(&user, &predicate);
    if q.basis.as_deref() != Some("step-events") {
        return step_window_flow(&state, &user, &stations, scope, window_hours)
            .await
            .into_response();
    }
    // `wall_now()`, not `state.clock` — the sanctioned wall reading,
    // and the only correct one for THIS basis. The clock port answers
    // the BUSINESS question ("what day is it in this company's
    // timeline"), which on a demo deployment is the simulator's; the
    // cube counts the log's events, so its window has to be the same
    // instant the log's `created_at` is written on.
    let as_of = boss_clock_client::wall_now();
    let since = as_of - chrono::Duration::hours(window_hours);
    if scope != JobScope::All {
        let rows=stations.iter().map(|spec|serde_json::json!({"station":spec.name,"kind":spec.kind,
            "basis":"unavailable","arrived":null,"served":null,"net":null,
            "unavailable_reason":format!("the distinct-step cube is global; use {STEP_WINDOW_BASIS} for this authorized scope")})).collect::<Vec<_>>();
        return Json(
            serde_json::json!({"basis":"step-events","total":rows.len(),"data":rows,
            "window_hours":window_hours,"since":since,"as_of":as_of,"scope":scope}),
        )
        .into_response();
    }
    let cells = match state.jobs.step_flow_cube(since).await {
        Ok(c) => c,
        Err(e) => return (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    };

    let rows: Vec<serde_json::Value> = stations
        .iter()
        .map(
            |spec| match crate::station_flow::station_flow(&spec.predicate, &cells) {
                Ok(flow) => serde_json::json!({
                    "station": spec.name,
                    "kind": spec.kind,
                    "basis": "step-events",
                    "arrived": flow.arrived,
                    "served": flow.served,
                    "net": flow.net(),
                    "unavailable_reason": serde_json::Value::Null,
                }),
                Err(blind) => serde_json::json!({
                    "station": spec.name,
                    "kind": spec.kind,
                    "basis": "unavailable",
                    "arrived": serde_json::Value::Null,
                    "served": serde_json::Value::Null,
                    "net": serde_json::Value::Null,
                    "unavailable_reason": blind.reason(),
                }),
            },
        )
        .collect();
    let total = rows.len();
    Json(serde_json::json!({
        "basis":"step-events",
        "data": rows,
        "total": total,
        "window_hours": window_hours,
        "since": since,
        "as_of": as_of,
    }))
    .into_response()
}

/// The `step-window-v1` answer: ONE read of the packets the window
/// touched, every station counted over it with its predicate bound to
/// the caller. A failed read is that failure on every station, never a
/// zero; a per-actor station this caller cannot bind (no identified
/// actor) says so rather than counting the unbound row.
async fn step_window_flow<R: JobsRepository + 'static, B: EventBus + 'static>(
    state: &Arc<JobsApiState<R, B>>,
    user: &boss_policy_client::User,
    stations: &[StationSpec],
    scope: JobScope,
    window_hours: i64,
) -> Json<serde_json::Value> {
    let as_of = boss_clock_client::now_from(&state.clock).await;
    let since = as_of - chrono::Duration::hours(window_hours);
    let active = active_rows(state).await;
    let packets = state
        .jobs
        .station_window_packets(since, &scope)
        .await
        .map(|packets| resolved_window(packets, &active))
        .map_err(|e| format!("the step-window read failed: {e}"));
    let in_window = |t: chrono::DateTime<chrono::Utc>| t >= since && t <= as_of;
    let unavailable = |spec: &StationSpec, why: &str| {
        serde_json::json!({"station": spec.name, "kind": spec.kind, "basis": "unavailable",
            "arrived": null, "departed": null, "served": null, "net": null,
            "caller_binding": null, "unavailable_reason": why})
    };
    let rows: Vec<serde_json::Value> = stations
        .iter()
        .map(|spec| {
            let packets = match &packets {
                Ok(packets) => packets,
                Err(why) => return unavailable(spec, why),
            };
            let Some(bound) = spec.bind_self(self_id(user)) else {
                return unavailable(
                    spec,
                    "a per-actor station, and this caller is not an identified actor, so it has no queue here",
                );
            };
            let flow =
                crate::station_window::station_window_flow(&bound.predicate, packets, in_window);
            serde_json::json!({
                "station": spec.name,
                "kind": spec.kind,
                "basis": STEP_WINDOW_BASIS,
                "arrived": flow.arrived,
                "departed": flow.departed,
                // What departed is what was served: a departure is a
                // completion (module header of `station_window`).
                "served": flow.departed,
                "net": i128::from(flow.arrived) - i128::from(flow.departed),
                "caller_binding": spec.predicate.binds_self().then(|| user.id.clone()),
                "unavailable_reason": serde_json::Value::Null,
            })
        })
        .collect();
    Json(serde_json::json!({
        "basis": STEP_WINDOW_BASIS,
        "total": rows.len(),
        "data": rows,
        "window_hours": window_hours,
        "since": since,
        "as_of": as_of,
        "scope": scope,
        "caller": user.id,
    }))
}

/// The addresses a step may declare and still stand at this station:
/// the id and the name of every station in its subtree (5a2556fe).
/// `None` for a station off the tree, whose membership is its
/// predicate alone.
pub(super) struct TreeAddresses {
    members: std::collections::BTreeSet<String>,
    rows: Vec<StationSpec>,
}

pub(super) async fn tree_addresses(
    reg: &Arc<dyn StationRegistry>,
    spec: &StationSpec,
) -> Result<Option<TreeAddresses>, Response> {
    if spec.id.is_none() {
        return Ok(None);
    }
    let rows = reg.list_active().await.map_err(station_err_response)?;
    let nodes = crate::station_tree::subtree(&rows, &spec.name)
        .map_err(|e| (StatusCode::SERVICE_UNAVAILABLE, e).into_response())?;
    Ok(Some(TreeAddresses {
        members: nodes
            .into_iter()
            .flat_map(|n| {
                n.station
                    .id
                    .into_iter()
                    .chain(std::iter::once(n.station.name))
            })
            .collect(),
        rows,
    }))
}

/// IS THIS PACKET AT THIS STATION — the one answer the queue read and
/// the claim door both give (backlog 1e7f5372, CLAUDE.md 9a). A packet
/// stands at a station when the station's predicate matches it, OR,
/// for a tree station, when its current step is addressed inside the
/// station's subtree ([`crate::station_queue::addressed_here`]).
///
/// It is a function because it was two. The queue counted addressed
/// work and the claim door asked the predicate alone, so a station
/// refused "packet is not at this station" for a packet its own queue
/// listed, and the root — whose predicate matches nothing — refused
/// every claim that named it (measured by builder run 4d5a0c24, design
/// 6e42c6de). `dispatch --next` stopped sending `?station=` to get past
/// it (car 0a9e11d0).
///
/// `spec` is BOUND (the caller's `@me`); `steps` are resolved against
/// the active row as the predicate reads them. `protocols` caches each
/// pinned Workflow row across calls, read only for a packet the
/// predicate refuses — so a predicate member of a retired kind still
/// answers. A row that cannot be read is a 503 naming the packet,
/// never a quiet "not here".
pub(super) async fn at_station<R: JobsRepository, B: EventBus>(
    state: &JobsApiState<R, B>,
    spec: &StationSpec,
    inside: Option<&TreeAddresses>,
    job: &Job,
    steps: &[boss_core::job::Step],
    protocols: &mut BTreeMap<(String, i32), crate::registry::WorkflowSpec>,
) -> Result<bool, Response> {
    if spec.predicate.matches(job, steps) {
        return Ok(true);
    }
    let Some(inside) = inside else {
        return Ok(false);
    };
    let key = (job.kind.clone(), job.workflow_version);
    if let std::collections::btree_map::Entry::Vacant(entry) = protocols.entry(key.clone()) {
        let Some(registry) = &state.kind_registry else {
            return Err((
                StatusCode::SERVICE_UNAVAILABLE,
                "station addresses require the workflow registry",
            )
                .into_response());
        };
        let protocol = registry
            .get_version(&job.kind, job.workflow_version)
            .await
            .map_err(|e| {
                (
                    StatusCode::SERVICE_UNAVAILABLE,
                    format!(
                        "cannot locate packet {} at station {}: {e}",
                        job.id, spec.name
                    ),
                )
                    .into_response()
            })?;
        entry.insert(protocol);
    }
    Ok(
        crate::station_queue::addressed_here(&inside.members, &protocols[&key], steps)
            || steps.iter().any(|step| {
                matches!(
                    step.status,
                    boss_core::job::StepStatus::Ready | boss_core::job::StepStatus::Active
                ) && crate::station_tree::claim_station(
                    &inside.rows,
                    Some(&protocols[&key]),
                    job,
                    step,
                )
                .is_some_and(|station| {
                    station
                        .id
                        .as_ref()
                        .is_some_and(|id| inside.members.contains(id))
                })
            }),
    )
}

/// `GET /api/stations/{name}/queue` — the station's evaluated,
/// ordered queue: derived membership (the predicate, bound to the
/// caller, over their policy-scoped packets), data-declared
/// discipline, and the complete membership count in the envelope.
pub(super) async fn station_queue<R: JobsRepository + 'static, B: EventBus + 'static>(
    State(state): State<Arc<JobsApiState<R, B>>>,
    CurrentUser(user): CurrentUser,
    Path(name): Path<String>,
) -> Response {
    let reg = match stations_or_503(&state) {
        Ok(r) => r,
        Err(r) => return r,
    };
    // Authored first, then the projection. A station that /api/stations
    // lists must have a queue that answers, or the registry advertises
    // doors that open onto nothing — so the lookup consults exactly the
    // same two sources the listing does, in the same order, through the
    // one resolver the claim door also uses.
    let row = match station_by_name(&state, reg, &name).await {
        Ok(s) => s,
        Err(r) => return r,
    };
    let today = boss_clock_client::now_from(&state.clock).await.date_naive();

    // Bind the self placeholder ONCE, here, before any packet is
    // compared — a per-actor station is one registry row whose queue
    // depends on who is asking. A caller with no identity is REFUSED
    // (401): there is nobody to bind `@me` to, so there is no queue to
    // answer with. It used to get the station's envelope with an empty
    // `data` and a 200 — the same bytes as "you have filed nothing",
    // a claim about the caller the server cannot make (backlog
    // c11e9d3c). It still never falls back to the unbound row.
    let Some(spec) = row.bind_self(self_id(&user)) else {
        return (
            StatusCode::UNAUTHORIZED,
            format!(
                "station {name} is per-actor (@me) and this request names no actor to bind it to"
            ),
        )
            .into_response();
    };

    // One policy path with /api/jobs: scope predicate → JobScope,
    // pushed into the adapter query — and a scope that admits nothing
    // is refused, not evaluated into an empty queue.
    let predicate = match readable_predicate(&state, &user).await {
        Ok(p) => p,
        Err(r) => return r,
    };
    let scope = JobScope::from_predicate(&user, &predicate);

    // Tree stations hold addressed work as well as their declared
    // predicate queue. A container's legacy tag never described its
    // protocol steps (5a2556fe), so it cannot narrow this read in SQL.
    let inside = match tree_addresses(reg, &spec).await {
        Ok(inside) => inside,
        Err(r) => return r,
    };

    // EVERY page of every read, to the total: this read once took one
    // MAX_LIMIT page and dropped the total, so past 1000 candidates the
    // queue answered a smaller question in silence (backlog f71d1e81).
    // A packet two reads both hold is evaluated once.
    let mut seen = std::collections::BTreeSet::new();
    let mut jobs = Vec::new();
    for filter in universe_reads(&spec, inside.is_some(), today, &scope) {
        match crate::list_every::list_every(state.jobs.as_ref(), &filter, MAX_LIMIT).await {
            Ok(page) => jobs.extend(
                page.into_iter()
                    .filter(|job| seen.insert(job.id.to_string())),
            ),
            Err(e) => return (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
        }
    }

    // Steps are fetched when the predicate reads step state, or when
    // the station's lens declares it needs them to draw the queue —
    // "where has this packet got to" is a fact about its steps, and a
    // surface without them can only render a list.
    let needs_steps = inside.is_some()
        || spec.predicate.needs_steps()
        || spec.lens.as_ref().is_some_and(|l| l.with_steps);
    // A step clause may read the agent block, which a packet admitted
    // before its kind declared one does not carry: resolve it against
    // the ACTIVE row before the predicate reads it (backlog 51aef4dd —
    // 47 page-audit packets sat three days absent from the agent
    // station). One registry read per queue, only when steps are read.
    let active = if needs_steps {
        active_rows(&state).await
    } else {
        BTreeMap::new()
    };
    let mut packets = Vec::with_capacity(jobs.len());
    // The steps AS RECORDED, for a lens that draws them: the resolution
    // decides membership, it is never shown as what the packet holds.
    let mut recorded: BTreeMap<String, Vec<boss_core::job::Step>> = BTreeMap::new();
    let mut protocols = BTreeMap::new();
    for job in jobs {
        let steps = if needs_steps {
            // A failed read is NOT an empty step list. Taken as one
            // (`unwrap_or_default()`, until 2026-09-23), the step clause
            // could not match, so the packet silently left the queue and
            // the 200 reported one member fewer (backlog c11e9d3c). The
            // whole answer fails instead, naming the packet.
            let steps = match state.jobs.list_steps(&job.id).await {
                Ok(s) => s,
                Err(e) => {
                    return (
                        StatusCode::INTERNAL_SERVER_ERROR,
                        format!(
                            "cannot evaluate station {name} — steps of packet {} unreadable: {e}",
                            job.id
                        ),
                    )
                        .into_response();
                }
            };
            let resolved = crate::agent_spec::resolved_steps(&steps, active.get(&job.kind));
            recorded.insert(job.id.to_string(), steps);
            resolved
        } else {
            Vec::new()
        };
        // The claim door asks this same function (1e7f5372).
        match at_station(&state, &spec, inside.as_ref(), &job, &steps, &mut protocols).await {
            Ok(true) => packets.push((job, steps)),
            Ok(false) => {}
            Err(r) => return r,
        }
    }

    // Membership was judged above; retain the shared window, discipline
    // and response construction without filtering twice.
    let mut membership = spec.clone();
    membership.predicate = Default::default();
    let mut queue = evaluate_station(&membership, packets, today);
    for (id, steps) in queue.steps.iter_mut() {
        if let Some(as_recorded) = recorded.remove(id) {
            *steps = as_recorded;
        }
    }
    Json(queue).into_response()
}

// ---------------------------------------------------------------------------
// Authoring — the runtime write path
// ---------------------------------------------------------------------------
//
// Stations are the substrate's routing table, and David's ratified
// answer (2026-08-13) was that they must be editable at run time:
// "stations need to be editable at run time. They should be data in a
// registry." The registry and the port already existed; without these
// routes the only way to redraw a queue was a SQL seed and a deploy,
// which is precisely the leak the three-layer reading calls out — a
// protocol that cannot be replaced without a deploy has leaked into
// the substrate.

/// `POST /api/stations` — append a draft version. Version numbering
/// is the registry's business (max+1); a draft is work in progress and
/// is deliberately NOT linted, matching the Workflow registry.
pub(super) async fn create_station<R: JobsRepository + 'static, B: EventBus + 'static>(
    State(state): State<Arc<JobsApiState<R, B>>>,
    CurrentUser(user): CurrentUser,
    Json(spec): Json<crate::stations::StationSpec>,
) -> Response {
    let reg = match stations_or_503(&state) {
        Ok(r) => r,
        Err(r) => return r,
    };
    if let Err(r) = station_policy_check(&state, &user, controls::CREATE_WORKFLOW).await {
        return r;
    }
    let (actor, now) = super::kinds::write_stamp(&state, &user).await;
    match reg.create_draft(spec, &actor, now).await {
        Ok(stored) => (StatusCode::CREATED, Json(stored)).into_response(),
        Err(e) => station_err_response(e),
    }
}

/// `POST /api/stations/_validate` — author-time dry run. Lints a spec
/// WITHOUT persisting, calling the same `stations::gate_publish` the
/// publish path enforces, so an editor showing "no problems"
/// publishes cleanly and a refused publish shows the same list.
///
/// Always 200: lint failures are data, not an HTTP error. The 422 on
/// publish and this 200 carry the same body.
pub(super) async fn validate_station<R: JobsRepository + 'static, B: EventBus + 'static>(
    State(state): State<Arc<JobsApiState<R, B>>>,
    CurrentUser(user): CurrentUser,
    Json(spec): Json<crate::stations::StationSpec>,
) -> Response {
    // Gated like create — the dry run is an authoring affordance.
    if let Err(r) = station_policy_check(&state, &user, controls::CREATE_WORKFLOW).await {
        return r;
    }
    let reg = match stations_or_503(&state) {
        Ok(reg) => reg,
        Err(response) => return response,
    };
    // The edges are judged against the live rows, as publish judges them
    // (backlog 5b80b9f4), so a clean dry run still publishes cleanly.
    let active = match reg.list_active().await {
        Ok(rows) => rows,
        Err(e) => return station_err_response(e),
    };
    // And the automaton against the automations registry, as publish
    // judges it (backlog d0cf2cdc).
    let registered = match reg.registered_automatons(&spec).await {
        Ok(ids) => ids,
        Err(e) => return station_err_response(e),
    };
    let problems = crate::stations::gate_publish(&spec, &active, &registered)
        .err()
        .unwrap_or_default();
    (StatusCode::OK, Json(lint_result_json(&problems))).into_response()
}

/// `GET /api/stations/{name}/versions` — every version of one name,
/// oldest first, drafts and retired included. The audit view: what
/// this queue used to be, and what is staged to replace it.
pub(super) async fn list_station_versions<R: JobsRepository + 'static, B: EventBus + 'static>(
    State(state): State<Arc<JobsApiState<R, B>>>,
    CurrentUser(user): CurrentUser,
    Path(name): Path<String>,
) -> Response {
    let reg = match stations_or_503(&state) {
        Ok(r) => r,
        Err(r) => return r,
    };
    if let Err(r) = station_policy_check(&state, &user, controls::READ_WORKFLOW).await {
        return r;
    }
    // A reader is served every version that was published and no draft
    // (backlog 1a4a4d03; the rule is `kinds::may_read_drafts`).
    let drafts = match super::kinds::may_read_drafts(&state, &user).await {
        Ok(d) => d,
        Err(r) => return r,
    };
    match reg.list_versions(&name).await {
        Ok(rows) => Json(
            rows.into_iter()
                .filter(|r| drafts || r.status != crate::registry::WorkflowStatus::Draft)
                .collect::<Vec<_>>(),
        )
        .into_response(),
        Err(e) => station_err_response(e),
    }
}

/// `GET /api/stations/{name}/versions/{version}` — one historical row.
pub(super) async fn get_station_version<R: JobsRepository + 'static, B: EventBus + 'static>(
    State(state): State<Arc<JobsApiState<R, B>>>,
    CurrentUser(user): CurrentUser,
    Path((name, version)): Path<(String, i32)>,
) -> Response {
    let reg = match stations_or_503(&state) {
        Ok(r) => r,
        Err(r) => return r,
    };
    if let Err(r) = station_policy_check(&state, &user, controls::READ_WORKFLOW).await {
        return r;
    }
    match reg.get_version(&name, version).await {
        Ok(row) if row.status == crate::registry::WorkflowStatus::Draft => {
            match super::kinds::may_read_drafts(&state, &user).await {
                Ok(true) => Json(row).into_response(),
                Ok(false) => {
                    super::kinds::draft_refusal(&format!("station {name} v{version}"), &user)
                }
                Err(r) => r,
            }
        }
        Ok(row) => Json(row).into_response(),
        Err(e) => station_err_response(e),
    }
}

/// `POST /api/stations/{name}/publish` — promote the latest draft to
/// ACTIVE, retiring the incumbent.
///
/// The viability gate (`stations::gate_publish`: the row's lint, its
/// edges against the active rows and its automaton against the
/// automations registry) runs inside `StationRegistry::publish`,
/// against the draft row the transaction actually promotes — not against
/// a copy re-read here, which could race a concurrent author. An
/// unviable draft comes back as `StationError::Unviable` and leaves as
/// 422 + the problem list.
pub(super) async fn publish_station<R: JobsRepository + 'static, B: EventBus + 'static>(
    State(state): State<Arc<JobsApiState<R, B>>>,
    CurrentUser(user): CurrentUser,
    Path(name): Path<String>,
) -> Response {
    let reg = match stations_or_503(&state) {
        Ok(r) => r,
        Err(r) => return r,
    };
    if let Err(r) = station_policy_check(&state, &user, controls::UPDATE_WORKFLOW).await {
        return r;
    }
    let (actor, now) = super::kinds::write_stamp(&state, &user).await;
    match reg.publish(&name, &actor, now).await {
        Ok(spec) => Json(spec).into_response(),
        Err(e) => station_err_response(e),
    }
}

/// `POST /api/stations/{name}/retire` — close the station. Idempotent:
/// retiring an already-retired name is a 204 that records nothing.
pub(super) async fn retire_station<R: JobsRepository + 'static, B: EventBus + 'static>(
    State(state): State<Arc<JobsApiState<R, B>>>,
    CurrentUser(user): CurrentUser,
    Path(name): Path<String>,
) -> Response {
    let reg = match stations_or_503(&state) {
        Ok(r) => r,
        Err(r) => return r,
    };
    if let Err(r) = station_policy_check(&state, &user, controls::UPDATE_WORKFLOW).await {
        return r;
    }
    let (actor, now) = super::kinds::write_stamp(&state, &user).await;
    match reg.retire(&name, &actor, now).await {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(e) => station_err_response(e),
    }
}

/// The reads that draw a station queue's evaluation universe: every
/// packet that could be a member, and as few others as the adapter can
/// be told to leave out. Membership is still judged afterwards, in
/// Rust; these only decide what gets paged and step-read.
///
/// The PREDICATE's read: in-flight packets, because stations hold
/// in-flight traffic. A station declaring a terminal window also wants
/// recently-departed packets, so its status filter opens up to "live OR
/// closed on/after the window's first day" — the adapter's
/// `closed_since`, the same rule `in_window` judges again in
/// `evaluate_station`, so the query only narrows what that keeps.
///
/// Kind and the bound `metadata_equals` push down into SQL so the pages
/// are drawn from the packets that can actually be members. Without the
/// metadata push-down, a per-actor station on a busy install would page
/// through every packet of the whole company to find the caller's own.
///
/// A TREE station also holds the work addressed to it (5a2556fe), of any
/// kind, so it reads a second, wider set: every OPEN packet in scope.
/// Open only, because `addressed_here` matches only ready or active
/// steps, and a departed packet holds none. Until backlog d650c8e6 that
/// widening replaced the predicate's read outright — no kind, no
/// metadata, and with a window no status either — so design-decided
/// paged all 149,049 packets ever filed and step-read each one: 297 s
/// for 145 rows, measured on 2026-11-10. Without a window the open read
/// already holds every open predicate member, so it is the only read.
fn universe_reads(
    spec: &StationSpec,
    tree: bool,
    today: chrono::NaiveDate,
    scope: &JobScope,
) -> Vec<JobFilter> {
    // Past the calendar's edge there is no first day to name, and the
    // window is still judged in `in_window` — the read is only wider.
    let closed_since = spec
        .terminal_window_days
        .and_then(|days| today.checked_sub_days(chrono::Days::new(u64::from(days))));
    let predicate = JobFilter {
        kind: spec.predicate.kind.clone(),
        status: spec
            .terminal_window_days
            .is_none()
            .then_some(JobStatus::Open),
        closed_since,
        metadata_contains: metadata_containment(&spec.predicate),
        scope: scope.clone(),
        ..Default::default()
    };
    if !tree {
        return vec![predicate];
    }
    let addressed = JobFilter {
        status: Some(JobStatus::Open),
        scope: scope.clone(),
        ..Default::default()
    };
    if spec.terminal_window_days.is_none() {
        return vec![addressed];
    }
    vec![predicate, addressed]
}

/// The `metadata_equals` clause of an already-BOUND predicate as a
/// containment document the adapter can push into SQL. `None` when the
/// predicate declares none.
///
/// Only ever built from a bound predicate: pushing an unbound `"@me"`
/// down would ask the database for packets that literally wrote the
/// placeholder.
fn metadata_containment(
    predicate: &crate::station_queue::StationPredicate,
) -> Option<serde_json::Value> {
    if predicate.metadata_equals.is_empty() {
        return None;
    }
    Some(serde_json::Value::Object(
        predicate
            .metadata_equals
            .iter()
            .map(|(k, v)| (k.clone(), serde_json::Value::String(v.clone())))
            .collect(),
    ))
}

#[cfg(test)]
mod universe_reads_tests {
    //! What a station queue reads, pinned without a database: the
    //! measured cost of getting it wrong was one queue read paging all
    //! 149,049 packets ever filed and reading their steps one packet at
    //! a time — design-decided took 297 s on 2026-11-10 (backlog
    //! d650c8e6), because a tree station dropped its push-down.
    use super::*;
    use crate::station_queue::{StationPredicate, StepMatch};
    use crate::stations::StationKind;

    fn today() -> chrono::NaiveDate {
        chrono::NaiveDate::from_ymd_opt(2026, 11, 10).unwrap()
    }

    fn tree(mut spec: StationSpec) -> StationSpec {
        spec.id = Some("ebbae18b-5bee-468e-8be7-53aa54a91e87".into());
        // Any parent makes it a tree row: engineering's design station,
        // design-decided's since the executive desk dissolved (878fd1e7).
        spec.parent = Some("cc856b32-3667-4e70-b6cd-667f6fe89007".into());
        spec
    }

    /// infra/platform/stations/design-decided.toml, in the words that
    /// decide its read: a kind, a step clause, a seven-day window.
    fn design_decided() -> StationSpec {
        let mut s = StationSpec::draft(
            "design-decided",
            "Decided designs",
            StationKind::Group,
            StationPredicate {
                kind: Some("design-doc".into()),
                step: Some(StepMatch {
                    slug: Some("review".into()),
                    status_in: vec![boss_core::job::StepStatus::Completed],
                    ..Default::default()
                }),
                ..Default::default()
            },
            chrono::DateTime::UNIX_EPOCH,
        );
        s.terminal_window_days = Some(7);
        s
    }

    /// infra/platform/stations/my-watchlist.toml, already BOUND to the
    /// caller — the handler binds `@me` before it plans a read.
    fn my_watchlist_bound_to(actor: &str) -> StationSpec {
        let mut s = StationSpec::draft(
            "my-watchlist",
            "My watchlist",
            StationKind::Actor,
            StationPredicate {
                metadata_equals: BTreeMap::from([("submitted_by".into(), actor.into())]),
                metadata_absent: vec!["watchlist_dismissed".into()],
                ..Default::default()
            },
            chrono::DateTime::UNIX_EPOCH,
        );
        s.terminal_window_days = Some(14);
        s
    }

    /// A read that names no kind, no metadata and no live-only status
    /// pages the whole company's history.
    fn unbounded(read: &JobFilter) -> bool {
        read.kind.is_none()
            && read.metadata_contains.is_none()
            && read.status != Some(JobStatus::Open)
            && read.closed_since.is_none()
    }

    #[test]
    fn a_terminal_window_tree_station_keeps_its_kind_and_window_push_down() {
        let reads = universe_reads(&tree(design_decided()), true, today(), &JobScope::default());
        assert!(
            !reads.iter().any(unbounded),
            "no read of a tree station may span every packet ever filed: {reads:?}"
        );
        let predicate = reads
            .iter()
            .find(|r| r.kind.as_deref() == Some("design-doc"))
            .unwrap_or_else(|| panic!("the predicate's read keeps its kind: {reads:?}"));
        assert_eq!(
            predicate.closed_since,
            chrono::NaiveDate::from_ymd_opt(2026, 11, 3),
            "and its seven-day window, in the query rather than after it"
        );
        // The step-addressed half reads every kind, and only what is
        // open: `addressed_here` matches only ready or active steps,
        // which a departed packet does not hold.
        let addressed = reads
            .iter()
            .find(|r| r.kind.is_none())
            .unwrap_or_else(|| panic!("a tree station reads its addressed work: {reads:?}"));
        assert_eq!(addressed.status, Some(JobStatus::Open));
        assert_eq!(addressed.closed_since, None);
        assert_eq!(reads.len(), 2, "{reads:?}");
    }

    #[test]
    fn a_per_actor_tree_station_keeps_its_metadata_push_down() {
        let reads = universe_reads(
            &tree(my_watchlist_bound_to("emp-r")),
            true,
            today(),
            &JobScope::default(),
        );
        assert!(!reads.iter().any(unbounded), "{reads:?}");
        let predicate = reads
            .iter()
            .find(|r| r.metadata_contains.is_some())
            .unwrap_or_else(|| panic!("the caller's own packets are found in SQL: {reads:?}"));
        assert_eq!(
            predicate.metadata_contains,
            Some(serde_json::json!({"submitted_by": "emp-r"}))
        );
        assert_eq!(
            predicate.closed_since,
            chrono::NaiveDate::from_ymd_opt(2026, 10, 27)
        );
        let addressed = reads
            .iter()
            .find(|r| r.metadata_contains.is_none())
            .unwrap_or_else(|| panic!("{reads:?}"));
        assert_eq!(addressed.status, Some(JobStatus::Open));
    }

    #[test]
    fn a_tree_station_without_a_window_reads_open_packets_once() {
        // engineering's shape: its addressed work is every open packet
        // of every kind, which already holds the predicate's open
        // members, so a second read would only be paid for twice.
        let engineering = tree(StationSpec::draft(
            "engineering",
            "Engineering",
            StationKind::Group,
            StationPredicate {
                tags_any: vec!["station:engineering".into()],
                ..Default::default()
            },
            chrono::DateTime::UNIX_EPOCH,
        ));
        let reads = universe_reads(&engineering, true, today(), &JobScope::default());
        assert_eq!(reads.len(), 1, "{reads:?}");
        assert_eq!(reads[0].status, Some(JobStatus::Open));
        assert_eq!(reads[0].kind, None);
        assert_eq!(reads[0].closed_since, None);
    }

    #[test]
    fn a_station_outside_the_tree_reads_its_predicate_alone() {
        let reads = universe_reads(&design_decided(), false, today(), &JobScope::default());
        assert_eq!(reads.len(), 1, "{reads:?}");
        assert_eq!(reads[0].kind.as_deref(), Some("design-doc"));
        assert_eq!(
            reads[0].closed_since,
            chrono::NaiveDate::from_ymd_opt(2026, 11, 3)
        );
    }

    #[test]
    fn every_read_carries_the_callers_scope() {
        let scope = JobScope::OwnerIs("emp-r".into());
        for read in universe_reads(&tree(design_decided()), true, today(), &scope) {
            assert_eq!(read.scope, scope);
        }
    }
}
