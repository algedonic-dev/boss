//! The production caller of `boss_jobs::pools::refresh_roster` (backlog
//! a6cb448a; design 4b27a65a, station-tree.md §3: `people:platform-admin`
//! is a root pool whose capacity is read from the roster, never set by
//! hand).
//!
//! WHY HERE. Console car 2d (5502c1cd, train #1543) built both halves —
//! the people-owned snapshot behind `RosterCapacitySnapshot` and the pool
//! owner's fenced `observe_roster` — and nothing joined them outside a
//! test, so every roster pool read `unknown` forever and the claim door
//! refused a billed claim on it (measured on origin/main d6d422af). The
//! snapshot reads the employee rows, the employee Classes and the people
//! outbox, so only this service can take it; the pool registry is the
//! jobs crate's adapter over the same database, and a module crate may
//! hold a core one. The dispatcher speaks HTTP and holds neither, so its
//! clock rule (`infra/dispatcher/rules/roster-pools-are-observed-every-five-minutes.toml`)
//! POSTs this door.
//!
//! WHAT THE CALLER CHOOSES: nothing. No count, no pool, no instant rides
//! the request. Every active pool that declares a roster is observed, the
//! owner rechecks each declaration under its lineage lock, and an
//! observation that changes nothing writes nothing — so a repeated firing
//! is idempotent and an unavailable roster reads `unknown`, never zero.
//!
//! WHO MAY. The answer is a count over every employee, so it is the roster
//! read at scope `all` ([`crate::grants::roster_scope`]): the operator
//! tier the dispatcher's rule actor presents, or `read` on `employee` at
//! `all`. And an identified actor, whom each published version names.
use std::sync::Arc;

use axum::extract::State;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::post;
use axum::{Json, Router};
use boss_core::actor::ActorId;
use boss_core::roster_capacity::RosterCapacitySnapshot;
use boss_jobs::pools::{PoolCapacity, PoolError, PoolRegistry, refresh_roster};
use boss_policy::Scope;
use boss_policy_client::{CurrentUser, PolicyClient};
use chrono::{DateTime, Utc};
use serde::Serialize;

/// One roster pool after its observation: the version that now stands and
/// its capacity, or why it could not be observed.
#[derive(Debug, Serialize)]
pub struct Observed {
    pub pool: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub version: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub capacity: Option<PoolCapacity>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// Observe every active roster pool once. One pool that cannot be
/// observed (a racing observation's conflict) does not keep the others
/// stale: each answers on its own row.
pub async fn refresh_roster_pools(
    registry: &dyn PoolRegistry,
    source: &dyn RosterCapacitySnapshot,
    actor: &ActorId,
    now: DateTime<Utc>,
) -> Result<Vec<Observed>, PoolError> {
    let declared = registry.list_active().await?;
    let mut observed = Vec::new();
    for expected in declared.iter().filter(|p| p.roster.is_some()) {
        observed.push(
            match refresh_roster(registry, source, expected, actor, now).await {
                Ok(spec) => Observed {
                    pool: spec.name,
                    version: Some(spec.version),
                    capacity: Some(spec.capacity),
                    error: None,
                },
                Err(e) => Observed {
                    pool: expected.name.clone(),
                    version: None,
                    capacity: None,
                    error: Some(e.to_string()),
                },
            },
        );
    }
    Ok(observed)
}

pub struct RosterRefreshState {
    pub registry: Arc<dyn PoolRegistry>,
    pub source: Arc<dyn RosterCapacitySnapshot>,
    pub policy: Option<Arc<dyn PolicyClient>>,
    pub clock: Arc<dyn boss_clock_client::ClockClient>,
}

pub fn roster_refresh_router(state: RosterRefreshState) -> Router {
    Router::new()
        .route("/api/people/roster-capacity/refresh", post(refresh))
        .with_state(Arc::new(state))
}

async fn refresh(
    State(state): State<Arc<RosterRefreshState>>,
    CurrentUser(user): CurrentUser,
) -> Response {
    match crate::grants::roster_scope(state.policy.as_ref(), &user).await {
        Ok(Scope::All) => {}
        Ok(_) => {
            return (
                StatusCode::FORBIDDEN,
                "a roster pool counts every employee: it requires the roster read at scope all",
            )
                .into_response();
        }
        Err(refused) => return refused,
    }
    let Some(actor) = user.ambient_actor() else {
        return (
            StatusCode::FORBIDDEN,
            "a roster observation requires an identified actor",
        )
            .into_response();
    };
    let now = boss_clock_client::now_from(&state.clock).await;
    match refresh_roster_pools(state.registry.as_ref(), state.source.as_ref(), &actor, now).await {
        Ok(observed) if observed.iter().all(|o| o.error.is_none()) => {
            Json(serde_json::json!({ "observed": observed })).into_response()
        }
        // Loud, with every row: the firing record names the pool.
        Ok(observed) => (
            StatusCode::CONFLICT,
            Json(serde_json::json!({ "observed": observed })),
        )
            .into_response(),
        Err(e) => (StatusCode::SERVICE_UNAVAILABLE, e.to_string()).into_response(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::http::Request;
    use boss_core::roster_capacity::{ClassRequirement, RosterSnapshot, RosterSnapshotError};
    use boss_jobs::pools::{InMemoryPools, PoolSpec};
    use std::sync::Mutex;
    use tower::ServiceExt;

    /// The roster as the people snapshot would read it: (revision, count).
    struct Roster(Mutex<(i64, u64)>);

    #[async_trait::async_trait]
    impl RosterCapacitySnapshot for Roster {
        async fn snapshot(
            &self,
            eligibility: &[ClassRequirement],
        ) -> Result<RosterSnapshot, RosterSnapshotError> {
            let (revision, count) = *self.0.lock().unwrap();
            Ok(RosterSnapshot {
                eligibility: eligibility.to_vec(),
                revision: Some(revision),
                count,
            })
        }
    }

    fn spec(value: serde_json::Value) -> PoolSpec {
        serde_json::from_value(value).unwrap()
    }

    async fn declared() -> Arc<InMemoryPools> {
        let pools = Arc::new(InMemoryPools::default());
        let actor = ActorId::automation("roster-refresh-fixture");
        let now = Utc::now();
        // The shipped row's shape: a people pool whose capacity is unknown
        // until it is observed, beside a configured pool no roster decides.
        pools
            .publish_declared(
                spec(
                    serde_json::json!({"name":"people:platform-admin","version":2,
                    "status":"active","unit":"people","kind":"flow",
                    "capacity":{"state":"unknown"},
                    "roster":{"eligibility":[{"attribute":"role","codes":["platform-admin"]}],
                              "basis":null}}),
                ),
                &actor,
                now,
            )
            .await
            .unwrap();
        pools
            .publish_declared(
                spec(
                    serde_json::json!({"name":"vcpu","version":1,"status":"active",
                    "unit":"vcpu","kind":"flow",
                    "capacity":{"state":"configured","amount":"16"}}),
                ),
                &actor,
                now,
            )
            .await
            .unwrap();
        pools
    }

    fn app(pools: Arc<InMemoryPools>, roster: Arc<Roster>) -> Router {
        roster_refresh_router(RosterRefreshState {
            registry: pools,
            source: roster,
            policy: None,
            clock: Arc::new(boss_clock_client::WallClockClient),
        })
    }

    async fn fire(app: Router, user: &str) -> (StatusCode, serde_json::Value) {
        let response = app
            .oneshot(
                Request::post("/api/people/roster-capacity/refresh")
                    .header("x-boss-user", user)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let status = response.status();
        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        (
            status,
            serde_json::from_slice(&body).unwrap_or(serde_json::Value::Null),
        )
    }

    const RULE: &str = r#"{"id":"rule:roster-pools-are-observed-every-five-minutes","role":"platform-admin","access_tier":"operator"}"#;

    async fn active(pools: &InMemoryPools, name: &str) -> PoolSpec {
        pools
            .list_active()
            .await
            .unwrap()
            .into_iter()
            .find(|p| p.name == name)
            .unwrap()
    }

    /// The acceptance of a6cb448a: a roster pool that read `unknown` reads
    /// the roster's count once the door is fired, and a roster change moves
    /// it — each a version with the observation's basis, never typed.
    #[tokio::test]
    async fn a_roster_change_moves_the_people_pool_capacity() {
        let pools = declared().await;
        let roster = Arc::new(Roster(Mutex::new((7, 1))));
        assert_eq!(
            active(&pools, "people:platform-admin").await.capacity,
            PoolCapacity::Unknown {}
        );

        let (status, body) = fire(app(pools.clone(), roster.clone()), RULE).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        let observed = active(&pools, "people:platform-admin").await;
        assert_eq!(
            observed.capacity,
            PoolCapacity::Configured { amount: 1.into() }
        );
        let basis = observed.roster.as_ref().unwrap().basis.as_ref().unwrap();
        assert_eq!((basis.revision, basis.count), (Some(7), Some(1)));
        assert!(basis.as_of.is_some(), "a measured row says when");
        assert_eq!(body["observed"][0]["pool"], "people:platform-admin");
        assert_eq!(body["observed"][0]["capacity"]["state"], "configured");

        // The same roster again writes nothing.
        fire(app(pools.clone(), roster.clone()), RULE).await;
        assert_eq!(
            active(&pools, "people:platform-admin").await.version,
            observed.version
        );

        // A second platform-admin is hired.
        *roster.0.lock().unwrap() = (9, 2);
        let (status, _) = fire(app(pools.clone(), roster), RULE).await;
        assert_eq!(status, StatusCode::OK);
        let moved = active(&pools, "people:platform-admin").await;
        assert_eq!(
            moved.capacity,
            PoolCapacity::Configured { amount: 2.into() }
        );
        assert_eq!(moved.version, observed.version + 1);
        assert_eq!(
            active(&pools, "vcpu").await.version,
            1,
            "a pool no roster decides is not touched"
        );
    }

    #[tokio::test]
    async fn a_narrower_roster_read_is_refused() {
        let narrow: Arc<dyn PolicyClient> = Arc::new(
            boss_policy_client::FakePolicyClient::builder()
                .allow(
                    "manager",
                    boss_policy::Action::Read,
                    boss_policy::Resource::employee(),
                    Scope::Team,
                )
                .build(),
        );
        let pools = declared().await;
        let app = roster_refresh_router(RosterRefreshState {
            registry: pools.clone(),
            source: Arc::new(Roster(Mutex::new((7, 1)))),
            policy: Some(narrow),
            clock: Arc::new(boss_clock_client::WallClockClient),
        });
        let (status, _) = fire(app, r#"{"id":"emp-1","role":"manager"}"#).await;
        assert_eq!(status, StatusCode::FORBIDDEN);
        assert_eq!(
            active(&pools, "people:platform-admin").await.capacity,
            PoolCapacity::Unknown {}
        );
    }
}
