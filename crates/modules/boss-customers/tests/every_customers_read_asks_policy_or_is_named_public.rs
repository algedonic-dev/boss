//! Every read the customers API serves asks policy or refuses its
//! caller, or is named public with a reason (backlog cda177ef,
//! 2026-11-19) — the people API's pin
//! (`crates/modules/boss-people/tests/every_people_read_asks_policy_or_is_named_public.rs`)
//! in the same shape, for the next module read on the item's list.
//!
//! WHY IT EXISTS. The OSS README says a Basic guest — the `visitor`
//! role — reads only what policy grants it. `GET /api/customers` and
//! `GET /api/customers/{id}` asked nobody, so that guest read every
//! DTC buyer's name, email and phone. They now ask `Read` on
//! `customer` at scope `all` (a buyer has no owner, account team, team
//! or department a narrower grant could match); this holds the NEXT read
//! to the rule.
//!
//! THE ROUTE LIST IS DERIVED, NOT TYPED (CLAUDE.md §9a). It is read out
//! of `pub fn router<` in `src/http.rs`, the router the service binary
//! merges as `router(state)`: every `.route(` whose method router
//! carries a `get(`. The role-report door the binary also merges is
//! core's (`boss_policy_client::role_inventory`), asks `policy-rule`,
//! and is pinned by the binary's own test.
//!
//! THE JUDGEMENT IS MEASURED, TWICE. Each route is requested (1) with
//! no identity at all, against a policy that denies everything and
//! counts every question — it passes when it refuses (401/403) or
//! asked — and (2) as a Basic guest session, against the core default
//! rules — it passes when it does not answer 2xx. Anything else must
//! sit on [`PUBLIC`] (sessionless by design, with the reason). And the
//! check must not refuse everyone: a grant at `all` reads, the core
//! defaults' deploy superuser and auditor read, and a narrower grant
//! is refused rather than handed a row its scope cannot name.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use async_trait::async_trait;
use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use boss_customers::InMemoryCustomers;
use boss_customers::http::{CustomersApiState, router};
use boss_customers::port::CustomersRepository;
use boss_customers::types::Customer;
use boss_policy_client::{
    Action, Decision, FakePolicyClient, PolicyClient, PolicyClientError, Predicate, Resource,
    Scope, User,
};
use tower::ServiceExt;

/// Sessionless by design, on every instance.
const PUBLIC: &[(&str, &str)] = &[(
    "/api/customers/health",
    "liveness; the machine gate exempts it for the same reason",
)];

const CUSTOMER: &str = "cust-read-pin";

/// A tenant role granted the customer read at `all`, as a tenant's
/// `policy_rules.toml` would grant it.
const READER: &str = "customer-reader";
/// A tenant role granted it only at `team` — a scope no buyer row can
/// be matched by. (Any scope narrower than `all` is refused alike; the
/// account-team scope is not used here because its policy name is a
/// word the factory taxonomy retired, infra/lint/factory-vocabulary.sh.)
const SCOPED: &str = "team-reader";

// ---------------------------------------------------------------------------
// The route list, derived from the router source the binary merges
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

/// Every GET path the service binary serves from this crate's router.
fn derived_get_routes() -> BTreeSet<String> {
    let bin = crate_file("src/bin/boss_customers_api.rs");
    assert!(
        bin.contains("router(state)"),
        "the binary no longer merges the domain router as `router(state)` — re-derive this \
         pin's source"
    );
    let file = "src/http.rs";
    let src = crate_file(file);
    let routes = routes_in(fn_body(&src, "pub fn router<", file), file);
    assert!(!routes.is_empty(), "{file}: read no routes from the router");
    routes
        .into_iter()
        .filter(|(_, get)| *get)
        .map(|(p, _)| p)
        .collect()
}

#[test]
fn the_reader_finds_the_routes_the_binary_serves() {
    let gets = derived_get_routes();
    for known in ["/api/customers", "/api/customers/{id}"] {
        assert!(gets.contains(known), "derived list lost {known}: {gets:#?}");
    }
    assert!(
        gets.len() >= 3,
        "only {} GET routes derived: {gets:#?}",
        gets.len()
    );
}

// ---------------------------------------------------------------------------
// The router, over one seeded buyer
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

/// The core defaults, plus the two tenant grants above.
fn default_rules() -> FakePolicyClient {
    boss_policy_client::defaults::default_rules()
        .into_iter()
        .fold(FakePolicyClient::builder(), |b, r| {
            b.allow(r.role, r.action, r.resource, r.scope)
        })
        .allow(READER, Action::Read, Resource::customer(), Scope::All)
        .allow(SCOPED, Action::Read, Resource::customer(), Scope::Team)
        .build()
}

async fn app(policy: Arc<dyn PolicyClient>) -> Router {
    let customers = Arc::new(InMemoryCustomers::new());
    customers
        .create_customer_at(
            &Customer {
                id: CUSTOMER.to_string(),
                name: "Read Pin".to_string(),
                email: Some("read-pin@example.test".to_string()),
                phone: Some("555-0100".to_string()),
                metadata: serde_json::json!({}),
                created_at: None,
            },
            chrono::Utc::now(),
        )
        .await
        .expect("seed the buyer the reads ask for");
    router(CustomersApiState {
        customers,
        clock: Arc::new(boss_clock_client::WallClockClient),
        policy,
    })
}

/// A concrete path for a route pattern: the seeded buyer for an id.
fn concrete(pattern: &str) -> String {
    pattern
        .split('/')
        .map(
            |seg| match seg.strip_prefix('{').and_then(|s| s.strip_suffix('}')) {
                Some(_) => CUSTOMER.to_string(),
                None => seg.to_string(),
            },
        )
        .collect::<Vec<_>>()
        .join("/")
}

/// The header the gateway forwards for a session in `role`.
fn session(id: &str, role: &str) -> String {
    serde_json::json!({
        "id": id,
        "role": role,
        "access_tier": "user",
        "territory_account_ids": [],
        "direct_report_ids": [],
        "department": null,
    })
    .to_string()
}

/// The header the gateway's `build_user_json` forwards for a guest
/// session on a Basic install.
fn basic_guest() -> String {
    session(
        boss_core::roles::GUEST_EMAIL,
        boss_core::roles::VISITOR_ROLE,
    )
}

async fn get(app: Router, path: &str, user: Option<&str>) -> (StatusCode, String) {
    let mut req = Request::builder().method("GET").uri(path);
    if let Some(user) = user {
        req = req.header("x-boss-user", user);
    }
    let resp = app.oneshot(req.body(Body::empty()).unwrap()).await.unwrap();
    let status = resp.status();
    let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
        .await
        .unwrap();
    (status, String::from_utf8_lossy(&body).into_owned())
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

#[tokio::test]
async fn every_customers_read_asks_policy_or_is_named_public() {
    let gets = derived_get_routes();
    let public: BTreeMap<&str, &str> = PUBLIC.iter().copied().collect();
    let mut failures = Vec::new();
    for name in public.keys() {
        if !gets.contains(*name) {
            failures.push(format!(
                "{name}: listed, but the router serves no such GET — delete its row"
            ));
        }
    }
    for route in &gets {
        let asked = Arc::new(AtomicUsize::new(0));
        let deny: Arc<dyn PolicyClient> = Arc::new(Recording {
            inner: FakePolicyClient::deny_all(),
            asked: asked.clone(),
        });
        // No header of any kind: what a caller reaching the port
        // directly sends.
        let (anonymous, _) = get(app(deny).await, &concrete(route), None).await;
        let (guest, _) = get(
            app(Arc::new(default_rules())).await,
            &concrete(route),
            Some(&basic_guest()),
        )
        .await;
        let answer = Answer {
            anonymous,
            asked: asked.load(Ordering::SeqCst),
            guest,
        };
        match (answer.checked(), public.get(route.as_str())) {
            (true, None) | (false, Some(_)) => {}
            (true, Some(_)) => failures.push(format!(
                "{route}: asks policy now ({answer:?}) — it is not public; delete its PUBLIC row"
            )),
            (false, None) => failures.push(format!(
                "{route}: answered {answer:?} — a read must refuse a caller with no identity or \
                 ask policy, and must not answer a Basic guest on the core defaults \
                 (controls::READ_CUSTOMER is the question); or name it PUBLIC with the reason \
                 it holds on every instance"
            )),
        }
    }
    assert!(
        failures.is_empty(),
        "{} of {} customers-API GET routes break the rule:\n  {}",
        failures.len(),
        gets.len(),
        failures.join("\n  ")
    );
}

/// The other half: the check must not refuse everyone. A grant at
/// `all` reads both doors; so do the core defaults' deploy superuser
/// and auditor (customer is a shipped resource), which is what the
/// operator's session and a recorded probe's reader carry.
#[tokio::test]
async fn a_grant_at_all_reads_the_buyers() {
    for role in [READER, "platform-admin", "audit-readonly"] {
        let user = session("emp-read-pin", role);
        let (status, body) = get(
            app(Arc::new(default_rules())).await,
            "/api/customers",
            Some(&user),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{role} lists: {body}");
        assert!(body.contains(CUSTOMER), "{role} lists the buyer: {body}");
        let (status, body) = get(
            app(Arc::new(default_rules())).await,
            &format!("/api/customers/{CUSTOMER}"),
            Some(&user),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{role} reads one: {body}");
        assert!(body.contains("read-pin@example.test"), "{role}: {body}");
    }
}

/// A grant narrower than `all` names no buyer — a buyer has no owner,
/// account team, team or department — so it is refused, saying so, rather
/// than handed the rows or an empty list that reads as "no buyers".
#[tokio::test]
async fn a_grant_narrower_than_all_is_refused_saying_why() {
    let user = session("emp-scoped-pin", SCOPED);
    for path in [
        "/api/customers".to_string(),
        format!("/api/customers/{CUSTOMER}"),
    ] {
        let (status, body) = get(app(Arc::new(default_rules())).await, &path, Some(&user)).await;
        assert_eq!(status, StatusCode::FORBIDDEN, "{path}: {body}");
        assert!(
            body.contains("scope all") && !body.contains(CUSTOMER),
            "{path}: the refusal names the scope it needs and carries no buyer: {body}"
        );
    }
}
