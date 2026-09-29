//! The delivery-policy registry answers the same on both adapters —
//! reliability mechanism C of design 3036296f, "adapters agree"
//! (`boss_testing::adapters_agree!`), on the next two ports in the
//! census order after the cadence registry (backlog be459ab9):
//! `DeliveryPolicyRepository` (what the train conductor reads) and
//! `DeliveryPolicyRegistry` (what the platform seed declares through),
//! which `InMemoryDeliveryPolicy` and `PgDeliveryPolicy` each implement
//! together.
//!
//! WHY THESE PORTS. One row is the whole policy the conductor decides by
//! — how many strikes hold a car, how long a train may sit, the budgets
//! that bound what a failure puts on the record, how many gates run at
//! once — and a train pins the version it departed under. Every door
//! test of `/api/delivery-policy` and every conductor test that stands a
//! policy up asks `InMemoryDeliveryPolicy`; production is answered by
//! `PgDeliveryPolicy`. `delivery_policy_pg.rs` states what the one
//! database holds and `delivery::in_memory::tests` what the double does,
//! each in its own words; this file is the one statement both are held
//! to — every method of both ports.
//!
//! The first run found one disagreement, fixed in this car (a production
//! change to both adapters, through one shared check):
//! - A ROW THE TABLE REFUSES. `delivery_policy` CHECKs every budget
//!   above 0; the double checked none, so a declaration the database
//!   refused (a storage error naming a constraint, a 500 at any door)
//!   landed in memory, retired the policy in force, and was then served
//!   to the conductor as the policy. Both adapters now refuse it as
//!   `BadRequest`, naming the policy and the column, before writing
//!   anything — `delivery::check_policy`, one check held to the table's
//!   by `the_policy_check_is_the_tables_check` below, both ways. Cases
//!   `a_row_the_table_refuses_is_a_bad_request_and_writes_nothing` and
//!   `every_budget_at_its_smallest_admitted_value_lands`.
//!
//! No ordering case: no method here answers more than one name, and
//! `live_versions` orders by version, an integer — so the Pg-locale
//! collation class (2987fb2d) has no surface on these ports. No facts
//! case: neither adapter records an event on publish, by the port's own
//! design (the train Job's `delivery_policy_version` stamp is the
//! record); this suite would be the place to add one to both.
//!
//! THE SHAPE, the other suites': each case states its answer — rows,
//! `(version, status)` pairs — and each adapter is held to that stated
//! answer, not merely to the other one. Instants are whole seconds, so
//! Postgres's microseconds and the double's nanoseconds compare equal.
//!
//! Rows the world starts with: the migrations seed `train-conductor`
//! straight into every Postgres database and the in-memory adapter
//! starts empty, so every policy name this file writes starts `suite-`.

use boss_core::actor::ActorId;
use boss_jobs::delivery::{
    DeliveryPolicyError, DeliveryPolicyRegistry, DeliveryPolicyRepository, DeliveryPolicyRow,
    DeliveryPolicySpec, InMemoryDeliveryPolicy, PgDeliveryPolicy, check_policy,
};
use boss_jobs::registry::WorkflowStatus;
use chrono::{DateTime, TimeZone, Utc};
use std::collections::BTreeSet;

boss_testing::adapters_agree! {
    adapters {
        in_memory => (InMemoryDeliveryPolicy::default(), ()),
        postgres => {
            let db = boss_testing::TestDb::new().await;
            (PgDeliveryPolicy::new(db.pool.clone()), db)
        },
    }
    cases {
        a_fresh_registry_holds_no_suite_policy,
        a_publish_lands_active_served_and_read_back_whole,
        a_version_bump_retires_the_prior_row_and_keeps_it_readable_by_version,
        a_publish_not_above_the_newest_is_a_conflict_and_writes_nothing,
        each_name_is_its_own_lineage,
        a_row_the_table_refuses_is_a_bad_request_and_writes_nothing,
        every_budget_at_its_smallest_admitted_value_lands,
    }
}

/// Both ports, as the conductor and the seed each hold them.
trait Policies: DeliveryPolicyRepository + DeliveryPolicyRegistry {}
impl<T: DeliveryPolicyRepository + DeliveryPolicyRegistry> Policies for T {}

// ----- fixtures ------------------------------------------------------------

fn actor() -> ActorId {
    ActorId::Automation("platform-workflow-seed".into())
}

fn at(h: u32, m: u32) -> DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 9, 29, h, m, 0).unwrap()
}

/// A policy whose every budget is distinct, so a column read into the
/// wrong field cannot compare equal.
fn row(name: &str, version: i32) -> DeliveryPolicyRow {
    DeliveryPolicyRow {
        name: name.into(),
        version,
        max_red_trains: 2,
        stall_hours: 6,
        consist_budget_secs: 60,
        consist_output_budget: 1200,
        consist_files_named: 7,
        skip_reason_file_budget: 96,
        blip_cause_budget: 80,
        ci_host_floor_gb: 40,
        gate_max_concurrent: 3,
    }
}

/// Every budget at 1 — the smallest value the table admits.
fn ones(name: &str) -> DeliveryPolicyRow {
    DeliveryPolicyRow {
        name: name.into(),
        version: 1,
        max_red_trains: 1,
        stall_hours: 1,
        consist_budget_secs: 1,
        consist_output_budget: 1,
        consist_files_named: 1,
        skip_reason_file_budget: 1,
        blip_cause_budget: 1,
        ci_host_floor_gb: 1,
        gate_max_concurrent: 1,
    }
}

/// `row` with the one budget `column` set to `value`.
fn with(mut r: DeliveryPolicyRow, column: &str, value: i32) -> DeliveryPolicyRow {
    let slot = match column {
        "max_red_trains" => &mut r.max_red_trains,
        "stall_hours" => &mut r.stall_hours,
        "consist_budget_secs" => &mut r.consist_budget_secs,
        "consist_output_budget" => &mut r.consist_output_budget,
        "consist_files_named" => &mut r.consist_files_named,
        "skip_reason_file_budget" => &mut r.skip_reason_file_budget,
        "blip_cause_budget" => &mut r.blip_cause_budget,
        "ci_host_floor_gb" => &mut r.ci_host_floor_gb,
        "gate_max_concurrent" => &mut r.gate_max_concurrent,
        other => panic!("{other} is not a budget of DeliveryPolicyRow"),
    };
    *slot = value;
    r
}

/// A declaration — what `infra/platform/delivery-policy/<name>.toml` is.
/// Declared retired and unstamped, so a publish that kept either would
/// show.
fn declared(row: DeliveryPolicyRow) -> DeliveryPolicySpec {
    DeliveryPolicySpec {
        status: WorkflowStatus::Retired,
        row,
        created_at: DateTime::<Utc>::UNIX_EPOCH,
    }
}

/// The row a publish at `now` writes: active, stamped by the seed's clock.
fn written(row: DeliveryPolicyRow, now: DateTime<Utc>) -> DeliveryPolicySpec {
    DeliveryPolicySpec {
        status: WorkflowStatus::Active,
        row,
        created_at: now,
    }
}

async fn publish<P: Policies>(
    p: &P,
    row: DeliveryPolicyRow,
    now: DateTime<Utc>,
) -> Result<DeliveryPolicySpec, DeliveryPolicyError> {
    p.publish_declared(declared(row), &actor(), now).await
}

async fn lineage<P: Policies>(p: &P, name: &str) -> Vec<(i32, WorkflowStatus)> {
    p.live_versions(name)
        .await
        .expect("live_versions answers")
        .into_iter()
        .map(|s| (s.version(), s.status))
        .collect()
}

async fn in_force<P: Policies>(p: &P, name: &str) -> Option<DeliveryPolicyRow> {
    p.active_policy(name).await.expect("active_policy answers")
}

async fn pinned<P: Policies>(p: &P, name: &str, version: i32) -> Option<DeliveryPolicyRow> {
    p.policy_version(name, version)
        .await
        .expect("policy_version answers")
}

// ----- cases ---------------------------------------------------------------

/// Nothing written: no suite policy is in force, no version of one is
/// readable, and no lineage holds one — each an answer, not an error.
async fn a_fresh_registry_holds_no_suite_policy<P: Policies>(p: &P, adapter: &str) {
    assert_eq!(in_force(p, "suite-none").await, None, "{adapter}");
    assert_eq!(pinned(p, "suite-none", 1).await, None, "{adapter}");
    assert!(lineage(p, "suite-none").await.is_empty(), "{adapter}");
}

/// A publish lands active at its declared version, stamped by the seed's
/// clock over the declaration's own status and `created_at`; the write
/// answers the row it wrote, the lineage reads that row back whole, and
/// the conductor's two reads serve it column for column.
async fn a_publish_lands_active_served_and_read_back_whole<P: Policies>(p: &P, adapter: &str) {
    let now = at(12, 0);
    let want = written(row("suite-one", 1), now);
    let got = publish(p, row("suite-one", 1), now)
        .await
        .expect("publish lands");
    assert_eq!(got, want, "{adapter}: the write answers the row written");
    assert_eq!(
        p.live_versions("suite-one").await.expect("lineage"),
        vec![want],
        "{adapter}: the lineage reads it back whole"
    );
    assert_eq!(
        in_force(p, "suite-one").await,
        Some(row("suite-one", 1)),
        "{adapter}: the conductor's read serves it"
    );
    assert_eq!(
        pinned(p, "suite-one", 1).await,
        Some(row("suite-one", 1)),
        "{adapter}: a train pinned to it reads it"
    );
    assert_eq!(pinned(p, "suite-one", 2).await, None, "{adapter}");
}

/// A publish above the newest retires the active row BY NAME and puts
/// the new one in force; the lineage keeps both in version order, and
/// the retired version stays readable by version — the case pinning
/// exists for, a train that departed under v1 while v2 landed.
async fn a_version_bump_retires_the_prior_row_and_keeps_it_readable_by_version<P: Policies>(
    p: &P,
    adapter: &str,
) {
    let v1 = row("suite-bump", 1);
    let v2 = DeliveryPolicyRow {
        version: 2,
        ci_host_floor_gb: 90,
        ..row("suite-bump", 1)
    };
    publish(p, v1.clone(), at(12, 0)).await.expect("v1");
    publish(p, v2.clone(), at(13, 0)).await.expect("v2");
    assert_eq!(
        p.live_versions("suite-bump").await.expect("lineage"),
        vec![
            DeliveryPolicySpec {
                status: WorkflowStatus::Retired,
                ..written(v1.clone(), at(12, 0))
            },
            written(v2.clone(), at(13, 0)),
        ],
        "{adapter}: the retired row keeps its own stamp"
    );
    assert_eq!(
        in_force(p, "suite-bump").await,
        Some(v2.clone()),
        "{adapter}"
    );
    assert_eq!(pinned(p, "suite-bump", 1).await, Some(v1), "{adapter}");
    assert_eq!(pinned(p, "suite-bump", 2).await, Some(v2), "{adapter}");
}

/// A publish at or below the newest version the lineage holds — any
/// status — is a `Conflict` naming that version, and writes nothing: the
/// same version (an overwrite), a lower one, and a version below 1 on a
/// fresh name. The policy in force is untouched; above it lands.
async fn a_publish_not_above_the_newest_is_a_conflict_and_writes_nothing<P: Policies>(
    p: &P,
    adapter: &str,
) {
    publish(p, row("suite-pin", 1), at(11, 0))
        .await
        .expect("v1");
    publish(p, row("suite-pin", 3), at(12, 0))
        .await
        .expect("v3");
    for v in [3, 2] {
        let err = publish(p, with(row("suite-pin", v), "stall_hours", 9), at(12, 5))
            .await
            .expect_err("not above v3");
        assert!(
            matches!(&err, DeliveryPolicyError::Conflict(m) if m.contains("(v3)")),
            "{adapter}: v{v}: {err}"
        );
    }
    let err = publish(p, row("suite-fresh", 0), at(12, 5))
        .await
        .expect_err("versions start at 1");
    assert!(
        matches!(&err, DeliveryPolicyError::Conflict(m) if m.contains("(v0)")),
        "{adapter}: {err}"
    );
    assert_eq!(
        lineage(p, "suite-pin").await,
        vec![(1, WorkflowStatus::Retired), (3, WorkflowStatus::Active)],
        "{adapter}: no refused publish wrote or retired a row"
    );
    assert!(lineage(p, "suite-fresh").await.is_empty(), "{adapter}");
    assert_eq!(
        in_force(p, "suite-pin").await,
        Some(row("suite-pin", 3)),
        "{adapter}: the policy in force is the one it was"
    );
    publish(p, row("suite-pin", 4), at(12, 10))
        .await
        .expect("v4 is above v3");
    assert_eq!(
        in_force(p, "suite-pin").await,
        Some(row("suite-pin", 4)),
        "{adapter}"
    );
}

/// Names are separate lineages: a publish retires only its own name's
/// row, a name's floor is its own newest, and a name differing only in
/// case is a different policy.
async fn each_name_is_its_own_lineage<P: Policies>(p: &P, adapter: &str) {
    publish(p, row("suite-a", 3), at(12, 0))
        .await
        .expect("a v3");
    publish(p, row("suite-b", 1), at(12, 5))
        .await
        .expect("b's floor is b's own");
    publish(p, row("suite-A", 1), at(12, 10))
        .await
        .expect("a name in another case is another name");
    for (name, version) in [("suite-a", 3), ("suite-b", 1), ("suite-A", 1)] {
        assert_eq!(
            lineage(p, name).await,
            vec![(version, WorkflowStatus::Active)],
            "{adapter}: {name}"
        );
        assert_eq!(
            in_force(p, name).await,
            Some(row(name, version)),
            "{adapter}: {name}"
        );
    }
    assert_eq!(pinned(p, "suite-b", 3).await, None, "{adapter}");
}

/// A declaration the table's CHECKs refuse — any budget at 0 or below —
/// is a `BadRequest` naming the policy and the column, and writes
/// nothing: the policy in force stays in force. Postgres used to answer
/// a storage error naming a constraint; the double used to land the row
/// and serve it to the conductor.
async fn a_row_the_table_refuses_is_a_bad_request_and_writes_nothing<P: Policies>(
    p: &P,
    adapter: &str,
) {
    publish(p, row("suite-bad", 1), at(12, 0))
        .await
        .expect("v1");
    for (i, (bad, column)) in refused_rows("suite-bad").into_iter().enumerate() {
        let bad = DeliveryPolicyRow {
            version: 2 + i as i32,
            ..bad
        };
        let err = publish(p, bad, at(12, 5))
            .await
            .expect_err("the table refuses it");
        assert!(
            matches!(&err, DeliveryPolicyError::BadRequest(m)
                if m.contains(column) && m.contains("suite-bad")),
            "{adapter}: {column} is refused naming the policy and itself: {err}"
        );
    }
    assert_eq!(
        lineage(p, "suite-bad").await,
        vec![(1, WorkflowStatus::Active)],
        "{adapter}: no refused declaration wrote or retired a row"
    );
    assert_eq!(
        in_force(p, "suite-bad").await,
        Some(row("suite-bad", 1)),
        "{adapter}: the conductor is still served the policy in force"
    );
}

/// The smallest policy the table admits lands on both.
async fn every_budget_at_its_smallest_admitted_value_lands<P: Policies>(p: &P, adapter: &str) {
    let got = publish(p, ones("suite-ones"), at(12, 0)).await;
    assert!(got.is_ok(), "{adapter}: {got:?}");
    assert_eq!(
        in_force(p, "suite-ones").await,
        Some(ones("suite-ones")),
        "{adapter}"
    );
}

/// Every budget `check_policy` judges, read off the row's own list
/// (`DeliveryPolicyRow::budgets`) rather than typed a second time here —
/// the pin below holds that list to the table's CHECKs.
fn budgets() -> Vec<&'static str> {
    row("any", 1).budgets().iter().map(|(c, _)| *c).collect()
}

/// Each budget at 0 and at -1, with the column its refusal names.
fn refused_rows(name: &str) -> Vec<(DeliveryPolicyRow, &'static str)> {
    budgets()
        .into_iter()
        .flat_map(|c| [0, -1].map(|v| (with(row(name, 1), c, v), c)))
        .collect()
}

// ----- the pin -------------------------------------------------------------

/// A FACT THAT LIVES TWICE (CLAUDE.md §9a): what `delivery_policy` admits
/// is stated by its CHECKs in SQL and by `delivery::check_policy` in
/// Rust, and the in-memory adapter answers by the Rust. Pinned BOTH ways
/// against the live table the migrations build:
/// - the LIST: every CHECK on the table but `status`'s is exactly
///   `<column> > 0`, and those columns are exactly the ones
///   `DeliveryPolicyRow::budgets` hands `check_policy` — a budget column
///   added with a CHECK, or a CHECK loosened or reshaped, goes red naming
///   it;
/// - the VERDICTS: every row this suite states an answer for, plus edge
///   rows, gets the same verdict from `check_policy` as from a raw
///   INSERT that bypasses both adapters — so a budget `check_policy`
///   forgets, or one it checks that the table does not, goes red naming
///   the row.
#[tokio::test(flavor = "multi_thread")]
async fn the_policy_check_is_the_tables_check() {
    let db = boss_testing::TestDb::new().await;

    let defs: Vec<(String, String)> = sqlx::query_as(
        "SELECT conname::text, pg_get_constraintdef(oid) FROM pg_constraint \
         WHERE conrelid = 'delivery_policy'::regclass AND contype = 'c' ORDER BY conname",
    )
    .fetch_all(&db.pool)
    .await
    .expect("read pg_constraint");
    let mut checked = BTreeSet::new();
    for (name, def) in &defs {
        if name == "delivery_policy_status_check" {
            continue;
        }
        let column = def
            .strip_prefix("CHECK ((")
            .and_then(|d| d.strip_suffix(" > 0))"))
            .unwrap_or_else(|| panic!("{name} is not `<column> > 0`: {def}"));
        checked.insert(column.to_string());
    }
    assert_eq!(
        checked,
        budgets()
            .into_iter()
            .map(String::from)
            .collect::<BTreeSet<_>>(),
        "the table's budget CHECKs are DeliveryPolicyRow::budgets: {defs:?}"
    );

    let corpus = [row("edge-typical", 1), ones("edge-ones")]
        .into_iter()
        .chain(refused_rows("edge-refused").into_iter().map(|(r, _)| r))
        .chain(
            budgets()
                .into_iter()
                .map(|c| with(row("edge-min", 1), c, i32::MIN)),
        )
        .chain(
            budgets()
                .into_iter()
                .map(|c| with(row("edge-max", 1), c, i32::MAX)),
        );
    let mut disagree = Vec::new();
    for r in corpus {
        let rust = check_policy(&r);
        let table = table_admits(&db.pool, &r).await;
        if rust.is_ok() != table {
            disagree.push(format!(
                "{r:?}: check_policy says {rust:?}, the table {}",
                if table { "admits it" } else { "refuses it" }
            ));
        }
    }
    assert!(
        disagree.is_empty(),
        "check_policy and delivery_policy's CHECKs disagree on:\n{}",
        disagree.join("\n")
    );
}

/// Would the live table take this row? A raw INSERT, bypassing both
/// adapters, rolled back whatever it answers.
async fn table_admits(pool: &sqlx::PgPool, r: &DeliveryPolicyRow) -> bool {
    let mut tx = pool.begin().await.expect("begin");
    let got = sqlx::query(
        "INSERT INTO delivery_policy (name, version, status, max_red_trains, stall_hours, \
         consist_budget_secs, consist_output_budget, consist_files_named, \
         skip_reason_file_budget, blip_cause_budget, ci_host_floor_gb, gate_max_concurrent) \
         VALUES ($1, 1, 'active', $2, $3, $4, $5, $6, $7, $8, $9, $10)",
    )
    .bind(&r.name)
    .bind(r.max_red_trains)
    .bind(r.stall_hours)
    .bind(r.consist_budget_secs)
    .bind(r.consist_output_budget)
    .bind(r.consist_files_named)
    .bind(r.skip_reason_file_budget)
    .bind(r.blip_cause_budget)
    .bind(r.ci_host_floor_gb)
    .bind(r.gate_max_concurrent)
    .execute(&mut *tx)
    .await;
    tx.rollback().await.expect("rollback");
    got.is_ok()
}
