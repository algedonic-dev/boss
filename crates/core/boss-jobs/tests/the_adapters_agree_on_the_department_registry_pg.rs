//! The departments registry answers the same on both
//! `DepartmentRegistry` adapters — reliability mechanism C of design
//! 3036296f, "adapters agree" (`boss_testing::adapters_agree!`), on the
//! next port in the census order after the subject-kind registry
//! (backlog be459ab9).
//!
//! WHY THIS PORT. The roster is read AT FIRE TIME by the weekly
//! department-retro rule, which opens one packet per row `list` answers,
//! and written by `boss tenant publish` through
//! `POST /api/departments/batch`. Every door test of both asks
//! `InMemoryDepartments`; production is answered by `PgDepartments`.
//! `departments_are_published_pg.rs` pins what only Postgres can hold
//! (the Subject identity row, the retirement stamp); this file is the one
//! statement both adapters are held to — both methods of the port, and
//! every write judged by the fact it leaves.
//!
//! The first run found one disagreement, in the Postgres adapter against
//! the port's own words, fixed in this car (a production change to
//! `PgDepartments::list`):
//! - ORDER. `list` promises "the registry's own `sort_order` then by
//!   code". Postgres broke a `sort_order` tie by the database's locale,
//!   which ignores `-` at first level, so `suite-ab` listed before
//!   `suite-a-z` there and after it in memory. It now sorts the code
//!   `COLLATE "C"` — byte order, the order the double holds and the only
//!   one both can; the Workflow, credentials and station registries'
//!   suites found and fixed the same defect the same way. Case
//!   `list_is_sort_order_then_code_in_byte_order`.
//!
//! THE SHAPE, the other suites': each case states its answer — the
//! outcome, the codes listed, the facts — and each adapter is held to
//! that stated answer, not merely to the other one. Every write is also
//! judged by the FACTS it leaves: the `department.*` events the in-memory
//! adapter collects and Postgres stages on `event_outbox` in the batch's
//! own transaction.
//!
//! Rows the world starts with: migration 20260919181324 seeds thirteen
//! departments into every Postgres database and the in-memory adapter
//! starts empty, so every code this file writes starts `suite-`, and an
//! unfiltered read (`list`, the outbox) is judged over those codes only.

use boss_core::actor::ActorId;
use boss_core::publish::{FieldChange, PublishMode};
use boss_core::publisher::EventStamp;
use boss_jobs::department::declare::{DepartmentInput, DepartmentsBatchOutcome};
use boss_jobs::department::registry::{
    DEPARTMENT_DECLARED, DEPARTMENT_UPDATED, DepartmentRegistry, InMemoryDepartments, PgDepartments,
};
use serde_json::{Value, json};

/// The registry under test and the facts its writes left, read the way
/// each adapter keeps them.
trait World {
    type R: DepartmentRegistry;
    fn repo(&self) -> &Self::R;
    /// Every `department.*` fact about a `suite-` code recorded so far,
    /// in the order recorded: `(kind, payload)`.
    async fn facts(&self) -> Vec<(String, Value)>;
}

fn is_suite_fact(kind: &str, payload: &Value) -> bool {
    kind.starts_with("department.")
        && payload["code"]
            .as_str()
            .is_some_and(|c| c.starts_with("suite-"))
}

struct InMemory(InMemoryDepartments);

impl World for InMemory {
    type R = InMemoryDepartments;
    fn repo(&self) -> &InMemoryDepartments {
        &self.0
    }
    async fn facts(&self) -> Vec<(String, Value)> {
        self.0
            .recorded_events()
            .into_iter()
            .filter(|e| is_suite_fact(&e.kind, &e.payload))
            .map(|e| (e.kind, e.payload))
            .collect()
    }
}

struct Postgres {
    repo: PgDepartments,
    pool: sqlx::PgPool,
}

impl World for Postgres {
    type R = PgDepartments;
    fn repo(&self) -> &PgDepartments {
        &self.repo
    }
    async fn facts(&self) -> Vec<(String, Value)> {
        let rows: Vec<(String, Value)> = sqlx::query_as(
            "SELECT kind, payload FROM event_outbox WHERE kind LIKE 'department.%' ORDER BY id",
        )
        .fetch_all(&self.pool)
        .await
        .expect("read the outbox");
        rows.into_iter()
            .filter(|(k, p)| is_suite_fact(k, p))
            .collect()
    }
}

boss_testing::adapters_agree! {
    adapters {
        in_memory => (InMemory(InMemoryDepartments::new()), ()),
        postgres => {
            let db = boss_testing::TestDb::new().await;
            let world = Postgres { repo: PgDepartments::new(db.pool.clone()), pool: db.pool.clone() };
            (world, db)
        },
    }
    cases {
        a_fresh_registry_lists_no_suite_department,
        an_absent_code_is_inserted_listed_and_declared,
        a_held_row_that_differs_is_kept_and_named_by_default,
        a_take_updates_every_declared_field_and_records_the_change,
        a_retirement_leaves_the_list_and_is_recorded_once,
        a_row_declared_retired_lands_but_is_never_listed,
        list_is_sort_order_then_code_in_byte_order,
    }
}

// ----- fixtures ------------------------------------------------------------

fn stamp() -> EventStamp {
    EventStamp::new("jobs", ActorId::Human("emp-cto".into()))
}

fn dept(code: &str, sort_order: i32) -> DepartmentInput {
    DepartmentInput {
        code: code.into(),
        display_name: format!("The {code} department"),
        function: "operations".into(),
        sort_order,
        retired: false,
    }
}

async fn publish<W: World>(
    w: &W,
    rows: &[DepartmentInput],
    mode: PublishMode,
) -> DepartmentsBatchOutcome {
    w.repo()
        .publish(rows, mode, &stamp())
        .await
        .expect("publish answers")
}

/// Every listed `suite-` code, in the order the registry answered.
async fn listed<W: World>(w: &W) -> Vec<String> {
    w.repo()
        .list()
        .await
        .expect("list answers")
        .into_iter()
        .map(|d| d.code)
        .filter(|c| c.starts_with("suite-"))
        .collect()
}

/// `(kind, code)` for each fact.
async fn fact_rows<W: World>(w: &W) -> Vec<(String, String)> {
    w.facts()
        .await
        .into_iter()
        .map(|(kind, p)| (kind, p["code"].as_str().unwrap_or("").to_string()))
        .collect()
}

fn facts(v: &[(&str, &str)]) -> Vec<(String, String)> {
    v.iter()
        .map(|(k, c)| (k.to_string(), c.to_string()))
        .collect()
}

/// The declaration as a `department.declared` payload states it.
fn declared_payload(d: &DepartmentInput) -> Value {
    json!({
        "code": d.code,
        "display_name": d.display_name,
        "function": d.function,
        "sort_order": d.sort_order,
        "retired": d.retired,
        "declared_by": "emp-cto",
        "_actor": "emp-cto",
    })
}

// ----- cases -----------------------------------------------------------------

/// Nothing written: no suite department is listed and no fact names one.
async fn a_fresh_registry_lists_no_suite_department<W: World>(w: &W, adapter: &str) {
    assert!(listed(w).await.is_empty(), "{adapter}");
    assert!(w.facts().await.is_empty(), "{adapter}");
}

/// A code the registry does not hold is inserted — every declared field
/// as declared — listed, and records ONE `department.declared` whose
/// payload is the declaration plus `declared_by`, signed by the stamp's
/// actor. The outcome counts it inserted.
async fn an_absent_code_is_inserted_listed_and_declared<W: World>(w: &W, adapter: &str) {
    let a = dept("suite-alpha", 5);
    let mut b = dept("suite-beta", 6);
    b.function = "revenue".into();
    let out = publish(w, &[a.clone(), b.clone()], PublishMode::InsertIfAbsent).await;
    assert_eq!(
        out,
        DepartmentsBatchOutcome {
            received: 2,
            inserted: 2,
            ..Default::default()
        },
        "{adapter}"
    );
    let got: Vec<_> = w
        .repo()
        .list()
        .await
        .expect("list")
        .into_iter()
        .filter(|d| d.code.starts_with("suite-"))
        .map(|d| (d.code, d.display_name, d.function))
        .collect();
    assert_eq!(
        got,
        vec![
            (a.code.clone(), a.display_name.clone(), a.function.clone()),
            (b.code.clone(), b.display_name.clone(), b.function.clone()),
        ],
        "{adapter}: every field reads back as declared"
    );
    let recorded = w.facts().await;
    assert_eq!(
        recorded,
        vec![
            (DEPARTMENT_DECLARED.to_string(), declared_payload(&a)),
            (DEPARTMENT_DECLARED.to_string(), declared_payload(&b)),
        ],
        "{adapter}: one fact per inserted row, the declaration as inserted"
    );
}

/// Under the default a held row is the truth: a declaration that differs
/// is KEPT, named with the fields it differs on, and changes nothing;
/// one identical to the held row is `unchanged`. Neither records a fact.
async fn a_held_row_that_differs_is_kept_and_named_by_default<W: World>(w: &W, adapter: &str) {
    let held = dept("suite-held", 5);
    publish(w, std::slice::from_ref(&held), PublishMode::InsertIfAbsent).await;
    let before = fact_rows(w).await;

    let mut differs = held.clone();
    differs.display_name = "Renamed".into();
    differs.sort_order = 9;
    let out = publish(
        w,
        &[differs, held.clone(), dept("suite-new", 7)],
        PublishMode::InsertIfAbsent,
    )
    .await;
    assert_eq!(
        (out.received, out.inserted, out.unchanged, out.updated.len()),
        (3, 1, 1, 0),
        "{adapter}: {out:?}"
    );
    assert_eq!(out.kept.len(), 1, "{adapter}: {out:?}");
    assert_eq!(out.kept[0].id, "suite-held", "{adapter}");
    assert_eq!(
        out.kept[0].differs,
        vec!["display_name".to_string(), "sort_order".to_string()],
        "{adapter}: the kept row names every field it differs on"
    );
    let got = w.repo().list().await.expect("list");
    let row = got
        .iter()
        .find(|d| d.code == "suite-held")
        .expect("still listed");
    assert_eq!(
        row.display_name, held.display_name,
        "{adapter}: the held row is untouched"
    );
    let mut want = before;
    want.push((DEPARTMENT_DECLARED.to_string(), "suite-new".to_string()));
    assert_eq!(
        fact_rows(w).await,
        want,
        "{adapter}: only the insert records"
    );
}

/// Under `Take` a held row that differs is UPDATED on every declared
/// field; the outcome names each change from → to, and one
/// `department.updated` records the row as it now reads, the same
/// changes and `updated_by`.
async fn a_take_updates_every_declared_field_and_records_the_change<W: World>(
    w: &W,
    adapter: &str,
) {
    let held = dept("suite-taken", 5);
    publish(w, std::slice::from_ref(&held), PublishMode::InsertIfAbsent).await;
    let mut declared = held.clone();
    declared.display_name = "Taken over".into();
    declared.function = "governance".into();
    declared.sort_order = 3;

    let out = publish(w, std::slice::from_ref(&declared), PublishMode::Take).await;
    assert_eq!(
        (out.received, out.inserted, out.unchanged, out.kept.len()),
        (1, 0, 0, 0),
        "{adapter}: {out:?}"
    );
    assert_eq!(out.updated.len(), 1, "{adapter}: {out:?}");
    let changes = vec![
        FieldChange::new("display_name", &held.display_name, &declared.display_name),
        FieldChange::new("function", &held.function, &declared.function),
        FieldChange::new("sort_order", held.sort_order, declared.sort_order),
    ];
    assert_eq!(out.updated[0].id, "suite-taken", "{adapter}");
    assert_eq!(out.updated[0].changes, changes, "{adapter}");

    let got = w.repo().list().await.expect("list");
    let row = got
        .iter()
        .find(|d| d.code == "suite-taken")
        .expect("listed");
    assert_eq!(
        (row.display_name.as_str(), row.function.as_str()),
        ("Taken over", "governance"),
        "{adapter}: the take landed"
    );

    let recorded = w.facts().await;
    assert_eq!(recorded.len(), 2, "{adapter}: {recorded:?}");
    assert_eq!(
        recorded[1],
        (
            DEPARTMENT_UPDATED.to_string(),
            json!({
                "code": "suite-taken",
                "display_name": "Taken over",
                "function": "governance",
                "sort_order": 3,
                "retired": false,
                "changes": changes,
                "updated_by": "emp-cto",
                "_actor": "emp-cto",
            })
        ),
        "{adapter}: the fact is the row as it now reads, and what changed"
    );
}

/// A take of `retired = true` withdraws the department: it leaves `list`
/// and records one `department.updated` naming `retired false → true`.
/// Taking the same declaration again is `unchanged` and SILENT. Taking
/// `retired = false` brings it back.
async fn a_retirement_leaves_the_list_and_is_recorded_once<W: World>(w: &W, adapter: &str) {
    let live = dept("suite-gone", 5);
    publish(w, std::slice::from_ref(&live), PublishMode::InsertIfAbsent).await;
    let mut retired = live.clone();
    retired.retired = true;
    for _ in 0..2 {
        publish(w, std::slice::from_ref(&retired), PublishMode::Take).await;
    }
    assert!(
        listed(w).await.is_empty(),
        "{adapter}: a retired row is not a department"
    );
    assert_eq!(
        fact_rows(w).await,
        facts(&[
            (DEPARTMENT_DECLARED, "suite-gone"),
            (DEPARTMENT_UPDATED, "suite-gone"),
        ]),
        "{adapter}: one retirement, one fact"
    );
    let recorded = w.facts().await;
    assert_eq!(
        recorded[1].1["changes"],
        json!([FieldChange::new("retired", false, true)]),
        "{adapter}"
    );

    let out = publish(w, std::slice::from_ref(&live), PublishMode::Take).await;
    assert_eq!(out.updated.len(), 1, "{adapter}: {out:?}");
    assert_eq!(listed(w).await, vec!["suite-gone".to_string()], "{adapter}");
}

/// A declaration that is itself `retired = true` for a code never held is
/// inserted — its code is taken, and it records `department.declared` —
/// but it is never listed. A later default publish of it is `unchanged`.
async fn a_row_declared_retired_lands_but_is_never_listed<W: World>(w: &W, adapter: &str) {
    let mut d = dept("suite-withdrawn", 5);
    d.retired = true;
    let out = publish(w, std::slice::from_ref(&d), PublishMode::InsertIfAbsent).await;
    assert_eq!(out.inserted, 1, "{adapter}: {out:?}");
    assert!(listed(w).await.is_empty(), "{adapter}");
    let out = publish(w, std::slice::from_ref(&d), PublishMode::InsertIfAbsent).await;
    assert_eq!(
        (out.inserted, out.unchanged),
        (0, 1),
        "{adapter}: the code is held: {out:?}"
    );
    assert_eq!(
        w.facts().await,
        vec![(DEPARTMENT_DECLARED.to_string(), declared_payload(&d))],
        "{adapter}"
    );
}

/// `list` is ordered by `sort_order`, then by code — the code in BYTE
/// order (`-` sorts before a letter), the order the double holds and the
/// only one both adapters can: a roster whose order depends on the
/// database's locale answers two questions.
async fn list_is_sort_order_then_code_in_byte_order<W: World>(w: &W, adapter: &str) {
    publish(
        w,
        &[
            dept("suite-b", 50),
            dept("suite-ab", 50),
            dept("suite-a-z", 50),
            dept("suite-zzz", 40),
        ],
        PublishMode::InsertIfAbsent,
    )
    .await;
    assert_eq!(
        listed(w).await,
        vec![
            "suite-zzz".to_string(),
            "suite-a-z".to_string(),
            "suite-ab".to_string(),
            "suite-b".to_string(),
        ],
        "{adapter}: sort_order first, then every code byte for byte"
    );
}
