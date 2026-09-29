//! Every ledger write door asks policy for a write on `ledger` (backlog
//! 34f0a954, 2026-09-28).
//!
//! Measured at origin/main 60dd439c: the doors that post to the books —
//! journal entries, bank settlements, bills, payroll, deposit
//! settlements, revenue schedules, COGS and inventory facts, tax filings
//! and accruals, tax-rate schedules, posting and projection rules, and
//! period create —
//! checked `reject_if_auditor`, which refuses the role string "auditor".
//! No seed, fixture or core role carries it: the platform's auditor is
//! `audit-readonly`. So the only check on those doors was the router-wide
//! `ledger` READ layer, and every Read holder wrote the books — the
//! `smoke-tester` fixture role, which is not on the gateway's read-only
//! floor, among them.
//!
//! Each of them now asks Create or Update on `ledger`, but for four
//! (period create asks Create on `ledger-period`, the resource closing
//! and reopening a period ask; the posting and projection rules ask
//! Create on `posting-rule` and the excise rate upsert Create on
//! `tax-regime`, backlog 432f0eb4 — the operating model's registries,
//! not the books), each through the
//! registry-write ladder (`boss_policy_client::writes::
//! require_registry_write`): no caller is 401, a deny or a grant narrower
//! than `all` is 403, a policy service that cannot answer is 503. The ask
//! is an extractor that runs before the body is read, so a refusal needs
//! no well-formed body and says nothing about one.
//!
//! The doors are read off the router's own source, so a write route
//! added later is swept here without anyone listing it. No database: the
//! pool is lazy and never connected, and a call the gate ADMITS is seen
//! as whatever the handler answers next (a 422 for a `{}` body, the lazy
//! pool's 500) — never as a 401 or a 403.

use std::sync::Arc;

use axum::body::Body;
use axum::http::{Method, Request, StatusCode};
use boss_ledger::http::{LedgerApiState, router};
use boss_policy_client::{Action, FakePolicyClient, PolicyClient, Resource, Scope};
use serde_json::{Value, json};
use tower::ServiceExt;

/// The router, as written. Its `.route(...)` calls are the one list of
/// doors; the sweep below reads them rather than keeping a second copy.
const ROUTER_SOURCE: &str = include_str!("../src/http.rs");

/// A POST that READS: it sums facts for a caller that already holds the
/// `ledger` read grant, and writes nothing.
const READ_BY_POST: &[&str] = &["/api/ledger/financial-facts/sum"];

/// The write doors that ask policy about a resource of their own rather
/// than `ledger`: creating a fiscal year (Create), closing and reopening
/// a period and the year-end close (Close/Update) on `ledger-period`
/// (25a4f7f9; period create joined it in this car, so one grant covers a
/// year's whole life), the chart and tax declarations (Create on
/// `ledger-account` / `tax-regime`, 59deda40), and the three registry
/// doors [`registry_doors`] names (432f0eb4). The period doors are
/// pinned by tests/a_period_lock_asks_policy.rs and
/// [`period_create_asks_create_on_ledger_period`] below, the chart and
/// tax doors by http_api's chart/tax batch tests, the registry doors by
/// [`the_rule_and_rate_registries_ask_create_on_their_own_resource`].
const DOORS_ON_THEIR_OWN_RESOURCE: &[&str] = &[
    "/api/ledger/periods",
    "/api/ledger/periods/{id}/lock",
    "/api/ledger/periods/{id}/unlock",
    "/api/ledger/periods/{id}/close",
    "/api/ledger/accounts/batch",
    "/api/ledger/tax/batch",
    "/api/ledger/posting-rules/batch",
    "/api/ledger/fact-projection-rules/batch",
    "/api/ledger/excise-rate-schedules",
];

/// The doors that write how the books are KEPT rather than what is in
/// them (backlog 432f0eb4, 2026-09-28): the posting and projection rule
/// registries, and the excise rate schedules the tax accrual reads. Each
/// rode the `ledger` grant a tenant gives its finance leads — the
/// brewery's controller and cfo hold Create and Update on it — and the
/// posting path takes the NEWEST rule version of a fact kind
/// (`load_newest_rule_in_tx`), so a finance lead could publish version
/// N+1 and redirect every later automated posting without writing an
/// entry. Each now asks Create on the registry it writes: the rules on
/// `posting-rule`; the rate schedule on `tax-regime`, with the tenant's
/// filing kinds and sales-tax rates, as Create because the door is an
/// upsert and an Update-only grant must not add a row.
fn registry_doors() -> Vec<(Method, &'static str, Resource)> {
    vec![
        (
            Method::POST,
            "/api/ledger/posting-rules/batch",
            Resource::posting_rule(),
        ),
        (
            Method::POST,
            "/api/ledger/fact-projection-rules/batch",
            Resource::posting_rule(),
        ),
        (
            Method::PUT,
            "/api/ledger/excise-rate-schedules",
            Resource::tax_regime(),
        ),
    ]
}

/// How many write routes the router serves, at equality: a door added
/// or removed is a change to this car's sweep that someone must read.
const WRITE_ROUTES: usize = 29;

/// The router's body, from `pub fn router(` to its closing brace.
fn router_fn() -> &'static str {
    let start = ROUTER_SOURCE
        .find("pub fn router(")
        .expect("http.rs defines pub fn router");
    let body = &ROUTER_SOURCE[start..];
    let end = body
        .find("\n}\n")
        .expect("pub fn router has a closing brace");
    &body[..end]
}

/// The doors that change a row already on the books — settle, sweep,
/// pay, remit, supersede — and so ask Update. Every other door of this
/// car's ADDS a row and asks Create.
const UPDATE_DOORS: &[&str] = &[
    "/api/ledger/financial-facts/{id}/supersede",
    "/api/ledger/bank-settlements/{id}/settle",
    "/api/ledger/bank-settlements/sweep",
    "/api/ledger/tax-filings/{id}/remit",
    "/api/ledger/bills/pay-run",
    "/api/ledger/bills/{id}/pay",
];

/// Every (method, path) the router serves with a method other than GET.
/// Read from `.route(...)` calls alone, which is sound only while the
/// router has no other way to add a route — pinned in
/// [`the_sweep_reads_every_write_door_off_the_router`].
fn write_routes() -> Vec<(Method, &'static str)> {
    let mut routes = Vec::new();
    for chunk in router_fn().split(".route(").skip(1) {
        let Some(path) = chunk.split('"').nth(1) else {
            continue;
        };
        for (needle, method) in [
            ("post(", Method::POST),
            ("put(", Method::PUT),
            ("patch(", Method::PATCH),
            ("delete(", Method::DELETE),
        ] {
            if chunk.contains(&format!("::{needle}")) || chunk.contains(&format!(".{needle}")) {
                routes.push((method, path));
            }
        }
    }
    routes
}

/// The doors that ask a write on `ledger`: every write route but the
/// read-by-POST and the doors on their own resource.
fn this_cars_doors() -> Vec<(Method, &'static str)> {
    write_routes()
        .into_iter()
        .filter(|(_, p)| !READ_BY_POST.contains(p) && !DOORS_ON_THEIR_OWN_RESOURCE.contains(p))
        .collect()
}

fn surface(policy: Arc<dyn PolicyClient>) -> axum::Router {
    // Never connected: a refusal answers before a handler touches it,
    // and a call the gate admits gives up on it in a second.
    let pool = sqlx::postgres::PgPoolOptions::new()
        .acquire_timeout(std::time::Duration::from_secs(1))
        .connect_lazy("postgres://nobody@127.0.0.1:1/none")
        .unwrap();
    router(LedgerApiState {
        pool,
        publisher: None,
        clock: Arc::new(boss_clock_client::WallClockClient),
        policy,
    })
}

fn signed(id: &str, role: &str) -> Value {
    json!({
        "id": id,
        "role": role,
        "access_tier": "user",
        "territory_account_ids": [],
        "direct_report_ids": [],
        "department": "finance",
    })
}

async fn send(
    app: axum::Router,
    method: &Method,
    path: &str,
    user: Option<&Value>,
) -> (StatusCode, String) {
    let uri = path.replace("{id}", "00000000-0000-0000-0000-000000000000");
    let mut req = Request::builder()
        .method(method.clone())
        .uri(uri)
        .header("content-type", "application/json");
    if let Some(u) = user {
        req = req.header("x-boss-user", u.to_string());
    }
    let resp = app
        .oneshot(req.body(Body::from("{}")).unwrap())
        .await
        .unwrap();
    let status = resp.status();
    let bytes = http_body_util::BodyExt::collect(resp.into_body())
        .await
        .unwrap()
        .to_bytes();
    (status, String::from_utf8_lossy(&bytes).into_owned())
}

fn assert_admitted(status: StatusCode, body: &str, what: &str) {
    assert!(
        status != StatusCode::UNAUTHORIZED && status != StatusCode::FORBIDDEN,
        "{what}: expected the write gate to admit, got {status}: {body}"
    );
}

/// The sweep can only be as good as its parse: an empty or shrunken
/// list would pass every assertion below by asserting nothing.
#[test]
fn the_sweep_reads_every_write_door_off_the_router() {
    // The parse reads `.route(` calls; a route added any other way — a
    // merged or nested router, a service, a `MethodRouter::on` — would
    // never reach the sweep, so the router may use none of them.
    let router = router_fn();
    for other_way in [".merge(", ".nest(", "_service(", "::on(", ".on("] {
        assert!(
            !router.contains(other_way),
            "pub fn router adds a route with `{other_way}`, which write_routes cannot read"
        );
    }
    let routes = write_routes();
    assert_eq!(
        routes.len(),
        WRITE_ROUTES,
        "the ledger router's write routes changed: {routes:#?}"
    );
    assert_eq!(
        this_cars_doors().len(),
        WRITE_ROUTES - READ_BY_POST.len() - DOORS_ON_THEIR_OWN_RESOURCE.len(),
        "a table above names a path the router does not serve"
    );
    for known in [
        (Method::POST, "/api/ledger/journal-entries"),
        (Method::POST, "/api/ledger/bank-settlements"),
        (Method::POST, "/api/ledger/periods"),
        (Method::PUT, "/api/ledger/excise-rate-schedules"),
        (Method::POST, "/api/ledger/bills/{id}/pay"),
    ] {
        assert!(routes.contains(&known), "the parse missed {known:?}");
    }
    // Every name in the tables above is a real route, so a renamed door
    // cannot fall out of the sweep by keeping its old spelling here.
    let paths: Vec<&str> = routes.iter().map(|(_, p)| *p).collect();
    for p in READ_BY_POST
        .iter()
        .chain(DOORS_ON_THEIR_OWN_RESOURCE)
        .chain(UPDATE_DOORS)
    {
        assert!(paths.contains(p), "{p} is not a write route any more");
    }
    for (method, path, _) in registry_doors() {
        assert!(
            routes.contains(&(method.clone(), path)),
            "{method} {path} is not a write route any more"
        );
        assert!(
            DOORS_ON_THEIR_OWN_RESOURCE.contains(&path),
            "{path} is a registry door, so the ledger sweep must not judge it"
        );
    }
}

/// The reachable case (triage of 34f0a954): `smoke-tester` holds `ledger`
/// Read by the SHIPPED defaults and is not on the gateway's read-only
/// floor, so its session reaches these doors with a real grant. It must
/// be refused 403 on every one — and so must the external auditor.
#[tokio::test(flavor = "multi_thread")]
async fn a_ledger_reader_is_refused_403_on_every_write_door() {
    let app = surface(Arc::new(
        FakePolicyClient::builder().with_default_rules().build(),
    ));
    for (id, role) in [
        ("emp-smoke", "smoke-tester"),
        ("emp-audit", "audit-readonly"),
    ] {
        let who = signed(id, role);
        for (method, path) in this_cars_doors() {
            let (status, body) = send(app.clone(), &method, path, Some(&who)).await;
            assert_eq!(
                status,
                StatusCode::FORBIDDEN,
                "{method} {path} admitted {role}, which holds only a ledger read grant: {body}"
            );
        }
    }
}

/// Judged against the grants that SHIP: the deploy superuser — the
/// finance page's operator today, and what the dispatcher's rules, the
/// tenant seed and a tenant engine's prepare sign as — passes every door.
#[tokio::test(flavor = "multi_thread")]
async fn the_shipped_defaults_admit_platform_admin_on_every_door() {
    let app = surface(Arc::new(
        FakePolicyClient::builder().with_default_rules().build(),
    ));
    let admin = signed("emp-david", "platform-admin");
    for (method, path) in this_cars_doors() {
        let (status, body) = send(app.clone(), &method, path, Some(&admin)).await;
        assert_admitted(status, &body, &format!("platform-admin on {method} {path}"));
    }
}

/// No caller, no write: every fact names its author, and an unsigned
/// request has none. These doors used to credit it to
/// `automation:platform`.
#[tokio::test(flavor = "multi_thread")]
async fn an_unsigned_write_is_refused_401() {
    let app = surface(Arc::new(boss_policy_client::PermissivePolicyClient));
    for (method, path) in this_cars_doors() {
        let (status, body) = send(app.clone(), &method, path, None).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED, "{method} {path}: {body}");
    }
}

/// The verb each door asks: a role granted Create alone passes the doors
/// that add a row and is refused the ones that change one, and a role
/// granted Update alone the reverse.
#[tokio::test(flavor = "multi_thread")]
async fn a_door_that_adds_a_row_asks_create_and_one_that_changes_a_row_asks_update() {
    for (granted, other) in [
        (Action::Create, Action::Update),
        (Action::Update, Action::Create),
    ] {
        let policy = FakePolicyClient::builder()
            .allow("bookkeeper", Action::Read, Resource::ledger(), Scope::All)
            .allow("bookkeeper", granted, Resource::ledger(), Scope::All)
            .build();
        let app = surface(Arc::new(policy));
        let who = signed("emp-books", "bookkeeper");
        for (method, path) in this_cars_doors() {
            let asks = if UPDATE_DOORS.contains(&path) {
                Action::Update
            } else {
                Action::Create
            };
            let (status, body) = send(app.clone(), &method, path, Some(&who)).await;
            if asks == granted {
                assert_admitted(status, &body, &format!("{granted:?} on {method} {path}"));
            } else {
                assert_eq!(
                    status,
                    StatusCode::FORBIDDEN,
                    "{method} {path} asks {other:?} but admitted a {granted:?}-only grant: {body}"
                );
            }
        }
    }
}

/// The books belong to no person and no department, so a grant narrower
/// than `all` cannot be shown to reach them.
#[tokio::test(flavor = "multi_thread")]
async fn a_department_grant_reaches_no_ledger_write() {
    let dept = Scope::Department("finance".into());
    let policy = FakePolicyClient::builder()
        .allow("bookkeeper", Action::Read, Resource::ledger(), Scope::All)
        .allow(
            "bookkeeper",
            Action::Create,
            Resource::ledger(),
            dept.clone(),
        )
        .allow("bookkeeper", Action::Update, Resource::ledger(), dept)
        .build();
    let app = surface(Arc::new(policy));
    let who = signed("emp-books", "bookkeeper");
    for (method, path) in this_cars_doors() {
        let (status, body) = send(app.clone(), &method, path, Some(&who)).await;
        assert_eq!(status, StatusCode::FORBIDDEN, "{method} {path}: {body}");
    }
}

/// Creating a fiscal year is Create on `ledger-period`, the resource
/// closing and reopening one already ask (25a4f7f9), so one grant covers
/// a year's whole life: a `ledger` Create grant alone does not open it,
/// and neither does a ledger reader's.
#[tokio::test(flavor = "multi_thread")]
async fn period_create_asks_create_on_ledger_period() {
    let path = "/api/ledger/periods";
    let policy = FakePolicyClient::builder()
        .allow("bookkeeper", Action::Read, Resource::ledger(), Scope::All)
        .allow("bookkeeper", Action::Create, Resource::ledger(), Scope::All)
        .allow("controller", Action::Read, Resource::ledger(), Scope::All)
        .allow(
            "controller",
            Action::Create,
            Resource::ledger_period(),
            Scope::All,
        )
        .build();
    let app = surface(Arc::new(policy));
    let (status, body) = send(
        app.clone(),
        &Method::POST,
        path,
        Some(&signed("emp-books", "bookkeeper")),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::FORBIDDEN,
        "a ledger Create grant opened period create: {body}"
    );
    let (status, body) = send(
        app,
        &Method::POST,
        path,
        Some(&signed("emp-ctl", "controller")),
    )
    .await;
    assert_admitted(status, &body, "Create on ledger-period");

    let shipped = surface(Arc::new(
        FakePolicyClient::builder().with_default_rules().build(),
    ));
    for (id, role) in [
        ("emp-smoke", "smoke-tester"),
        ("emp-audit", "audit-readonly"),
    ] {
        let (status, body) = send(
            shipped.clone(),
            &Method::POST,
            path,
            Some(&signed(id, role)),
        )
        .await;
        assert_eq!(status, StatusCode::FORBIDDEN, "{role}: {body}");
    }
    let (status, body) = send(
        shipped.clone(),
        &Method::POST,
        path,
        Some(&signed("emp-david", "platform-admin")),
    )
    .await;
    assert_admitted(status, &body, "platform-admin on period create");
    // Unsigned: past a read layer that admits it (the shipped one refuses
    // the anonymous caller 403 first), the write still has no author.
    let permissive = surface(Arc::new(boss_policy_client::PermissivePolicyClient));
    let (status, body) = send(permissive, &Method::POST, path, None).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED, "unsigned: {body}");
}

/// The rule and rate registries ask Create on their own resource
/// ([`registry_doors`], backlog 432f0eb4): the brewery's finance-lead
/// grant — Read, Create and Update on `ledger`, scope all, what
/// examples/brewery/seeds/policy_rules.toml gives controller and cfo —
/// publishes no rule and writes no rate; Create on the door's resource
/// does; Update on it alone does not, because the rate door is an upsert
/// and an Update-only grant must not add a row. Under the SHIPPED
/// defaults the ledger readers are refused and platform-admin — the role
/// `boss tenant publish` (automation:tenant-seed) and the brewery's
/// prepare (automation:brewery-seed) sign as — is admitted. Unsigned,
/// there is no author.
#[tokio::test(flavor = "multi_thread")]
async fn the_rule_and_rate_registries_ask_create_on_their_own_resource() {
    let finance_lead = surface(Arc::new(
        FakePolicyClient::builder()
            .with_default_rules()
            .allow("controller", Action::Read, Resource::ledger(), Scope::All)
            .allow("controller", Action::Create, Resource::ledger(), Scope::All)
            .allow("controller", Action::Update, Resource::ledger(), Scope::All)
            .build(),
    ));
    let controller = signed("emp-ctl", "controller");
    let shipped = surface(Arc::new(
        FakePolicyClient::builder().with_default_rules().build(),
    ));
    let permissive = surface(Arc::new(boss_policy_client::PermissivePolicyClient));

    for (method, path, resource) in registry_doors() {
        let (status, body) = send(finance_lead.clone(), &method, path, Some(&controller)).await;
        assert_eq!(
            status,
            StatusCode::FORBIDDEN,
            "{method} {path} admitted a ledger Create+Update grant; it asks Create on {resource}: {body}"
        );

        for (granted, admits) in [(Action::Create, true), (Action::Update, false)] {
            let policy = FakePolicyClient::builder()
                .allow("keeper", Action::Read, Resource::ledger(), Scope::All)
                .allow("keeper", granted, resource.clone(), Scope::All)
                .build();
            let (status, body) = send(
                surface(Arc::new(policy)),
                &method,
                path,
                Some(&signed("emp-keeper", "keeper")),
            )
            .await;
            if admits {
                assert_admitted(
                    status,
                    &body,
                    &format!("{granted:?} on {resource}: {method} {path}"),
                );
            } else {
                assert_eq!(
                    status,
                    StatusCode::FORBIDDEN,
                    "{method} {path} admitted {granted:?} on {resource} alone: {body}"
                );
            }
        }

        for (id, role) in [
            ("emp-smoke", "smoke-tester"),
            ("emp-audit", "audit-readonly"),
        ] {
            let (status, body) =
                send(shipped.clone(), &method, path, Some(&signed(id, role))).await;
            assert_eq!(
                status,
                StatusCode::FORBIDDEN,
                "{role} on {method} {path}: {body}"
            );
        }
        for seed in [
            "emp-david",
            "automation:tenant-seed",
            "automation:brewery-seed",
        ] {
            let (status, body) = send(
                shipped.clone(),
                &method,
                path,
                Some(&signed(seed, "platform-admin")),
            )
            .await;
            assert_admitted(
                status,
                &body,
                &format!("{seed} (platform-admin) on {method} {path}"),
            );
        }

        let (status, body) = send(permissive.clone(), &method, path, None).await;
        assert_eq!(
            status,
            StatusCode::UNAUTHORIZED,
            "unsigned {method} {path}: {body}"
        );
    }
}

/// The tree pin: no HTTP handler in boss-ledger reads the caller's role
/// at all — a write door asks policy, and the role is policy's input,
/// never a handler's. The last shape that did, `reject_if_auditor`,
/// refused the role string "auditor", which no seed, fixture or core role
/// carries (the platform's auditor is `audit-readonly`), so for as long
/// as it existed it refused nobody (backlog 34f0a954); its last five
/// calls sat on doors that ask policy about their own resource, and it
/// was deleted with them (backlog 432f0eb4). So the pin is absolute: no
/// `.role` read in code, and no function of that name to call.
#[test]
fn no_ledger_http_handler_compares_a_role_string() {
    let dir = boss_testing::repo_root().join("crates/modules/boss-ledger/src");
    let mut files = vec![dir.join("http.rs")];
    let mut handlers: Vec<_> = std::fs::read_dir(dir.join("http"))
        .unwrap()
        .map(|e| e.unwrap().path())
        .filter(|p| p.extension().is_some_and(|x| x == "rs"))
        .collect();
    handlers.sort();
    boss_testing::assert_roster_floor!(
        handlers,
        12,
        "boss-ledger's http modules (14 on 2026-09-28)"
    );
    files.extend(handlers);

    let mut offenders = Vec::new();
    for file in &files {
        let name = file.file_name().unwrap().to_string_lossy().into_owned();
        let text = std::fs::read_to_string(file).unwrap();
        for (n, line) in text.lines().enumerate() {
            let code = line.split("//").next().unwrap_or("");
            if reads_a_role(code) || code.contains("reject_if_auditor") {
                offenders.push(format!("{name}:{}: {}", n + 1, line.trim()));
            }
        }
    }
    assert!(
        offenders.is_empty(),
        "a ledger handler decides by the caller's role — a write door asks policy \
         (super::LedgerCreate / super::LedgerUpdate, or its own resource), never a role:\n{}",
        offenders.join("\n")
    );
}

/// `.role` as a whole field — `user.role == ...`, `&u.role`, `.role.as_str()`
/// — and not `.roles` or `.role_id`.
fn reads_a_role(code: &str) -> bool {
    code.match_indices(".role").any(|(i, m)| {
        !code[i + m.len()..]
            .chars()
            .next()
            .is_some_and(|c| c.is_alphanumeric() || c == '_')
    })
}

/// The pin's own detector, on the shapes it must and must not see — so
/// a green pin is a real absence, not a matcher that sees nothing.
#[test]
fn the_role_pin_sees_a_role_read_and_nothing_else() {
    for seen in [
        "    if user.role == \"auditor\" {",
        "    if user.role.as_str() != \"x\" {",
        "    matches!(&u.role, r)",
        "    let r = user.role;",
    ] {
        assert!(reads_a_role(seen), "missed: {seen}");
    }
    for unseen in [
        "    let ids = user.roles.clone();",
        "    let id = row.role_id;",
        "    let tier = user.access_tier;",
    ] {
        assert!(!reads_a_role(unseen), "false positive: {unseen}");
    }
}
