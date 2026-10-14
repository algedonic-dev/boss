//! The caller's role as the record holds it (design abf9eeae, car 2).
//! Registry adapters answer this port; no role taxonomy or privilege
//! inference belongs here. A row with no role differs from no row, and
//! neither is a registry that could not answer.

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RoleRecord {
    /// The registry's canonical identity, including when an alias matched.
    pub actor_id: String,
    /// A Class code, never a closed list of platform roles.
    pub role: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum RoleLookupError {
    #[error("role registry unavailable: {0}")]
    Unavailable(String),
    #[error("role registry ambiguous: {0}")]
    Ambiguous(String),
}

/// Whether `actor` is a member of the family a registry row declares in
/// `signs_for` — an id its signer writes one per firing
/// (`automation:rule:<name>` under the dispatcher's `automation:rule:`).
/// The prefix alone names no member.
///
/// ONE definition, read by the registry's own `row_of_record`
/// (`boss-jobs`, which answers the audit log's actors) and by the
/// resolver every service mounts (`boss-policy-client`, which answers a
/// request's). They were two readers of one field and only the first
/// read it: measured 2026-10-07, the resolver called 34 dispatcher rules
/// unregistered while their row sat in the registry (backlog ddf0773e).
///
/// SHAPE, BOTH HALVES (review ed9181b4, F2 and F5, 2026-10-07). A prefix
/// that is not [`is_family_prefix`] has no members: the registry's write
/// side refuses one, and a reader does not take that on trust. And the
/// tail must be `:`-separated `[A-Za-z0-9-]` segments, none empty — every
/// live dispatcher rule name (124, read 2026-10-10) and the ops runner's
/// `<host>:<moment>-<pid>`. Before this, `rule: `, `rule:../../x` and a
/// name carrying U+202E each resolved to the dispatcher's platform-admin
/// row. The colon stays a separator because the ops runner's members
/// need it, so `rule:a:b:c` still resolves; that a caller can TYPE any
/// well-formed member at all is the enforce arm's precondition (F1: a
/// family is trusted only for the identity a credential bound), which
/// no shape check can supply.
pub fn is_family_member(signs_for: &str, actor: &str) -> bool {
    is_family_prefix(signs_for)
        && actor
            .strip_prefix(signs_for)
            .is_some_and(|tail| tail.split(':').all(is_tail_segment))
}

/// Whether `prefix` is `automation:<slug>:` — the shape of a registry
/// row's `signs_for` (the SQL CHECK on `automation_actors` spells it
/// `^automation:[a-z0-9][a-z0-9-]*:$`). A slug holds no colon, so two
/// well-formed prefixes never nest. ONE definition, moved here from
/// `boss-jobs` so the resolver checks the shape the write side does.
pub fn is_family_prefix(prefix: &str) -> bool {
    prefix
        .strip_prefix("automation:")
        .and_then(|s| s.strip_suffix(':'))
        .is_some_and(is_slug)
}

/// `[a-z0-9][a-z0-9-]*` — an automation id's slug.
pub fn is_slug(s: &str) -> bool {
    s.chars()
        .next()
        .is_some_and(|c| c.is_ascii_lowercase() || c.is_ascii_digit())
        && s.chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
}

fn is_tail_segment(s: &str) -> bool {
    !s.is_empty() && s.chars().all(|c| c.is_ascii_alphanumeric() || c == '-')
}

#[async_trait]
pub trait RoleOfRecord: Send + Sync {
    async fn role_for(&self, actor: &str) -> Result<Option<RoleRecord>, RoleLookupError>;
}

#[cfg(test)]
mod tests {
    use super::*;

    /// boss-core had no unit test for the one definition both readers
    /// share; five mutations of the lookup survived review ed9181b4
    /// (2026-10-07) for want of one.
    #[test]
    fn a_family_prefix_is_automation_slug_colon_and_nothing_else() {
        for good in [
            "automation:rule:",
            "automation:ops-runner:",
            "automation:a1:",
        ] {
            assert!(is_family_prefix(good), "{good}");
        }
        for bad in [
            "",
            "a",
            "automation:",
            "automation:ops-",
            "automation:rule",
            "automation:rule:tenant-",
            "automation:rule:x:",
            "automation:Rule:",
            "automation:-rule:",
            "rule:",
            " automation:rule:",
        ] {
            assert!(!is_family_prefix(bad), "{bad:?}");
        }
    }

    #[test]
    fn a_member_is_its_prefix_and_a_well_formed_tail() {
        for member in [
            "automation:rule:auto-park-on-gate-green",
            "automation:rule:x",
            "automation:rule:a-tenant-declared-rule-9",
        ] {
            assert!(is_family_member("automation:rule:", member), "{member}");
        }
        assert!(is_family_member(
            "automation:ops-runner:",
            "automation:ops-runner:forge:20260930T201448Z-2040087"
        ));
        for stranger in [
            // The prefix alone, a near miss, another family.
            "automation:rule:",
            "automation:rules-engine",
            "automation:ruler:x",
            "automation:ops-runner:x",
            // A tail that is not `:`-separated `[A-Za-z0-9-]` segments
            // (review ed9181b4, F5: each of these resolved to the
            // dispatcher's platform-admin row).
            "automation:rule: ",
            "automation:rule:x ",
            "automation:rule: x",
            "automation:rule:../../x",
            "automation:rule:a/b",
            "automation:rule:a.b",
            "automation:rule:x\u{202e}",
            "automation:rule:x\n",
            "automation:rule::x",
            "automation:rule:x:",
            "automation:rule:x::y",
        ] {
            assert!(
                !is_family_member("automation:rule:", stranger),
                "{stranger:?}"
            );
        }
    }

    /// A malformed prefix names no member, whoever hands it over: the
    /// registry's write side refuses one, and this reader does not take
    /// that on trust (F2).
    #[test]
    fn a_malformed_prefix_has_no_members() {
        for (prefix, actor) in [
            ("automation:", "automation:anything-at-all"),
            ("a", "automation:rule:x"),
            ("automation:ops-", "automation:ops-x"),
            ("automation:rule:tenant-", "automation:rule:tenant-x"),
        ] {
            assert!(!is_family_member(prefix, actor), "{prefix:?} {actor:?}");
        }
    }
}
