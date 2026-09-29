//! The policy store's two adapters, held to one statement of the
//! `PolicyRepository` contract (`boss_testing::adapters_agree!`, design
//! 3036296f mechanism C; backlog be459ab9, the fifth port it reaches
//! after the jobs store's list filters, the agent-run log, the Workflow
//! registry and the Class registry).
//!
//! WHY THIS PORT NEXT. Policy is the privilege model: every write in the
//! estate asks it, and every door test that asks through
//! `FakePolicyClient` is answered by `InMemoryPolicy`, while production
//! is answered by `PgPolicy`. A disagreement between the two is a test
//! suite measuring a different privilege model from the one that ships.
//! Two such disagreements had already been found and fixed by hand, one
//! at a time, each in only the adapter it was found in — the in-memory
//! override listing that answered expired rows (F5 of backlog b8e75382)
//! and the in-memory upsert that let a new override replace another
//! key's row under the same id (S2 of the hold review of car a8becd52).
//! This file states each of those once, for both adapters, and the two
//! it found on its first run (the in-memory rule listing answered
//! retired rules, and forgot an operator had retired a bootstrap rule)
//! are pinned by `a_retired_rule_leaves_the_listing` and
//! `an_operator_retirement_of_a_bootstrap_rule_survives_reconcile`.
//!
//! The audit table is the Postgres adapter's alone — the in-memory
//! double keeps no audit — so what a write leaves in `policy_rule_audit`
//! stays in `postgres_roundtrip.rs`. Order is not compared: the port
//! promises none, and Postgres sorts while the double hashes.

use std::sync::Mutex;

use boss_policy::port::{PolicyRepository, ReconcileStats};
use boss_policy::postgres::PgPolicy;
use boss_policy::{Action, InMemoryPolicy, PolicyError, PolicyRule, Resource, Scope, UserOverride};

async fn a_fresh_pg_policy() -> (PgPolicy, boss_testing::TestDb) {
    let db = boss_testing::TestDb::new().await;
    (PgPolicy::new(db.pool.clone()), db)
}

/// A whole second `hours` from now: Postgres keeps microseconds and the
/// double keeps nanoseconds, so an expiry compared across both must
/// carry neither.
fn hours_from_now(hours: i64) -> chrono::DateTime<chrono::Utc> {
    chrono::DateTime::from_timestamp(chrono::Utc::now().timestamp() + hours * 3600, 0)
        .expect("a representable instant")
}

fn override_of(id: &str, user: &str, scope: Scope, expires_in_hours: Option<i64>) -> UserOverride {
    UserOverride {
        id: id.into(),
        user_id: user.into(),
        resource: Resource::ledger(),
        action: Action::Read,
        scope,
        reason: format!("{id} for {user}"),
        expires_at: expires_in_hours.map(hours_from_now),
    }
}

async fn rule_ids<R: PolicyRepository>(repo: &R) -> Vec<String> {
    let mut ids: Vec<String> = repo
        .list_rules()
        .await
        .unwrap()
        .into_iter()
        .map(|r| r.id)
        .collect();
    ids.sort();
    ids
}

async fn override_ids<R: PolicyRepository>(repo: &R, user: &str) -> Vec<String> {
    let mut ids: Vec<String> = repo
        .list_user_overrides(user)
        .await
        .unwrap()
        .into_iter()
        .map(|o| o.id)
        .collect();
    ids.sort();
    ids
}

// ----- rules -------------------------------------------------------------

/// A written rule reads back whole, and a second upsert under its id
/// rewrites the scope in place: one row, the new scope.
async fn a_rule_upsert_reads_back_and_rewrites_in_place<R: PolicyRepository>(
    repo: &R,
    adapter: &str,
) {
    let narrow = PolicyRule::new("sales-rep", Resource::job(), Action::Read, Scope::Territory);
    repo.upsert_rule(&narrow, "t").await.unwrap();
    assert_eq!(
        repo.rule_for(&narrow.id).await.unwrap(),
        Some(narrow.clone()),
        "{adapter}"
    );

    let wide = PolicyRule::new("sales-rep", Resource::job(), Action::Read, Scope::All);
    repo.upsert_rule(&wide, "t").await.unwrap();
    assert_eq!(
        repo.rule_for(&narrow.id).await.unwrap(),
        Some(wide),
        "{adapter}"
    );
    assert_eq!(rule_ids(repo).await, vec![narrow.id], "{adapter}");
}

/// The port lists ACTIVE rules. A retired rule leaves the listing and is
/// still answered by id, inactive, for an operator to inspect.
async fn a_retired_rule_leaves_the_listing<R: PolicyRepository>(repo: &R, adapter: &str) {
    let kept = PolicyRule::new("service-tech", Resource::job(), Action::Read, Scope::Self_);
    let retired = PolicyRule::new("service-tech", Resource::job(), Action::Close, Scope::Self_);
    repo.upsert_rule(&kept, "t").await.unwrap();
    repo.upsert_rule(&retired, "t").await.unwrap();
    repo.deactivate_rule(&retired.id, "t").await.unwrap();

    assert_eq!(
        rule_ids(repo).await,
        vec![kept.id],
        "{adapter}: the listing answers active rules only"
    );
    let back = repo.rule_for(&retired.id).await.unwrap().expect("by id");
    assert!(!back.active, "{adapter}: answered by id, inactive");
}

async fn retiring_or_reading_a_missing_rule<R: PolicyRepository>(repo: &R, adapter: &str) {
    assert_eq!(
        repo.rule_for("nobody:job:read").await.unwrap(),
        None,
        "{adapter}"
    );
    let err = repo
        .deactivate_rule("nobody:job:read", "t")
        .await
        .expect_err("no such rule");
    assert!(
        matches!(err, PolicyError::NotFound(_)),
        "{adapter}: {err:?}"
    );
}

/// The judge sees the row under the rule's id, retired or not, and its
/// refusal writes nothing.
async fn a_refused_rule_write_sees_its_row_and_writes_nothing<R: PolicyRepository>(
    repo: &R,
    adapter: &str,
) {
    let held = PolicyRule::new("service-tech", Resource::job(), Action::Close, Scope::Self_);
    repo.upsert_rule(&held, "t").await.unwrap();
    repo.deactivate_rule(&held.id, "t").await.unwrap();
    let retired = repo.rule_for(&held.id).await.unwrap();

    let seen = Mutex::new(None);
    let refuse = |existing: Option<&PolicyRule>| {
        *seen.lock().unwrap() = Some(existing.cloned());
        Err("refused by the suite".to_string())
    };
    let widen = PolicyRule::new("service-tech", Resource::job(), Action::Close, Scope::All);
    let err = repo
        .upsert_rule_judged(&widen, "t", &refuse)
        .await
        .expect_err("refused");
    assert!(matches!(err, PolicyError::Refused(_)), "{adapter}: {err:?}");
    assert_eq!(
        seen.into_inner().unwrap(),
        Some(retired.clone()),
        "{adapter}: the judge saw the retired row"
    );
    assert_eq!(repo.rule_for(&held.id).await.unwrap(), retired, "{adapter}");

    // And a refused creation leaves no row at all.
    let fresh = PolicyRule::new("auditor", Resource::job(), Action::Read, Scope::All);
    let err = repo
        .upsert_rule_judged(&fresh, "t", &|existing: Option<&PolicyRule>| {
            assert_eq!(existing, None);
            Err("refused by the suite".to_string())
        })
        .await
        .expect_err("refused");
    assert!(matches!(err, PolicyError::Refused(_)), "{adapter}: {err:?}");
    assert_eq!(repo.rule_for(&fresh.id).await.unwrap(), None, "{adapter}");
}

// ----- bootstrap reconcile ---------------------------------------------

/// One reconcile, every branch: a missing default is inserted, a
/// drifted bootstrap row refreshed, an operator's edit preserved, a
/// matching row left alone — counted the same by both adapters.
async fn reconcile_inserts_refreshes_preserves_and_leaves_alone<R: PolicyRepository>(
    repo: &R,
    adapter: &str,
) {
    let drifted = PolicyRule::new("cto", Resource::workflow(), Action::Read, Scope::Self_);
    let tuned = PolicyRule::new("cfo", Resource::workflow(), Action::Read, Scope::Self_);
    let matching = PolicyRule::new("coo", Resource::workflow(), Action::Read, Scope::All);
    repo.upsert_rule(&drifted, "bootstrap").await.unwrap();
    repo.upsert_rule(&tuned, "emp-cfo").await.unwrap();
    repo.upsert_rule(&matching, "bootstrap").await.unwrap();

    let defaults = [
        PolicyRule::new("guest", Resource::workflow(), Action::Read, Scope::All),
        PolicyRule::new("cto", Resource::workflow(), Action::Read, Scope::All),
        PolicyRule::new("cfo", Resource::workflow(), Action::Read, Scope::All),
        matching.clone(),
    ];
    let stats = repo.bootstrap_reconcile(&defaults).await.unwrap();
    assert_eq!(
        stats,
        ReconcileStats {
            inserted: 1,
            refreshed: 1,
            preserved: 1,
            unchanged: 1,
        },
        "{adapter}"
    );
    assert_eq!(
        repo.rule_for(&drifted.id).await.unwrap().map(|r| r.scope),
        Some(Scope::All),
        "{adapter}: the drifted default healed"
    );
    assert_eq!(
        repo.rule_for(&tuned.id).await.unwrap().map(|r| r.scope),
        Some(Scope::Self_),
        "{adapter}: the operator's edit survived"
    );
    assert!(
        repo.rule_for("guest:workflow:read")
            .await
            .unwrap()
            .is_some(),
        "{adapter}: the missing default was inserted"
    );
}

/// An operator who retires a bootstrap rule has edited it: the row is
/// theirs now, and the next boot's reconcile preserves the retirement
/// rather than reviving the grant.
async fn an_operator_retirement_of_a_bootstrap_rule_survives_reconcile<R: PolicyRepository>(
    repo: &R,
    adapter: &str,
) {
    let default = PolicyRule::new("guest", Resource::ledger(), Action::Read, Scope::All);
    repo.bootstrap_reconcile(std::slice::from_ref(&default))
        .await
        .unwrap();
    repo.deactivate_rule(&default.id, "emp-founder")
        .await
        .unwrap();

    let stats = repo
        .bootstrap_reconcile(std::slice::from_ref(&default))
        .await
        .unwrap();
    assert_eq!(stats.preserved, 1, "{adapter}: {stats:?}");
    assert_eq!(stats.refreshed, 0, "{adapter}: {stats:?}");
    assert!(
        !repo
            .rule_for(&default.id)
            .await
            .unwrap()
            .expect("row")
            .active,
        "{adapter}: the retired grant stays retired"
    );
    assert_eq!(rule_ids(repo).await, Vec::<String>::new(), "{adapter}");
}

// ----- user overrides --------------------------------------------------

/// The listing answers one user's LIVE overrides; the lookup by id
/// answers an expired one too.
async fn the_override_listing_is_live_and_the_lookup_is_not<R: PolicyRepository>(
    repo: &R,
    adapter: &str,
) {
    let open = override_of("ov-open", "emp-1", Scope::All, None);
    let later = UserOverride {
        resource: Resource::employee(),
        ..override_of("ov-later", "emp-1", Scope::All, Some(24))
    };
    let lapsed = UserOverride {
        resource: Resource::job(),
        ..override_of("ov-lapsed", "emp-1", Scope::All, Some(-1))
    };
    let other = override_of("ov-other", "emp-2", Scope::All, None);
    for ov in [&open, &later, &lapsed, &other] {
        repo.upsert_user_override(ov, "t").await.unwrap();
    }

    assert_eq!(
        override_ids(repo, "emp-1").await,
        vec!["ov-later".to_string(), "ov-open".to_string()],
        "{adapter}"
    );
    assert_eq!(
        repo.user_override("ov-lapsed").await.unwrap(),
        Some(lapsed),
        "{adapter}"
    );
    assert_eq!(
        repo.user_override("ov-later").await.unwrap(),
        Some(later),
        "{adapter}"
    );
    assert_eq!(
        repo.user_override("ov-none").await.unwrap(),
        None,
        "{adapter}"
    );
}

/// A second override on the same (user, resource, action) rewrites the
/// row already there — scope, reason and expiry — and keeps its id, even
/// when that row had expired.
async fn an_override_on_a_held_key_rewrites_that_row<R: PolicyRepository>(repo: &R, adapter: &str) {
    let lapsed = override_of("ov-first", "emp-1", Scope::Self_, Some(-1));
    repo.upsert_user_override(&lapsed, "t").await.unwrap();

    let revived = override_of("ov-second", "emp-1", Scope::All, Some(24));
    repo.upsert_user_override(&revived, "t").await.unwrap();

    assert_eq!(
        repo.user_override("ov-first").await.unwrap(),
        Some(UserOverride {
            id: "ov-first".into(),
            ..revived
        }),
        "{adapter}: the held row, rewritten"
    );
    assert_eq!(
        repo.user_override("ov-second").await.unwrap(),
        None,
        "{adapter}"
    );
    assert_eq!(
        override_ids(repo, "emp-1").await,
        vec!["ov-first"],
        "{adapter}"
    );
}

/// A new override under an id another key owns is a Conflict, and both
/// rows stand as they were.
async fn an_override_id_owned_by_another_key_is_a_conflict<R: PolicyRepository>(
    repo: &R,
    adapter: &str,
) {
    let deny = override_of("ov-1", "emp-1", Scope::None, None);
    repo.upsert_user_override(&deny, "t").await.unwrap();

    let squatter = override_of("ov-1", "emp-2", Scope::All, None);
    let err = repo
        .upsert_user_override(&squatter, "t")
        .await
        .expect_err("the id is taken");
    assert!(
        matches!(err, PolicyError::Conflict(_)),
        "{adapter}: {err:?}"
    );
    assert_eq!(
        repo.user_override("ov-1").await.unwrap(),
        Some(deny),
        "{adapter}"
    );
    assert_eq!(
        override_ids(repo, "emp-2").await,
        Vec::<String>::new(),
        "{adapter}"
    );
}

/// The judge of an override write sees the row on its conflict key,
/// expired or not, and a refusal writes nothing.
async fn a_refused_override_write_sees_its_row_and_writes_nothing<R: PolicyRepository>(
    repo: &R,
    adapter: &str,
) {
    let lapsed = override_of("ov-held", "emp-1", Scope::Self_, Some(-1));
    repo.upsert_user_override(&lapsed, "t").await.unwrap();

    let seen = Mutex::new(None);
    let refuse = |existing: Option<&UserOverride>| {
        *seen.lock().unwrap() = Some(existing.cloned());
        Err("refused by the suite".to_string())
    };
    let revive = override_of("ov-new", "emp-1", Scope::All, None);
    let err = repo
        .upsert_user_override_judged(&revive, "t", &refuse)
        .await
        .expect_err("refused");
    assert!(matches!(err, PolicyError::Refused(_)), "{adapter}: {err:?}");
    assert_eq!(
        seen.into_inner().unwrap(),
        Some(Some(lapsed.clone())),
        "{adapter}: the judge saw the expired row on the key"
    );
    assert_eq!(
        repo.user_override("ov-held").await.unwrap(),
        Some(lapsed),
        "{adapter}: untouched"
    );
    assert_eq!(
        repo.user_override("ov-new").await.unwrap(),
        None,
        "{adapter}"
    );
}

/// Retiring an override lapses it: the listing drops it, the lookup
/// still answers it, expired. A missing id is NotFound, and a refused
/// retirement leaves the override live.
async fn retiring_an_override_lapses_it<R: PolicyRepository>(repo: &R, adapter: &str) {
    let kept = override_of("ov-kept", "emp-1", Scope::All, None);
    let gone = UserOverride {
        resource: Resource::job(),
        ..override_of("ov-gone", "emp-1", Scope::All, None)
    };
    repo.upsert_user_override(&kept, "t").await.unwrap();
    repo.upsert_user_override(&gone, "t").await.unwrap();

    let err = repo
        .deactivate_user_override_judged("ov-kept", "t", &|existing: Option<&UserOverride>| {
            assert_eq!(existing.map(|o| o.id.as_str()), Some("ov-kept"));
            Err("refused by the suite".to_string())
        })
        .await
        .expect_err("refused");
    assert!(matches!(err, PolicyError::Refused(_)), "{adapter}: {err:?}");

    repo.deactivate_user_override("ov-gone", "t").await.unwrap();
    assert_eq!(
        override_ids(repo, "emp-1").await,
        vec!["ov-kept"],
        "{adapter}"
    );
    let lapsed = repo.user_override("ov-gone").await.unwrap().expect("by id");
    assert!(
        lapsed.expires_at.is_some_and(|at| at <= chrono::Utc::now()),
        "{adapter}: lapsed, {lapsed:?}"
    );

    let err = repo
        .deactivate_user_override("ov-none", "t")
        .await
        .expect_err("no such override");
    assert!(
        matches!(err, PolicyError::NotFound(_)),
        "{adapter}: {err:?}"
    );
}

boss_testing::adapters_agree! {
    adapters {
        in_memory => (InMemoryPolicy::new(), ()),
        postgres => a_fresh_pg_policy().await,
    }
    cases {
        a_rule_upsert_reads_back_and_rewrites_in_place,
        a_retired_rule_leaves_the_listing,
        retiring_or_reading_a_missing_rule,
        a_refused_rule_write_sees_its_row_and_writes_nothing,
        reconcile_inserts_refreshes_preserves_and_leaves_alone,
        an_operator_retirement_of_a_bootstrap_rule_survives_reconcile,
        the_override_listing_is_live_and_the_lookup_is_not,
        an_override_on_a_held_key_rewrites_that_row,
        an_override_id_owned_by_another_key_is_a_conflict,
        a_refused_override_write_sees_its_row_and_writes_nothing,
        retiring_an_override_lapses_it,
    }
}
