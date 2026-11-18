use boss_core::actor::ActorId;
use boss_jobs::pools::{InMemoryPools, PgPools, PoolRegistry, PoolSpec, seed_pools};
use chrono::{DateTime, Utc};
use serde_json::json;

fn pool(name: &str, version: i32) -> PoolSpec {
    serde_json::from_value(json!({
        "name":name,"version":version,"status":"active","unit":"slot",
        "kind":"flow","capacity":{"state":"unknown"},"department":null,
        "created_at":"2026-10-31T00:00:00Z"
    }))
    .expect("declaration")
}

async fn seed_contract(registry: &dyn PoolRegistry) {
    let now = DateTime::<Utc>::UNIX_EPOCH;
    let actor = ActorId::Automation("platform-workflow-seed".into());
    let first = pool("robots:fable", 1);
    seed_pools(registry, std::slice::from_ref(&first), &actor, now, false)
        .await
        .expect("seed");
    seed_pools(registry, std::slice::from_ref(&first), &actor, now, false)
        .await
        .expect("repeat");
    let rows = registry.list_versions(&first.name).await.expect("versions");
    assert_eq!(rows.len(), 1, "repeat must not invent history");
    assert_eq!(
        serde_json::to_value(&rows[0]).expect("wire")["capacity"],
        json!({"state":"unknown"})
    );
    let mut changed = first.clone();
    changed.unit = "changed without a version".into();
    assert!(
        seed_pools(registry, &[changed], &actor, now, false)
            .await
            .is_err()
    );
    let mut second = pool(&first.name, 2);
    second.capacity =
        serde_json::from_value(json!({"state":"configured","amount":"0"})).expect("explicit zero");
    seed_pools(registry, &[second], &actor, now, false)
        .await
        .expect("version bump");
    let rows = registry.list_versions(&first.name).await.expect("history");
    assert_eq!(rows.len(), 2);
    assert_eq!(
        rows.iter()
            .filter(|p| p.status == boss_jobs::registry::WorkflowStatus::Active)
            .count(),
        1
    );
    assert!(
        registry.publish_declared(first, &actor, now).await.is_err(),
        "stale writer may not retire the newer version"
    );
}

// One statement of the port contract, executed against both adapters.
// The adapter-specific tests below additionally inspect each event store.
boss_testing::adapters_agree! {
    adapters {
        in_memory => (InMemoryPools::default(), ()),
        postgres => {
            let db = boss_testing::TestDb::new().await;
            (PgPools::new(db.pool.clone()), db)
        },
    }
    cases {
        both_adapters_preserve_the_declared_seed_contract,
        both_adapters_publish_and_retire_through_the_live_door,
        both_adapters_hold_a_signed_publish_to_its_baseline_inside_the_write,
    }
}

/// Review R1 of car 4019ba69 (2026-11-08): a publish signed over live
/// v1 whose lineage has since gained v2 is refused by the WRITE, naming
/// both versions, and writes nothing — the version floor alone would
/// have taken v3 over a v2 nobody approved. Over the version that is
/// newest it lands; the floor still applies beneath it.
async fn both_adapters_hold_a_signed_publish_to_its_baseline_inside_the_write<R: PoolRegistry>(
    registry: &R,
    adapter: &str,
) {
    use boss_jobs::registry::WorkflowStatus::{Active, Retired};
    let now = DateTime::<Utc>::UNIX_EPOCH;
    let actor = ActorId::Automation("pools-door-test".into());
    registry
        .publish_declared(pool("robots:door", 1), &actor, now)
        .await
        .expect("v1");
    // The baseline the approval was signed over is v1; another publisher
    // lands v2 before the signed write reaches the registry.
    registry
        .publish_declared(pool("robots:door", 2), &actor, now)
        .await
        .expect("v2 from another publisher");
    let refused = registry
        .publish_over(pool("robots:door", 3), 1, &actor, now)
        .await
        .expect_err("a moved baseline is refused by the write");
    let text = refused.to_string();
    assert!(
        text.contains("v1") && text.contains("v2"),
        "{adapter}: names the signed and the live version: {text}"
    );
    let rows = registry
        .list_versions("robots:door")
        .await
        .expect("lineage");
    assert_eq!(
        rows.iter()
            .map(|p| (p.version, p.status))
            .collect::<Vec<_>>(),
        vec![(1, Retired), (2, Active)],
        "{adapter}: the refusal wrote nothing — v2 active, no v3"
    );
    registry
        .publish_over(pool("robots:door", 3), 2, &actor, now)
        .await
        .expect("over the version that is live, the signed row lands");
    assert!(
        registry
            .publish_over(pool("robots:door", 3), 3, &actor, now)
            .await
            .is_err(),
        "{adapter}: the version floor holds beneath the baseline"
    );
    let rows = registry
        .list_versions("robots:door")
        .await
        .expect("lineage");
    assert_eq!(
        rows.iter()
            .map(|p| (p.version, p.status))
            .collect::<Vec<_>>(),
        vec![(1, Retired), (2, Retired), (3, Active)],
        "{adapter}"
    );
}

async fn both_adapters_preserve_the_declared_seed_contract<R: PoolRegistry>(
    registry: &R,
    _adapter: &str,
) {
    seed_contract(registry).await;
}

/// The live pools door's registry half (backlog f399458f): a publish
/// bumps the lineage and retires the incumbent, a retire takes the
/// active version out of service and answers the row it retired, and a
/// name with nothing active answers None and writes nothing. The tree
/// file and the live row must be able to agree, so the version is the
/// author's, never minted here.
async fn both_adapters_publish_and_retire_through_the_live_door<R: PoolRegistry>(
    registry: &R,
    adapter: &str,
) {
    use boss_jobs::registry::WorkflowStatus::{Active, Retired};
    let now = DateTime::<Utc>::UNIX_EPOCH;
    let actor = ActorId::Automation("pools-door-test".into());
    registry
        .publish_declared(pool("robots:door", 1), &actor, now)
        .await
        .expect("v1");
    let mut second = pool("robots:door", 2);
    second.capacity =
        serde_json::from_value(json!({"state":"configured","amount":"6"})).expect("six");
    registry
        .publish_declared(second, &actor, now)
        .await
        .expect("v2 bumps the lineage");
    let rows = registry
        .list_versions("robots:door")
        .await
        .expect("lineage");
    assert_eq!(
        rows.iter()
            .map(|p| (p.version, p.status))
            .collect::<Vec<_>>(),
        vec![(1, Retired), (2, Active)],
        "{adapter}: oldest first, one active"
    );

    let retired = registry
        .retire("robots:door", &actor, now)
        .await
        .expect("retire")
        .expect("an active version was retired");
    assert_eq!((retired.version, retired.status), (2, Retired), "{adapter}");
    assert!(
        registry
            .list_active()
            .await
            .expect("active")
            .iter()
            .all(|p| p.name != "robots:door"),
        "{adapter}: a retired pool is not served as live"
    );
    let rows = registry
        .list_versions("robots:door")
        .await
        .expect("lineage");
    assert_eq!(
        rows.iter()
            .map(|p| (p.version, p.status))
            .collect::<Vec<_>>(),
        vec![(1, Retired), (2, Retired)],
        "{adapter}: retire is a status change, never a deletion"
    );
    assert!(
        registry
            .retire("robots:door", &actor, now)
            .await
            .expect("second retire")
            .is_none(),
        "{adapter}: nothing active, nothing retired"
    );
    assert!(
        registry
            .publish_declared(pool("robots:door", 2), &actor, now)
            .await
            .is_err(),
        "{adapter}: a retired version number is never reused"
    );
    registry
        .publish_declared(pool("robots:door", 3), &actor, now)
        .await
        .expect("a retired lineage republishes above its newest");
}

#[tokio::test]
async fn in_memory_pool_retire_records_one_fact_naming_the_row_it_retired() {
    let registry = InMemoryPools::default();
    both_adapters_publish_and_retire_through_the_live_door(&registry, "in-memory").await;
    let kinds: Vec<String> = registry
        .recorded_events()
        .expect("events")
        .into_iter()
        .map(|e| e.kind)
        .collect();
    assert_eq!(
        kinds,
        [
            boss_jobs::pools::PUBLISHED,
            boss_jobs::pools::PUBLISHED,
            boss_jobs::pools::RETIRED,
            boss_jobs::pools::PUBLISHED,
        ],
        "one fact per write, and the empty retire writes none"
    );
}

#[cfg(feature = "postgres")]
#[tokio::test]
async fn postgres_pool_retire_lands_its_fact_in_the_same_transaction() {
    let db = boss_testing::TestDb::new().await;
    let registry = PgPools::new(db.pool.clone());
    both_adapters_publish_and_retire_through_the_live_door(&registry, "postgres").await;
    let retired: Vec<(String, i32, String)> = sqlx::query_as(
        "SELECT payload->>'name', (payload->>'version')::INT, payload->>'status' \
         FROM event_outbox WHERE kind = 'jobs.pool.retired'",
    )
    .fetch_all(&db.pool)
    .await
    .expect("outbox");
    assert_eq!(
        retired,
        vec![("robots:door".to_string(), 2, "retired".to_string())]
    );
}

/// The live door runs the seed's three cross-registry lints (a pool's
/// station, the station tree, every active protocol's bills) on the
/// registry before and after the change, and refuses only what the
/// change breaks (backlog f399458f).
#[test]
fn the_door_lint_names_only_what_a_change_breaks() {
    use boss_jobs::pools::registry_change_failures;
    let mut stationed = pool("robots:door", 1);
    stationed.station = Some("no-such-station".into());
    let failures = registry_change_failures(&[], std::slice::from_ref(&stationed), &[], &[]);
    assert!(
        failures.iter().any(|f| f.contains("no-such-station")),
        "{failures:?}"
    );
    // A failure the live registry already holds is not this change's.
    assert!(
        registry_change_failures(
            std::slice::from_ref(&stationed),
            std::slice::from_ref(&stationed),
            &[],
            &[]
        )
        .is_empty()
    );
    // Retiring a pool an active protocol's step bills is refused, naming
    // the protocol and the step.
    let workflow = boss_jobs::registry::WorkflowSpec::platform_seed(
        "bills-the-door",
        "Bills the door",
        "platform",
        vec!["custom".into()],
        vec![boss_jobs::registry::StepSpec {
            title: "work".into(),
            kind: "task".into(),
            ready_when: "true".into(),
            resources: Some(serde_json::from_value(json!({"model":"door"})).expect("bill")),
            ..Default::default()
        }],
    );
    let live = [pool("robots:door", 1)];
    let failures = registry_change_failures(&live, &[], &[], std::slice::from_ref(&workflow));
    assert!(
        failures
            .iter()
            .any(|f| f.contains("bills-the-door") && f.contains("robots:door")),
        "{failures:?}"
    );
}

#[tokio::test]
async fn in_memory_pool_seed_is_versioned_idempotent_and_keeps_unknown_distinct_from_zero() {
    let registry = InMemoryPools::default();
    seed_contract(&registry).await;
    assert_eq!(
        registry.recorded_events().expect("events").len(),
        2,
        "one event per actual publish"
    );
}

#[test]
fn every_authored_resource_dimension_names_a_declared_pool() {
    let pools =
        boss_jobs::pools::load_pools(boss_jobs::pools::platform_pools_path()).expect("pool files");
    let step: boss_jobs::registry::StepSpec = serde_json::from_value(json!({
        "title":"work","kind":"task","ready_when":"true",
        "resources":{"model":"opus","vcpu":"8","ram_gb":"32","disk_gb":"70"}
    }))
    .expect("bill");
    boss_jobs::pools::validate_bill(step.resources.as_ref().expect("retained bill"), &pools)
        .expect("declared pools");
    let mut bad = step.resources.expect("bill");
    bad.model = Some("undeclared-model-class".into());
    assert!(
        boss_jobs::pools::validate_bill(&bad, &pools)
            .unwrap_err()
            .contains("robots:undeclared-model-class")
    );
    bad.model = None;
    bad.vcpu = Some((-1).into());
    assert!(boss_jobs::pools::validate_bill(&bad, &pools).is_err());
}

#[cfg(feature = "postgres")]
#[tokio::test]
async fn postgres_pool_seed_keeps_the_same_contract_and_outbox() {
    let db = boss_testing::TestDb::new().await;
    let registry = boss_jobs::pools::PgPools::new(db.pool.clone());
    seed_contract(&registry).await;
    let events: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM event_outbox WHERE kind = 'jobs.pool.published'")
            .fetch_one(&db.pool)
            .await
            .expect("outbox");
    assert_eq!(events, 2);
}

#[test]
fn pool_shapes_refuse_ambiguous_capacity_and_unowned_stock() {
    let base = serde_json::to_value(pool("vcpu", 1)).expect("wire");
    for capacity in [
        json!({"state":"unknown","amount":0}),
        json!({"state":"configured"}),
        json!({"state":"configured","amount":12}),
    ] {
        let mut bad = base.clone();
        bad["capacity"] = capacity;
        assert!(
            serde_json::from_value::<PoolSpec>(bad.clone()).is_err(),
            "must refuse {bad}"
        );
    }
    let mut stock = pool("disk:ci-workspaces", 1);
    stock.kind = boss_jobs::pools::PoolKind::Stock;
    assert!(stock.validate().is_err());
    stock.reclaimer = Some("infra/forge/reap-dead-ci-jobs.sh".into());
    stock.validate().expect("named stock owner");
    // reclaimer_kind is a packet kind on a stock, never a path and never
    // on a flow (backlog 5ece507d).
    stock.reclaimer_kind = Some("maintenance-reap-ci-jobs".into());
    stock
        .validate()
        .expect("a stock names the kind its reclaimer files");
    for bad in ["", "infra/forge/reap-dead-ci-jobs.sh", "Maintenance"] {
        stock.reclaimer_kind = Some(bad.into());
        assert!(stock.validate().is_err(), "must refuse kind {bad:?}");
    }
    stock.reclaimer_kind = None;
    let mut flow = pool("vcpu", 1);
    flow.reclaimer_kind = Some("maintenance-reap-ci-jobs".into());
    assert!(flow.validate().is_err(), "a flow returns nothing");
    stock.department = Some("it".into());
    assert!(
        stock.validate().is_err(),
        "department dimension is reserved, not activated"
    );
}

#[cfg(feature = "postgres")]
#[tokio::test]
async fn a_failed_pool_outbox_write_rolls_back_the_version_and_retirement() {
    let db = boss_testing::TestDb::new().await;
    let registry = boss_jobs::pools::PgPools::new(db.pool.clone());
    let actor = ActorId::Automation("platform-workflow-seed".into());
    registry
        .publish_declared(pool("vcpu", 1), &actor, DateTime::<Utc>::UNIX_EPOCH)
        .await
        .expect("v1");
    sqlx::raw_sql("CREATE FUNCTION reject_pool_outbox() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN IF NEW.kind='jobs.pool.published' THEN RAISE EXCEPTION 'own pool outbox control'; END IF; RETURN NEW; END $$; CREATE TRIGGER reject_pool_outbox BEFORE INSERT ON event_outbox FOR EACH ROW EXECUTE FUNCTION reject_pool_outbox();").execute(&db.pool).await.expect("own isolated failure seam");
    let error = registry
        .publish_declared(pool("vcpu", 2), &actor, DateTime::<Utc>::UNIX_EPOCH)
        .await
        .expect_err("outbox refuses");
    assert!(error.to_string().contains("own pool outbox control"));
    let rows = registry
        .list_versions("vcpu")
        .await
        .expect("read after rollback");
    assert_eq!(rows.len(), 1, "v2 must roll back");
    assert_eq!(
        rows[0].status,
        boss_jobs::registry::WorkflowStatus::Active,
        "v1 retirement must roll back too"
    );
    let count: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM event_outbox WHERE kind='jobs.pool.published'")
            .fetch_one(&db.pool)
            .await
            .expect("facts");
    assert_eq!(count, 1, "no fact for refused v2");
}

#[cfg(feature = "postgres")]
#[tokio::test(flavor = "multi_thread")]
async fn the_actual_seed_binary_publishes_the_declared_pool_bundle_once() {
    let db = boss_testing::TestDb::new().await;
    let scratch = boss_testing::scratch_dir("resource-pool-seed-binary");
    let file = scratch.join("workflow.toml");
    boss_testing::write_file(
        &file,
        r#"
[[workflow]]
kind="resources-seed-test"
label="Resource seed test"
category="test"
subject_kinds=["custom"]
[[workflow.step]]
title="start"
kind="trigger"
ready_when="true"
resources={model="opus",vcpu=8,ram_gb=32,disk_gb=70}
[[workflow.step]]
title="done"
kind="outcome"
ready_when="steps.start.done"
terminal={outcome="done"}
"#,
    );
    let declared =
        boss_jobs::pools::load_pools(boss_jobs::pools::platform_pools_path()).expect("bundle");
    for _ in 0..2 {
        let output =
            tokio::process::Command::new(env!("CARGO_BIN_EXE_boss-platform-workflow-seed"))
                .env("BOSS_POSTGRES_URL", db.url())
                .arg("--seed-path")
                .arg(&file)
                .arg("--pools-path")
                .arg(boss_jobs::pools::platform_pools_path())
                // The real pool bundle names real stations
                // (`disk:ci-workspaces` → `forge-ci`), and the seed's
                // pool-station lint refuses a station the run did not
                // load. `--seed-path` is a file here, so no station
                // bundle sits beside it: pair the real pools with the
                // real stations.
                .arg("--stations-path")
                .arg(boss_jobs::station_seed::platform_stations_path())
                // And the real stations name real automatons, which the
                // seed refuses unless a registered automation answers to
                // each (backlog d0cf2cdc): pair them with the real
                // automations bundle.
                .arg("--automations-path")
                .arg(boss_jobs::agents::automations::platform_automations_path())
                .output()
                .await
                .expect("actual seed executable");
        print!("{}", String::from_utf8_lossy(&output.stdout));
        eprint!("{}", String::from_utf8_lossy(&output.stderr));
        assert!(
            output.status.success(),
            "seed exit {:?}",
            output.status.code()
        );
    }
    let registry = boss_jobs::pools::PgPools::new(db.pool.clone());
    let actual = registry.list_active().await.expect("actual active rows");
    assert_eq!(actual.len(), declared.len());
    let count: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM event_outbox WHERE kind='jobs.pool.published'")
            .fetch_one(&db.pool)
            .await
            .expect("copied publish facts");
    assert_eq!(
        count,
        declared.len() as i64,
        "second seed publishes no duplicate fact"
    );
    for wanted in declared {
        let found = actual
            .iter()
            .find(|p| p.name == wanted.name)
            .expect("declared row");
        assert_eq!(found.capacity, wanted.capacity);
        assert_eq!(found.version, wanted.version);
    }
}

/// Factory taxonomy M3 (item 0a78f8f8): the seed judges a step's
/// `role:<slug>` executor against the live Class registry before any
/// publish — the employee ROLE codes, not departments or statuses that
/// share the subject kind, and not a retired role.
#[cfg(feature = "postgres")]
#[tokio::test]
async fn the_seed_reads_the_live_employee_roles_for_the_executor_lint() {
    let db = boss_testing::TestDb::new().await;
    sqlx::query(
        "INSERT INTO classes (subject_kind, code, display_name, member_attribute, sort_order, retired_at) \
         VALUES ('employee', 'retired-role-fixture', 'Retired', 'role', 999, '2026-11-04T00:00:00Z')",
    )
    .execute(&db.pool)
    .await
    .expect("retired role fixture");
    let roles = boss_jobs::step_executor::live_role_codes(&db.pool)
        .await
        .expect("classes read");
    assert!(roles.contains("platform-admin"), "{roles:?}");
    assert!(
        !roles.contains("it"),
        "a department is not a role: {roles:?}"
    );
    assert!(!roles.contains("full-time"), "{roles:?}");
    assert!(!roles.contains("retired-role-fixture"), "{roles:?}");
}

/// Pass item 3 of design ccfe87ff (backlog 56efd9c2): the seed is the one
/// publish door a pool row has, and it refuses a pool naming a station no
/// active row of the station bundle declares — by name, before it opens a
/// database connection. The URL names a host that cannot resolve, so a
/// lint that let the row through would fail on the connect instead, and
/// that failure does not name the station.
#[cfg(feature = "postgres")]
#[tokio::test]
async fn the_seed_refuses_a_pool_naming_an_undeclared_station_before_any_publish() {
    let scratch = boss_testing::scratch_dir("pool-station-seed-refusal");
    let workflow = scratch.join("workflow.toml");
    boss_testing::write_file(
        &workflow,
        r#"
[[workflow]]
kind="pool-station-seed-test"
label="Pool station seed test"
category="test"
subject_kinds=["custom"]
[[workflow.step]]
title="start"
kind="trigger"
ready_when="true"
[[workflow.step]]
title="done"
kind="outcome"
ready_when="steps.start.done"
terminal={outcome="done"}
"#,
    );
    let pools = scratch.join("pools");
    boss_testing::create_dir(&pools);
    let row = |station: &str| {
        format!(
            "[[pool]]\nname='disk:fixture'\nversion=1\nstatus='active'\nunit='GiB'\n\
             kind='stock'\nreclaimer='infra/forge/reap-dead-ci-jobs.sh'\n\
             capacity={{state='unknown'}}\nstation='{station}'\n"
        )
    };
    let run = |pools: &std::path::Path| {
        std::process::Command::new(env!("CARGO_BIN_EXE_boss-platform-workflow-seed"))
            .env(
                "BOSS_POSTGRES_URL",
                "postgres://boss@unreached.invalid/none",
            )
            .arg("--seed-path")
            .arg(&workflow)
            .arg("--pools-path")
            .arg(pools)
            .arg("--stations-path")
            .arg(boss_jobs::station_seed::platform_stations_path())
            .output()
            .expect("actual seed executable")
    };
    boss_testing::write_file(&pools.join("disk:fixture.toml"), &row("no-such-station"));
    let output = run(&pools);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(!output.status.success(), "{stderr}");
    assert!(
        stderr.contains("no-such-station") && stderr.contains("disk:fixture"),
        "the refusal names the pool and the station: {stderr}"
    );
    // The control: a declared station passes the lint and the run goes on
    // to the connect, which fails without naming any station.
    boss_testing::write_file(&pools.join("disk:fixture.toml"), &row("gates"));
    let output = run(&pools);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(!output.status.success(), "unreachable database: {stderr}");
    assert!(!stderr.contains("names station"), "{stderr}");
}
