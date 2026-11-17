//! Axum routes for the agents registry: the roster's list and its one
//! write, the tenant batch (backlog f56155f0, 2026-09-17). Until this
//! door the registry's rows arrived by migration only, so the company's
//! own agent was declared nowhere the product read.
//!
//! **Reads admit `crate::trust::can_read`** — operator machinery and
//! the auditor tier, the recorded-probe reader (`boss-sor-read
//! /api/agents` is how the car for this surface is proved). **Writes
//! admit `crate::trust::is_trusted`** — operator tier, the tier
//! `boss tenant publish` signs with. A request with no identity header
//! is refused on both (e84de48e).
//!
//! `POST /api/agents/batch[?mode=insert-if-absent|take]` takes a bare
//! JSON array of `AgentInput` (the classes and locations batch shape),
//! validates every row with the same `validate_agent` the TOML loader
//! ran, refuses the whole batch (422, deterministic) naming the row,
//! and answers what it did — rows inserted, rows the registry held and
//! KEPT with the declared fields they differ on named (the default;
//! design e187198f: the instance is the truth), rows a `take` UPDATED
//! with each change named (field, from, to; the rule of 09887242, now
//! only by decision), rows already as declared — so what a publish
//! moved, and what it did not, is read in the publish line, never in
//! silence. This batch IS the tenant's update door: the declaration is
//! the whole row, so a per-row PUT would only repeat it.
//!
//! A declared `role` is a Class code under `(employee, role)` and a
//! declared `department` a row of the departments registry — the same
//! rows `boss-people` checks an employee's against — and this door
//! checks both BEFORE the batch lands (backlog ab192a9f): an
//! undeclared code refuses the whole batch by agent, attribute and
//! code. The department was an `(employee, department)` Class until
//! backlog c87e3d6d (2026-09-27) collapsed that second list onto the
//! registry. The check lives here, not in the adapters, because this
//! is the one write path and the code is data the database cannot
//! vouch for (a CHECK copied from the registry would drift from it).
//! `classes: None` / `departments: None` skip them, the people
//! adapter's posture for in-memory and test paths; the service binary
//! always wires both.
//!
//! A declared `model_class` is the `<class>` of an active
//! `robots:<class>` pool (factory taxonomy T2d, backlog 06d8988b):
//! what a `model:<class>` step's claim reads to admit a robot. This door
//! refuses a class no active pool declares, by agent, class and pool.
//! `pools: None` does NOT skip it — a declared class with no registry to
//! ask is refused (503), because the claim would read the recorded class
//! as a fact somebody checked. A row that records NO class is not
//! refused — a robot whose model no pool declares has none to give —
//! but the answer names it (`no_model_class`, backlog 3317f4db), read
//! back after the batch: since the dispatcher nominates a `model:` step
//! by the same class, such a robot is one no `model:` step reaches.
//!
//! The class is also a fact a POOL states (backlog d088c248): since
//! af5dd547 a model-slot pool lists the model ids it meters, and a
//! `from-run` bill reads a run's class off that list. Two places can
//! disagree, so this door refuses a declared class a pool contradicts —
//! the agent's `default_model` listed by another class's pool, or the
//! class's own pool listing models that do not include it — naming the
//! agent, the model and each pool. A pin, not a collapse: deriving the
//! class from the pools and retiring the declared column is the
//! collapse, and it is a car of its own (the claim door, the dispatcher's
//! roster and both adapters read the column).
//!
//! A declared seat — a name in `stations`, the ordered stations an
//! executor serves (design 506e0a1c car 1, backlog 1a74c2bc) — is an
//! ACTIVE station of the station registry, checked here the way the
//! class is: an unknown or retired station refuses the whole batch by
//! agent and station, and `stations: None` refuses a declared seat (503)
//! rather than record one nobody checked.

use std::sync::Arc;

use axum::extract::{Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};

use boss_classes_client::ClassesClient;
use boss_core::primitives::ClassRef;
use boss_core::publish::ModeQuery;
use boss_policy_client::CurrentUser;

use crate::department::registry::DepartmentRegistry;
use crate::pools::PoolRegistry;
use crate::stations::StationRegistry;
use crate::trust::{can_read, is_trusted};

use super::port::{AgentsError, AgentsRegistry};
use super::types::{AgentInput, validate_agent};

pub struct AgentsApiState {
    pub registry: Arc<dyn AgentsRegistry>,
    /// The Class registry an agent's `role` is checked against. `None`
    /// skips the check (test and in-memory paths).
    pub classes: Option<Arc<dyn ClassesClient>>,
    /// The departments registry an agent's `department` is checked
    /// against — the rows `GET /api/departments` serves, the same ones
    /// an employee's department is validated against since backlog
    /// c87e3d6d (it was an `(employee, department)` Class until then).
    /// `None` skips the check (test and in-memory paths).
    pub departments: Option<Arc<dyn DepartmentRegistry>>,
    /// The pool registry an agent's `model_class` is checked against:
    /// the class must be the `<class>` of an active `robots:<class>`
    /// pool (T2d, backlog 06d8988b). Unlike the two above, `None` does
    /// not skip: a row DECLARING a class is refused, since a class no
    /// one could check is not a class recorded; a row declaring none
    /// needs no pool.
    pub pools: Option<Arc<dyn PoolRegistry>>,
    /// The station registry an agent's seats (`stations`) are checked
    /// against: each must name an ACTIVE station (design 506e0a1c car 1,
    /// backlog 1a74c2bc). Like `pools`, `None` does not skip: a row
    /// declaring a seat is refused, since a seat nobody checked is not a
    /// seat recorded; a row declaring none, or `[]`, needs no registry.
    pub stations: Option<Arc<dyn StationRegistry>>,
}

/// Why a declared department is refused: the code is not an active
/// row of the departments registry. `Ok(None)` when every declared
/// department is held; `Err` when the registry could not answer.
async fn undeclared_department(
    departments: &dyn DepartmentRegistry,
    rows: &[AgentInput],
) -> Result<Option<String>, String> {
    if rows.iter().all(|a| a.department.is_none()) {
        return Ok(None);
    }
    let held = departments
        .list()
        .await
        .map_err(|e| format!("departments registry: {e}"))?;
    Ok(rows.iter().find_map(|a| {
        let code = a.department.as_deref()?;
        (!held.iter().any(|d| d.code == code)).then(|| {
            format!(
                "agent {}: department `{code}` is not an active department in the departments \
                 registry (GET /api/departments) — declare it in seeds/departments.toml first, \
                 as for an employee's",
                a.id
            )
        })
    }))
}

/// Why a declared role is refused: the code is not an active Class
/// under `(employee, role)`. `Ok(None)` when every declared role is
/// held; `Err` when the registry could not answer, which is a
/// different fact from "not held" and is answered as one.
async fn undeclared_class(
    classes: &dyn ClassesClient,
    rows: &[AgentInput],
) -> Result<Option<String>, String> {
    for a in rows {
        let Some(code) = &a.role else { continue };
        // On its OWN axis (backlog ab1e6ff8): the employee drawer holds
        // role, status and employment_type codes side by side, and
        // `class_exists` accepted a department as a role.
        let held = classes
            .class_exists_on(&ClassRef::new("employee", code.as_str()), "role")
            .await
            .map_err(|e| format!("classes registry: {e}"))?;
        if !held {
            return Ok(Some(format!(
                "agent {}: role `{code}` is not an active Class in the registry \
                 (subject_kind employee, member_attribute role) — declare the Class \
                 in seeds/classes.json first, as for an employee's",
                a.id
            )));
        }
    }
    Ok(None)
}

/// Why a declared model class is refused: no active `robots:<class>`
/// pool declares it (T2d, backlog 06d8988b). `Ok(None)` when every
/// declared class is held; `Err` when the registry could not answer.
async fn undeclared_model_class(
    pools: &dyn PoolRegistry,
    rows: &[AgentInput],
) -> Result<Option<String>, String> {
    if rows.iter().all(|a| a.model_class.is_none()) {
        return Ok(None);
    }
    let active = pools
        .list_active()
        .await
        .map_err(|e| format!("pool registry: {e}"))?;
    for a in rows {
        let Some(class) = &a.model_class else {
            continue;
        };
        let pool = crate::resources::robot_pool(class);
        let Some(own) = active.iter().find(|p| p.name == pool) else {
            return Ok(Some(format!(
                "agent {}: model_class `{class}` names no active pool `{pool}` — a model class \
                 is the <class> of a declared robot pool (infra/platform/pools/), never a \
                 model id",
                a.id
            )));
        };
        if let Some(why) = contradicted_class(a, class, own, &active) {
            return Ok(Some(why));
        }
    }
    Ok(None)
}

/// Why a declared seat is refused: the station registry holds no ACTIVE
/// row of that name (design 506e0a1c car 1, backlog 1a74c2bc) — a draft
/// or a retired station queues nothing to pull. `Ok(None)` when every
/// declared seat is held; `Err` when the registry could not answer.
async fn unknown_station(
    stations: &dyn StationRegistry,
    rows: &[AgentInput],
) -> Result<Option<String>, String> {
    let active = stations
        .list_active()
        .await
        .map_err(|e| format!("station registry: {e}"))?;
    Ok(rows.iter().find_map(|a| {
        let unknown = a
            .stations
            .as_deref()?
            .iter()
            .find(|seat| !active.iter().any(|s| &s.name == *seat))?;
        Some(format!(
            "agent {}: station `{unknown}` is not an active station in the station registry \
             (GET /api/stations) — an executor is seated only where a station exists; declare \
             the station first (infra/platform/stations/)",
            a.id
        ))
    }))
}

/// Why a declared class is contradicted by the pools' `models` lists
/// (backlog d088c248), or `None` when they agree. The pools that list the
/// agent's `default_model` must be exactly the class's own; when none
/// lists it, the class's own pool must list nothing, since a pool that
/// lists its models and omits this one says this model is not its class.
/// Pure, over the active rows one read returned.
fn contradicted_class(
    a: &AgentInput,
    class: &str,
    own: &crate::pools::PoolSpec,
    active: &[crate::pools::PoolSpec],
) -> Option<String> {
    let listing: Vec<&str> = active
        .iter()
        .filter(|p| p.name.starts_with(crate::resources::ROBOTS_PREFIX))
        .filter(|p| p.models.contains(&a.default_model))
        .map(|p| p.name.as_str())
        .collect();
    let agrees = match listing.as_slice() {
        [] => own.models.is_empty(),
        [only] => *only == own.name,
        _ => false,
    };
    (!agrees).then(|| {
        format!(
            "agent {}: model_class `{class}` disagrees with the pool registry — default_model `{}` \
             is listed by {}, and `{}` meters {}; a model's class is the robot pool whose \
             `models` lists it (backlog d088c248), so declare that class or correct the pool row",
            a.id,
            a.default_model,
            if listing.is_empty() {
                "no active robot pool".to_string()
            } else {
                listing.join(" and ")
            },
            own.name,
            if own.models.is_empty() {
                "no models".to_string()
            } else {
                own.models.join(", ")
            },
        )
    })
}

pub fn router(state: AgentsApiState) -> Router {
    let shared = Arc::new(state);
    Router::new()
        .route("/api/agents", get(list))
        .route("/api/agents/automations", get(list_automations))
        .route("/api/agents/batch", post(publish))
        .with_state(shared)
}

/// `GET /api/agents/automations` — every writing automation's row and
/// the role it signs as (backlog ddf0773e, design abf9eeae car 1). The
/// same reader posture as the roster: `can_read`, refused without an
/// identity. Read-only by design — the rows land from the platform
/// bundle, and no door writes a role here.
async fn list_automations(
    State(state): State<Arc<AgentsApiState>>,
    CurrentUser(user): CurrentUser,
) -> Response {
    if !can_read(&user) {
        return StatusCode::FORBIDDEN.into_response();
    }
    match state.registry.list_automations().await {
        Ok(rows) => Json(serde_json::json!({ "data": rows, "total": rows.len() })).into_response(),
        Err(e) => err_response(e),
    }
}

fn err_response(e: AgentsError) -> Response {
    match e {
        AgentsError::Unpriced(m) => (
            StatusCode::UNPROCESSABLE_ENTITY,
            format!(
                "default_model `{m}` is not a rate-card model — the registry refuses a default \
                 it cannot price (spell it as the card does, e.g. opus-5[1m], not claude-…)"
            ),
        )
            .into_response(),
        AgentsError::Storage(m) => (StatusCode::INTERNAL_SERVER_ERROR, m).into_response(),
    }
}

async fn list(
    State(state): State<Arc<AgentsApiState>>,
    CurrentUser(user): CurrentUser,
) -> Response {
    if !can_read(&user) {
        return StatusCode::FORBIDDEN.into_response();
    }
    match state.registry.list().await {
        Ok(rows) => Json(serde_json::json!({ "data": rows, "total": rows.len() })).into_response(),
        Err(e) => err_response(e),
    }
}

async fn publish(
    State(state): State<Arc<AgentsApiState>>,
    CurrentUser(user): CurrentUser,
    Query(ModeQuery { mode }): Query<ModeQuery>,
    Json(rows): Json<Vec<AgentInput>>,
) -> Response {
    if !is_trusted(&user) {
        return StatusCode::FORBIDDEN.into_response();
    }
    if let Some(why) = rows.iter().find_map(|a| validate_agent(a).err()) {
        return (StatusCode::UNPROCESSABLE_ENTITY, why).into_response();
    }
    if let Some(classes) = &state.classes {
        match undeclared_class(classes.as_ref(), &rows).await {
            Ok(None) => {}
            Ok(Some(why)) => return (StatusCode::UNPROCESSABLE_ENTITY, why).into_response(),
            Err(why) => return (StatusCode::INTERNAL_SERVER_ERROR, why).into_response(),
        }
    }
    if let Some(departments) = &state.departments {
        match undeclared_department(departments.as_ref(), &rows).await {
            Ok(None) => {}
            Ok(Some(why)) => return (StatusCode::UNPROCESSABLE_ENTITY, why).into_response(),
            Err(why) => return (StatusCode::INTERNAL_SERVER_ERROR, why).into_response(),
        }
    }
    match (
        &state.pools,
        rows.iter().find_map(|a| a.model_class.as_ref()),
    ) {
        (_, None) => {}
        (None, Some(class)) => {
            return (
                StatusCode::SERVICE_UNAVAILABLE,
                format!(
                    "no pool registry is wired to this door, so model_class `{class}` cannot be \
                     checked against an active robot pool and is not recorded"
                ),
            )
                .into_response();
        }
        (Some(pools), Some(_)) => match undeclared_model_class(pools.as_ref(), &rows).await {
            Ok(None) => {}
            Ok(Some(why)) => return (StatusCode::UNPROCESSABLE_ENTITY, why).into_response(),
            Err(why) => return (StatusCode::INTERNAL_SERVER_ERROR, why).into_response(),
        },
    }
    // The seats, the same posture as the class: a seat with no registry
    // to ask is refused, not recorded (design 506e0a1c car 1).
    match (
        &state.stations,
        rows.iter()
            .find_map(|a| a.stations.as_deref().and_then(<[String]>::first)),
    ) {
        (_, None) => {}
        (None, Some(seat)) => {
            return (
                StatusCode::SERVICE_UNAVAILABLE,
                format!(
                    "no station registry is wired to this door, so the seat at station `{seat}` \
                     cannot be checked against an active station and is not recorded"
                ),
            )
                .into_response();
        }
        (Some(stations), Some(_)) => match unknown_station(stations.as_ref(), &rows).await {
            Ok(None) => {}
            Ok(Some(why)) => return (StatusCode::UNPROCESSABLE_ENTITY, why).into_response(),
            Err(why) => return (StatusCode::INTERNAL_SERVER_ERROR, why).into_response(),
        },
    }
    // Same envelope construction as the credentials door: the actor
    // the request signed with rides as `_actor` (and is named again
    // as `declared_by` on each inserted row's fact), the stamp is
    // wall-clock, and the source is `jobs` — this service is the one
    // recording. A trusted sibling with no identity is the platform.
    let actor = user
        .ambient_actor()
        .unwrap_or_else(|| boss_core::actor::ActorId::Automation("platform".into()));
    let stamp = boss_core::publisher::EventStamp::new("jobs", actor);
    let mut out = match state.registry.publish(&rows, mode, &stamp).await {
        Ok(out) => out,
        Err(e) => return err_response(e),
    };
    // WHICH OF THESE ROBOTS NO MODEL STEP REACHES (backlog 3317f4db):
    // each declared row that records no class once the batch is applied,
    // read back — under the default a kept row keeps the class it holds
    // whatever the declaration omits. The batch has landed, so a read
    // that fails says so beside what landed rather than answer a clean
    // list nobody read.
    match state.registry.list().await {
        Ok(held) => {
            out.no_model_class = rows
                .iter()
                .filter(|a| held.iter().any(|h| h.id == a.id && h.model_class.is_none()))
                .map(|a| a.id.clone())
                .collect::<std::collections::BTreeSet<_>>()
                .into_iter()
                .collect();
            Json(out).into_response()
        }
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            format!(
                "the batch landed ({}), but the registry could not be read back to name the \
                 robots that record no model class: {e}",
                out.summary()
            ),
        )
            .into_response(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::http::Request;
    use boss_policy_client::{AccessTier, User};
    use http_body_util::BodyExt;
    use serde_json::{Value, json};
    use tower::ServiceExt;

    use crate::agents::InMemoryAgents;
    use crate::department::declare::DepartmentInput;
    use crate::department::registry::InMemoryDepartments;

    fn header(id: &str, role: &str, tier: AccessTier) -> String {
        serde_json::to_string(&User {
            id: id.into(),
            role: role.into(),
            access_tier: tier,
            territory_account_ids: Vec::new(),
            direct_report_ids: Vec::new(),
            department: None,
        })
        .expect("the user header serializes")
    }

    fn seed() -> Option<String> {
        Some(header(
            "automation:tenant-seed",
            "platform-admin",
            AccessTier::Operator,
        ))
    }

    fn probe_reader() -> Option<String> {
        Some(header(
            "audit-readonly",
            "audit-readonly",
            AccessTier::Auditor,
        ))
    }

    async fn send(
        app: Router,
        method: &str,
        path: &str,
        body: Option<Value>,
        user: Option<String>,
    ) -> (StatusCode, String) {
        let mut req = Request::builder().method(method).uri(path);
        if body.is_some() {
            req = req.header("content-type", "application/json");
        }
        if let Some(u) = user {
            req = req.header("x-boss-user", u);
        }
        let body = body.map(|b| Body::from(b.to_string())).unwrap_or_default();
        let resp = app
            .oneshot(req.body(body).expect("request builds"))
            .await
            .expect("the router answers");
        let status = resp.status();
        let bytes = resp
            .into_body()
            .collect()
            .await
            .expect("body collects")
            .to_bytes();
        (status, String::from_utf8_lossy(&bytes).into_owned())
    }

    fn app(registry: &Arc<InMemoryAgents>) -> Router {
        router(AgentsApiState {
            registry: registry.clone() as Arc<dyn AgentsRegistry>,
            classes: None,
            departments: None,
            pools: None,
            stations: None,
        })
    }

    /// The door with both registries wired: the Class registry holding
    /// the tenant's `engineering-agent` role and — the shape the live
    /// drawer had on 2026-09-27 — an `engineering` department CLASS,
    /// and the departments registry holding `it` alone. Since backlog
    /// c87e3d6d an agent's department is a departments row, so the
    /// Class no longer admits it.
    fn app_with_classes(registry: &Arc<InMemoryAgents>) -> Router {
        use boss_classes_client::FakeClassesClient;
        use boss_core::primitives::Class;
        let on = |code: &str, attribute: &str| Class {
            subject_kind: "employee".into(),
            code: code.into(),
            display_name: code.into(),
            parent_code: None,
            member_attribute: Some(attribute.into()),
            metadata: Value::Null,
            sort_order: 0,
            retired_at: None,
        };
        router(AgentsApiState {
            registry: registry.clone() as Arc<dyn AgentsRegistry>,
            classes: Some(Arc::new(FakeClassesClient::with_classes(vec![
                on("engineering-agent", "role"),
                on("engineering", "department"),
            ]))),
            departments: Some(Arc::new(InMemoryDepartments::new().with(
                &DepartmentInput {
                    code: "it".into(),
                    display_name: "IT".into(),
                    function: "operations".into(),
                    sort_order: 1,
                    retired: false,
                },
            ))),
            pools: None,
            stations: None,
        })
    }

    fn batch() -> Value {
        json!([{
            "id": "agent-claude", "display_name": "Claude (engineering)",
            "default_model": "opus-5[1m]", "aliases": ["claude@algedonic.dev"]
        }])
    }

    /// The tenant batch lands, the roster lists it with its alias, a
    /// declaration that disagrees with a registered row is KEPT by
    /// default with the differing fields named (design e187198f: the
    /// instance is the truth), UPDATES it under `?mode=take` naming
    /// each change (backlog 09887242), a re-run of the same declaration
    /// changes nothing and says so, and an alias the tenant did not
    /// declare is kept.
    #[tokio::test]
    async fn a_tenant_publishes_and_the_roster_lists_the_agent() {
        let registry = Arc::new(InMemoryAgents::new());
        let (status, body) = send(
            app(&registry),
            "POST",
            "/api/agents/batch",
            Some(batch()),
            seed(),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{body}");
        let out: Value = serde_json::from_str(&body).unwrap();
        assert_eq!(out["inserted"], 1);
        assert_eq!(out["updated"], json!([]));
        assert_eq!(out["unchanged"], 0);

        let (status, body) = send(app(&registry), "GET", "/api/agents", None, probe_reader()).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        let listing: Value = serde_json::from_str(&body).unwrap();
        assert_eq!(listing["total"], 1);
        assert_eq!(listing["data"][0]["id"], "agent-claude");
        assert_eq!(listing["data"][0]["default_model"], "opus-5[1m]");
        assert_eq!(
            listing["data"][0]["aliases"],
            json!(["claude@algedonic.dev"])
        );

        // An operator-added login the tenant's file does not list.
        let registry =
            Arc::new(InMemoryAgents::new().with_agent("agent-claude", ["ops-added@algedonic.dev"]));
        let mut renamed = batch();
        renamed[0]["display_name"] = json!("Claude (renamed)");
        // The default: the held row is kept, the declared login lands
        // (nobody held it), and the answer names what still differs.
        let (_, body) = send(
            app(&registry),
            "POST",
            "/api/agents/batch",
            Some(renamed.clone()),
            seed(),
        )
        .await;
        let out: Value = serde_json::from_str(&body).unwrap();
        assert_eq!(out["inserted"], 0, "a held row is not inserted twice");
        assert_eq!(out["unchanged"], 0);
        assert_eq!(out["kept"][0]["id"], "agent-claude");
        assert_eq!(
            out["kept"][0]["differs"],
            json!(["display_name", "default_model"]),
            "{out}"
        );
        assert_eq!(
            out["updated"][0]["changes"][0]["field"], "aliases",
            "the declared login landed, and that is the one change: {out}"
        );
        let rows = registry.list().await.unwrap();
        assert_eq!(
            rows[0].display_name, "agent-claude",
            "the instance's row is kept under the default"
        );
        assert_eq!(
            rows[0].aliases,
            ["claude@algedonic.dev", "ops-added@algedonic.dev"],
            "the declared alias landed and the undeclared one is kept"
        );

        // Under take the declaration overwrites, naming each change.
        let (_, body) = send(
            app(&registry),
            "POST",
            "/api/agents/batch?mode=take",
            Some(renamed.clone()),
            seed(),
        )
        .await;
        let out: Value = serde_json::from_str(&body).unwrap();
        assert_eq!(out["inserted"], 0);
        assert_eq!(out["kept"], json!([]));
        assert_eq!(out["unchanged"], 0);
        assert_eq!(out["updated"][0]["id"], "agent-claude");
        let fields: Vec<&str> = out["updated"][0]["changes"]
            .as_array()
            .unwrap()
            .iter()
            .map(|c| c["field"].as_str().unwrap())
            .collect();
        assert_eq!(fields, ["display_name", "default_model"]);
        assert_eq!(out["updated"][0]["changes"][0]["from"], "agent-claude");
        assert_eq!(out["updated"][0]["changes"][0]["to"], "Claude (renamed)");
        let rows = registry.list().await.unwrap();
        assert_eq!(
            rows[0].display_name, "Claude (renamed)",
            "the declaration wins under take"
        );

        // The same declaration again: nothing moves, and the answer
        // says so rather than naming a phantom update.
        let (_, body) = send(
            app(&registry),
            "POST",
            "/api/agents/batch",
            Some(renamed),
            seed(),
        )
        .await;
        let out: Value = serde_json::from_str(&body).unwrap();
        assert_eq!(out["inserted"], 0);
        assert_eq!(out["updated"], json!([]));
        assert_eq!(out["kept"], json!([]));
        assert_eq!(out["unchanged"], 1);
        let events = registry.recorded_events();
        assert_eq!(
            events.len(),
            2,
            "one agent.updated for the alias that landed, one for the take, none for the re-run: {events:?}"
        );
        assert_eq!(events[1].kind, super::super::AGENT_UPDATED);
        assert_eq!(events[1].payload["id"], "agent-claude");
        assert_eq!(events[1].payload["display_name"], "Claude (renamed)");
        assert_eq!(events[1].payload["changes"][0]["field"], "display_name");
        assert_eq!(events[1].payload["updated_by"], "automation:tenant-seed");
        assert_eq!(events[1].payload["_actor"], events[1].payload["updated_by"]);

        // A mode the door does not know is a caller error, not a
        // silent default.
        let (status, _) = send(
            app(&registry),
            "POST",
            "/api/agents/batch?mode=overwrite",
            Some(batch()),
            seed(),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
    }

    /// The fact a declaration leaves (backlog d9409039, 2026-09-17):
    /// one `agent.declared` per row INSERTED — the row as inserted plus
    /// `declared_by`, the actor the request signed with — one
    /// `agent.updated` for the row the registry already held and the
    /// declaration moved, nothing for the batch.
    #[tokio::test]
    async fn a_batch_records_one_declared_event_per_inserted_row_and_one_updated_for_a_held_row() {
        let registry = Arc::new(InMemoryAgents::new().with_agent("agent-claude", []));
        let mut rows = batch();
        rows.as_array_mut().unwrap().push(json!({
            "id": "agent-scout", "display_name": "Scout",
            "default_model": "opus-5[1m]", "aliases": ["scout@algedonic.dev"]
        }));
        rows.as_array_mut().unwrap().push(json!({
            "id": "agent-clerk", "display_name": "Clerk", "default_model": "opus-5[1m]",
            "max_concurrent_runs": 2
        }));
        let (status, body) = send(
            app(&registry),
            "POST",
            "/api/agents/batch",
            Some(rows),
            seed(),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{body}");
        let out: Value = serde_json::from_str(&body).unwrap();
        assert_eq!(out["inserted"], 2);
        assert_eq!(out["updated"][0]["id"], "agent-claude");

        let all = registry.recorded_events();
        assert!(all.iter().all(|e| e.source == "jobs"), "{all:?}");
        let kinds: Vec<&str> = all.iter().map(|e| e.kind.as_str()).collect();
        assert_eq!(
            kinds,
            [
                super::super::AGENT_UPDATED,
                super::super::AGENT_DECLARED,
                super::super::AGENT_DECLARED
            ],
            "one agent.updated for the held agent-claude, one agent.declared per inserted row"
        );
        assert_eq!(all[0].payload["id"], "agent-claude");
        let events: Vec<_> = all
            .into_iter()
            .filter(|e| e.kind == super::super::AGENT_DECLARED)
            .collect();
        let ids: Vec<&str> = events
            .iter()
            .map(|e| e.payload["id"].as_str().unwrap())
            .collect();
        assert_eq!(ids, ["agent-scout", "agent-clerk"]);
        assert_eq!(events[0].payload["display_name"], json!("Scout"));
        assert_eq!(events[0].payload["default_model"], json!("opus-5[1m]"));
        assert_eq!(events[0].payload["aliases"], json!(["scout@algedonic.dev"]));
        assert_eq!(events[1].payload["max_concurrent_runs"], json!(2));
        assert_eq!(
            events[0].payload["declared_by"],
            json!("automation:tenant-seed"),
            "the actor the request signed with, named on the fact"
        );
        assert_eq!(
            events[0].payload["_actor"], events[0].payload["declared_by"],
            "declared_by and the stamp's actor are one value"
        );
    }

    /// An agent's role is a Class code under `(employee, role)` and its
    /// department a row of the departments registry — what an
    /// employee's are validated against (backlog ab192a9f; the
    /// department since c87e3d6d). A declared
    /// code the registry holds lands and reads back; one it does not
    /// hold refuses the WHOLE batch, 422, naming the agent, the
    /// attribute and the code, and nothing lands. A row that declares
    /// neither reads `role: null` — the KEY is always on the wire, so
    /// a reader tells "holds no role" from "an older registry".
    #[tokio::test]
    async fn a_role_is_a_class_and_a_department_a_registry_row_checked_at_the_door() {
        let registry = Arc::new(InMemoryAgents::new());
        let mut declared = batch();
        declared[0]["role"] = json!("engineering-agent");
        declared[0]["department"] = json!("it");
        let (status, body) = send(
            app_with_classes(&registry),
            "POST",
            "/api/agents/batch",
            Some(declared.clone()),
            seed(),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{body}");
        let (_, body) = send(
            app_with_classes(&registry),
            "GET",
            "/api/agents",
            None,
            probe_reader(),
        )
        .await;
        let listing: Value = serde_json::from_str(&body).unwrap();
        assert_eq!(listing["data"][0]["role"], "engineering-agent");
        assert_eq!(listing["data"][0]["department"], "it");

        // An undeclared code, and a declared code on the OTHER axis: a
        // department is not a role however active its row is (backlog
        // ab1e6ff8 — this door asked the axis-blind question until then).
        // And `engineering`, a department CLASS the departments registry
        // does not hold — agent-claude's live department on 2026-09-27
        // (c87e3d6d): the Class no longer admits it. A role refusal
        // names the Class registry, a department refusal the
        // departments registry.
        for (attribute, code, registry_named) in [
            ("role", "wizard", "Class"),
            ("department", "narnia", "departments registry"),
            ("role", "engineering", "Class"),
            ("department", "engineering-agent", "departments registry"),
            ("department", "engineering", "departments registry"),
        ] {
            let mut bad = declared.clone();
            bad[0][attribute] = json!(code);
            let (status, body) = send(
                app_with_classes(&registry),
                "POST",
                "/api/agents/batch",
                Some(bad),
                seed(),
            )
            .await;
            assert_eq!(
                status,
                StatusCode::UNPROCESSABLE_ENTITY,
                "{attribute}: {body}"
            );
            assert!(
                body.contains("agent-claude")
                    && body.contains(attribute)
                    && body.contains(code)
                    && body.contains(registry_named),
                "{attribute}: {body}"
            );
        }
        let rows = registry.list().await.unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(
            rows[0].role.as_deref(),
            Some("engineering-agent"),
            "a refused batch lands nothing: the held row keeps its declared role"
        );
        assert_eq!(
            registry.recorded_events().len(),
            1,
            "one agent.declared for the row that landed, nothing for the refusals"
        );

        // A declaration that names neither: the key is there, null.
        let registry = Arc::new(InMemoryAgents::new());
        let (status, body) = send(
            app_with_classes(&registry),
            "POST",
            "/api/agents/batch",
            Some(batch()),
            seed(),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{body}");
        let (_, body) = send(
            app_with_classes(&registry),
            "GET",
            "/api/agents",
            None,
            probe_reader(),
        )
        .await;
        let listing: Value = serde_json::from_str(&body).unwrap();
        let row = listing["data"][0].as_object().unwrap();
        assert!(row.contains_key("role") && row["role"].is_null(), "{row:?}");
        assert!(
            row.contains_key("department") && row["department"].is_null(),
            "{row:?}"
        );
    }

    /// The door with the pool registry wired: `robots:opus` active,
    /// and no pool for any other class.
    async fn app_with_pools(registry: &Arc<InMemoryAgents>) -> Router {
        use crate::pools::{InMemoryPools, PoolRegistry, PoolSpec};
        let pools = Arc::new(InMemoryPools::default());
        let spec: PoolSpec = serde_json::from_value(json!({
            "name": "robots:opus", "version": 1, "status": "active",
            "unit": "slot", "kind": "flow", "capacity": { "state": "unknown" },
        }))
        .unwrap();
        pools
            .publish_declared(
                spec,
                &boss_core::actor::ActorId::Automation("test".into()),
                chrono::Utc::now(),
            )
            .await
            .unwrap();
        router(AgentsApiState {
            registry: registry.clone() as Arc<dyn AgentsRegistry>,
            classes: None,
            departments: None,
            pools: Some(pools as Arc<dyn PoolRegistry>),
            stations: None,
        })
    }

    /// A robot's model class is the `<class>` of an active
    /// `robots:<class>` pool (factory taxonomy T2d, backlog
    /// 06d8988b): a declared class the pool registry holds lands and
    /// reads back; one it does not hold refuses the WHOLE batch, 422,
    /// naming the agent, the class and the pool it lacks, and nothing
    /// lands; and a door with no pool registry to ask refuses a declared
    /// class rather than recording one nobody checked — no evidence is
    /// not a pass. A declaration naming no class needs no pool.
    #[tokio::test]
    async fn a_model_class_is_checked_against_the_active_pools_at_the_door() {
        let registry = Arc::new(InMemoryAgents::new());
        let mut declared = batch();
        declared[0]["model_class"] = json!("opus");
        let (status, body) = send(
            app_with_pools(&registry).await,
            "POST",
            "/api/agents/batch",
            Some(declared.clone()),
            seed(),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{body}");
        let (_, body) = send(app(&registry), "GET", "/api/agents", None, probe_reader()).await;
        let listing: Value = serde_json::from_str(&body).unwrap();
        assert_eq!(listing["data"][0]["model_class"], "opus", "{listing}");

        let mut unpooled = declared.clone();
        unpooled[0]["model_class"] = json!("haiku");
        let (status, body) = send(
            app_with_pools(&registry).await,
            "POST",
            "/api/agents/batch?mode=take",
            Some(unpooled.clone()),
            seed(),
        )
        .await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
        assert!(
            body.contains("agent-claude") && body.contains("robots:haiku"),
            "{body}"
        );
        assert_eq!(
            registry.list().await.unwrap()[0].model_class.as_deref(),
            Some("opus"),
            "a refused batch lands nothing"
        );

        // Nothing to ask: a declared class is refused, a bare row lands.
        let (status, body) = send(
            app(&registry),
            "POST",
            "/api/agents/batch?mode=take",
            Some(declared),
            seed(),
        )
        .await;
        assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE, "{body}");
        assert!(body.contains("opus") && body.contains("pool"), "{body}");
        let (status, body) = send(
            app(&Arc::new(InMemoryAgents::new())),
            "POST",
            "/api/agents/batch",
            Some(batch()),
            seed(),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{body}");
    }

    /// The door with the station registry wired: `landed-review` and
    /// `design-review` active, `retired-bench` retired, and no `build`.
    fn app_with_stations(registry: &Arc<InMemoryAgents>) -> Router {
        use crate::stations::{InMemoryStations, StationRegistry, StationSpec};
        let stations = InMemoryStations::new();
        for (name, status) in [
            ("landed-review", "active"),
            ("design-review", "active"),
            ("retired-bench", "retired"),
        ] {
            let spec: StationSpec = serde_json::from_value(json!({
                "name": name, "version": 1, "status": status, "title": name,
                "kind": "group", "predicate": {}, "created_at": "2026-11-09T00:00:00Z",
            }))
            .unwrap();
            stations.seed(spec).unwrap();
        }
        router(AgentsApiState {
            registry: registry.clone() as Arc<dyn AgentsRegistry>,
            classes: None,
            departments: None,
            pools: None,
            stations: Some(Arc::new(stations) as Arc<dyn StationRegistry>),
        })
    }

    /// AN EXECUTOR IS SEATED AT STATIONS THAT EXIST (design 506e0a1c car
    /// 1, backlog 1a74c2bc). Seats the station registry holds active land
    /// and read back in the order declared; a seat at a station it does
    /// not hold — `build`, which no row declares yet, or a retired one —
    /// refuses the WHOLE batch, 422, naming the agent and the station,
    /// and nothing lands; a door with no station registry to ask refuses
    /// a declared seat rather than record one nobody checked (no evidence
    /// is not a pass). Under take, a declaration that names no seats
    /// keeps the held ones, and `[]` unseats, naming the change.
    #[tokio::test]
    async fn a_seat_is_checked_against_the_active_stations_at_the_door() {
        let registry = Arc::new(InMemoryAgents::new());
        let mut seated = batch();
        seated[0]["stations"] = json!(["landed-review", "design-review"]);
        let (status, body) = send(
            app_with_stations(&registry),
            "POST",
            "/api/agents/batch",
            Some(seated.clone()),
            seed(),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{body}");
        let (_, body) = send(app(&registry), "GET", "/api/agents", None, probe_reader()).await;
        let listing: Value = serde_json::from_str(&body).unwrap();
        assert_eq!(
            listing["data"][0]["stations"],
            json!(["landed-review", "design-review"]),
            "{listing}"
        );

        for unknown in ["build", "retired-bench"] {
            let mut bad = seated.clone();
            bad[0]["stations"] = json!(["landed-review", unknown]);
            let (status, body) = send(
                app_with_stations(&registry),
                "POST",
                "/api/agents/batch?mode=take",
                Some(bad),
                seed(),
            )
            .await;
            assert_eq!(
                status,
                StatusCode::UNPROCESSABLE_ENTITY,
                "{unknown}: {body}"
            );
            assert!(
                body.contains("agent-claude") && body.contains(&format!("`{unknown}`")),
                "names the agent and the station: {body}"
            );
            assert_eq!(
                registry.list().await.unwrap()[0].stations,
                ["landed-review", "design-review"],
                "a refused batch lands nothing"
            );
        }

        // Nothing to ask: a declared seat is refused, 503.
        let (status, body) = send(
            app(&registry),
            "POST",
            "/api/agents/batch?mode=take",
            Some(seated),
            seed(),
        )
        .await;
        assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE, "{body}");
        assert!(
            body.contains("landed-review") && body.contains("station"),
            "{body}"
        );

        // Omission keeps the seats, even under take; `[]` unseats and
        // needs no registry, since it names no station to check.
        let (status, body) = send(
            app(&registry),
            "POST",
            "/api/agents/batch?mode=take",
            Some(batch()),
            seed(),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(
            registry.list().await.unwrap()[0].stations,
            ["landed-review", "design-review"],
            "a declaration that names no seats unseats nobody"
        );
        let mut unseat = batch();
        unseat[0]["stations"] = json!([]);
        let (status, body) = send(
            app(&registry),
            "POST",
            "/api/agents/batch?mode=take",
            Some(unseat),
            seed(),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert!(body.contains("stations"), "the change is named: {body}");
        assert!(registry.list().await.unwrap()[0].stations.is_empty());
    }

    /// The door with the two model-slot pools wired, each listing the
    /// model ids it meters (`PoolSpec::models`, backlog af5dd547).
    async fn app_with_model_pools(
        registry: &Arc<InMemoryAgents>,
        listed: &[(&str, &[&str])],
    ) -> Router {
        use crate::pools::{InMemoryPools, PoolRegistry, PoolSpec};
        let pools = Arc::new(InMemoryPools::default());
        for (name, models) in listed {
            let spec: PoolSpec = serde_json::from_value(json!({
                "name": name, "version": 1, "status": "active",
                "unit": "slot", "kind": "flow", "capacity": { "state": "unknown" },
                "models": models,
            }))
            .unwrap();
            pools
                .publish_declared(
                    spec,
                    &boss_core::actor::ActorId::Automation("test".into()),
                    chrono::Utc::now(),
                )
                .await
                .unwrap();
        }
        router(AgentsApiState {
            registry: registry.clone() as Arc<dyn AgentsRegistry>,
            classes: None,
            departments: None,
            pools: Some(pools as Arc<dyn PoolRegistry>),
            stations: None,
        })
    }

    /// THE MODEL-TO-CLASS FACT CANNOT DISAGREE WITH ITSELF (backlog
    /// d088c248; CLAUDE.md 9a). An agents row says default_model ->
    /// model_class, and since af5dd547 a model-slot pool says class ->
    /// models, which is what a `from-run` bill reads a run's class by. A
    /// declared class that a pool contradicts is refused, 422, naming the
    /// agent, its model and every pool involved, and nothing lands: the
    /// model listed by ANOTHER class's pool, or the class's own pool
    /// listing models that do not include it. A class pool that lists no
    /// models says nothing about the model, so the declaration stands as
    /// before (every live pool until 35e9babf item 5 publishes).
    #[tokio::test]
    async fn a_declared_class_a_pool_contradicts_is_refused_naming_agent_and_pool() {
        let listed: &[(&str, &[&str])] = &[
            ("robots:opus", &["opus-5-5", "opus-5[1m]"]),
            ("robots:sol", &["gpt-6.1-sol"]),
        ];
        let registry = Arc::new(InMemoryAgents::new());
        let mut wrong = batch();
        wrong[0]["model_class"] = json!("sol");
        let (status, body) = send(
            app_with_model_pools(&registry, listed).await,
            "POST",
            "/api/agents/batch",
            Some(wrong),
            seed(),
        )
        .await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
        for named in ["agent-claude", "opus-5[1m]", "robots:opus", "robots:sol"] {
            assert!(body.contains(named), "names {named}: {body}");
        }
        assert!(registry.list().await.unwrap().is_empty(), "nothing lands");

        // The class's own pool lists models, and not this one.
        let only_55: &[(&str, &[&str])] = &[("robots:opus", &["opus-5-5"])];
        let mut unlisted = batch();
        unlisted[0]["model_class"] = json!("opus");
        let (status, body) = send(
            app_with_model_pools(&registry, only_55).await,
            "POST",
            "/api/agents/batch",
            Some(unlisted.clone()),
            seed(),
        )
        .await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
        for named in ["agent-claude", "opus-5[1m]", "robots:opus", "opus-5-5"] {
            assert!(body.contains(named), "names {named}: {body}");
        }

        // Agreement lands, and a class pool listing nothing judges nothing.
        let (status, body) = send(
            app_with_model_pools(&registry, listed).await,
            "POST",
            "/api/agents/batch",
            Some(unlisted.clone()),
            seed(),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{body}");
        let (status, body) = send(
            app_with_model_pools(&Arc::new(InMemoryAgents::new()), &[("robots:opus", &[])]).await,
            "POST",
            "/api/agents/batch",
            Some(unlisted),
            seed(),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{body}");
    }

    /// A ROBOT WITH NO CLASS IS NAMED, NOT SILENT (backlog 3317f4db). A
    /// `model:<class>` step is claimed — and, since this item, nominated
    /// — only by the class an agents row records, so a row recording
    /// none is a robot no such step will ever reach. Every live robot
    /// stood that way after T2d (read 2026-11-05: agent-claude,
    /// agent-claude-b, agent-codex all `model_class: null`). The batch
    /// is NOT refused for it: a class is the `<class>` of a declared
    /// robot pool and agent-codex runs `gpt-6.1-sol`, which none
    /// declares, so a refusal would refuse every tenant batch carrying
    /// it. The answer names each declared row that records no class
    /// once the batch is applied — read back from the registry, so a
    /// kept row that already holds a class is not named for a
    /// declaration that omits the key.
    #[tokio::test]
    async fn a_batch_names_each_robot_that_records_no_model_class() {
        let registry = Arc::new(InMemoryAgents::new());
        let mut declared = batch();
        declared[0]["model_class"] = json!("opus");
        declared.as_array_mut().unwrap().push(json!({
            "id": "agent-codex", "display_name": "Codex", "default_model": "opus-5"
        }));
        let (status, body) = send(
            app_with_pools(&registry).await,
            "POST",
            "/api/agents/batch",
            Some(declared.clone()),
            seed(),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{body}");
        let out: Value = serde_json::from_str(&body).unwrap();
        assert_eq!(out["no_model_class"], json!(["agent-codex"]), "{out}");

        // The default keeps a held row: declaring agent-claude without a
        // class leaves its `opus`, so only the row recording none is named.
        let (status, body) = send(
            app_with_pools(&registry).await,
            "POST",
            "/api/agents/batch",
            Some(batch()),
            seed(),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{body}");
        let out: Value = serde_json::from_str(&body).unwrap();
        assert_eq!(out["no_model_class"], json!([]), "{out}");

        // A take that declares the class clears the name.
        declared[1]["model_class"] = json!("opus");
        let (status, body) = send(
            app_with_pools(&registry).await,
            "POST",
            "/api/agents/batch?mode=take",
            Some(declared),
            seed(),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{body}");
        let out: Value = serde_json::from_str(&body).unwrap();
        assert_eq!(out["no_model_class"], json!([]), "{out}");
    }

    /// A bad declaration refuses the WHOLE batch, 422, naming the row;
    /// nothing lands.
    #[tokio::test]
    async fn a_bad_declaration_refuses_the_batch_by_name() {
        let registry = Arc::new(InMemoryAgents::new());
        let mut b = batch();
        b.as_array_mut().unwrap().push(json!({
            "id": "claude@algedonic.dev", "display_name": "x", "default_model": "opus-5"
        }));
        let (status, body) =
            send(app(&registry), "POST", "/api/agents/batch", Some(b), seed()).await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
        assert!(body.contains("claude@algedonic.dev"), "{body}");
        assert!(registry.list().await.unwrap().is_empty());
    }

    /// The probe reader reads; a request with no identity header does
    /// not (backlog e84de48e, 2026-09-25 — the roster, aliases and all,
    /// was one of the fifteen reads a headerless caller was trusted
    /// with); a user-tier session writes nothing; the auditor reads and
    /// cannot write.
    #[tokio::test]
    async fn reads_admit_the_probe_reader_and_writes_are_operator_machinery() {
        let registry = Arc::new(InMemoryAgents::new());
        for (user, want) in [
            (probe_reader(), StatusCode::OK),
            (None, StatusCode::FORBIDDEN),
        ] {
            let (status, _) = send(app(&registry), "GET", "/api/agents", None, user.clone()).await;
            assert_eq!(status, want, "{user:?}");
        }
        let david = Some(header("emp-david", "platform-admin", AccessTier::User));
        let (status, _) = send(
            app(&registry),
            "POST",
            "/api/agents/batch",
            Some(batch()),
            david,
        )
        .await;
        assert_eq!(status, StatusCode::FORBIDDEN);
        let (status, _) = send(
            app(&registry),
            "POST",
            "/api/agents/batch",
            Some(batch()),
            probe_reader(),
        )
        .await;
        assert_eq!(status, StatusCode::FORBIDDEN);
    }

    /// The automation half reads back what the platform seed landed —
    /// to the probe reader, never to a caller with no identity — and is
    /// not part of the roster: an automation holding platform-admin is
    /// not a role holder the dispatcher could nominate (backlog
    /// ddf0773e, design abf9eeae car 1).
    #[tokio::test]
    async fn the_automations_read_back_and_stay_off_the_roster() {
        use crate::agents::automations::AutomationActor;
        let registry = Arc::new(InMemoryAgents::new());
        let row = AutomationActor {
            id: "automation:dispatcher".into(),
            role: "platform-admin".into(),
            description: "the dispatcher".into(),
            signs_for: Some("automation:rule:".into()),
            access_tier: None,
            station: None,
        };
        let stamp = boss_core::publisher::EventStamp::new(
            "jobs",
            boss_core::actor::ActorId::Automation("platform-workflow-seed".into()),
        );
        registry
            .declare_automations(std::slice::from_ref(&row), &stamp)
            .await
            .unwrap();
        let (status, body) = send(
            app(&registry),
            "GET",
            "/api/agents/automations",
            None,
            probe_reader(),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{body}");
        let v: Value = serde_json::from_str(&body).unwrap();
        assert_eq!(v["total"], 1);
        assert_eq!(v["data"][0], serde_json::to_value(&row).unwrap());
        let (status, _) = send(app(&registry), "GET", "/api/agents/automations", None, None).await;
        assert_eq!(status, StatusCode::FORBIDDEN);
        let (_, roster) = send(app(&registry), "GET", "/api/agents", None, probe_reader()).await;
        let roster: Value = serde_json::from_str(&roster).unwrap();
        assert_eq!(roster["total"], 0, "{roster}");
    }
}
