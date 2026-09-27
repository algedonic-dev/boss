//! A write that touches one person's rows, asked of policy the one way
//! (backlog 11721a25, 2026-09-27).
//!
//! WHY IT EXISTS. The calendar's reservation door and the scheduling
//! writes in boss-jobs took no caller: anyone reaching either port — the
//! LAN machine door, or a signed-in session off the read-only floor —
//! could put a hard reservation on any employee's calendar, cancel one,
//! or rewrite a tech's week. And the author each recorded was body text
//! (`created_by`, `?actor=`), so the record said whatever the caller
//! typed. Two core services need the same two answers, so they live here
//! rather than twice (CLAUDE.md §9a).
//!
//! [`require_reaching`] is the gate: `action` on `resource`, in a scope
//! that reaches the person the write touches — `all` anyone, `team` the
//! caller and their direct reports, `self` the caller. A department or
//! territory grant reaches nobody here: a schedule row carries an
//! employee id and no department, the same answer the schedule reads
//! give (a621d091). A write whose rows cannot be placed on one person
//! before it runs (`None`) takes an `all` grant.
//!
//! [`recorded_author`] is provenance: a caller records ITSELF. A body
//! naming someone else is refused — unless the caller is a sibling
//! service (an automation identity), which authorised its own caller
//! before it called and names on whose behalf it writes: the jobs
//! API's step hook reserves the assignee's time for the person who
//! started the step.

use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};

use crate::{Action, Decision, PolicyClient, Resource, Scope, User};

/// Whether a grant at `scope` reaches the rows of `person` (`None`: a
/// write that cannot name one person before it runs).
pub fn scope_reaches(scope: &Scope, user: &User, person: Option<&str>) -> bool {
    // An anonymous visitor is nobody's self, whatever id it carries.
    let is_self =
        |p: &str| p == user.id && !boss_core::roles::is_anonymous_visitor(&user.id, &user.role);
    match (scope, person) {
        (Scope::All, _) => true,
        (Scope::Self_, Some(p)) => is_self(p),
        (Scope::Team, Some(p)) => is_self(p) || user.direct_report_ids.iter().any(|r| r == p),
        _ => false,
    }
}

/// Ask `policy` for `action` on `resource` and require the granted
/// scope to reach `person`, or answer the response that refuses. A
/// policy service that cannot be asked answers its own 503: a gate that
/// cannot be asked is not a gate that passed.
pub async fn require_reaching(
    policy: &dyn PolicyClient,
    user: &User,
    action: Action,
    resource: Resource,
    person: Option<&str>,
) -> Result<(), Response> {
    let scope = granted(policy, user, action, resource.clone()).await?;
    if scope_reaches(&scope, user, person) {
        return Ok(());
    }
    let whom = person.unwrap_or("rows it cannot name before it runs");
    Err((
        StatusCode::FORBIDDEN,
        format!(
            "{} (role {}) holds {} on `{}` at {}, which does not reach {whom}",
            user.id,
            user.role,
            action.as_str(),
            resource.as_str(),
            scope.to_db_string()
        ),
    )
        .into_response())
}

/// The scope `policy` grants `user` for `action` on `resource`, or the
/// response that refuses: 403 for a deny, the client's own 503/500 for a
/// policy service that could not be asked.
async fn granted(
    policy: &dyn PolicyClient,
    user: &User,
    action: Action,
    resource: Resource,
) -> Result<Scope, Response> {
    match policy.check(user, action, resource).await {
        Ok(Decision::Allow { scope }) => Ok(scope),
        Ok(Decision::Deny { reason }) => Err((StatusCode::FORBIDDEN, reason).into_response()),
        Err(e) => Err(e.into_response()),
    }
}

/// [`require_reaching`] for a write BY ID, where the caller named a row
/// and not a person: `owner` is the person the stored row belongs to,
/// `None` when no such row exists (or it belongs to nobody). A refusal
/// here is ONE answer — no person named, and the same words for a row
/// outside the grant and for an id that does not exist — because the
/// caller must learn from it neither whose the row is nor whether the id
/// is real (the adversarial review of car 11721a25, 2026-09-27: the
/// first cut echoed the row's employee, and "rows it cannot name" for a
/// missing id). An `all` grant reaches a missing id too, and goes on to
/// the write's own 404: it may know every id there is.
pub async fn require_reaching_row(
    policy: &dyn PolicyClient,
    user: &User,
    action: Action,
    resource: Resource,
    owner: Option<&str>,
) -> Result<(), Response> {
    let scope = granted(policy, user, action, resource.clone()).await?;
    if scope_reaches(&scope, user, owner) {
        return Ok(());
    }
    Err(row_refusal(action, &resource))
}

/// The one refusal of a write by id: names the action and resource the
/// caller asked for and nothing about the row.
pub fn row_refusal(action: Action, resource: &Resource) -> Response {
    (
        StatusCode::FORBIDDEN,
        format!(
            "{} on this `{}` row is not in your grant, or there is no such row",
            action.as_str(),
            resource.as_str()
        ),
    )
        .into_response()
}

/// Whether `user` is a sibling service: an automation identity, which
/// may name the person it writes for.
fn is_sibling(user: &User) -> bool {
    matches!(
        user.ambient_actor(),
        Some(boss_core::actor::ActorId::Automation(_))
    )
}

/// A request that names an author other than its signed caller. It
/// answers 403, naming both.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NotTheAuthor {
    pub named: String,
    pub signed: String,
}

impl IntoResponse for NotTheAuthor {
    fn into_response(self) -> Response {
        (
            StatusCode::FORBIDDEN,
            format!(
                "the request names {} as its author, but it is signed by {}; \
                 a caller records only itself",
                self.named, self.signed
            ),
        )
            .into_response()
    }
}

/// The author a write records: the signed caller. `named` is what the
/// body or query said — absent or blank is the caller; the caller's own
/// id is the caller; anything else is refused unless the caller is a
/// sibling service writing on someone's behalf.
pub fn recorded_author(user: &User, named: Option<&str>) -> Result<String, NotTheAuthor> {
    match named.map(str::trim).filter(|n| !n.is_empty()) {
        None => Ok(user.id.clone()),
        Some(n) if n == user.id || is_sibling(user) => Ok(n.to_string()),
        Some(n) => Err(NotTheAuthor {
            named: n.to_string(),
            signed: user.id.clone(),
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{AccessTier, FakePolicyClient};

    fn user(id: &str, role: &str, reports: &[&str]) -> User {
        User {
            id: id.into(),
            role: role.into(),
            access_tier: AccessTier::User,
            territory_account_ids: vec![],
            direct_report_ids: reports.iter().map(|s| s.to_string()).collect(),
            department: Some("service".into()),
        }
    }

    #[test]
    fn each_scope_reaches_its_own_people() {
        let mgr = user("emp-mgr", "service-mgr", &["emp-1"]);
        assert!(scope_reaches(&Scope::All, &mgr, Some("emp-9")));
        assert!(scope_reaches(&Scope::All, &mgr, None));
        assert!(scope_reaches(&Scope::Team, &mgr, Some("emp-1")));
        assert!(scope_reaches(&Scope::Team, &mgr, Some("emp-mgr")));
        assert!(!scope_reaches(&Scope::Team, &mgr, Some("emp-9")));
        assert!(scope_reaches(&Scope::Self_, &mgr, Some("emp-mgr")));
        assert!(!scope_reaches(&Scope::Self_, &mgr, Some("emp-1")));
        // Only `all` reaches a write that cannot name its person.
        assert!(!scope_reaches(&Scope::Team, &mgr, None));
        assert!(!scope_reaches(&Scope::Self_, &mgr, None));
    }

    /// A schedule row carries no department, so a department grant
    /// cannot be shown to reach it (a621d091) — even the caller's own
    /// department.
    #[test]
    fn a_department_or_territory_grant_reaches_nobody() {
        let u = user("emp-mgr", "service-mgr", &[]);
        for scope in [
            Scope::Department("service".into()),
            Scope::Territory,
            Scope::None,
        ] {
            assert!(!scope_reaches(&scope, &u, Some("emp-mgr")), "{scope:?}");
        }
    }

    #[test]
    fn an_anonymous_visitor_is_nobodys_self() {
        for u in [
            user("anonymous", "guest", &[]),
            user("emp-audit", "audit-readonly", &[]),
        ] {
            assert!(!scope_reaches(&Scope::Self_, &u, Some(&u.id.clone())));
        }
    }

    #[tokio::test]
    async fn require_reaching_refuses_a_deny_and_an_out_of_scope_grant() {
        let policy = FakePolicyClient::builder()
            .allow("staff", Action::Create, Resource::schedule(), Scope::Self_)
            .build();
        let me = user("emp-1", "staff", &[]);
        assert!(
            require_reaching(
                &policy,
                &me,
                Action::Create,
                Resource::schedule(),
                Some("emp-1")
            )
            .await
            .is_ok()
        );
        let other = require_reaching(
            &policy,
            &me,
            Action::Create,
            Resource::schedule(),
            Some("emp-2"),
        )
        .await
        .unwrap_err();
        assert_eq!(other.status(), StatusCode::FORBIDDEN);
        let denied = require_reaching(
            &policy,
            &me,
            Action::Delete,
            Resource::schedule(),
            Some("emp-1"),
        )
        .await
        .unwrap_err();
        assert_eq!(denied.status(), StatusCode::FORBIDDEN);
    }

    /// A by-id refusal is one answer: a row outside the grant and an id
    /// with no row read the same, and neither names the row's person.
    #[tokio::test]
    async fn a_row_refusal_names_nobody_and_hides_whether_the_row_exists() {
        let policy = FakePolicyClient::builder()
            .allow("staff", Action::Delete, Resource::schedule(), Scope::Self_)
            .build();
        let me = user("emp-1", "staff", &[]);
        let text = |r: Response| async move {
            let status = r.status();
            let bytes = axum::body::to_bytes(r.into_body(), usize::MAX)
                .await
                .unwrap();
            (status, String::from_utf8_lossy(&bytes).to_string())
        };
        let theirs = require_reaching_row(
            &policy,
            &me,
            Action::Delete,
            Resource::schedule(),
            Some("emp-ceo"),
        )
        .await
        .unwrap_err();
        let missing =
            require_reaching_row(&policy, &me, Action::Delete, Resource::schedule(), None)
                .await
                .unwrap_err();
        let (theirs, missing) = (text(theirs).await, text(missing).await);
        assert_eq!(theirs.0, StatusCode::FORBIDDEN);
        assert!(!theirs.1.contains("emp-ceo"), "{theirs:?}");
        assert_eq!(theirs, missing);
        assert!(
            require_reaching_row(
                &policy,
                &me,
                Action::Delete,
                Resource::schedule(),
                Some("emp-1")
            )
            .await
            .is_ok()
        );
    }

    #[test]
    fn a_caller_records_itself_and_is_refused_naming_another() {
        let me = user("emp-1", "staff", &[]);
        assert_eq!(recorded_author(&me, None).unwrap(), "emp-1");
        assert_eq!(recorded_author(&me, Some("  ")).unwrap(), "emp-1");
        assert_eq!(recorded_author(&me, Some("emp-1")).unwrap(), "emp-1");
        let forged = recorded_author(&me, Some("emp-ceo")).unwrap_err();
        assert_eq!(
            forged,
            NotTheAuthor {
                named: "emp-ceo".into(),
                signed: "emp-1".into()
            }
        );
        assert_eq!(forged.into_response().status(), StatusCode::FORBIDDEN);
        // An operator-tier human is still a human: it names only itself.
        let admin = User {
            access_tier: AccessTier::Operator,
            ..user("emp-david", "platform-admin", &[])
        };
        assert!(recorded_author(&admin, Some("emp-ceo")).is_err());
    }

    /// A sibling service writes for the person whose request it is
    /// serving, and names them — the jobs API's step hook reserves time
    /// for whoever started the step.
    #[test]
    fn a_sibling_service_names_whom_it_writes_for() {
        for id in ["automation:jobs", "rule:reserve-on-start", "fleet-sim"] {
            let sibling = user(id, "platform-admin", &[]);
            assert_eq!(recorded_author(&sibling, Some("emp-7")).unwrap(), "emp-7");
            assert_eq!(recorded_author(&sibling, None).unwrap(), id);
        }
    }
}
