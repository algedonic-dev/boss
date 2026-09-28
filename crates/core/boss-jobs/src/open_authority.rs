//! Who may open a packet of which kind (design 222fc982, decided by
//! David 2026-09-27; backlog 6dc75abd).
//!
//! Opening a packet is a Create, asked of the KIND being opened. Two
//! grants answer it, either one enough:
//!
//! - Create on `job` — the ALL-KINDS grant. `boss_policy_client::defaults`
//!   gives it to platform-admin (every live opener signs as it: the CLI,
//!   the conductor, the dispatcher's rules, the ops runner, the gateway's
//!   inquiry door, David's session) and to break-glass (the rollback
//!   lever). It opens a packet of any kind, as it did before this.
//! - Create on `job:<kind>` ([`Resource::job_of_kind`]) — one kind. A
//!   tenant grants each role the kinds its surfaces file (the feedback
//!   widget files `user-feedback` from every page), so a role that may
//!   file feedback may not thereby file an `ops-request`.
//!
//! The all-kinds grant is asked FIRST. The two orders decide the same
//! thing, and this one keeps the common open — an operator's, a rule's —
//! at the one policy question it cost before, so the narrow path, not
//! the busy one, carries the second.
//!
//! What this does not express: withholding one kind from an all-kinds
//! holder. A `none` row on `job:ops-request` does not narrow a role that
//! holds Create on `job`; to narrow a role, withdraw its all-kinds grant
//! and grant it the kinds. That is the shape the decision chose — plain
//! `job` is the superset, not a default the kinds refine.

use boss_policy_client::{Action, Decision, PolicyClient, PolicyClientError, Resource, User};

/// Whether `user` may open a packet of `kind`, and on which grant.
///
/// `kind` is `None` when the body names none — the only grant that can
/// answer that is the all-kinds one. A kind carrying `:` is refused on
/// the narrow path rather than looked up: a Workflow kind is a slug, and
/// a rule id is derived by joining with `:` (`types::rule_id`), so no
/// kind that carries one names a packet anyone can open. A policy
/// service that cannot be asked is an `Err`, never an Allow.
pub async fn may_open(
    policy: &dyn PolicyClient,
    user: &User,
    kind: Option<&str>,
) -> Result<Decision, PolicyClientError> {
    let every_kind = policy.check(user, Action::Create, Resource::job()).await?;
    if every_kind.is_allowed() {
        return Ok(every_kind);
    }
    let Some(kind) = kind else {
        return Ok(every_kind);
    };
    if kind.contains(boss_policy_client::types::RULE_ID_SEPARATOR) {
        return Ok(Decision::Deny {
            reason: format!(
                "a packet kind never carries '{sep}', so no grant opens {kind:?}",
                sep = boss_policy_client::types::RULE_ID_SEPARATOR
            ),
        });
    }
    let resource = Resource::job_of_kind(kind);
    match policy.check(user, Action::Create, resource.clone()).await? {
        allowed @ Decision::Allow { .. } => Ok(allowed),
        Decision::Deny { reason } => Ok(Decision::Deny {
            // Names the narrow grant a tenant would add, then what the
            // policy said of it — the fix is one read away.
            reason: format!(
                "opening a {kind} packet needs Create on {resource} (or on job, every kind): \
                 {reason}"
            ),
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use boss_policy_client::{AccessTier, FakePolicyClient, Scope};

    fn user(role: &str) -> User {
        User {
            id: "emp-1".into(),
            role: role.into(),
            access_tier: AccessTier::User,
            territory_account_ids: vec![],
            direct_report_ids: vec![],
            department: None,
        }
    }

    fn policy() -> FakePolicyClient {
        FakePolicyClient::builder()
            .allow("admin", Action::Create, Resource::job(), Scope::All)
            .allow(
                "filer",
                Action::Create,
                Resource::job_of_kind("user-feedback"),
                Scope::All,
            )
            .build()
    }

    #[tokio::test]
    async fn the_all_kinds_grant_opens_any_kind_and_no_kind() {
        let p = policy();
        for kind in [Some("user-feedback"), Some("ops-request"), None] {
            assert!(
                may_open(&p, &user("admin"), kind)
                    .await
                    .unwrap()
                    .is_allowed(),
                "{kind:?}"
            );
        }
    }

    #[tokio::test]
    async fn a_kind_grant_opens_that_kind_only() {
        let p = policy();
        let filer = user("filer");
        assert!(
            may_open(&p, &filer, Some("user-feedback"))
                .await
                .unwrap()
                .is_allowed()
        );
        for kind in [Some("ops-request"), Some("user-feedback-x"), None] {
            assert!(
                !may_open(&p, &filer, kind).await.unwrap().is_allowed(),
                "{kind:?}"
            );
        }
    }

    #[tokio::test]
    async fn a_kind_carrying_the_rule_id_separator_is_refused_before_it_is_looked_up() {
        // `filer` + `job:user-feedback:create` is the grant's own id;
        // no spelling of a kind reaches it but the kind itself.
        let p = FakePolicyClient::builder()
            .allow(
                "filer",
                Action::Create,
                Resource::new("job:a:b"),
                Scope::All,
            )
            .build();
        let d = may_open(&p, &user("filer"), Some("a:b")).await.unwrap();
        assert!(!d.is_allowed(), "{d:?}");
    }
}
