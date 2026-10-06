//! `GET /api/policy/coverage` — every control, the real people who hold
//! it, and every control nobody holds (design 1c4e42e1; backlog
//! 47aed706).
//!
//! The judgement is `boss_policy_client::coverage::coverage`, one pure
//! function; this file is the reads it needs. Rules and overrides are
//! this service's own table. The roster, each person's passkeys and the
//! active workflows live in other services, so they arrive through the
//! [`CoverageSources`] port — the people API and the jobs API over HTTP
//! in production ([`HttpCoverageSources`]), fixed values in a test.
//!
//! A READ THAT CANNOT BE TRUSTED WHOLE IS REFUSED, NEVER REPORTED. A dark
//! people service answering an empty roster would make every control an
//! orphan, and a backstop reading that would file an alarm per control —
//! a wrong target answers instead of erroring (CLAUDE.md §Doors). So a
//! source that fails is a 502 naming it, and an empty roster or an empty
//! workflow registry is a 502 too: every instance has its founder and
//! its platform protocols, so empty is a dark read, not a fact.
//!
//! WHO SEES WHAT (review of this car, M1). Who holds each control, and
//! who is a real person at all, is the authority map by name — a
//! targeting map — so it goes only to a caller who may read the rule
//! table itself (`http::may_read_rule_table`, the same judgement as
//! `GET /api/policy/rules`). Any other named caller sees the orphans
//! alone: the gaps, with no holder named. Refused outright is exactly
//! what that rule-table read refuses by identity (re-review L6): the
//! anonymous visitor's ids (`ANONYMOUS_VISITOR_IDS` — the guest session
//! in any role) and the identity-less `guest` role, since the gateway
//! proxies `/api/policy/{*rest}` for every session. The seeded auditor
//! (`audit-readonly`, its own id) reads the rule table, so it reads the
//! holders too.
//! The design said "readable by anyone"; the review narrowed it, and
//! the gap list stays visible to every signed-in person.
//!
//! IT REFUSES NOTHING ELSE. Nothing here blocks a write; the guards that
//! will ask this function before a policy, people or publish write wait
//! for DR readiness (62dac114).

use std::sync::Arc;

use async_trait::async_trait;
use axum::extract::State;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::{Json, Router};
use serde_json::{Value, json};

use boss_policy_client::CurrentUser;
use boss_policy_client::coverage::{self, Key, Person, TIER_COUNTS_PATH, WorkflowFacts};
use boss_policy_client::engine::PolicyEngine;
use boss_policy_client::port::PolicyRepository;
use boss_policy_client::types::{AccessTier, UserOverride};

/// The path this router answers.
pub const COVERAGE_PATH: &str = "/api/policy/coverage";

/// What the read does not enumerate, said on every answer so an empty
/// `orphans` is never read as "everything is covered" past its edge
/// (design 1c4e42e1, "Limits, stated").
pub const UNMEASURED: [&str; 6] = [
    "resources only data names — a View's source (boss-views) — are not enumerated; every \
     other door asks through a declared control (boss_policy_client::controls)",
    "the readers of /api/credentials entries are not enumerated: nothing maps a credential to \
     the door that reads it",
    "per-kind packet grants (create on job:<kind>) are covered by create on job, the all-kinds \
     grant the open door takes first",
    "break-glass binding is the recovery path, watched by its own alarm \
     (boss-gateway break_glass_alarm.rs), never a control a person holds",
    "packets pinned to a superseded workflow version are not read: the controls come from the \
     ACTIVE versions, so a sign-off or authority role an older version still asks of its \
     in-flight packets, and the active one dropped, is not measured",
    "a BOSS_PLATFORM_OWNER override is not read, and need not be: elevation does not honour \
     it (it changes only who dispatcher alarms are filed to), so the owner is the roster's \
     first hire among the active platform-admins, here as at the gateway",
];

/// The three reads coverage needs from other services. Each answers the
/// whole set or an error naming what it could not read.
#[async_trait]
pub trait CoverageSources: Send + Sync {
    /// The active roster.
    async fn roster(&self) -> Result<Vec<Person>, String>;
    /// Every bound passkey's holder and tier.
    async fn keys(&self) -> Result<Vec<Key>, String>;
    /// The active workflows.
    async fn workflows(&self) -> Result<Vec<WorkflowFacts>, String>;
}

pub struct CoverageApiState<R: PolicyRepository> {
    pub repo: Arc<R>,
    pub sources: Arc<dyn CoverageSources>,
}

pub fn router<R: PolicyRepository + 'static>(state: CoverageApiState<R>) -> Router {
    Router::new()
        .route(COVERAGE_PATH, get(read_coverage::<R>))
        .with_state(Arc::new(state))
}

fn dark(source: &str, why: impl std::fmt::Display) -> Response {
    (
        StatusCode::BAD_GATEWAY,
        Json(json!({
            "error": format!("coverage cannot be judged: {source} {why}"),
            "source": source,
            "hint": "a read that cannot be trusted whole is refused rather than reported — \
                     an empty roster would read as every control orphaned",
        })),
    )
        .into_response()
}

async fn read_coverage<R: PolicyRepository + 'static>(
    State(state): State<Arc<CoverageApiState<R>>>,
    CurrentUser(user): CurrentUser,
) -> Response {
    if user.is_anonymous() {
        return (
            StatusCode::UNAUTHORIZED,
            "the coverage read is read by a named caller; this request names none",
        )
            .into_response();
    }
    if boss_core::roles::ANONYMOUS_VISITOR_IDS.contains(&user.id.as_str()) {
        return (
            StatusCode::FORBIDDEN,
            format!(
                "{} ({}) is an anonymous visitor, and the coverage read is the instance's own \
                 map of who can act",
                user.id, user.role
            ),
        )
            .into_response();
    }
    // Judged before any source is read: the answer's shape depends on it.
    let engine = PolicyEngine::new(state.repo.clone());
    let names_holders = crate::http::may_read_rule_table(&engine, &user)
        .await
        .is_ok();
    let roster: Vec<Person> = match state.sources.roster().await {
        Ok(r) => r.into_iter().filter(|p| p.active).collect(),
        Err(e) => return dark("the people roster", e),
    };
    if roster.is_empty() {
        return dark("the people roster", "answered no active employee");
    }
    let workflows = match state.sources.workflows().await {
        Ok(w) if w.is_empty() => return dark("the workflow registry", "answered no workflow"),
        Ok(w) => w,
        Err(e) => return dark("the workflow registry", e),
    };
    let keys = match state.sources.keys().await {
        Ok(k) => k,
        Err(e) => return dark("the passkey tier counts", e),
    };
    let mut overrides: Vec<UserOverride> = Vec::new();
    for p in &roster {
        match state.repo.list_user_overrides(&p.id).await {
            Ok(o) => overrides.extend(o),
            Err(e) => return (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
        }
    }
    let rules = match state.repo.list_rules().await {
        Ok(r) => r,
        Err(e) => return (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    };
    let controls = coverage::controls(&workflows);
    let held = coverage::report(&controls, &rules, &overrides, &roster, &keys);
    let orphans = coverage::coverage(&controls, &rules, &overrides, &roster, &keys);
    if !names_holders {
        return Json(json!({
            "holders_named": false,
            "total": held.len(),
            "orphans": orphans,
            "unmeasured": UNMEASURED,
            "hint": "who holds each control is shown to a caller who may read the policy rules",
        }))
        .into_response();
    }
    let real: Vec<&str> = coverage::real_people(&roster, &keys)
        .iter()
        .map(|p| p.id.as_str())
        .collect();
    Json(json!({
        "holders_named": true,
        "active_people": roster.len(),
        "real_people": real,
        "total": held.len(),
        "controls": held,
        "orphans": orphans,
        "unmeasured": UNMEASURED,
    }))
    .into_response()
}

// ---------------------------------------------------------------------------
// The production adapter: the people API and the jobs API.
// ---------------------------------------------------------------------------

/// Who this service signs its reads as: its own automation, at the
/// operator tier the passkey tier counts answer (`boss-people`
/// `machinery_read_gate`), with the platform-admin role the roster and
/// the workflow reads grant — the shape `PLATFORM_OWNER_READER` signs
/// the roster read with. It is NOT the gateway: the credential listing
/// answers only the gateway's id (e199c02d), and this read never needs
/// key material, only counts.
pub const COVERAGE_READER: &str = r#"{"id":"automation:policy-coverage","role":"platform-admin","access_tier":"operator","territory_account_ids":[],"direct_report_ids":[],"department":"platform"}"#;

pub struct HttpCoverageSources {
    people_base: String,
    jobs_base: String,
    http: boss_core::machine_token::Client,
}

impl HttpCoverageSources {
    pub fn new(people_base: impl Into<String>, jobs_base: impl Into<String>) -> Self {
        let (people_base, http) = boss_core::http_client::base(people_base);
        let jobs_base = jobs_base.into().trim_end_matches('/').to_string();
        Self {
            people_base,
            jobs_base,
            http,
        }
    }

    async fn get(&self, url: &str) -> Result<Value, String> {
        let resp = self
            .http
            .get(url)
            .header("x-boss-user", COVERAGE_READER)
            .send()
            .await
            .map_err(|e| format!("GET {url}: {e}"))?;
        let status = resp.status();
        if !status.is_success() {
            return Err(format!("GET {url} answered {status}"));
        }
        resp.json()
            .await
            .map_err(|e| format!("GET {url} answered no JSON: {e}"))
    }
}

/// The rows of a listing that answers a bare array or `{"data": [...]}`;
/// any other shape is no answer.
fn rows(body: Value, what: &str) -> Result<Vec<Value>, String> {
    match body {
        Value::Array(rows) => Ok(rows),
        Value::Object(mut m) => match m.remove("data") {
            Some(Value::Array(rows)) => Ok(rows),
            _ => Err(format!("{what} answered no array of rows")),
        },
        _ => Err(format!("{what} answered no array of rows")),
    }
}

/// One `/api/people` row as a roster entry.
pub fn person_from_row(row: &Value) -> Option<Person> {
    let id = row.get("id")?.as_str()?.to_string();
    let role = row.get("role").and_then(Value::as_str).map(str::to_string);
    let active = row.get("status").and_then(Value::as_str) == Some("active");
    let hire_date = row
        .get("hire_date")
        .and_then(Value::as_str)
        .and_then(|d| d.parse().ok());
    Some(Person {
        id,
        role,
        active,
        hire_date,
    })
}

/// One `{employee_id, user, operator}` count row as that many keys. A
/// row that names no employee is no answer; a count that is not a
/// whole number is refused rather than read as zero.
pub fn keys_from_row(row: &Value) -> Result<Vec<Key>, String> {
    let id = row
        .get("employee_id")
        .and_then(Value::as_str)
        .ok_or_else(|| format!("a tier count row names no employee: {row}"))?;
    let count = |tier: &str| {
        row.get(tier)
            .and_then(Value::as_u64)
            .ok_or_else(|| format!("the {tier} count for {id} is not a whole number: {row}"))
    };
    let key = |access_tier| Key {
        employee_id: id.to_string(),
        access_tier,
    };
    let user = count("user")?;
    let operator = count("operator")?;
    Ok((0..user)
        .map(|_| key(AccessTier::User))
        .chain((0..operator).map(|_| key(AccessTier::Operator)))
        .collect())
}

#[async_trait]
impl CoverageSources for HttpCoverageSources {
    async fn roster(&self) -> Result<Vec<Person>, String> {
        let url = format!("{}/api/people?status=active", self.people_base);
        let body = self.get(&url).await?;
        Ok(rows(body, "GET /api/people")?
            .iter()
            .filter_map(person_from_row)
            .collect())
    }

    async fn keys(&self) -> Result<Vec<Key>, String> {
        let url = format!("{}{TIER_COUNTS_PATH}", self.people_base);
        let body = self.get(&url).await?;
        let mut keys = Vec::new();
        for row in rows(body, "GET webauthn-credentials/tiers")? {
            keys.extend(keys_from_row(&row)?);
        }
        Ok(keys)
    }

    async fn workflows(&self) -> Result<Vec<WorkflowFacts>, String> {
        let url = format!("{}/api/workflows", self.jobs_base);
        let body = self.get(&url).await?;
        rows(body, "GET /api/workflows")?
            .into_iter()
            .map(|w| serde_json::from_value(w).map_err(|e| format!("GET /api/workflows row: {e}")))
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use boss_policy_client::in_memory::InMemoryPolicy;
    use boss_policy_client::types::{Action, PolicyRule, Resource, Scope};
    use tower::ServiceExt;

    struct Fixed {
        roster: Result<Vec<Person>, String>,
        keys: Result<Vec<Key>, String>,
        workflows: Result<Vec<WorkflowFacts>, String>,
    }

    #[async_trait]
    impl CoverageSources for Fixed {
        async fn roster(&self) -> Result<Vec<Person>, String> {
            self.roster.clone()
        }
        async fn keys(&self) -> Result<Vec<Key>, String> {
            self.keys.clone()
        }
        async fn workflows(&self) -> Result<Vec<WorkflowFacts>, String> {
            self.workflows.clone()
        }
    }

    /// A test roster in the shape of this instance on 2026-09-29: the
    /// founder with three user-tier keys, and the system audit account
    /// with none.
    fn instance() -> Fixed {
        Fixed {
            roster: Ok(vec![
                Person {
                    id: "emp-founder".into(),
                    role: Some("platform-admin".into()),
                    active: true,
                    hire_date: Some("2026-01-01".parse().unwrap()),
                },
                Person {
                    id: "emp-audit".into(),
                    role: Some("audit-readonly".into()),
                    active: true,
                    hire_date: None,
                },
            ]),
            keys: keys_from_row(&json!({"employee_id": "emp-founder", "user": 3, "operator": 0})),
            workflows: Ok(serde_json::from_value(json!([
                {"kind": "passkey-promotion", "status": "active", "steps": [
                    {"title": "authorise", "authority_role": "platform-admin",
                     "sign_offs_required": ["platform-admin"],
                     "assurance_required": "presence"}]}
            ]))
            .unwrap()),
        }
    }

    async fn repo_with_defaults() -> Arc<InMemoryPolicy> {
        let repo = Arc::new(InMemoryPolicy::new());
        for r in boss_policy_client::defaults::default_rules() {
            repo.upsert_rule(&r, "test").await.unwrap();
        }
        repo
    }

    async fn read(sources: Fixed, repo: Arc<InMemoryPolicy>, who: Option<&str>) -> (u16, Value) {
        let app = router(CoverageApiState {
            repo,
            sources: Arc::new(sources),
        });
        let mut req = axum::http::Request::get(COVERAGE_PATH);
        if let Some(who) = who {
            req = req.header("x-boss-user", who);
        }
        let resp = app
            .oneshot(req.body(axum::body::Body::empty()).unwrap())
            .await
            .unwrap();
        let status = resp.status().as_u16();
        let bytes = http_body_util::BodyExt::collect(resp.into_body())
            .await
            .unwrap()
            .to_bytes();
        (
            status,
            serde_json::from_slice(&bytes)
                .unwrap_or(Value::String(String::from_utf8_lossy(&bytes).into_owned())),
        )
    }

    const SIGNED: &str = r#"{"id":"automation:rule:x","role":"platform-admin"}"#;

    fn orphan_ids(body: &Value) -> Vec<String> {
        body["orphans"]
            .as_array()
            .unwrap()
            .iter()
            .map(|o| o["control"].as_str().unwrap().to_string())
            .collect()
    }

    /// The live gap, on a test roster: the founder holds every control
    /// but the operator tier, which is reported with 0 holders and the
    /// remedy that names the promotion door and the register fix.
    #[tokio::test]
    async fn the_read_reports_the_operator_tier_as_the_one_gap() {
        let (status, body) = read(instance(), repo_with_defaults().await, Some(SIGNED)).await;
        assert_eq!(status, 200, "{body}");
        assert_eq!(orphan_ids(&body), vec!["operator-tier"], "{body:#}");
        assert_eq!(body["real_people"], json!(["emp-founder"]));
        assert_eq!(body["active_people"], 2);
        let remedy = body["orphans"][0]["remedy"].as_str().unwrap();
        assert!(remedy.contains("2a228d0c") && remedy.contains("1d9970d1"));
        let controls = body["controls"].as_array().unwrap();
        assert_eq!(body["total"], controls.len());
        let owner = controls
            .iter()
            .find(|c| c["control"] == "platform-owner")
            .unwrap();
        assert_eq!(owner["holders"], json!(["emp-founder"]));
        assert!(
            controls
                .iter()
                .any(|c| c["control"] == "presence:passkey-promotion:authorise")
        );
        assert_eq!(
            body["unmeasured"].as_array().unwrap().len(),
            UNMEASURED.len()
        );
    }

    /// A pair the founder's role is denied shows up as an orphan — the
    /// read reflects the rule table as it stands.
    #[tokio::test]
    async fn a_pair_no_real_person_holds_is_an_orphan() {
        let repo = repo_with_defaults().await;
        repo.upsert_rule(
            &PolicyRule::new(
                "platform-admin",
                Resource::class(),
                Action::Create,
                Scope::None,
            ),
            "test",
        )
        .await
        .unwrap();
        let (status, body) = read(instance(), repo, Some(SIGNED)).await;
        assert_eq!(status, 200);
        assert_eq!(
            orphan_ids(&body),
            vec!["operator-tier", "policy:create:class"]
        );
    }

    #[tokio::test]
    async fn a_dark_or_empty_source_is_refused_never_reported() {
        let mut down = instance();
        down.roster = Err("GET /api/people: connection refused".into());
        let (status, body) = read(down, repo_with_defaults().await, Some(SIGNED)).await;
        assert_eq!(status, 502);
        assert!(
            body["error"]
                .as_str()
                .unwrap()
                .contains("connection refused")
        );

        let mut empty = instance();
        empty.roster = Ok(vec![]);
        let (status, body) = read(empty, repo_with_defaults().await, Some(SIGNED)).await;
        assert_eq!(status, 502, "{body}");
        assert!(body.get("orphans").is_none(), "no report from a dark read");

        let mut none = instance();
        none.workflows = Ok(vec![]);
        let (status, _) = read(none, repo_with_defaults().await, Some(SIGNED)).await;
        assert_eq!(status, 502);

        // The key read refused (a storage gate, a dark service) is dark
        // too — never "nobody holds a key", which would orphan everything.
        let mut refused = instance();
        refused.keys = Err("GET /api/people/webauthn-credentials/tiers answered 403".into());
        let (status, body) = read(refused, repo_with_defaults().await, Some(SIGNED)).await;
        assert_eq!(status, 502, "{body}");
        assert!(body["error"].as_str().unwrap().contains("tier counts"));
    }

    /// A promoted key makes the founder the operator tier's holder, and
    /// the read's one gap closes.
    #[tokio::test]
    async fn a_promoted_key_covers_the_operator_tier() {
        let mut promoted = instance();
        promoted.keys =
            keys_from_row(&json!({"employee_id": "emp-founder", "user": 2, "operator": 1}));
        let (status, body) = read(promoted, repo_with_defaults().await, Some(SIGNED)).await;
        assert_eq!(status, 200);
        assert!(orphan_ids(&body).is_empty(), "{body:#}");
    }

    /// The header exactly as the gateway's `/api/policy/{*rest}` proxy
    /// signs a session (`role_headers::build_user_json`): id, role, the
    /// tier, and the territory, reports and department keys.
    fn session(id: &str, role: &str, tier: &str) -> String {
        json!({"id": id, "role": role, "access_tier": tier,
               "territory_account_ids": [], "direct_report_ids": [], "department": null})
        .to_string()
    }

    /// Review M1: a guest session — `POST /api/auth/guest` mints
    /// `guest@algedonic.dev` as `visitor`, or `audit-readonly` where the
    /// audit read is opted in — is refused the map of who can act.
    #[tokio::test]
    async fn a_guest_session_is_refused_the_coverage_read() {
        for role in ["visitor", "audit-readonly"] {
            let who = session("guest@algedonic.dev", role, "user");
            let (status, body) = read(instance(), repo_with_defaults().await, Some(&who)).await;
            assert_eq!(status, 403, "{role}: {body}");
            assert!(body.get("controls").is_none() && body.get("real_people").is_none());
        }
    }

    /// A named session that may not read the rule table sees the gaps
    /// and nobody's name; the owner's own session, which may, sees the
    /// holders.
    #[tokio::test]
    async fn holders_are_named_only_to_a_reader_of_the_rule_table() {
        let clerk = session("emp-clerk", "clerk", "user");
        let (status, body) = read(instance(), repo_with_defaults().await, Some(&clerk)).await;
        assert_eq!(status, 200, "{body}");
        assert_eq!(body["holders_named"], false);
        assert_eq!(orphan_ids(&body), vec!["operator-tier"]);
        for hidden in ["controls", "real_people", "active_people"] {
            assert!(body.get(hidden).is_none(), "{hidden} leaked: {body}");
        }
        assert!(
            !body.to_string().contains("emp-founder"),
            "no holder named anywhere: {body}"
        );

        // The seeded auditor reads the rule table, so it reads the holders
        // (re-review L6): refused is only what the rule-table read refuses.
        let auditor = session("emp-audit", "audit-readonly", "user");
        let (status, body) = read(instance(), repo_with_defaults().await, Some(&auditor)).await;
        assert_eq!(status, 200, "{body}");
        assert_eq!(body["holders_named"], true, "{body}");

        let owner = session("emp-founder", "platform-admin", "user");
        let (status, body) = read(instance(), repo_with_defaults().await, Some(&owner)).await;
        assert_eq!(status, 200);
        assert_eq!(body["holders_named"], true);
        assert_eq!(body["real_people"], json!(["emp-founder"]));
    }

    #[tokio::test]
    async fn an_anonymous_caller_is_turned_away() {
        let (status, _) = read(instance(), repo_with_defaults().await, None).await;
        assert_eq!(status, 401);
    }

    #[test]
    fn the_wire_rows_read_as_people_and_keys() {
        let p =
            person_from_row(&json!({"id": "emp-a", "role": "clerk", "status": "active"})).unwrap();
        assert!(p.active);
        let gone = person_from_row(&json!({"id": "emp-b", "status": "terminated"})).unwrap();
        assert!(!gone.active && gone.role.is_none());
        assert!(person_from_row(&json!({"role": "x"})).is_none());
        let keys =
            keys_from_row(&json!({"employee_id": "emp-a", "user": 2, "operator": 1})).unwrap();
        let tiers: Vec<AccessTier> = keys.iter().map(|k| k.access_tier).collect();
        assert_eq!(
            tiers,
            vec![AccessTier::User, AccessTier::User, AccessTier::Operator]
        );
        assert!(keys.iter().all(|k| k.employee_id == "emp-a"));
        // A row that is not a whole count is refused, never read as zero.
        assert!(keys_from_row(&json!({"user": 1, "operator": 0})).is_err());
        assert!(
            keys_from_row(&json!({"employee_id": "emp-a", "user": "2", "operator": 0})).is_err()
        );
        assert!(keys_from_row(&json!({"employee_id": "emp-a", "user": 1})).is_err());
        assert!(rows(json!({"error": "x"}), "GET x").is_err());
        assert_eq!(rows(json!({"data": [1]}), "GET x").unwrap().len(), 1);
    }
}
