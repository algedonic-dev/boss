//! The platform delivery-policy bundle (`infra/platform/delivery-policy/`)
//! declares every ACTIVE row the migrations produce — at that version,
//! column for column, or at a later version — and can be the registry's
//! only home. Since 2026-09-28 it LEADS them: `train-conductor` v3 (the
//! fourth gate bay, backlog 366c2ed5) exists in the bundle alone, the
//! edit path migrations-declare-schema-only leaves — the same rule the
//! cadence (cab50f4c) and station (08372fdb) pins settled on their own
//! first bundle-only bumps.
//!
//! MEASURED 2026-09-18 on origin/main 478231fb (backlog 393d3234,
//! consolidation H4, car 4 of 4 — the last registry). Two migrations
//! were the only place the delivery policy was declared: 202608242117
//! created the table and seeded `train-conductor` v1 as the constants
//! train.rs had compiled in, verbatim; 202609050500 retired v1 and
//! published v2 by copying it with `ci_host_floor_gb` at 40. Three more
//! touched the table without declaring a row — 202609030800 and
//! 202609031000 each added a column with a default (the floor at 90,
//! the gate bound at 3), and 20260918102236 dropped
//! `consist_excluded_lints` (H9). One active row comes out of that, at
//! v2, and `boss-cli`'s `the_seeded_policy_equals_the_compiled_fallback`
//! already held it equal to the conductor's compiled fallback. The
//! conductor reads the LIVE row over `/api/delivery/policy/<name>` and
//! is untouched by this move; what changes is where the row is
//! DECLARED.
//!
//! TWO PINS, in the order the move needs them:
//!
//!   1. The bundle is COMPLETE: every active row a TestDb holds after
//!      the migrations has a file, at that version or ahead of it, and
//!      where the versions match, column for column (`created_at`
//!      excepted — it is when the deployment was built, not part of the
//!      declaration).
//!   2. The bundle can be the ONLY home: with the `delivery_policy`
//!      table emptied, the seed's publish function recreates the
//!      BUNDLE exactly — same version, same columns, active.
//!
//! And the three edges of the decision table: a bundle row that
//! differs from the live active row of the same (name, version) is
//! refused by name and field, a version bump publishes and retires the
//! live row, and a lineage an operator retired stays retired — a boot
//! never re-activates a policy someone switched off (the conductor runs
//! on its compiled fallback and says so).

use boss_core::actor::ActorId;
use boss_jobs::delivery::{DeliveryPolicyRegistry, DeliveryPolicySpec, PgDeliveryPolicy};
use boss_jobs::delivery_policy_seed::{
    DeliveryPolicySeedError, SeedOutcome, platform_delivery_policy_path, seed_delivery_policies,
};
use boss_jobs::registry::WorkflowStatus;
use boss_jobs::seed_loader::load_delivery_policies;
use boss_testing::TestDb;
use std::collections::BTreeMap;

const POLICY: &str = "train-conductor";

fn actor() -> ActorId {
    ActorId::Automation("platform-workflow-seed".into())
}

/// A row as a declaration: everything but `created_at`, keyed by name.
fn declarations(rows: &[DeliveryPolicySpec]) -> BTreeMap<String, serde_json::Value> {
    rows.iter()
        .map(|s| {
            let mut v = serde_json::to_value(s).expect("a policy serializes");
            v.as_object_mut()
                .expect("a policy is an object")
                .remove("created_at");
            (s.name().to_string(), v)
        })
        .collect()
}

fn bundle() -> Vec<DeliveryPolicySpec> {
    load_delivery_policies(platform_delivery_policy_path())
        .expect("the platform delivery-policy bundle parses")
}

/// Every ACTIVE row, name-ordered, read the way the seed reads a
/// lineage — one name at a time — so the pin never grows a second
/// SELECT that could drift from the port's.
async fn active_rows(registry: &PgDeliveryPolicy, names: &[String]) -> Vec<DeliveryPolicySpec> {
    let mut out = Vec::new();
    for name in names {
        out.extend(
            registry
                .live_versions(name)
                .await
                .expect("live_versions")
                .into_iter()
                .filter(|r| r.status == WorkflowStatus::Active),
        );
    }
    out
}

/// The names the migrations left in the table, active or not.
async fn every_name(db: &TestDb) -> Vec<String> {
    sqlx::query_scalar("SELECT DISTINCT name FROM delivery_policy ORDER BY name")
        .fetch_all(&db.pool)
        .await
        .expect("names")
}

/// How many bundle rows sit at a HIGHER version than the live active
/// row of their name — the rows a seed over `live` PUBLISHES. Derived,
/// never assumed zero: the pins below took it as zero until 2026-09-28,
/// which was true only while no policy had changed since the cutover;
/// the first bundle-only bump (v3, backlog 366c2ed5) made it one.
fn ahead_of(live: &[DeliveryPolicySpec]) -> usize {
    let live: BTreeMap<&str, i32> = live.iter().map(|s| (s.name(), s.version())).collect();
    bundle()
        .iter()
        .filter(|s| live.get(s.name()).is_some_and(|v| s.version() > *v))
        .count()
}

/// PIN 1 — every active row the migrations produce is declared in the
/// bundle, at its version or AHEAD of it, and where the versions match,
/// column for column; the bundle declares no name the migrations do
/// not (one pipeline, one policy).
#[tokio::test(flavor = "multi_thread")]
async fn the_bundle_declares_every_active_row_the_migrations_produce() {
    let db = TestDb::new().await;
    let registry = PgDeliveryPolicy::new(db.pool.clone());
    let live = active_rows(&registry, &every_name(&db).await).await;
    assert!(!live.is_empty(), "the migrations seed at least one policy");

    let from_migrations = declarations(&live);
    let from_bundle = declarations(&bundle());

    let migration_names: Vec<&String> = from_migrations.keys().collect();
    let bundle_names: Vec<&String> = from_bundle.keys().collect();
    assert_eq!(
        migration_names, bundle_names,
        "the bundle's names must be exactly the migrations' active names (a policy \
         the migrations seed and the bundle does not declare has no home once \
         migrations declare schema only)"
    );
    // THE BUNDLE MAY BE AHEAD; IT MAY NEVER BE BEHIND; AND WHERE THE
    // VERSIONS MATCH, EVERY COLUMN MUST AGREE — the rule the cadence
    // pin settled on its own first bump (backlog cab50f4c, David chose
    // it over plain equality) and the station pin followed (08372fdb).
    for (name, migrated) in &from_migrations {
        let declared = &from_bundle[name];
        let dv = declared["version"].as_i64().expect("a bundle version");
        let mv = migrated["version"].as_i64().expect("a migration version");
        assert!(
            dv >= mv,
            "infra/platform/delivery-policy/{name}.toml declares v{dv}, BEHIND the v{mv} \
             the migrations produce — a fresh database would serve the older row and \
             history would silently win"
        );
        if dv == mv {
            assert_eq!(
                declared, migrated,
                "infra/platform/delivery-policy/{name}.toml is at the migrations' version \
                 but differs from it — a column changed without the version bump that says \
                 so (left = bundle, right = migrations)"
            );
        }
    }

    // And what a booted deployment then SERVES is the bundle: the seed
    // over the migrations publishes the rows that lead.
    seed_delivery_policies(&registry, &bundle(), &actor(), chrono::Utc::now(), false)
        .await
        .expect("the bundle seeds over the migrations");
    assert_eq!(
        declarations(&active_rows(&registry, &every_name(&db).await).await),
        from_bundle,
        "after the seed, the active rows are the bundle, every column"
    );
}

/// PIN 2 — with the table emptied, the seed alone rebuilds exactly the
/// bundle: same version, same columns, active.
#[tokio::test(flavor = "multi_thread")]
async fn an_emptied_registry_is_rebuilt_from_the_bundle_alone() {
    let db = TestDb::new().await;
    let registry = PgDeliveryPolicy::new(db.pool.clone());
    let names = every_name(&db).await;
    // WHAT THE SEED MUST REBUILD IS THE BUNDLE, because the bundle is
    // the home. Reading the expectation off the migrations would assert
    // that an emptied registry comes back as HISTORY rather than as the
    // current declaration (the cadence and station pins' same fix).
    let expected = declarations(&bundle());

    sqlx::query("DELETE FROM delivery_policy")
        .execute(&db.pool)
        .await
        .expect("empty the delivery_policy table");
    assert!(
        active_rows(&registry, &names).await.is_empty(),
        "the table is empty before the seed runs"
    );

    let report = seed_delivery_policies(&registry, &bundle(), &actor(), chrono::Utc::now(), false)
        .await
        .expect("the seed publishes into an empty registry");
    assert_eq!(
        report.count(|o| matches!(o, SeedOutcome::Inserted)),
        expected.len(),
        "every bundle policy is inserted into an empty registry: {report}"
    );

    let after = active_rows(&registry, &names).await;
    assert_eq!(
        declarations(&after),
        expected,
        "the seed must recreate the BUNDLE exactly — the bundle can be the only home"
    );
    for row in &after {
        let versions = registry
            .live_versions(row.name())
            .await
            .expect("live_versions");
        assert_eq!(
            versions.len(),
            1,
            "{} has exactly the declared version and no synthetic history: {:?}",
            row.name(),
            versions.iter().map(|v| v.version()).collect::<Vec<_>>()
        );
    }
    let policy = after
        .iter()
        .find(|r| r.name() == POLICY)
        .expect("the conductor's policy is active");
    let declared = bundle()
        .into_iter()
        .find(|s| s.name() == POLICY)
        .expect("the bundle declares the conductor's policy")
        .version();
    assert_eq!(
        policy.version(),
        declared,
        "the policy lands at the version the bundle declares, with no history below it"
    );
}

/// On a registry the migrations already filled, the first seed
/// publishes exactly the rows the bundle moved ahead and inserts
/// nothing; the next boot then finds every row present and writes
/// nothing — the insert-if-missing posture every boot relies on.
///
/// Until 2026-09-28 this asserted that the FIRST seed found everything
/// present, which assumed no policy had changed since the cutover. The
/// first bundle-only bump (v3, backlog 366c2ed5) moved the one row
/// ahead, so the property is now stated on the boot AFTER the one that
/// delivers the change — which is the boot every later restart is.
#[tokio::test(flavor = "multi_thread")]
async fn a_present_registry_is_left_untouched() {
    let db = TestDb::new().await;
    let registry = PgDeliveryPolicy::new(db.pool.clone());
    let names = every_name(&db).await;
    // BEFORE the seed, because the seed is what changes it.
    let ahead = ahead_of(&active_rows(&registry, &names).await);

    let first = seed_delivery_policies(&registry, &bundle(), &actor(), chrono::Utc::now(), false)
        .await
        .expect("a present registry is not a failure");
    assert_eq!(
        first.count(|o| matches!(o, SeedOutcome::Published { .. })),
        ahead,
        "exactly the rows the bundle moved ahead are published: {first}"
    );
    assert_eq!(
        first.count(|o| matches!(o, SeedOutcome::Present)),
        bundle().len() - ahead,
        "every bundle row at the live version is already present: {first}"
    );
    assert_eq!(first.count(|o| matches!(o, SeedOutcome::Inserted)), 0);

    let before = declarations(&active_rows(&registry, &names).await);
    let rows_before: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM delivery_policy")
        .fetch_one(&db.pool)
        .await
        .expect("count rows");

    let report = seed_delivery_policies(&registry, &bundle(), &actor(), chrono::Utc::now(), false)
        .await
        .expect("a present registry is not a failure");
    assert_eq!(
        report.count(|o| matches!(o, SeedOutcome::Present)),
        bundle().len(),
        "on the next boot every bundle row is present: {report}"
    );
    assert_eq!(report.count(|o| matches!(o, SeedOutcome::Inserted)), 0);

    let rows_after: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM delivery_policy")
        .fetch_one(&db.pool)
        .await
        .expect("count rows");
    assert_eq!(rows_after, rows_before, "a no-op seed writes no row");
    assert_eq!(declarations(&active_rows(&registry, &names).await), before);
}

/// The refusal: a bundle row edited WITHOUT a version bump differs
/// from the live active row of the same (name, version), and the seed
/// refuses it by name and field rather than overwriting or ignoring.
#[tokio::test(flavor = "multi_thread")]
async fn a_bundle_row_that_differs_from_the_live_row_is_refused_by_field() {
    let db = TestDb::new().await;
    let registry = PgDeliveryPolicy::new(db.pool.clone());
    let names = every_name(&db).await;
    // Bring the registry to the bundle first, so the live active row is
    // at the version the edited row below still claims — the bundle
    // itself may lead the migrations (see `ahead_of`), and an edit to a
    // row that leads would publish rather than refuse.
    seed_delivery_policies(&registry, &bundle(), &actor(), chrono::Utc::now(), false)
        .await
        .expect("the bundle seeds over the migrations");
    let before = declarations(&active_rows(&registry, &names).await);

    let mut specs = bundle();
    let edited = specs
        .iter_mut()
        .find(|s| s.name() == POLICY)
        .expect("the bundle declares the conductor's policy");
    let declared = edited.version();
    edited.row.gate_max_concurrent += 1;

    let err = seed_delivery_policies(&registry, &specs, &actor(), chrono::Utc::now(), false)
        .await
        .expect_err("a drifted bundle row is refused");
    match &err {
        DeliveryPolicySeedError::Refused { rows: refusals, .. } => {
            assert_eq!(refusals.len(), 1, "{err}");
            assert_eq!(refusals[0].name, POLICY);
            assert_eq!(refusals[0].version, declared);
            assert_eq!(
                refusals[0].fields,
                vec!["gate_max_concurrent".to_string()],
                "the refusal names every differing column"
            );
        }
        other => panic!("expected a refusal, got {other}"),
    }
    let text = err.to_string();
    assert!(
        text.contains(POLICY)
            && text.contains("version")
            && text.contains("gate_max_concurrent")
            && text.contains("infra/platform/delivery-policy/"),
        "the refusal must name the policy, the edit path, the field and the bundle: {text}"
    );
    assert_eq!(
        declarations(&active_rows(&registry, &names).await),
        before,
        "a refused seed writes nothing"
    );
}

/// A version bump IS the edit path: a bundle row one version ahead of
/// the live active row is published, retiring the live one, and the
/// declared version is the one that lands — retire-by-name, then
/// insert, the order the one-active-per-name partial index demands.
#[tokio::test(flavor = "multi_thread")]
async fn a_version_bump_publishes_and_retires_the_live_row() {
    let db = TestDb::new().await;
    let registry = PgDeliveryPolicy::new(db.pool.clone());
    // Bring the registry to the bundle first, so the ONE bump this test
    // makes is the only thing the seed below can publish — the bundle
    // itself may lead the migrations (see `ahead_of`).
    seed_delivery_policies(&registry, &bundle(), &actor(), chrono::Utc::now(), false)
        .await
        .expect("the bundle seeds over the migrations");
    let live = registry
        .live_versions(POLICY)
        .await
        .expect("lineage")
        .into_iter()
        .find(|r| r.status == WorkflowStatus::Active)
        .expect("the policy is active");
    // A value the live row does not hold, so the served row can only
    // carry it if the bump published.
    let raised = live.row.gate_max_concurrent + 1;

    let mut specs = bundle();
    let bumped = specs
        .iter_mut()
        .find(|s| s.name() == POLICY)
        .expect("the bundle declares the conductor's policy");
    bumped.row.version = live.version() + 1;
    bumped.row.gate_max_concurrent = raised;

    let report = seed_delivery_policies(&registry, &specs, &actor(), chrono::Utc::now(), false)
        .await
        .expect("a version bump publishes");
    assert_eq!(
        report.count(|o| matches!(o, SeedOutcome::Published { .. })),
        1,
        "{report}"
    );

    let lineage = registry.live_versions(POLICY).await.expect("lineage");
    let now_active = lineage
        .iter()
        .find(|r| r.status == WorkflowStatus::Active)
        .expect("the policy is active");
    assert_eq!(now_active.version(), live.version() + 1);
    assert_eq!(now_active.row.gate_max_concurrent, raised);
    let old = lineage
        .iter()
        .find(|r| r.version() == live.version())
        .expect("the prior version is history");
    assert_eq!(old.status, WorkflowStatus::Retired);

    // What the conductor reads is what the bundle now says — and the
    // version a train departed under is still readable, retired.
    let served: Vec<(i32, i32)> = sqlx::query_as(
        "SELECT version, gate_max_concurrent FROM delivery_policy \
         WHERE status = 'active' AND name = $1",
    )
    .bind(POLICY)
    .fetch_all(&db.pool)
    .await
    .expect("the active row");
    assert_eq!(served, vec![(live.version() + 1, raised)]);
}

/// A lineage with NO active row is one an operator retired, and a
/// boot leaves it that way — the seed reports it and writes nothing.
/// Re-activation is an explicit publish (a version bump in the bundle
/// would NOT do it either: the lineage is judged before any version
/// is compared).
#[tokio::test(flavor = "multi_thread")]
async fn a_retired_lineage_stays_retired() {
    let db = TestDb::new().await;
    let registry = PgDeliveryPolicy::new(db.pool.clone());
    sqlx::query("UPDATE delivery_policy SET status = 'retired' WHERE name = $1")
        .bind(POLICY)
        .execute(&db.pool)
        .await
        .expect("the operator retires the policy");

    let report = seed_delivery_policies(&registry, &bundle(), &actor(), chrono::Utc::now(), false)
        .await
        .expect("a retired lineage is not a failure");
    let policy = report
        .rows
        .iter()
        .find(|r| r.name == POLICY)
        .expect("the report names the policy");
    assert!(
        matches!(policy.outcome, SeedOutcome::Retired { newest: 2 }),
        "the policy is reported retired, not re-inserted: {report}"
    );

    let active: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM delivery_policy WHERE name = $1 AND status = 'active'",
    )
    .bind(POLICY)
    .fetch_one(&db.pool)
    .await
    .expect("count active policy rows");
    assert_eq!(
        active, 0,
        "a boot never re-activates a policy an operator retired"
    );
}
