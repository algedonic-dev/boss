//! Postgres-backed coverage for the station registry
//! (116-stations.sql, 118-watchlist-station.sql, 124-repair-station.sql):
//!
//! - the platform seed ships the SDLC batch stations and the filer's
//!   watchlist, active, with predicates the evaluator can actually
//!   parse (a seed row the Rust shape rejects would be a silent dead
//!   station);
//! - every registry write stages its event in `event_outbox` in the
//!   SAME transaction as the stations row (the workflow-registry
//!   posture; the InMemory contract is pinned in `stations::tests`);
//! - every `jobs.station.*` kind is registered in `event_kinds` so the
//!   audit trigger admits it. Deliberately an exact-set assertion
//!   rather than a contains-check: a marker emitted but never
//!   registered is rejected by the trigger at run time, which is the
//!   expensive place to find out.

use boss_core::actor::ActorId;
use boss_jobs::agents::AgentsRegistry;
use boss_jobs::events::{
    STATION_CLAIMED, STATION_DRAFT_SAVED, STATION_PUBLISHED, STATION_QUARANTINED, STATION_RETIRED,
};
use boss_jobs::registry::WorkflowStatus;
use boss_jobs::station_queue::{StationPredicate, StepMatch};
use boss_jobs::stations::StationError;
use boss_jobs::{PgStations, StationKind, StationRegistry, StationSpec};
use boss_testing::TestDb;

fn author() -> ActorId {
    ActorId::Human("emp-cto".into())
}

#[tokio::test(flavor = "multi_thread")]
async fn floor_coordinates_survive_pg() {
    let db = TestDb::new().await;
    let registry = PgStations::new(db.pool.clone());
    let mut row = spec("department-roundtrip");
    row.floor = Some(boss_jobs::stations::FloorPlacement { row: 2, col: 5 });
    registry
        .create_draft(row.clone(), &author(), row.created_at)
        .await
        .unwrap();
    let active = registry
        .publish(&row.name, &author(), row.created_at)
        .await
        .unwrap();
    assert_eq!(registry.get_active(&row.name).await.unwrap(), active);
    assert_eq!(active.floor, row.floor);
}

/// Pass item 4 of design ccfe87ff (backlog 1bb8c479): a station's edges
/// survive both writes — an authored draft published, and the seed's
/// declared publish — and a row that declares none reads back with none.
#[tokio::test(flavor = "multi_thread")]
async fn declared_edges_survive_pg_and_a_row_without_them_reads_none() {
    let db = TestDb::new().await;
    let registry = PgStations::new(db.pool.clone());
    // The declared row first: the authored publish judges its edge
    // against the active rows (backlog 5b80b9f4), so its target is live
    // before it publishes. The seed's declared publish judges no edge —
    // the seed asks that of the whole bundle before any row is written.
    let mut declared = spec("edges-declared");
    declared.status = WorkflowStatus::Active;
    declared.edges = vec!["edges-authored".into(), "edges-none".into()];
    registry
        .publish_declared(declared.clone(), &author(), declared.created_at)
        .await
        .unwrap();
    assert_eq!(
        registry.get_active("edges-declared").await.unwrap().edges,
        declared.edges
    );

    let mut authored = spec("edges-authored");
    authored.edges = vec!["edges-declared".into()];
    registry
        .create_draft(authored.clone(), &author(), authored.created_at)
        .await
        .unwrap();
    let active = registry
        .publish(&authored.name, &author(), authored.created_at)
        .await
        .unwrap();
    assert_eq!(active.edges, authored.edges);
    assert_eq!(registry.get_active(&authored.name).await.unwrap(), active);

    let none = spec("edges-none");
    registry
        .create_draft(none.clone(), &author(), none.created_at)
        .await
        .unwrap();
    let read = registry
        .publish(&none.name, &author(), none.created_at)
        .await
        .unwrap();
    assert!(read.edges.is_empty(), "{:?}", read.edges);
}

/// T2 of design 4b27a65a (backlog 3f58c823): the six tree fields survive
/// both writes and both reads (the typed SELECT and the claim path's
/// `to_jsonb`), and the ROLLBACK HALF: a row written by the column list a
/// build before this car INSERTs reads back outside the tree, and the
/// columns that build's SELECT names read a tree row unchanged.
#[tokio::test(flavor = "multi_thread")]
async fn the_tree_fields_survive_pg_and_an_old_builds_row_reads_outside_the_tree() {
    let db = TestDb::new().await;
    let registry = PgStations::new(db.pool.clone());
    let mut declared = spec("tree-declared");
    // Postgres keeps microseconds and the PG `publish_declared` hands back
    // the row it was given, so the fixture's clock is held to what the
    // column stores or `read == published` could never hold.
    declared.created_at = chrono::SubsecRound::trunc_subsecs(declared.created_at, 6);
    declared.status = WorkflowStatus::Active;
    declared.id = Some("tree-id-1".into());
    declared.parent = Some("tree-id-0".into());
    declared.mode = Some(boss_jobs::StationMode::Standing);
    declared.capacity = Some(2);
    // The rail's line (design c4854fd4, backlog 85cb2103): a column of its
    // own, written by both INSERTs and read by both reads.
    declared.line = Some("it".into());
    // Its place on the parent's floor (backlog e3a927c3): a JSONB column
    // of its own, written by both INSERTs and read by both reads.
    declared.placement = Some(boss_jobs::stations::StationPlacement {
        x: 590,
        y: 120,
        w: 230,
        h: 170,
    });
    declared.machines = vec!["automation:gate-runner".into()];
    declared.allocation.insert(
        "host:w-1".into(),
        boss_jobs::resources::Quantity::from(8_u64),
    );
    let published = registry
        .publish_declared(declared.clone(), &author(), declared.created_at)
        .await
        .unwrap();
    let read = registry.get_active("tree-declared").await.unwrap();
    assert_eq!(read, published);
    assert_eq!(read.line, declared.line, "the line survives the typed read");
    assert_eq!(
        read.placement, declared.placement,
        "the placement survives the typed read"
    );
    assert_eq!(
        (
            read.id,
            read.parent,
            read.mode,
            read.capacity,
            read.machines,
            read.allocation
        ),
        (
            declared.id.clone(),
            declared.parent.clone(),
            declared.mode,
            declared.capacity,
            declared.machines.clone(),
            declared.allocation.clone()
        )
    );
    let jsonb: serde_json::Value =
        sqlx::query_scalar("SELECT to_jsonb(s) FROM stations s WHERE name = 'tree-declared'")
            .fetch_one(&db.pool)
            .await
            .unwrap();
    let via_jsonb: StationSpec = serde_json::from_value(jsonb).expect("the claim path's read");
    assert_eq!(via_jsonb.allocation, declared.allocation);
    assert_eq!(via_jsonb.parent, declared.parent);
    assert_eq!(via_jsonb.line, declared.line, "and the claim path's read");
    assert_eq!(
        via_jsonb.placement, declared.placement,
        "and the placement through the claim path's read"
    );

    // The INSERT a build before this car makes, column for column.
    sqlx::query(
        "INSERT INTO stations (name, version, status, title, kind, predicate, discipline,
             created_at, edges)
         VALUES ('tree-old-build', 1, 'active', 'Old', 'group',
                 '{\"kind\":\"ship-a-change\"}', '[\"priority\",\"age\"]', now(), '{}')",
    )
    .execute(&db.pool)
    .await
    .expect("an old build's insert still lands");
    let old = registry.get_active("tree-old-build").await.unwrap();
    assert!(
        old.id.is_none() && old.parent.is_none() && old.mode.is_none() && old.capacity.is_none(),
        "{old:?}"
    );
    assert!(old.machines.is_empty() && old.allocation.is_empty());
    assert_eq!(old.placement, None, "a NULL placement reads as absent");
    let jsonb: serde_json::Value =
        sqlx::query_scalar("SELECT to_jsonb(s) FROM stations s WHERE name = 'tree-old-build'")
            .fetch_one(&db.pool)
            .await
            .unwrap();
    serde_json::from_value::<StationSpec>(jsonb).expect("NULL tree columns read as absent");

    // The SELECT a build before this car makes reads the tree row.
    let old_select: (String, i32, String, String, serde_json::Value, Vec<String>) = sqlx::query_as(
        "SELECT name, version, status, kind, predicate, edges FROM stations
             WHERE name = 'tree-declared'",
    )
    .fetch_one(&db.pool)
    .await
    .unwrap();
    assert_eq!(old_select.3, "group");
}

/// The seed is the one publish door a platform station row has, and it
/// refuses a station whose edge names a station no active row of the
/// bundle declares — naming both stations, before it opens a database
/// connection. The bundle is the real platform one plus one fixture
/// station; the URL names a host that cannot resolve, so a lint that let
/// the row through would fail on the connect instead, and that failure
/// names neither station.
#[tokio::test]
async fn the_seed_refuses_an_edge_to_an_undeclared_station_before_any_publish() {
    let scratch = boss_testing::scratch_dir("station-edge-seed-refusal");
    let workflow = scratch.join("workflow.toml");
    boss_testing::write_file(
        &workflow,
        r#"
[[workflow]]
kind="station-edge-seed-test"
label="Station edge seed test"
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
    let stations = scratch.join("stations");
    boss_testing::create_dir(&stations);
    let real = std::path::Path::new(boss_jobs::station_seed::platform_stations_path());
    for file in boss_jobs::seed_loader::bundle_files(real).expect("the real station bundle") {
        let name = file.file_name().expect("a file name");
        std::fs::copy(&file, stations.join(name)).expect("copy a real station row");
    }
    let row = |edges: &str| {
        format!(
            "[[station]]\nname='edge-fixture'\nversion=1\nstatus='active'\n\
             title='Edge fixture'\nkind='group'\nedges=[{edges}]\n\
             [station.predicate]\nkind='station-edge-seed-test'\n"
        )
    };
    let run = || {
        std::process::Command::new(env!("CARGO_BIN_EXE_boss-platform-workflow-seed"))
            .env(
                "BOSS_POSTGRES_URL",
                "postgres://boss@unreached.invalid/none",
            )
            .arg("--seed-path")
            .arg(&workflow)
            .arg("--stations-path")
            .arg(&stations)
            // The copied rows are the real tree, and they hold the real
            // pools: the seed judges the tree across both bundles (T2 of
            // design 4b27a65a), so it is handed the pool bundle too.
            .arg("--pools-path")
            .arg(boss_jobs::pools::platform_pools_path())
            .output()
            .expect("actual seed executable")
    };
    boss_testing::write_file(
        &stations.join("edge-fixture.toml"),
        &row("'no-such-station'"),
    );
    let output = run();
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(!output.status.success(), "{stderr}");
    assert!(
        // The station lint's own rendering since backlog 5b80b9f4: one
        // problem type for the seed and the publish door.
        stderr.contains("[edge-fixture]") && stderr.contains("`no-such-station`"),
        "the refusal names both stations: {stderr}"
    );
    // Codex R1 P2 on car 3eb6ecca: a KNOWN station named twice used to pass
    // here and reach the database, and the floor's reader then refused the
    // whole floor. The seed refuses it, naming the station and the target.
    boss_testing::write_file(
        &stations.join("edge-fixture.toml"),
        &row("'gates', 'gates'"),
    );
    let output = run();
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(!output.status.success(), "{stderr}");
    assert!(
        stderr.contains("edge-fixture") && stderr.contains("`gates`"),
        "the refusal names the station and the repeated target: {stderr}"
    );
    assert!(!stderr.contains("connecting to Postgres"), "{stderr}");
    // The control: an edge to a declared station passes the lint and the
    // run goes on to the connect, which fails without naming an edge.
    boss_testing::write_file(&stations.join("edge-fixture.toml"), &row("'gates'"));
    let output = run();
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(!output.status.success(), "unreachable database: {stderr}");
    assert!(!stderr.contains("no-such-station"), "{stderr}");
    assert!(!stderr.contains("station edge lint"), "{stderr}");
    assert!(stderr.contains("connecting to Postgres"), "{stderr}");
}

/// Backlog d0cf2cdc: the seed asks the publish door's question of every
/// station row — is an equipment station's automaton a REGISTERED
/// automation? — before it publishes anything. Registered is what
/// `automation_actors` holds once the run declares the automations
/// bundle, so the real stations pass against the real automations, and a
/// fixture station naming `automation:nobody` refuses the whole run,
/// naming the station and the automaton, with no station and no
/// automation written.
#[tokio::test(flavor = "multi_thread")]
async fn the_seed_refuses_an_equipment_automaton_no_registered_automation_answers_to() {
    let db = TestDb::new().await;
    let scratch = boss_testing::scratch_dir("station-automaton-seed-refusal");
    let workflow = scratch.join("workflow.toml");
    boss_testing::write_file(
        &workflow,
        r#"
[[workflow]]
kind="station-automaton-seed-test"
label="Station automaton seed test"
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
    let stations = scratch.join("stations");
    boss_testing::create_dir(&stations);
    let real = std::path::Path::new(boss_jobs::station_seed::platform_stations_path());
    for file in boss_jobs::seed_loader::bundle_files(real).expect("the real station bundle") {
        let name = file.file_name().expect("a file name");
        std::fs::copy(&file, stations.join(name)).expect("copy a real station row");
    }
    let row = |automaton: &str| {
        format!(
            "[[station]]\nname='kettle'\nversion=1\nstatus='active'\n\
             title='Kettle'\nkind='equipment'\nautomaton='{automaton}'\nbays=1\n\
             [station.predicate]\nkind='station-automaton-seed-test'\n"
        )
    };
    let run = || {
        std::process::Command::new(env!("CARGO_BIN_EXE_boss-platform-workflow-seed"))
            .env("BOSS_POSTGRES_URL", db.url())
            .arg("--seed-path")
            .arg(&workflow)
            .arg("--stations-path")
            .arg(&stations)
            .arg("--pools-path")
            .arg(boss_jobs::pools::platform_pools_path())
            .arg("--automations-path")
            .arg(boss_jobs::agents::automations::platform_automations_path())
            .output()
            .expect("actual seed executable")
    };
    async fn count(pool: &sqlx::PgPool, sql: &str) -> i64 {
        sqlx::query_scalar(sql)
            .fetch_one(pool)
            .await
            .expect("count rows")
    }
    const KETTLES: &str = "SELECT count(*) FROM stations WHERE name = 'kettle'";

    boss_testing::write_file(&stations.join("kettle.toml"), &row("automation:nobody"));
    let output = run();
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(!output.status.success(), "{stderr}");
    assert!(
        stderr.contains("[kettle]") && stderr.contains("`automation:nobody`"),
        "the refusal names the station and the automaton: {stderr}"
    );
    assert_eq!(count(&db.pool, KETTLES).await, 0, "no station written");
    assert_eq!(
        count(&db.pool, "SELECT count(*) FROM automation_actors").await,
        0,
        "refused before any publish: {stderr}"
    );

    // The control: a registered automaton seeds, kettle and all.
    boss_testing::write_file(
        &stations.join("kettle.toml"),
        &row("automation:gate-runner"),
    );
    let output = run();
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(output.status.success(), "{stderr}");
    assert_eq!(count(&db.pool, KETTLES).await, 1, "{stderr}");
}

fn spec(name: &str) -> StationSpec {
    StationSpec::draft(
        name,
        format!("Test {name}"),
        StationKind::Group,
        StationPredicate {
            kind: Some("ship-a-change".into()),
            ..Default::default()
        },
        chrono::Utc::now(),
    )
}

async fn outbox_kinds(db: &TestDb) -> Vec<String> {
    sqlx::query_scalar("SELECT kind FROM event_outbox ORDER BY id")
        .fetch_all(&db.pool)
        .await
        .expect("read outbox kinds")
}

/// An equipment row's automaton and bays survive the Pg adapter, and
/// the table refuses what the viability lint refuses even to a write
/// that never passes a door (item 0a78f8f8, register entry
/// an-equipment-station-declares-its-bays): the deleted `batch` kind,
/// an equipment row with no bays, a generic row with bays.
#[tokio::test(flavor = "multi_thread")]
async fn an_equipment_row_round_trips_and_the_table_refuses_what_the_lint_refuses() {
    let db = TestDb::new().await;
    let registry = PgStations::new(db.pool.clone());
    let mut row = spec("test-equipment");
    row.kind = StationKind::Equipment;
    row.automaton = Some("automation:gate-runner".into());
    row.bays = Some(3);

    // Backlog d0cf2cdc: the door reads `automation_actors`, so an
    // automaton spelled as an automation id but held by no row is
    // refused, naming it, and nothing goes active.
    let mut stray = row.clone();
    stray.automaton = Some("automation:nobody".into());
    registry
        .create_draft(stray.clone(), &author(), stray.created_at)
        .await
        .expect("a draft may name any automaton");
    let refused = registry
        .publish(&stray.name, &author(), stray.created_at)
        .await
        .expect_err("an unregistered automaton is refused");
    let StationError::Unviable(problems) = &refused else {
        panic!("refused as unviable, not {refused}");
    };
    assert_eq!(problems.len(), 1, "{problems:?}");
    assert_eq!(problems[0].field, "automaton");
    assert!(
        problems[0].reason.contains("`automation:nobody`"),
        "{problems:?}"
    );
    assert!(
        registry.get_active(&row.name).await.is_err(),
        "nothing went active"
    );
    assert!(
        registry
            .registered_automatons(&stray)
            .await
            .expect("the registry reads")
            .is_empty(),
        "the dry run's read finds no row either"
    );

    // Registered, the same row publishes.
    boss_jobs::agents::PgAgents::new(db.pool.clone())
        .declare_automations(
            &[boss_jobs::agents::AutomationActor {
                id: "automation:gate-runner".into(),
                role: "platform-admin".into(),
                description: "Runs the gates".into(),
                signs_for: None,
                access_tier: None,
                station: None,
            }],
            &boss_core::publisher::EventStamp::new("jobs", author()),
        )
        .await
        .expect("declare the gate runner");
    assert_eq!(
        registry
            .registered_automatons(&row)
            .await
            .expect("the registry reads"),
        vec!["automation:gate-runner".to_string()]
    );
    registry
        .create_draft(row.clone(), &author(), row.created_at)
        .await
        .expect("draft an equipment row");
    registry
        .publish(&row.name, &author(), row.created_at)
        .await
        .expect("publish an equipment row");
    let read = registry.get_active(&row.name).await.expect("read it back");
    assert_eq!(
        (read.kind, read.automaton.as_deref(), read.bays),
        (
            StationKind::Equipment,
            Some("automation:gate-runner"),
            Some(3)
        )
    );

    for (kind, automaton, bays, why) in [
        ("batch", None, None, "the deleted kind"),
        (
            "equipment",
            Some("automation:gate-runner"),
            None,
            "equipment without bays",
        ),
        ("equipment", None, Some(1), "equipment without an automaton"),
        (
            "equipment",
            Some("emp-david"),
            Some(1),
            "a person as the automaton",
        ),
        (
            "equipment",
            Some("automation:gate-runner"),
            Some(0),
            "zero bays",
        ),
        ("group", None, Some(2), "a generic row with bays"),
        (
            "group",
            Some("automation:gate-runner"),
            None,
            "a generic row with an automaton",
        ),
    ] {
        let refused = sqlx::query(
            "INSERT INTO stations (name, version, status, title, kind, predicate, automaton, bays) \
             VALUES ('raw-write', 1, 'draft', 'Raw', $1, '{}'::jsonb, $2, $3)",
        )
        .bind(kind)
        .bind(automaton)
        .bind(bays)
        .execute(&db.pool)
        .await;
        assert!(refused.is_err(), "the table admitted {why}");
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn platform_seed_ships_the_sdlc_batch_stations() {
    let db = TestDb::new().await;
    let registry = PgStations::new(db.pool.clone());

    let active = registry.list_active().await.expect("list_active");
    let names: Vec<&str> = active.iter().map(|s| s.name.as_str()).collect();
    assert_eq!(
        names,
        vec![
            "design-review",
            "loading-dock",
            "my-watchlist",
            "q.platform-admin.task",
            "repair"
        ],
        "the platform SDLC batch stations seed active, plus the one \
         per-actor row (per-employee stations stay tenant data; \
         `my-watchlist` needs no roster because @me binds at read time), \
         plus the one constraint queue authored over its projection \
         (20260915023614: a machine-owned proof is not a person's task)"
    );

    // The repair queue (migration 124, David's bb86d687): red trains,
    // held on a fact that lives on the STEP. Asserting the clause
    // rather than just the name — the row is only useful if it reads
    // the conductor's verdict where the conductor writes it.
    let repair = registry.get_active("repair").await.expect("repair row");
    assert_eq!(repair.predicate.kind.as_deref(), Some("pr-train"));
    let ci = repair.predicate.step.as_ref().expect("ci-step clause");
    assert_eq!(ci.slug.as_deref(), Some("ci"));
    assert_eq!(
        ci.metadata_equals.get("result"),
        Some(&"failing".to_string())
    );
    assert_eq!(
        repair.discipline,
        vec![boss_jobs::station_queue::DisciplineKey::Age],
        "age alone — every train carries the same priority, so ordering \
         by it first would sort by nothing and then by age"
    );

    let dock = registry.get_active("loading-dock").await.expect("dock row");
    // The migrations alone hold the dock as `group`: the one that
    // deleted `batch` (20261103234537) relabels every stored batch row
    // by the rule the log's reader uses, and the dock's `equipment` row
    // is the bundle's v5, published by the seed this database never ran.
    assert_eq!(dock.kind, StationKind::Group);
    assert_eq!((&dock.automaton, dock.bays), (&None, None));
    assert_eq!(dock.status, WorkflowStatus::Active);
    // The dockRows predicate, ported to data: parked cars only.
    assert_eq!(dock.predicate.kind.as_deref(), Some("ship-a-change"));
    assert_eq!(dock.predicate.metadata_present, vec!["branch".to_string()]);
    assert_eq!(dock.predicate.metadata_absent, vec!["train".to_string()]);
    assert!(dock.predicate.step.is_some(), "review-step clause present");
    assert_eq!(
        dock.discipline,
        boss_jobs::station_queue::default_discipline(),
        "priority, then age"
    );

    let review = registry
        .get_active("design-review")
        .await
        .expect("review row");
    // `design-doc`, not `design-doc-review`: a design doc IS a packet
    // now, so the queue holds the docs themselves rather than a review
    // Job opened per markdown file. The live cluster published this as
    // station v3 through the API and the tree's seed never learned;
    // 202609101900 converged it, because with the corpus panel deleted
    // this predicate is the whole page.
    assert_eq!(review.predicate.kind.as_deref(), Some("design-doc"));
}

/// The design-review station's page context round-trips through the
/// `stations.lens` column (138-station-lens.sql).
///
/// Pinned because this row is what `/it/design` draws its header and
/// panel set from, so a seed that fails to parse is a page with no
/// name. The panel keys are the renderers `DesignReviewPage` ships —
/// a key nothing renders is a declared panel that never appears, which
/// is exactly what this assertion caught on 2026-09-10: the seed said
/// `["rejections", "corpus"]` and the bundle shipped neither any
/// longer, both panels having gone with the markdown corpus they
/// rendered.
#[tokio::test(flavor = "multi_thread")]
async fn the_design_review_lens_round_trips() {
    let db = TestDb::new().await;
    let registry = PgStations::new(db.pool.clone());

    let review = registry
        .get_active("design-review")
        .await
        .expect("review row");
    let lens = review.lens.expect("the review queue declares a lens");
    assert_eq!(lens.title, "Design review");
    assert_eq!(
        lens.eyebrow.as_deref(),
        Some("System Model · Design review")
    );
    assert_eq!(
        lens.panels,
        vec!["queue".to_string()],
        "one panel, and it renders this station's own packets"
    );

    // A station no page renders reads back as "none declared" rather
    // than as an empty page.
    let dock = registry.get_active("loading-dock").await.expect("dock row");
    assert_eq!(dock.lens, None);
}

/// An authored lens carries through the write path: the seed is data,
/// and so is every row published after it.
#[tokio::test(flavor = "multi_thread")]
async fn an_authored_lens_survives_draft_and_publish() {
    let db = TestDb::new().await;
    let registry = PgStations::new(db.pool.clone());
    let now = chrono::Utc::now();

    let mut authored = spec("night-review");
    authored.lens = Some(boss_jobs::StationLens {
        eyebrow: None,
        title: "Night review".into(),
        subtitle: Some("What came in after hours".into()),
        panels: vec!["queue".into()],
        with_steps: false,
    });
    let expected = authored.lens.clone();
    registry
        .create_draft(authored, &author(), now)
        .await
        .expect("draft");
    let published = registry
        .publish("night-review", &author(), now)
        .await
        .expect("publish");
    assert_eq!(published.lens, expected);
    let read_back = registry.get_active("night-review").await.expect("active");
    assert_eq!(read_back.lens, expected);
}

/// The seeded watchlist row survives the round trip through the
/// `stations` columns AND evaluates — the seed is only as good as the
/// Rust shape's ability to read it back, and this row is the first to
/// use both new pieces (the `@me` placeholder in the predicate JSONB,
/// the `terminal_window_days` column).
#[tokio::test(flavor = "multi_thread")]
async fn the_watchlist_row_round_trips_and_binds() {
    let db = TestDb::new().await;
    let registry = PgStations::new(db.pool.clone());

    let row = registry
        .get_active("my-watchlist")
        .await
        .expect("watchlist row");
    assert_eq!(row.kind, StationKind::Actor);
    assert_eq!(row.terminal_window_days, Some(14));
    assert_eq!(
        row.discipline,
        vec![boss_jobs::station_queue::DisciplineKey::Recency],
        "newest activity first — a watchlist is read, not pulled from"
    );
    assert!(
        row.predicate.binds_self(),
        "the stored predicate still carries the placeholder; binding \
         happens at the read edge, per request"
    );

    // And it binds: one row, a concrete queue per actor.
    let bound = row.bind_self(Some("emp-r")).expect("an actor binds");
    assert_eq!(
        bound.predicate.metadata_equals.get("submitted_by"),
        Some(&"emp-r".to_string())
    );
    assert_eq!(row.bind_self(None), None, "no actor, no queue");
}

/// The push-down the watchlist reads through — `metadata @> $1` in
/// `list_jobs`. Without it a per-actor station pages through the whole
/// company's newest packets to find one person's, so this is a
/// correctness test, not an optimization one.
#[tokio::test(flavor = "multi_thread")]
async fn metadata_containment_narrows_in_sql() {
    use boss_core::job::{Job, JobStatus, Priority, Subject};
    use boss_jobs::port::{JobFilter, JobsRepository};

    let db = TestDb::new().await;
    let jobs = boss_jobs::PgJobs::new(db.pool.clone());
    let today = chrono::Utc::now().date_naive();

    for who in ["emp-r", "emp-r", "emp-s"] {
        let mut j = Job::new(
            "user-feedback",
            Subject::new("custom", "/ux/jobs"),
            "filed",
            who,
            Priority::Standard,
            today,
        );
        j.status = JobStatus::Open;
        j.metadata = serde_json::json!({ "submitted_by": who });
        jobs.create_job(&j).await.expect("create");
    }

    let filter = JobFilter {
        metadata_contains: Some(serde_json::json!({ "submitted_by": "emp-r" })),
        ..Default::default()
    };
    let (rows, total) = jobs.list_jobs(&filter, 100, 0).await.expect("list");
    assert_eq!(rows.len(), 2);
    assert_eq!(total, 2, "the count query filters identically");
    assert!(rows.iter().all(|j| j.metadata["submitted_by"] == "emp-r"));

    // Absent filter is not a filter — the existing callers are
    // untouched by the new bind.
    let (all, _) = jobs
        .list_jobs(&JobFilter::default(), 100, 0)
        .await
        .expect("list all");
    assert_eq!(all.len(), 3);
}

#[tokio::test(flavor = "multi_thread")]
async fn pg_writes_stage_their_events_in_the_outbox() {
    let db = TestDb::new().await;
    let registry = PgStations::new(db.pool.clone());
    let now = chrono::Utc::now();

    registry
        .create_draft(spec("night-dock"), &author(), now)
        .await
        .expect("draft");
    registry
        .publish("night-dock", &author(), now)
        .await
        .expect("publish");
    registry
        .retire("night-dock", &author(), now)
        .await
        .expect("retire");
    // Idempotent second retire: no row touched, nothing staged.
    registry
        .retire("night-dock", &author(), now)
        .await
        .expect("retire again");

    assert_eq!(
        outbox_kinds(&db).await,
        vec![
            STATION_DRAFT_SAVED.to_string(),
            STATION_PUBLISHED.to_string(),
            STATION_RETIRED.to_string(),
        ],
        "each registry write stages exactly one outbox row, in write order"
    );

    // Versioning against the seeded loading-dock row: a new draft
    // appends the NEXT version and publish demotes the one before it.
    //
    // The numbers are 3 and 2, not 2 and 1, because
    // 133-dock-wip-limit.sql seeds an active v2 carrying the WIP limit.
    // Written as next/previous relative to what the seed leaves active
    // so the next migration that versions this station does not have to
    // come back and edit arithmetic — the only thing this test is about
    // is that a draft appends and a publish demotes.
    let seeded = registry
        .get_active("loading-dock")
        .await
        .expect("seeded active dock");
    let next = seeded.version + 1;
    let draft = registry
        .create_draft(spec("loading-dock"), &author(), now)
        .await
        .expect("draft the next version");
    assert_eq!(draft.version, next);
    registry
        .publish("loading-dock", &author(), now)
        .await
        .expect("publish the next version");
    let previous = registry
        .get_version("loading-dock", seeded.version)
        .await
        .expect("previously active row");
    assert_eq!(previous.status, WorkflowStatus::Retired);
    let active = registry.get_active("loading-dock").await.expect("active");
    assert_eq!(active.version, next);
}

/// The migration that gave the dock its brake, run against the state it
/// was actually written FOR.
///
/// A scratch database exercises only the easy half: the schema loads in
/// order and the clause lands. The live cluster is the hard half — its
/// `loading-dock` was at version 3, published through the API with no
/// migration behind it, while this tree's migrations produce version 2.
/// That is exactly the divergence that made hardcoding a version number
/// wrong, and nothing else in the suite can see it. So this test
/// reproduces it: publish an un-clause'd row on top of the seeded one
/// the way a hand edit would, re-run the migration, and require it to
/// heal.
///
/// Third run asserts the guard, not politeness: `migrate.sh` applies a
/// file once, but the guard is what lets the INSERT compute
/// `max(version) + 1` without stacking a new version every time anyone
/// loads the directory.
#[tokio::test(flavor = "multi_thread")]
async fn the_dock_brake_migration_lands_on_a_hand_published_row() {
    const MIGRATION: &str = include_str!(
        "../../../../infra/postgres/schema/20260910201453-a-held-car-is-not-at-the-dock.sql"
    );
    /// The clause the row must carry, by the only definition that
    /// matters — what the evaluator parses out of the column.
    fn brake(spec: &StationSpec) -> Vec<String> {
        spec.predicate
            .step
            .as_ref()
            .map(|s| s.metadata_unmarked.clone())
            .unwrap_or_default()
    }

    // This deliberately replays 20260910201453, so retain the schema it
    // ran against. Normal loads apply it before the later column drop;
    // migrate.sh's ledger never replays it after that drop (d2cb38af).
    let db = TestDb::new_without(&["the-station-upstream-column-is-dropped"]).await;
    let registry = PgStations::new(db.pool.clone());
    let now = chrono::Utc::now();

    // The seed's own answer, from the ordered schema load.
    let seeded = registry
        .get_active("loading-dock")
        .await
        .expect("seeded active dock");
    assert_eq!(
        brake(&seeded),
        vec!["hold".to_string()],
        "the migration is in the schema directory, so a fresh database \
         must already hold the brake"
    );

    // Now the live cluster's shape: a later version published by hand
    // that knows nothing about the clause.
    let by_hand = StationSpec::draft(
        "loading-dock",
        "Loading dock — parked ship-a-change cars",
        StationKind::Group,
        StationPredicate {
            kind: Some("ship-a-change".into()),
            status: Some(boss_core::job::JobStatus::Open),
            metadata_present: vec!["branch".into()],
            metadata_absent: vec!["train".into()],
            step: Some(StepMatch {
                slug: Some("review".into()),
                status_in: vec![
                    boss_core::job::StepStatus::Ready,
                    boss_core::job::StepStatus::Active,
                ],
                ..Default::default()
            }),
            ..Default::default()
        },
        now,
    );
    registry
        .create_draft(by_hand, &author(), now)
        .await
        .expect("draft a hand-authored version");
    let hand_published = registry
        .publish("loading-dock", &author(), now)
        .await
        .expect("publish it");
    assert!(brake(&hand_published).is_empty(), "the hazard is set up");

    sqlx::raw_sql(MIGRATION)
        .execute(&db.pool)
        .await
        .expect("the migration applies to a cluster with a hand-published row");
    let healed = registry
        .get_active("loading-dock")
        .await
        .expect("one active row after the migration");
    assert_eq!(
        brake(&healed),
        vec!["hold".to_string()],
        "the migration must carry the brake onto whatever version is active, \
         not onto a version number it guessed"
    );
    assert_eq!(
        healed.version,
        hand_published.version + 1,
        "the new version follows the row it superseded"
    );

    sqlx::raw_sql(MIGRATION)
        .execute(&db.pool)
        .await
        .expect("a second application is legal");
    let again = registry
        .get_active("loading-dock")
        .await
        .expect("still exactly one active row");
    assert_eq!(
        again.version, healed.version,
        "re-applying must be a no-op, or the guard is not guarding and every \
         load of this directory appends a version"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn station_event_kinds_are_registered() {
    let db = TestDb::new().await;
    let kinds: Vec<String> = sqlx::query_scalar(
        "SELECT kind_pattern FROM event_kinds WHERE kind_pattern LIKE 'jobs.station.%' ORDER BY kind_pattern",
    )
    .fetch_all(&db.pool)
    .await
    .expect("read event_kinds");
    assert_eq!(
        kinds,
        // Alphabetical, because the query is ORDER BY kind_pattern —
        // `claimed` sorts before `draft_saved`; `quarantined` sorts
        // between `published` and `retired`.
        //
        // This list is the second home of a fact that lives in the
        // event_kinds INSERTs of the station migrations (§9a's equality
        // test), and it did exactly its job: adding
        // `jobs.station.quarantined` in 120 reddened here, which is how
        // an unregistered marker gets caught before it reaches
        // production rather than after.
        vec![
            STATION_CLAIMED.to_string(),
            STATION_DRAFT_SAVED.to_string(),
            STATION_PUBLISHED.to_string(),
            STATION_QUARANTINED.to_string(),
            boss_jobs::events::STATION_RESOURCE_REFUSED.to_string(),
            STATION_RETIRED.to_string(),
            boss_jobs::events::STATION_SELECTION_EVIDENCE.to_string(),
        ]
    );
}
