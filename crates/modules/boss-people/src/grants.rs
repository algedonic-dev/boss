//! How a people read or write meets policy — one place, so the
//! roster, the single row and the change log ask the same question
//! the same way.
//!
//! Backlog 8cdad84c (2026-09-23): the live people-api ran with
//! `policy: None`, so the employee Update gate was skipped in
//! production, and the change log (`/api/people/changes`,
//! `/api/people/{id}/changes`) made no policy call at all — read or
//! write — while its rows can carry pay in `from_value` / `to_value`.
//! The binary now wires the policy client; this module is what every
//! surface that touches an employee row asks.

use std::sync::Arc;

use axum::Json;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use boss_policy::{AccessTier, Decision, Scope, User};
use boss_policy_client::{Pair, PolicyClient, controls};
use serde::Serialize;

/// Ask policy for a declared control, answering the scope granted or
/// the response that refuses. `None` policy allows at `All`: that is
/// the test path the write gate has always had (the binary wires a
/// client since 8cdad84c). A policy service that cannot answer refuses
/// with 500 — a gate that cannot be asked is not a gate that passed.
pub(crate) async fn require(
    policy: Option<&Arc<dyn PolicyClient>>,
    user: &User,
    control: Pair,
) -> Result<Scope, Response> {
    let Some(policy) = policy else {
        return Ok(Scope::All);
    };
    match policy.ask(user, control).await {
        Ok(Decision::Allow { scope }) => Ok(scope),
        Ok(Decision::Deny { reason }) => Err((StatusCode::FORBIDDEN, reason).into_response()),
        Err(e) => Err(e.into_response()),
    }
}

/// Which employee rows `user` may read, or the response that refuses
/// (backlog cda177ef, 2026-09-27). The list, the single row, the
/// reports list and the requisitions asked nobody, so a Basic guest —
/// the `visitor` role, granted nothing on `employee` — read every name,
/// email, role and department on the roster, while the OSS README said
/// it read only what policy grants it.
///
/// A person is read by grant: `employee` Read, in its scope (403
/// without one). MACHINERY is read by tier: an operator-tier caller
/// reads every row without the question, the way this crate's scope
/// routes and the jobs API's operator doors (`boss_jobs::trust`) admit
/// it. That tier is set only by in-cluster callers — the gateway
/// strips every `x-boss-*` a browser sends — and the machinery signs
/// under roles no policy row names: the jobs API's owner resolution
/// signs `system`, which holds none, and must still resolve every
/// automation-opened packet's owner, including while the policy
/// service is down, which it never depended on before.
pub(crate) async fn roster_scope(
    policy: Option<&Arc<dyn PolicyClient>>,
    user: &User,
) -> Result<Scope, Response> {
    if user.access_tier == AccessTier::Operator {
        return Ok(Scope::All);
    }
    require(policy, user, controls::READ_EMPLOYEE).await
}

/// Whether a roster read in `scope` shows `employee_id`'s row. A
/// caller's own row is always theirs to read, grant or none — the
/// schedules rule (a621d091) — so the Me page answers for every
/// employee; everything else is [`covers`].
pub(crate) fn shows(
    scope: Option<&Scope>,
    user: &User,
    employee_id: &str,
    department: Option<&str>,
) -> bool {
    employee_id == user.id || scope.is_some_and(|s| covers(s, user, employee_id, department))
}

/// The scope in which `user` may read compensation, or `None` for
/// nowhere. No policy wired is no grant: unlike [`require`], a private
/// field must not fail open on a test convenience (c7484d0e). A policy
/// service that cannot answer is no grant either — the roster and the
/// change log still answer, with the pay taken off.
pub(crate) async fn compensation_scope(
    policy: Option<&Arc<dyn PolicyClient>>,
    user: &User,
) -> Option<Scope> {
    let policy = policy?;
    match policy.ask(user, controls::READ_COMPENSATION).await {
        Ok(Decision::Allow { scope }) => Some(scope),
        Ok(Decision::Deny { .. }) => None,
        Err(e) => {
            tracing::warn!(error = %e, user = %user.id, "compensation check failed; pay redacted");
            None
        }
    }
}

/// Whether a grant in `scope` covers the employee `employee_id` (in
/// `department`), in the vocabulary every other grant uses: `self` is
/// the caller's own row, `team` adds their direct reports,
/// `department:<d>` is that department. `territory` names accounts,
/// which an employee row has none of.
pub(crate) fn covers(
    scope: &Scope,
    user: &User,
    employee_id: &str,
    department: Option<&str>,
) -> bool {
    match scope {
        Scope::All => true,
        Scope::Self_ => employee_id == user.id,
        Scope::Team => {
            employee_id == user.id || user.direct_report_ids.iter().any(|r| r == employee_id)
        }
        Scope::Department(d) => department == Some(d.as_str()),
        Scope::None | Scope::Territory => false,
    }
}

/// The change kinds whose values are known not to be pay: the status
/// flips `derive_change_kind` writes (the values are employee
/// statuses), and the role / department / location moves. Every OTHER
/// kind — `promotion` today, and any kind the schema admits later —
/// is treated as pay-bearing, so a new kind fails closed rather than
/// leaking until someone remembers to list it.
pub(crate) const PAY_FREE_CHANGE_KINDS: &[&str] = &[
    "onboard",
    "offboard",
    "leave-start",
    "leave-end",
    "role-change",
    "department-change",
    "transfer",
];

/// The keys a pay-bearing change row loses without a compensation
/// grant that covers it. `notes` goes with the values: a promotion's
/// note is where a raise is written down in words.
const PAY_CHANGE_FIELDS: &[&str] = &["from_value", "to_value", "notes"];

/// What one reader may see of the change log: the employee rows its
/// `employee` Read grant covers, and the pay on those its
/// `compensation` grant covers.
pub(crate) struct ChangeReader {
    pub user: User,
    pub read: Scope,
    pub pay: Option<Scope>,
}

impl ChangeReader {
    /// Gate a change-log read: `employee` Read is required (403 without
    /// it), and the compensation scope is looked up for redaction.
    pub(crate) async fn admit(
        policy: Option<&Arc<dyn PolicyClient>>,
        user: User,
    ) -> Result<Self, Response> {
        let read = require(policy, &user, controls::READ_EMPLOYEE).await?;
        let pay = compensation_scope(policy, &user).await;
        Ok(Self { user, read, pay })
    }

    /// One change row as this reader may see it: `None` when the row's
    /// employee is outside the Read scope; the row with its values
    /// removed — keys absent, as on the roster — when the kind is
    /// pay-bearing and the compensation grant does not cover it.
    pub(crate) fn sees(
        &self,
        mut row: serde_json::Value,
        employee_id: &str,
        department: Option<&str>,
        kind: &str,
    ) -> Option<serde_json::Value> {
        if !covers(&self.read, &self.user, employee_id, department) {
            return None;
        }
        let pay_shown = self
            .pay
            .as_ref()
            .is_some_and(|scope| covers(scope, &self.user, employee_id, department));
        if !pay_shown
            && !PAY_FREE_CHANGE_KINDS.contains(&kind)
            && let Some(obj) = row.as_object_mut()
        {
            for field in PAY_CHANGE_FIELDS {
                obj.remove(*field);
            }
        }
        Some(row)
    }

    /// The change-log response: every row this reader [`sees`](Self::sees),
    /// in order. `key` names a row's employee id, that employee's
    /// department and the change kind — each read surface has its own
    /// row shape, and this is the one question both ask.
    pub(crate) fn respond<T: Serialize>(
        &self,
        rows: &[T],
        key: impl Fn(&T) -> (&str, Option<&str>, &str),
    ) -> Response {
        let seen: Result<Vec<_>, _> = rows
            .iter()
            .map(|row| {
                let (employee_id, department, kind) = key(row);
                serde_json::to_value(row).map(|v| self.sees(v, employee_id, department, kind))
            })
            .collect();
        match seen {
            Ok(rows) => Json(rows.into_iter().flatten().collect::<Vec<_>>()).into_response(),
            Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use boss_policy::{Action, Resource};
    use boss_policy_client::FakePolicyClient;
    use serde_json::json;

    fn user(id: &str, role: &str, reports: &[&str]) -> User {
        serde_json::from_value(json!({
            "id": id,
            "role": role,
            "direct_report_ids": reports,
        }))
        .unwrap()
    }

    fn row(kind: &str) -> serde_json::Value {
        json!({
            "employee_id": "emp-002",
            "kind": kind,
            "from_value": "8500000",
            "to_value": "9500000",
            "notes": "raise to 95k",
        })
    }

    fn reader(read: Scope, pay: Option<Scope>, u: User) -> ChangeReader {
        ChangeReader { user: u, read, pay }
    }

    /// A promotion row seen without a compensation grant keeps who and
    /// when, and loses the values and the note — keys absent.
    #[test]
    fn a_pay_bearing_change_loses_its_values_without_a_compensation_grant() {
        let r = reader(Scope::All, None, user("emp-009", "auditor", &[]));
        let seen = r
            .sees(row("promotion"), "emp-002", Some("sales"), "promotion")
            .expect("employee Read at all covers every row");
        let obj = seen.as_object().unwrap();
        for field in PAY_CHANGE_FIELDS {
            assert!(!obj.contains_key(*field), "{field} leaked: {seen}");
        }
        assert_eq!(seen["employee_id"], "emp-002");
        assert_eq!(seen["kind"], "promotion");
    }

    /// A kind nobody listed is pay-bearing: the list names what is
    /// SAFE, so a new kind fails closed.
    #[test]
    fn an_unlisted_kind_is_treated_as_pay() {
        let r = reader(Scope::All, None, user("emp-009", "auditor", &[]));
        let seen = r
            .sees(row("salary-change"), "emp-002", None, "salary-change")
            .unwrap();
        assert!(seen.get("to_value").is_none(), "{seen}");
    }

    /// A status flip carries statuses, not pay, and stays whole.
    #[test]
    fn a_pay_free_change_keeps_its_values() {
        let r = reader(Scope::All, None, user("emp-009", "auditor", &[]));
        let seen = r
            .sees(row("offboard"), "emp-002", None, "offboard")
            .unwrap();
        assert_eq!(seen, row("offboard"));
    }

    /// The compensation grant's scope is honoured row by row, exactly
    /// as on the roster.
    #[test]
    fn a_compensation_grant_shows_the_pay_on_the_rows_it_covers() {
        let r = reader(
            Scope::All,
            Some(Scope::Department("sales".into())),
            user("emp-hr", "hr", &[]),
        );
        let covered = r
            .sees(row("promotion"), "emp-002", Some("sales"), "promotion")
            .unwrap();
        assert_eq!(covered, row("promotion"));
        let outside = r
            .sees(row("promotion"), "emp-003", Some("warehouse"), "promotion")
            .unwrap();
        assert!(outside.get("from_value").is_none(), "{outside}");
    }

    /// The employee Read scope decides which rows appear at all: a team
    /// grant sees its own and its reports' changes, nobody else's.
    #[test]
    fn the_employee_read_scope_filters_the_change_log() {
        let r = reader(Scope::Team, None, user("emp-001", "mgr", &["emp-002"]));
        assert!(
            r.sees(row("offboard"), "emp-001", None, "offboard")
                .is_some()
        );
        assert!(
            r.sees(row("offboard"), "emp-002", None, "offboard")
                .is_some()
        );
        assert!(
            r.sees(row("offboard"), "emp-003", None, "offboard")
                .is_none()
        );
    }

    /// A reader with no employee Read grant is refused outright, and a
    /// grant on `employee` alone never reveals pay.
    #[tokio::test]
    async fn admit_asks_for_employee_read_and_looks_up_pay_separately() {
        let deny: Arc<dyn PolicyClient> = Arc::new(FakePolicyClient::deny_all());
        let refused = ChangeReader::admit(Some(&deny), user("emp-1", "staff", &[])).await;
        assert_eq!(
            refused.err().map(|r| r.status()),
            Some(StatusCode::FORBIDDEN)
        );

        let read_only: Arc<dyn PolicyClient> = Arc::new(
            FakePolicyClient::builder()
                .allow("staff", Action::Read, Resource::employee(), Scope::All)
                .build(),
        );
        let admitted = ChangeReader::admit(Some(&read_only), user("emp-1", "staff", &[]))
            .await
            .unwrap_or_else(|_| panic!("employee Read admits"));
        assert_eq!(admitted.read, Scope::All);
        assert_eq!(admitted.pay, None);
    }
}
