//! The service read: the ONE rule every service's policy check is
//! answered on, held active at scope all by the rule doors (backlog
//! 0028804f; review 1a73d5ce, F1; row D of design b08725c2).
//!
//! WHY. Every service asks `POST /api/policy/check` signed as its own
//! `automation:<service>` id at the role [`User::service`] gives it, and
//! the check answers that caller only if it holds Read on `policy-rule`
//! at scope all (`http::read_for_refusal`). No service has an override,
//! so each holds that read through one row: the rule for (that role,
//! [`READ_POLICY_RULE`]). Under the policy check's `enforce` mode — live
//! since 2026-10-06 — a service the bound refuses has its EVERY check
//! refused, which its client reads as policy-unreachable: every door
//! behind every service fails closed, for everyone.
//!
//! The lockout guard (`crate::guard`, G1) kept that row only by accident:
//! it is also the founder's own read of the table, and G1 refuses a write
//! that takes a control from its LAST REAL PERSON. Give the read a second
//! route — an override on the founder (a grant, 201), or a second real
//! person who reads at `audit-readonly` — and G1 has nothing to refuse:
//! DELETE answered 204, a narrowing 200, and every service check 403.
//! Two writes by any holder of policy authority.
//!
//! WHAT IT DECIDES. A rule write whose RESULT leaves that row anything
//! but active at scope all is refused 409 and writes nothing. It is a
//! pure function of the row the write would leave ([`narrowed_by`]):
//!
//! * no registry, roster, passkey or workflow read, and no call to
//!   another service — so no source can go dark under it and it forms no
//!   cycle with the services it protects;
//! * no dependence on the check's mode — a row that would lock the
//!   estate out the moment the word flips to `enforce` is refused before
//!   it exists, in `off` and `report` too;
//! * keyed on the row's ID, because the engine finds a rule ONLY by its
//!   derived id (`types::rule_id`) and reads only `active` and `scope`
//!   off it: the row under this id is the whole of what a service check
//!   is answered on, whatever any other row says.
//!
//! THE ROLE IS READ, NEVER SPELLED. "The role services sign at" is
//! [`User::service`]'s own, and the pair is the const the check itself
//! asks its caller for, so neither can drift from the door it guards.
//! Every service signs through that one constructor, so there is one
//! such role; a second would be a second row to hold here.
//!
//! NOT ON THE BOOT PATH. `bootstrap_reconcile` writes through the port
//! with no door and no judge, so this can never refuse a start (a boot
//! guard that refuses to start takes the system of record down). Boot
//! REPORTS what it finds ([`found`]) and restores nothing an operator
//! wrote: reconcile inserts a missing default and refreshes a
//! bootstrap-owned one, and preserves an operator-owned row — and since
//! the doors now refuse the narrowing, such a row is residue from before
//! this guard or a hand edit of the table, which a machine should name
//! rather than overwrite.
//!
//! THE REPAIR NEEDS NO OTHER SERVICE. With the row narrowed, `enforce`
//! refuses the people and jobs APIs' own policy checks, so the lockout
//! guard's roster, passkey and workflow reads are dark and every guarded
//! write answers 503. Writing this rule exactly as it must stand is
//! therefore judged without those reads (`http::write_rule`): it takes
//! no one's hold, and it is the write that brings the sources back. The
//! founder and break-glass both make it with every source dark.
//!
//! WHAT IT DOES NOT CLOSE. An override on a service's own id is the
//! other spelling of the same deny (it is read before any rule); the
//! override door's refusal for it is the sibling car's
//! (`guard::narrows_a_services_policy_read`, review 4bbb0f93 F1). The
//! model makes the rest impossible, and the door tests pin each: a rule
//! has no expiry, there is no deny rule but scope none on the same row,
//! the id is the primary key and is derived from a role that carries no
//! separator, and a rule body with a field left out is not a rule.

use boss_policy_client::controls::READ_POLICY_RULE;
use boss_policy_client::port::{PolicyError, PolicyRepository};
use boss_policy_client::types::{PolicyRule, Scope, User};

/// The rule as it must stand: (the role every service signs at, Read on
/// `policy-rule`), active, at scope all. The service named is immaterial
/// — [`User::service`] gives every service the same role.
pub fn rule() -> PolicyRule {
    PolicyRule::new(
        User::service("policy").role,
        READ_POLICY_RULE.resource(),
        READ_POLICY_RULE.action(),
        Scope::All,
    )
}

/// True when `left` — the row a write would LEAVE in the table — is the
/// service read in any state but active at scope all.
pub fn narrowed_by(left: &PolicyRule) -> bool {
    let kept = rule();
    left.id == kept.id && !(left.active && left.scope == kept.scope)
}

/// What is wrong with `left`, as a refusal and the boot report word it.
fn state_of(left: &PolicyRule) -> String {
    if left.active {
        format!("active at scope {}", left.scope.to_db_string())
    } else {
        format!("inactive (scope {})", left.scope.to_db_string())
    }
}

/// The write that restores the rule, as a request an operator can send:
/// the rule door takes it from any holder of policy write authority, and
/// from break-glass, because it is a rule core ships exactly — and
/// judges it on the policy table alone (`http::write_rule`), so it is
/// written while every other service is dark, which under `enforce` is
/// exactly when it is needed. The JSON comes first so a reader can take
/// the request off the text whole.
pub fn restore_request() -> String {
    let body = serde_json::json!({ "rule": rule() });
    format!(
        "POST /api/policy/rules {body} (sent by a holder of policy write authority or by \
         break-glass; it reads nothing outside the policy table, so it is written while every \
         other service is dark)"
    )
}

/// The 409 for a write that would leave `left`: the rule, what the write
/// would have done, why, and the way out as requests to send.
pub fn refusal(left: &PolicyRule) -> String {
    let kept = rule();
    format!(
        "rule {id} stays active at scope all, and this write would leave it {state}; nothing \
         was written. Every service signs its policy checks at role {role} and is answered on \
         this one rule, so under the policy check mode `enforce` any other state of it refuses \
         every service's every check and every door behind every service fails closed — and a \
         row written while the mode is `off` or `report` would do that the moment the word \
         flips (backlog 0028804f; review 1a73d5ce, F1). The way out is another write, never \
         this one: to take Read on policy-rule from ONE person, deny it on that person — POST \
         /api/policy/user-overrides {{\"override\":{{\"id\":\"<a new id>\",\"user_id\":\"<the \
         person>\",\"resource\":\"{resource}\",\"action\":\"{action}\",\"scope\":\"none\",\
         \"reason\":\"<why>\",\"expires_at\":null}}}}; to give it to another role, POST \
         /api/policy/rules with a rule for that role. If this rule is ever found in another \
         state, restore it with {restore}",
        id = kept.id,
        state = state_of(left),
        role = kept.role,
        resource = kept.resource.as_str(),
        action = kept.action.as_str(),
        restore = restore_request(),
    )
}

/// What a boot finds wrong with the service read, or `None` when it
/// stands. Read after `bootstrap_reconcile`, so a missing row has
/// already been inserted and a bootstrap-owned one refreshed; what is
/// left is an operator-owned row reconcile preserves. Reported, never
/// repaired and never a reason not to start.
pub async fn found<R: PolicyRepository>(repo: &R) -> Result<Option<String>, PolicyError> {
    let kept = rule();
    let state = match repo.rule_for(&kept.id).await? {
        Some(row) if !narrowed_by(&row) => return Ok(None),
        Some(row) => state_of(&row),
        None => "missing".to_string(),
    };
    Ok(Some(format!(
        "rule {} is {state}, not active at scope all: every service signs its policy checks at \
         role {} and is answered on this rule, so the policy check mode `enforce` refuses every \
         service's every check while it stands (backlog 0028804f). Boot preserved it — it was \
         not written by bootstrap — and repaired nothing. Restore it with {}",
        kept.id,
        kept.role,
        restore_request(),
    )))
}

#[cfg(test)]
mod tests {
    use super::*;
    use boss_policy_client::defaults::default_rules;
    use boss_policy_client::engine::PolicyEngine;
    use boss_policy_client::in_memory::InMemoryPolicy;
    use boss_policy_client::types::rule_id;
    use std::sync::Arc;

    /// THE ROLE IS THE ONE SERVICES SIGN AT, and the row is the one the
    /// engine consults for a service's read of the table — derived here
    /// the way the engine derives it, from a signing identity, so a
    /// literal in [`rule`] that drifts from `User::service` fails.
    #[test]
    fn the_rule_is_the_row_the_engine_reads_for_any_services_check() {
        for svc in ["jobs", "people", "gateway", "dispatcher", "anything-new"] {
            let signer = User::service(svc);
            assert_eq!(
                rule().id,
                rule_id(
                    &signer.role,
                    &READ_POLICY_RULE.resource(),
                    READ_POLICY_RULE.action()
                ),
                "automation:{svc}"
            );
            assert_eq!(rule().role, signer.role);
        }
        assert_eq!(rule().id, "platform-admin:policy-rule:read");
        assert!(rule().active);
        assert_eq!(rule().scope, Scope::All);
    }

    /// Core ships the rule exactly — which is what makes the restore a
    /// write break-glass may make (`authority::judge_rule`), and what
    /// makes a fresh boot insert it.
    #[test]
    fn core_ships_the_rule_exactly() {
        assert!(default_rules().contains(&rule()));
    }

    /// The whole judgement, over every scope and both flags: only active
    /// at scope all stands, and no other row is this guard's business.
    #[test]
    fn only_active_at_scope_all_stands_and_no_other_row_is_judged() {
        let scopes = [
            Scope::None,
            Scope::Self_,
            Scope::Territory,
            Scope::Team,
            Scope::Department("platform".into()),
            Scope::All,
        ];
        for scope in &scopes {
            for active in [true, false] {
                let left = PolicyRule {
                    scope: scope.clone(),
                    active,
                    ..rule()
                };
                assert_eq!(
                    narrowed_by(&left),
                    !(active && *scope == Scope::All),
                    "{left:?}"
                );
                // Any other role, resource or action: never refused.
                for other in [
                    PolicyRule::new(
                        "audit-readonly",
                        left.resource.clone(),
                        left.action,
                        scope.clone(),
                    ),
                    PolicyRule::new(
                        left.role.clone(),
                        boss_policy_client::types::Resource::job(),
                        left.action,
                        scope.clone(),
                    ),
                    PolicyRule::new(
                        left.role.clone(),
                        left.resource.clone(),
                        boss_policy_client::controls::UPDATE_POLICY_RULE.action(),
                        scope.clone(),
                    ),
                ] {
                    let other = PolicyRule { active, ..other };
                    assert!(!narrowed_by(&other), "{other:?}");
                }
            }
        }
    }

    /// The refusal names the rule, what the write would have left, why,
    /// and requests to send — not a sentence about intent.
    #[test]
    fn the_refusal_names_the_rule_the_state_the_reason_and_the_requests() {
        let retired = PolicyRule {
            active: false,
            ..rule()
        };
        let why = refusal(&retired);
        for part in [
            "platform-admin:policy-rule:read",
            "inactive",
            "nothing was written",
            "every service",
            "`enforce`",
            "POST /api/policy/user-overrides",
            "POST /api/policy/rules {\"rule\":",
        ] {
            assert!(why.contains(part), "missing {part:?}: {why}");
        }
        let team = PolicyRule {
            scope: Scope::Team,
            ..rule()
        };
        assert!(refusal(&team).contains("active at scope team"));
    }

    async fn service_reads(repo: &Arc<InMemoryPolicy>) -> bool {
        PolicyEngine::new(Arc::clone(repo))
            .ask(&User::service("jobs"), READ_POLICY_RULE)
            .await
            .expect("engine")
            == boss_policy_client::types::Decision::Allow { scope: Scope::All }
    }

    /// (d) THE BOOT PATH, MEASURED. Reconcile goes through the port with
    /// no door: it is never refused, whatever state the row is in. It
    /// INSERTS the rule when it is missing and REFRESHES it when it is
    /// bootstrap-owned and drifted — and PRESERVES it when an operator
    /// wrote it, narrowed or retired. That last case is what [`found`]
    /// reports and repairs nothing of.
    #[tokio::test]
    async fn boot_is_never_refused_restores_what_bootstrap_owns_and_reports_the_rest() {
        let defaults = default_rules();
        let id = rule().id;

        // Missing: inserted.
        let repo = Arc::new(InMemoryPolicy::new());
        repo.bootstrap_reconcile(&defaults).await.expect("boot");
        assert!(service_reads(&repo).await);
        assert_eq!(found(repo.as_ref()).await.expect("read"), None);

        // Bootstrap-owned and drifted: refreshed.
        for drift in [
            PolicyRule {
                scope: Scope::None,
                ..rule()
            },
            PolicyRule {
                active: false,
                ..rule()
            },
        ] {
            let repo = Arc::new(InMemoryPolicy::new());
            repo.upsert_rule(&drift, "bootstrap").await.expect("seed");
            repo.bootstrap_reconcile(&defaults).await.expect("boot");
            assert!(service_reads(&repo).await, "{drift:?}");
            assert_eq!(found(repo.as_ref()).await.expect("read"), None);
        }

        // Operator-owned: preserved by reconcile, the boot is not
        // refused, and the report names the row, its state and the
        // request that restores it — which then does.
        let narrowed = PolicyRule {
            scope: Scope::Team,
            ..rule()
        };
        let repo = Arc::new(InMemoryPolicy::new());
        repo.upsert_rule(&narrowed, "emp-founder")
            .await
            .expect("seed");
        let stats = repo.bootstrap_reconcile(&defaults).await.expect("boot");
        assert_eq!(stats.preserved, 1);
        assert!(!service_reads(&repo).await, "reconcile does not restore it");
        let report = found(repo.as_ref())
            .await
            .expect("read")
            .expect("a narrowed rule is reported");
        assert!(
            report.contains(&id) && report.contains("active at scope team"),
            "{report}"
        );
        assert!(report.contains(&restore_request()), "{report}");
        assert_eq!(
            repo.rule_for(&id).await.expect("read"),
            Some(narrowed),
            "the report writes nothing"
        );

        let retired = Arc::new(InMemoryPolicy::with_rules(defaults.clone()));
        retired
            .deactivate_rule(&id, "emp-founder")
            .await
            .expect("seed");
        retired.bootstrap_reconcile(&defaults).await.expect("boot");
        let report = found(retired.as_ref()).await.expect("read");
        assert!(report.is_some_and(|r| r.contains("inactive")));

        // Never inserted and never reconciled (no table a real boot
        // leaves, but the report must not read absence as standing).
        let empty = InMemoryPolicy::new();
        let report = found(&empty).await.expect("read");
        assert!(report.is_some_and(|r| r.contains("missing")));
    }
}
