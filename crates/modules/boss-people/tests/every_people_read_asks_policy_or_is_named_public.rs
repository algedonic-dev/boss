//! Every read the people API serves asks policy or refuses its caller,
//! or is named public with a reason (backlog cda177ef, 2026-09-27) —
//! the module-side twin of the jobs API's pin
//! (`crates/core/boss-jobs/tests/every_jobs_read_asks_policy_or_is_named_public.rs`,
//! backlog e5f7b51e), in the same shape.
//!
//! WHY IT EXISTS. The review of guest cars 1+3 (design 2830b6b7)
//! found the OSS README promising that a Basic guest — the `visitor`
//! role — reads only what policy grants it, while `GET /api/people`
//! asked nobody and handed it the whole roster: names, emails, roles,
//! departments. So did the single row, the reports list and the
//! requisitions. Those are fixed; this holds the NEXT read to the rule.
//!
//! THE ROUTE LIST IS DERIVED, NOT TYPED (CLAUDE.md §9a). It is read out
//! of the router source the service binary assembles: every
//! `boss_people::<module>::<router>(` the binary merges, plus the CRUD
//! `router(state)` from `src/http.rs`, and within each, every `.route(`
//! whose method router carries a `get(`. A router the binary merges
//! that this test does not also mount answers the fallback (418) and
//! fails naming the route.
//!
//! THE JUDGEMENT IS MEASURED, TWICE. Each route is requested (1) with
//! no identity at all, against a policy that denies everything and
//! counts every question — it passes when it refuses (401/403) or
//! asked — and (2) as a Basic guest session, against the core default
//! rules — it passes when it does not answer 2xx. Anything else must
//! sit on [`PUBLIC`] (sessionless by design, with the reason) or
//! [`PENDING`] (a ratchet: may only shrink).

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use async_trait::async_trait;
use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use boss_people::PeopleRepository;
use boss_people::http::{PeopleApiState, router};
use boss_people::postgres::PgPeople;
use boss_policy_client::{
    Action, Decision, FakePolicyClient, PolicyClient, PolicyClientError, Predicate, Resource, User,
};
use boss_testing::TestDb;
use sqlx::PgPool;
use tower::ServiceExt;

mod common;

/// Sessionless by design, on every instance.
const PUBLIC: &[(&str, &str)] = &[(
    "/api/people/health",
    "liveness and build; the machine gate exempts it for the same reason",
)];

/// THE RATCHET: reads that still answer a caller without asking,
/// measured on this car. Delete a row when its route is fixed — the
/// test fails until you do.
const PENDING: &[(&str, &str)] = &[(
    "/api/people/{id}/exists",
    "one bool, whether an employee id exists. Not refusable yet: its callers — boss-assets' actor_id write guard through boss_people_client::ReqwestPeopleClient::employee_exists — send no identity, so a refusal here refuses their writes; they must sign first",
)];

const EMP: &str = "emp-read-pin";

fn email() -> String {
    format!("{EMP}@boss.io")
}

// ---------------------------------------------------------------------------
// The route list, derived from the router source the binary assembles
// ---------------------------------------------------------------------------

fn crate_file(rel: &str) -> String {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join(rel);
    std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()))
}

/// The body of `fn <name>` in `src`: from its signature to the first
/// line that closes a top-level item.
fn fn_body<'a>(src: &'a str, signature: &str, file: &str) -> &'a str {
    let start = src
        .find(signature)
        .unwrap_or_else(|| panic!("{file}: no `{signature}` — did the router move?"));
    let rest = &src[start..];
    let end = rest
        .find("\n}\n")
        .unwrap_or_else(|| panic!("{file}: `{signature}` never closes"));
    &rest[..end]
}

/// Whether `text` calls `get(` as a method router rather than naming a
/// handler that starts `get_`.
fn has_get(text: &str) -> bool {
    text.match_indices("get(").any(|(i, _)| {
        text[..i]
            .chars()
            .next_back()
            .is_none_or(|c| !(c.is_alphanumeric() || c == '_'))
    })
}

/// Every `(path, has GET)` a router fn body declares.
fn routes_in(body: &str, file: &str) -> Vec<(String, bool)> {
    let mut out = Vec::new();
    let mut rest = body;
    while let Some(i) = rest.find(".route(") {
        rest = &rest[i + ".route(".len()..];
        let trimmed = rest.trim_start();
        let lit = trimmed.strip_prefix('"').unwrap_or_else(|| {
            panic!(
                "{file}: a `.route(` whose path is not a string literal — this pin reads paths \
                 from the source; spell it as a literal or teach the pin: {}",
                &trimmed[..trimmed.len().min(80)]
            )
        });
        let close = lit.find('"').expect("unterminated path literal");
        let path = lit[..close].to_string();
        let after = &lit[close + 1..];
        let mut depth = 1usize;
        let mut end = after.len();
        for (j, c) in after.char_indices() {
            match c {
                '(' => depth += 1,
                ')' => {
                    depth -= 1;
                    if depth == 0 {
                        end = j;
                        break;
                    }
                }
                _ => {}
            }
        }
        out.push((path, has_get(&after[..end])));
        rest = &after[end..];
    }
    out
}

/// Every GET path the service binary serves, read from the source it
/// assembles.
fn derived_get_routes() -> BTreeSet<String> {
    let bin = crate_file("src/bin/boss_people_api.rs");
    assert!(
        bin.contains("app.merge(router(state))"),
        "the binary no longer merges the CRUD router as `router(state)` — re-derive this pin's source"
    );
    let mut sources = vec![("src/http.rs".to_string(), "pub fn router<".to_string())];
    for (i, _) in bin.match_indices("boss_people::") {
        let tail = &bin[i + "boss_people::".len()..];
        let Some((module, rest)) = tail.split_once("::") else {
            continue;
        };
        let Some(func) = rest.split('(').next() else {
            continue;
        };
        let ident =
            |s: &str| !s.is_empty() && s.chars().all(|c| c.is_ascii_lowercase() || c == '_');
        if ident(module) && ident(func) && is_router_function(func) {
            sources.push((format!("src/{module}.rs"), format!("pub fn {func}(")));
        }
    }
    assert!(
        sources.len() >= 7,
        "read only {} router sources out of the binary; the pin's reader is broken, not the routes",
        sources.len()
    );
    let mut gets = BTreeSet::new();
    for (file, signature) in &sources {
        let src = crate_file(file);
        let body = fn_body(&src, signature, file);
        let routes = routes_in(body, file);
        assert!(
            !routes.is_empty(),
            "{file}: read no routes from `{signature}`"
        );
        gets.extend(routes.into_iter().filter(|(_, get)| *get).map(|(p, _)| p));
    }
    gets
}

fn is_router_function(name: &str) -> bool {
    name.ends_with("_router")
        || name
            .strip_suffix("_with_reports")
            .is_some_and(|base| base.ends_with("_router"))
}

#[test]
fn the_route_reader_includes_report_routers_and_excludes_other_helpers() {
    for name in [
        "scope_router",
        "scope_router_with_reports",
        "workflow_router",
    ] {
        assert!(is_router_function(name), "lost router {name}");
    }
    for name in [
        "scope_router_settings",
        "scope_with_reports",
        "bootstrap_by_email",
    ] {
        assert!(!is_router_function(name), "read helper {name} as a router");
    }
}

// ---------------------------------------------------------------------------
// The assembled router
// ---------------------------------------------------------------------------

/// Answers as `inner` does and counts every question it is asked.
struct Recording {
    inner: FakePolicyClient,
    asked: Arc<AtomicUsize>,
}

#[async_trait]
impl PolicyClient for Recording {
    async fn check(
        &self,
        user: &User,
        action: Action,
        resource: Resource,
    ) -> Result<Decision, PolicyClientError> {
        self.asked.fetch_add(1, Ordering::SeqCst);
        self.inner.check(user, action, resource).await
    }
    async fn scope_predicate(
        &self,
        user: &User,
        resource: Resource,
    ) -> Result<Predicate, PolicyClientError> {
        self.asked.fetch_add(1, Ordering::SeqCst);
        self.inner.scope_predicate(user, resource).await
    }
}

fn default_rules() -> FakePolicyClient {
    boss_policy_client::defaults::default_rules()
        .into_iter()
        .fold(FakePolicyClient::builder(), |b, r| {
            b.allow(r.role, r.action, r.resource, r.scope)
        })
        .build()
}

const UNMOUNTED: StatusCode = StatusCode::IM_A_TEAPOT;

/// The same routers the binary merges, over one database.
fn assembled(pool: &PgPool, policy: Arc<dyn PolicyClient>) -> Router {
    let clock: Arc<dyn boss_clock_client::ClockClient> =
        Arc::new(boss_clock_client::WallClockClient);
    boss_people::workflows::workflow_router(
        pool.clone(),
        Arc::new(PgPeople::new(pool.clone())),
        None,
        clock.clone(),
        Some(policy.clone()),
    )
    .merge(boss_people::requisitions::requisitions_router(
        pool.clone(),
        None,
        clock.clone(),
        Some(policy.clone()),
    ))
    .merge(boss_people::employee_changes::employee_changes_router(
        pool.clone(),
        None,
        clock.clone(),
        Some(policy.clone()),
    ))
    .merge(boss_people::scope::scope_router(pool.clone()))
    .merge(boss_people::webauthn::webauthn_router(
        pool.clone(),
        clock.clone(),
    ))
    .merge(boss_people::pto::pto_router(
        boss_people::pto::PtoApiState {
            calendar: None,
            policy: Some(policy.clone()),
        },
    ))
    .merge(router(PeopleApiState {
        people: Arc::new(PgPeople::new(pool.clone())),
        publisher: None,
        policy: Some(policy),
        subject_kinds: None,
        clock,
    }))
    .fallback(|| async { (UNMOUNTED, "this pin does not mount the route") })
    .layer(axum::middleware::from_fn(
        boss_policy_client::request_context_middleware,
    ))
}

/// A concrete path for a route pattern: the seeded employee for an id,
/// its address for an email, a word for anything else.
fn concrete(pattern: &str) -> String {
    pattern
        .split('/')
        .map(
            |seg| match seg.strip_prefix('{').and_then(|s| s.strip_suffix('}')) {
                Some("id") => EMP.to_string(),
                Some("email") => email(),
                Some(_) => "pin-probe".to_string(),
                None => seg.to_string(),
            },
        )
        .collect::<Vec<_>>()
        .join("/")
}

/// The header the gateway's `build_user_json` forwards for a guest
/// session on a Basic install.
fn basic_guest() -> String {
    serde_json::json!({
        "id": boss_core::roles::GUEST_EMAIL,
        "role": boss_core::roles::VISITOR_ROLE,
        "access_tier": "user",
        "territory_account_ids": [],
        "direct_report_ids": [],
        "department": null,
    })
    .to_string()
}

#[derive(Debug)]
struct Answer {
    anonymous: StatusCode,
    asked: usize,
    guest: StatusCode,
}

impl Answer {
    fn checked(&self) -> bool {
        (matches!(
            self.anonymous,
            StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN
        ) || self.asked > 0)
            && !self.guest.is_success()
    }
}

async fn status(app: Router, route: &str, user: Option<&str>) -> StatusCode {
    let mut req = Request::builder().method("GET").uri(concrete(route));
    if let Some(user) = user {
        req = req.header("x-boss-user", user);
    }
    app.oneshot(req.body(Body::empty()).unwrap())
        .await
        .unwrap()
        .status()
}

async fn measure(pool: &PgPool, routes: &BTreeSet<String>) -> BTreeMap<String, Answer> {
    let mut out = BTreeMap::new();
    for route in routes {
        let asked = Arc::new(AtomicUsize::new(0));
        let deny: Arc<dyn PolicyClient> = Arc::new(Recording {
            inner: FakePolicyClient::deny_all(),
            asked: asked.clone(),
        });
        // No header of any kind: what a caller reaching the port
        // directly sends, and what a sessionless route forwards.
        let anonymous = status(assembled(pool, deny), route, None).await;
        let defaults: Arc<dyn PolicyClient> = Arc::new(default_rules());
        let guest = status(assembled(pool, defaults), route, Some(&basic_guest())).await;
        out.insert(
            route.clone(),
            Answer {
                anonymous,
                asked: asked.load(Ordering::SeqCst),
                guest,
            },
        );
    }
    out
}

#[test]
fn the_reader_finds_the_routes_the_binary_serves() {
    let gets = derived_get_routes();
    // Controls whose answer is known: the CRUD list, a merged module's
    // read, and a POST-only route that must NOT be read as a GET.
    for known in [
        "/api/people",
        "/api/people/{id}/changes",
        "/api/people/by-email/{email}/bootstrap",
    ] {
        assert!(gets.contains(known), "derived list lost {known}: {gets:#?}");
    }
    for post_only in ["/api/people/pto", "/api/people/presence-challenges"] {
        assert!(
            !gets.contains(post_only),
            "{post_only} is a POST, read as a GET"
        );
    }
    assert!(gets.len() >= 10, "only {} GET routes derived", gets.len());
}

#[tokio::test]
async fn every_people_read_asks_policy_or_is_named_public() {
    let db = TestDb::new().await;
    PgPeople::new(db.pool.clone())
        .create_employee(&common::employee_fixture(EMP))
        .await
        .expect("seed the employee the reads ask for");
    let gets = derived_get_routes();
    let answers = measure(&db.pool, &gets).await;

    let public: BTreeMap<&str, &str> = PUBLIC.iter().copied().collect();
    let pending: BTreeMap<&str, &str> = PENDING.iter().copied().collect();
    let mut failures = Vec::new();
    for name in public.keys().chain(pending.keys()) {
        if !gets.contains(*name) {
            failures.push(format!(
                "{name}: listed, but the router serves no such GET — delete its row"
            ));
        }
        if public.contains_key(name) && pending.contains_key(name) {
            failures.push(format!("{name}: on both lists — it is one or the other"));
        }
    }
    for (route, answer) in &answers {
        let route = route.as_str();
        if answer.anonymous == UNMOUNTED {
            failures.push(format!(
                "{route}: the binary serves it, but this pin's assembly does not mount its \
                 router — merge that router in `assembled`"
            ));
            continue;
        }
        match (answer.checked(), public.get(route), pending.get(route)) {
            (true, None, None) | (false, Some(_), None) | (false, None, Some(_)) => {}
            (true, Some(_), _) => failures.push(format!(
                "{route}: asks policy now ({answer:?}) — it is not public; delete its PUBLIC row"
            )),
            (true, None, Some(_)) => failures.push(format!(
                "{route}: asks policy now ({answer:?}) — the ratchet shrinks; delete its PENDING row"
            )),
            (false, None, None) => failures.push(format!(
                "{route}: answered {answer:?} — a read must refuse a caller with no identity or \
                 ask policy, and must not answer a Basic guest on the core defaults \
                 (crate::grants::roster_scope is the roster's question); or name it PUBLIC with \
                 the reason it holds on every instance"
            )),
            (false, Some(_), Some(_)) => {}
        }
    }
    assert!(
        failures.is_empty(),
        "{} of {} people-API GET routes break the rule:\n  {}",
        failures.len(),
        gets.len(),
        failures.join("\n  ")
    );
}
