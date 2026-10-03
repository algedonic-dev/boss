//! The cadence registry answers the same on both adapters — reliability
//! mechanism C of design 3036296f, "adapters agree"
//! (`boss_testing::adapters_agree!`), on the next two ports in the census
//! order after the department and step-plugin registries (backlog
//! be459ab9): `CadenceRepository` (what the conductor fires) and
//! `CadenceRegistry` (what declares a rule), which `InMemoryCadence` and
//! `PgCadence` each implement together.
//!
//! WHY THESE PORTS. The rows are the train's schedule: the conductor
//! reads `active_rules` every tick and claims each window through
//! `claim_firing`, and the platform bundle seed and the operator's
//! `POST /api/cadence/rules/{name}/publish|retire` write through the
//! registry half. Every door test of `/api/cadence` asks
//! `InMemoryCadence`; production is answered by `PgCadence`.
//! `cadence_pg.rs` states what the one database holds and
//! `cadence::in_memory::tests` what the double does, each in its own
//! words; this file is the one statement both are held to — every
//! method of both ports, and every registry write judged by the fact it
//! leaves.
//!
//! The first run found three disagreements, fixed in this car
//! (production changes to both adapters, through one shared check each):
//! - ORDER. `active_rules` promises "name-ordered". Postgres sorted by
//!   the database's locale, which ignores `-` at first level, so
//!   `suite-ab` served before `suite-a-z` there and after it in memory.
//!   It now sorts `COLLATE "C"` — byte order, the order the double holds
//!   and the only one both can; the Workflow, credentials, station and
//!   department registries' suites found and fixed the same defect the
//!   same way. Case `active_rules_is_name_ordered_in_byte_order`.
//! - A ROW THE TABLE REFUSES. `cadence_rules` carries CHECKs on the
//!   verb, the basis and each basis's parameter group; the double
//!   applied none of them, so a publish the database refused (a 500
//!   naming a constraint) landed in memory and answered 200. Both
//!   adapters now refuse it as `BadRequest`, naming the column, before
//!   writing anything — `cadence::check_rule`, one check held to the
//!   table's by `the_rule_check_is_the_tables_check` below, both ways.
//!   Cases `a_row_the_table_refuses_is_a_bad_request_and_writes_nothing`
//!   and `every_row_the_table_admits_lands`.
//! - A CLAIM WITH NO DETAIL. `NewFiring::detail` defaults to JSON null
//!   on the wire. Postgres stored that null and `record_outcome`'s
//!   `detail || $2` then built an ARRAY, so the rc it had just recorded
//!   read back as "no outcome yet" — for ever — while the double
//!   replaced the null and read it. Both adapters now land a null detail
//!   as `{}` and refuse a detail that is not an object, through
//!   `cadence::claim_detail`. Cases
//!   `a_claim_with_no_detail_still_reads_its_outcome` and
//!   `a_claim_whose_detail_is_not_an_object_is_a_bad_request`.
//!
//! THE SHAPE, the other suites': each case states its answer — rows,
//! `(version, status)` pairs, names, whole facts — and each adapter is
//! held to that stated answer, not merely to the other one. Every
//! registry write is also judged by the FACTS it leaves: the
//! `jobs.cadence.*` events the in-memory adapter collects and Postgres
//! stages on `event_outbox` in the row's own transaction, payload = the
//! row written. Instants are whole seconds, so Postgres's microseconds
//! and the double's nanoseconds compare equal.
//!
//! Rows the world starts with: the migrations seed the train's rules
//! (`train-reconcile`, `train-window`, …) straight into every Postgres
//! database and the in-memory adapter starts empty, so every rule name
//! this file writes starts `suite-`, and an unfiltered read
//! (`active_rules`, the outbox) is judged over those names only.

use boss_core::actor::ActorId;
use boss_jobs::board_decision::BoardDecision;
use boss_jobs::cadence::{
    BASIS_COLUMNS, CONDUCTOR_VERBS, CadenceError, CadenceRegistry, CadenceRepository,
    CadenceRuleRow, CadenceRuleSpec, FiringOutcome, InMemoryCadence, LastFiring, NewFiring,
    OPEN_VERB_PREFIX, PgCadence, check_rule,
};
use boss_jobs::events::{CADENCE_PUBLISHED, CADENCE_RETIRED};
use boss_jobs::registry::WorkflowStatus;
use chrono::{DateTime, NaiveDate, TimeZone, Utc};
use serde_json::{Value, json};
use std::collections::BTreeSet;

/// Both ports under test and what their writes left, read the way each
/// adapter keeps it.
trait World {
    type R: CadenceRepository + CadenceRegistry;
    fn repo(&self) -> &Self::R;
    /// Every `jobs.cadence.*` fact about a `suite-` rule recorded so far,
    /// in the order recorded: `(kind, payload)`.
    async fn facts(&self) -> Vec<(String, Value)>;
    /// The `detail` a claimed firing holds, `None` when never claimed.
    async fn detail(&self, firing_id: &str) -> Option<Value>;
}

fn is_suite_fact(kind: &str, payload: &Value) -> bool {
    kind.starts_with("jobs.cadence.")
        && payload["name"]
            .as_str()
            .is_some_and(|n| n.starts_with("suite-"))
}

struct InMemory(InMemoryCadence);

impl World for InMemory {
    type R = InMemoryCadence;
    fn repo(&self) -> &InMemoryCadence {
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
    async fn detail(&self, firing_id: &str) -> Option<Value> {
        self.0.firing(firing_id).await.map(|f| f.detail)
    }
}

struct Postgres {
    repo: PgCadence,
    pool: sqlx::PgPool,
}

impl World for Postgres {
    type R = PgCadence;
    fn repo(&self) -> &PgCadence {
        &self.repo
    }
    async fn facts(&self) -> Vec<(String, Value)> {
        let rows: Vec<(String, Value)> = sqlx::query_as(
            "SELECT kind, payload FROM event_outbox WHERE kind LIKE 'jobs.cadence.%' ORDER BY id",
        )
        .fetch_all(&self.pool)
        .await
        .expect("read the outbox");
        rows.into_iter()
            .filter(|(k, p)| is_suite_fact(k, p))
            .collect()
    }
    async fn detail(&self, firing_id: &str) -> Option<Value> {
        sqlx::query_scalar("SELECT detail FROM cadence_firings WHERE firing_id = $1")
            .bind(firing_id)
            .fetch_optional(&self.pool)
            .await
            .expect("read the firing")
    }
}

boss_testing::adapters_agree! {
    adapters {
        in_memory => (InMemory(InMemoryCadence::default()), ()),
        postgres => {
            let db = boss_testing::TestDb::new().await;
            let world = Postgres { repo: PgCadence::new(db.pool.clone()), pool: db.pool.clone() };
            (world, db)
        },
    }
    cases {
        a_fresh_registry_holds_no_suite_rule_and_no_firing,
        a_publish_of_each_basis_lands_active_served_and_recorded,
        a_version_bump_retires_the_prior_row_and_serves_the_new_one,
        a_publish_not_above_the_newest_is_a_conflict_and_writes_nothing,
        a_retire_returns_the_active_row_and_keeps_it_as_history,
        a_retire_of_nothing_active_is_none_and_silent,
        every_row_the_table_admits_lands,
        a_row_the_table_refuses_is_a_bad_request_and_writes_nothing,
        active_rules_is_name_ordered_in_byte_order,
        a_claim_is_exactly_once_and_the_first_claim_stands,
        last_firing_is_the_newest_firing_of_its_own_rule,
        an_outcome_merges_into_the_claims_detail,
        a_claim_with_no_detail_still_reads_its_outcome,
        a_claim_whose_detail_is_not_an_object_is_a_bad_request,
        an_outcome_for_an_unclaimed_firing_writes_nothing,
    }
}

// ----- fixtures ------------------------------------------------------------

fn actor() -> ActorId {
    ActorId::Human("emp-cto".into())
}

fn at(h: u32, m: u32) -> DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 9, 29, h, m, 0).unwrap()
}

/// A row with no basis parameters — each constructor below fills its own.
fn bare(name: &str, verb: &str, basis: &str) -> CadenceRuleRow {
    CadenceRuleRow {
        name: name.into(),
        verb: verb.into(),
        basis: basis.into(),
        every_minutes: None,
        at_times: None,
        min_dock_depth: None,
        cooldown_minutes: None,
        cadence: None,
        anchor_date: None,
        business_calendar: None,
    }
}

fn wall(name: &str, every: i32) -> CadenceRuleRow {
    CadenceRuleRow {
        every_minutes: Some(every),
        ..bare(name, "reconcile", "wall")
    }
}

fn clock(name: &str, times: &[&str]) -> CadenceRuleRow {
    CadenceRuleRow {
        at_times: Some(json!(times)),
        ..bare(name, "run", "clock")
    }
}

fn depth(name: &str, min: i32, cooldown: i32) -> CadenceRuleRow {
    CadenceRuleRow {
        min_dock_depth: Some(min),
        cooldown_minutes: Some(cooldown),
        ..bare(name, "board", "queue-depth")
    }
}

fn calendar(name: &str, cadence: &str, time: &str) -> CadenceRuleRow {
    CadenceRuleRow {
        cadence: Some(cadence.into()),
        anchor_date: NaiveDate::from_ymd_opt(2026, 8, 28),
        at_times: Some(json!([time])),
        ..bare(name, "open:protocol-retro", "calendar")
    }
}

/// A declaration at `version` — what a bundle file or a publish body is.
fn declared(row: CadenceRuleRow, version: i32) -> CadenceRuleSpec {
    CadenceRuleSpec {
        version,
        status: WorkflowStatus::Active,
        row,
        created_at: DateTime::<Utc>::UNIX_EPOCH,
    }
}

/// The row a publish at `now` writes: active, stamped by the door's clock.
fn written(row: CadenceRuleRow, version: i32, now: DateTime<Utc>) -> CadenceRuleSpec {
    CadenceRuleSpec {
        created_at: now,
        ..declared(row, version)
    }
}

fn retired(spec: &CadenceRuleSpec) -> CadenceRuleSpec {
    CadenceRuleSpec {
        status: WorkflowStatus::Retired,
        ..spec.clone()
    }
}

/// The fact a registry write records: the row, plus the actor.
fn fact(kind: &str, spec: &CadenceRuleSpec) -> (String, Value) {
    let mut payload = serde_json::to_value(spec).expect("a spec serializes");
    payload["_actor"] = json!("emp-cto");
    (kind.to_string(), payload)
}

async fn publish<W: World>(
    w: &W,
    row: CadenceRuleRow,
    version: i32,
    now: DateTime<Utc>,
) -> Result<CadenceRuleSpec, CadenceError> {
    w.repo()
        .publish_declared(declared(row, version), &actor(), now)
        .await
}

/// Every active `suite-` row, in the order the conductor's read served it.
async fn active<W: World>(w: &W) -> Vec<CadenceRuleRow> {
    w.repo()
        .active_rules()
        .await
        .expect("active_rules answers")
        .into_iter()
        .filter(|r| r.name.starts_with("suite-"))
        .collect()
}

async fn lineage<W: World>(w: &W, name: &str) -> Vec<(i32, WorkflowStatus)> {
    w.repo()
        .live_versions(name)
        .await
        .expect("live_versions answers")
        .into_iter()
        .map(|s| (s.version, s.status))
        .collect()
}

fn firing(id: &str, rule: &str, fired_at: DateTime<Utc>, detail: Value) -> NewFiring {
    NewFiring {
        firing_id: id.into(),
        rule_name: rule.into(),
        verb: "board".into(),
        basis: "queue-depth".into(),
        fired_at,
        detail,
    }
}

async fn claim<W: World>(w: &W, f: &NewFiring) -> bool {
    w.repo().claim_firing(f).await.expect("claim answers")
}

async fn last<W: World>(w: &W, rule: &str) -> Option<LastFiring> {
    w.repo()
        .last_firing(rule)
        .await
        .expect("last_firing answers")
}

fn outcome(rc: i32, runtime_secs: u64, decision: Option<BoardDecision>) -> FiringOutcome {
    FiringOutcome {
        rc,
        runtime_secs,
        board_decision: decision,
    }
}

// ----- cases: the registry half ---------------------------------------------

/// Nothing written: no suite rule is served or held, no suite rule has
/// fired, and no fact names one.
async fn a_fresh_registry_holds_no_suite_rule_and_no_firing<W: World>(w: &W, adapter: &str) {
    assert!(active(w).await.is_empty(), "{adapter}");
    assert!(lineage(w, "suite-none").await.is_empty(), "{adapter}");
    assert_eq!(last(w, "suite-none").await, None, "{adapter}");
    assert!(w.facts().await.is_empty(), "{adapter}");
}

/// A publish of each basis lands active at its declared version, stamped
/// by the door's clock over the declaration's `created_at`; the lineage
/// reads it back whole, the conductor's read serves its row, and one
/// `jobs.cadence.published` records the row written.
async fn a_publish_of_each_basis_lands_active_served_and_recorded<W: World>(w: &W, adapter: &str) {
    let rows = [
        wall("suite-wall", 10),
        clock("suite-clock", &["06:00", "18:00"]),
        depth("suite-depth", 3, 30),
        calendar("suite-cal", "weekly", "06:10"),
    ];
    let now = at(12, 0);
    let mut want_facts = Vec::new();
    for row in &rows {
        let want = written(row.clone(), 1, now);
        let got = publish(w, row.clone(), 1, now)
            .await
            .expect("publish lands");
        assert_eq!(got, want, "{adapter}: the write answers the row written");
        assert_eq!(
            w.repo().live_versions(&row.name).await.expect("lineage"),
            vec![want.clone()],
            "{adapter}: the lineage reads it back whole"
        );
        want_facts.push(fact(CADENCE_PUBLISHED, &want));
    }
    let names: Vec<&str> = ["suite-cal", "suite-clock", "suite-depth", "suite-wall"].into();
    let by_name = |n: &str| rows.iter().find(|r| r.name == n).cloned().unwrap();
    assert_eq!(
        active(w).await,
        names.into_iter().map(by_name).collect::<Vec<_>>(),
        "{adapter}: every basis is served, column for column"
    );
    assert_eq!(w.facts().await, want_facts, "{adapter}");
}

/// A publish above the newest version retires the active row BY NAME and
/// serves the new one; the lineage keeps both, and each publish recorded
/// the row as it was written.
async fn a_version_bump_retires_the_prior_row_and_serves_the_new_one<W: World>(
    w: &W,
    adapter: &str,
) {
    let v1 = publish(w, wall("suite-bump", 10), 1, at(12, 0))
        .await
        .expect("v1");
    let v2 = publish(w, wall("suite-bump", 5), 2, at(13, 0))
        .await
        .expect("v2");
    assert_eq!(
        lineage(w, "suite-bump").await,
        vec![(1, WorkflowStatus::Retired), (2, WorkflowStatus::Active)],
        "{adapter}"
    );
    assert_eq!(active(w).await, vec![wall("suite-bump", 5)], "{adapter}");
    assert_eq!(
        w.facts().await,
        vec![fact(CADENCE_PUBLISHED, &v1), fact(CADENCE_PUBLISHED, &v2)],
        "{adapter}"
    );
}

/// A publish at or below the newest version the lineage holds — any
/// status — is a `Conflict` naming that version, and writes and records
/// nothing: the same version (an overwrite), a lower one, a lower one
/// under a retired lineage, and a version below 1 on a fresh name. Above
/// it lands.
async fn a_publish_not_above_the_newest_is_a_conflict_and_writes_nothing<W: World>(
    w: &W,
    adapter: &str,
) {
    let v3 = publish(w, wall("suite-pin", 10), 3, at(12, 0))
        .await
        .expect("v3");
    for v in [3, 2] {
        let err = publish(w, wall("suite-pin", 5), v, at(12, 5))
            .await
            .expect_err("not above v3");
        assert!(
            matches!(&err, CadenceError::Conflict(m) if m.contains("(v3)")),
            "{adapter}: v{v}: {err}"
        );
    }
    let gone = w
        .repo()
        .retire("suite-pin", &actor(), at(12, 10))
        .await
        .expect("retire")
        .expect("v3 was active");
    let err = publish(w, wall("suite-pin", 5), 2, at(12, 15))
        .await
        .expect_err("a retired lineage still bounds the version");
    assert!(matches!(err, CadenceError::Conflict(_)), "{adapter}: {err}");
    let err = publish(w, wall("suite-fresh", 5), 0, at(12, 15))
        .await
        .expect_err("versions start at 1");
    assert!(
        matches!(&err, CadenceError::Conflict(m) if m.contains("(v0)")),
        "{adapter}: {err}"
    );
    assert_eq!(
        lineage(w, "suite-pin").await,
        vec![(3, WorkflowStatus::Retired)],
        "{adapter}: no refused publish wrote a row"
    );
    assert!(lineage(w, "suite-fresh").await.is_empty(), "{adapter}");
    assert_eq!(
        w.facts().await,
        vec![fact(CADENCE_PUBLISHED, &v3), fact(CADENCE_RETIRED, &gone)],
        "{adapter}: no refused publish recorded a fact"
    );
    let v4 = publish(w, wall("suite-pin", 5), 4, at(12, 20))
        .await
        .expect("v4 is above v3");
    assert_eq!(v4.status, WorkflowStatus::Active, "{adapter}");
    assert_eq!(active(w).await, vec![wall("suite-pin", 5)], "{adapter}");
}

/// A retire answers the active row as retired — its version, its row,
/// its original stamp — leaves the lineage holding it as history, stops
/// the conductor's read serving it, and records `jobs.cadence.retired`
/// with that row.
async fn a_retire_returns_the_active_row_and_keeps_it_as_history<W: World>(w: &W, adapter: &str) {
    let v1 = publish(w, depth("suite-gone", 2, 60), 1, at(12, 0))
        .await
        .expect("v1");
    let v2 = publish(w, depth("suite-gone", 1, 30), 2, at(13, 0))
        .await
        .expect("v2");
    let got = w
        .repo()
        .retire("suite-gone", &actor(), at(14, 0))
        .await
        .expect("retire answers");
    assert_eq!(got, Some(retired(&v2)), "{adapter}");
    assert_eq!(
        lineage(w, "suite-gone").await,
        vec![(1, WorkflowStatus::Retired), (2, WorkflowStatus::Retired)],
        "{adapter}"
    );
    assert!(active(w).await.is_empty(), "{adapter}");
    assert_eq!(
        w.facts().await,
        vec![
            fact(CADENCE_PUBLISHED, &v1),
            fact(CADENCE_PUBLISHED, &v2),
            fact(CADENCE_RETIRED, &retired(&v2)),
        ],
        "{adapter}"
    );
}

/// A retire of a name with nothing active — never declared, or already
/// retired — answers `None` and writes and records nothing.
async fn a_retire_of_nothing_active_is_none_and_silent<W: World>(w: &W, adapter: &str) {
    let retire = |name: &'static str| async move {
        w.repo()
            .retire(name, &actor(), at(12, 30))
            .await
            .expect("retire answers")
    };
    assert_eq!(retire("suite-never").await, None, "{adapter}");
    let v1 = publish(w, wall("suite-once", 10), 1, at(12, 0))
        .await
        .expect("v1");
    assert_eq!(retire("suite-once").await, Some(retired(&v1)), "{adapter}");
    assert_eq!(retire("suite-once").await, None, "{adapter}");
    assert_eq!(
        w.facts().await,
        vec![
            fact(CADENCE_PUBLISHED, &v1),
            fact(CADENCE_RETIRED, &retired(&v1))
        ],
        "{adapter}: the no-op retires recorded nothing"
    );
}

/// Every verb and basis shape the table admits lands on both: each
/// conductor verb, an `open:<kind>`, each basis with its own parameter
/// group — and a calendar rule naming a business calendar, which the
/// table admits (the conductor refuses to fire it, which is its own
/// judgement, not the registry's).
async fn every_row_the_table_admits_lands<W: World>(w: &W, adapter: &str) {
    for row in admitted_rows() {
        let name = row.name.clone();
        let got = publish(w, row, 1, at(12, 0)).await;
        assert!(got.is_ok(), "{adapter}: {name}: {got:?}");
    }
}

/// Rows the table admits — also half of the pin's corpus below.
fn admitted_rows() -> Vec<CadenceRuleRow> {
    let mut rows: Vec<CadenceRuleRow> = [
        "preflight",
        "reconcile",
        "board",
        "run",
        "refresh",
        "open:protocol-retro",
        "open:a-2",
    ]
    .iter()
    .enumerate()
    .map(|(i, verb)| CadenceRuleRow {
        verb: (*verb).into(),
        ..wall(&format!("suite-verb-{i}"), 10)
    })
    .collect();
    rows.push(clock("suite-one-time", &["06:05"]));
    rows.push(depth("suite-deep", 12, 120));
    rows.push(CadenceRuleRow {
        business_calendar: Some("us-banking".into()),
        ..calendar("suite-banking", "monthly", "09:00")
    });
    rows
}

/// A declaration the table's CHECKs refuse is a `BadRequest` naming the
/// column it breaks, and writes and records nothing — on both adapters,
/// where Postgres used to answer a storage error naming a constraint and
/// the double used to land it.
async fn a_row_the_table_refuses_is_a_bad_request_and_writes_nothing<W: World>(
    w: &W,
    adapter: &str,
) {
    for (row, column) in refused_rows() {
        let name = row.name.clone();
        let err = publish(w, row, 1, at(12, 0))
            .await
            .expect_err("the table refuses it");
        assert!(
            matches!(&err, CadenceError::BadRequest(m) if m.contains(column) && m.contains(&name)),
            "{adapter}: {name} is refused naming itself and {column}: {err}"
        );
        assert!(lineage(w, &name).await.is_empty(), "{adapter}: {name}");
    }
    assert!(active(w).await.is_empty(), "{adapter}");
    assert!(w.facts().await.is_empty(), "{adapter}");
}

/// Rows the table refuses, each with the column its refusal names — the
/// other half of the pin's corpus below.
fn refused_rows() -> Vec<(CadenceRuleRow, &'static str)> {
    vec![
        (
            CadenceRuleRow {
                verb: "deploy".into(),
                ..wall("suite-bad-verb", 10)
            },
            "verb",
        ),
        (
            CadenceRuleRow {
                verb: "open:".into(),
                ..wall("suite-bad-open", 10)
            },
            "verb",
        ),
        (
            CadenceRuleRow {
                verb: "open:Protocol-Retro".into(),
                ..wall("suite-bad-kind", 10)
            },
            "verb",
        ),
        (
            CadenceRuleRow {
                basis: "hourly".into(),
                ..wall("suite-bad-basis", 10)
            },
            "basis",
        ),
        (wall("suite-bad-zero", 0), "every_minutes"),
        (
            CadenceRuleRow {
                every_minutes: None,
                ..wall("suite-bad-no-every", 10)
            },
            "every_minutes",
        ),
        (
            CadenceRuleRow {
                cooldown_minutes: Some(5),
                ..wall("suite-bad-wall-cooldown", 10)
            },
            "cooldown_minutes",
        ),
        (
            CadenceRuleRow {
                business_calendar: Some("us-banking".into()),
                ..wall("suite-bad-wall-calendar", 10)
            },
            "business_calendar",
        ),
        (
            CadenceRuleRow {
                at_times: None,
                ..clock("suite-bad-clock", &["06:00"])
            },
            "at_times",
        ),
        (
            CadenceRuleRow {
                every_minutes: Some(10),
                ..clock("suite-bad-clock-every", &["06:00"])
            },
            "every_minutes",
        ),
        (depth("suite-bad-depth-zero", 0, 30), "min_dock_depth"),
        (depth("suite-bad-cooldown-zero", 2, 0), "cooldown_minutes"),
        (
            CadenceRuleRow {
                at_times: Some(json!(["06:00", "18:00"])),
                ..calendar("suite-bad-two-times", "weekly", "06:00")
            },
            "at_times",
        ),
        (
            CadenceRuleRow {
                anchor_date: None,
                ..calendar("suite-bad-no-anchor", "weekly", "06:00")
            },
            "anchor_date",
        ),
        (
            CadenceRuleRow {
                cadence: None,
                ..calendar("suite-bad-no-cadence", "weekly", "06:00")
            },
            "cadence",
        ),
        (
            CadenceRuleRow {
                min_dock_depth: Some(2),
                ..calendar("suite-bad-cal-depth", "weekly", "06:00")
            },
            "min_dock_depth",
        ),
    ]
}

/// `active_rules` is name-ordered, the name in BYTE order (`-` sorts
/// before a letter) — the order the double holds and the only one both
/// adapters can: a schedule whose order depends on the database's locale
/// answers two questions.
async fn active_rules_is_name_ordered_in_byte_order<W: World>(w: &W, adapter: &str) {
    for name in ["suite-b", "suite-ab", "suite-a-z"] {
        publish(w, wall(name, 10), 1, at(12, 0))
            .await
            .expect("publish");
    }
    let names: Vec<String> = active(w).await.into_iter().map(|r| r.name).collect();
    assert_eq!(
        names,
        vec!["suite-a-z", "suite-ab", "suite-b"],
        "{adapter}: every name byte for byte"
    );
}

// ----- cases: the conductor's half ------------------------------------------

/// The first claim of a firing id wins and the second loses, whatever it
/// carries: the stored firing is the first claim's — its instant and its
/// detail — and no outcome is recorded yet.
async fn a_claim_is_exactly_once_and_the_first_claim_stands<W: World>(w: &W, adapter: &str) {
    let first = firing(
        "suite-f1",
        "suite-board",
        at(12, 0),
        json!({"dock_depth": 4}),
    );
    let second = firing(
        "suite-f1",
        "suite-board",
        at(12, 1),
        json!({"dock_depth": 9}),
    );
    assert!(claim(w, &first).await, "{adapter}: the first claim wins");
    assert!(!claim(w, &second).await, "{adapter}: the second loses");
    assert_eq!(
        last(w, "suite-board").await,
        Some(LastFiring {
            firing_id: "suite-f1".into(),
            fired_at: at(12, 0),
            rc: None,
            board_decision: None,
        }),
        "{adapter}"
    );
    assert_eq!(
        w.detail("suite-f1").await,
        Some(json!({"dock_depth": 4})),
        "{adapter}"
    );
}

/// `last_firing` is the newest firing BY ITS INSTANT, not by the order
/// claimed, and only of the rule asked; a rule that never fired is `None`.
async fn last_firing_is_the_newest_firing_of_its_own_rule<W: World>(w: &W, adapter: &str) {
    for (id, rule, t) in [
        ("suite-a-10", "suite-a", at(10, 0)),
        ("suite-a-12", "suite-a", at(12, 0)),
        ("suite-a-11", "suite-a", at(11, 0)),
        ("suite-b-13", "suite-b", at(13, 0)),
    ] {
        assert!(claim(w, &firing(id, rule, t, json!({}))).await, "{adapter}");
    }
    let id =
        |rule: &'static str| async move { last(w, rule).await.map(|l| (l.firing_id, l.fired_at)) };
    assert_eq!(
        id("suite-a").await,
        Some(("suite-a-12".into(), at(12, 0))),
        "{adapter}"
    );
    assert_eq!(
        id("suite-b").await,
        Some(("suite-b-13".into(), at(13, 0))),
        "{adapter}"
    );
    assert_eq!(id("suite-c").await, None, "{adapter}");
}

/// An outcome MERGES into the claim's detail — the dock depth that
/// triggered the firing survives beside the rc, the runtime and a board's
/// decision — and `last_firing` reads the rc and the decision back.
async fn an_outcome_merges_into_the_claims_detail<W: World>(w: &W, adapter: &str) {
    let decision = BoardDecision::NoBoardableCar {
        reason: "no train departed — every car on the dock is held".into(),
    };
    claim(
        w,
        &firing(
            "suite-f2",
            "suite-board",
            at(12, 0),
            json!({"dock_depth": 4}),
        ),
    )
    .await;
    w.repo()
        .record_outcome("suite-f2", &outcome(-2, 9, Some(decision.clone())))
        .await
        .expect("record_outcome answers");
    let got = last(w, "suite-board").await.expect("fired");
    assert_eq!(
        (got.rc, got.board_decision),
        (Some(-2), Some(decision.clone())),
        "{adapter}"
    );
    assert_eq!(
        w.detail("suite-f2").await,
        Some(json!({
            "dock_depth": 4,
            "rc": -2,
            "runtime_secs": 9,
            "board_decision": decision,
        })),
        "{adapter}"
    );
}

/// A claim whose detail is JSON null — what `NewFiring` defaults to on
/// the wire when the body omits it — lands as `{}`, and its outcome then
/// reads back like any other.
async fn a_claim_with_no_detail_still_reads_its_outcome<W: World>(w: &W, adapter: &str) {
    assert!(
        claim(w, &firing("suite-f3", "suite-wall", at(12, 0), Value::Null)).await,
        "{adapter}"
    );
    assert_eq!(w.detail("suite-f3").await, Some(json!({})), "{adapter}");
    w.repo()
        .record_outcome("suite-f3", &outcome(0, 1, None))
        .await
        .expect("record_outcome answers");
    assert_eq!(
        last(w, "suite-wall").await.and_then(|l| l.rc),
        Some(0),
        "{adapter}: the rc recorded is the rc read"
    );
    assert_eq!(
        w.detail("suite-f3").await,
        Some(json!({"rc": 0, "runtime_secs": 1})),
        "{adapter}"
    );
}

/// A claim whose detail is neither an object nor null is a `BadRequest`
/// naming `detail`, and claims nothing: an outcome merges into an object,
/// and there is no merge into an array or a scalar both adapters share.
async fn a_claim_whose_detail_is_not_an_object_is_a_bad_request<W: World>(w: &W, adapter: &str) {
    for (i, detail) in [json!([1]), json!("depth 4"), json!(4)]
        .into_iter()
        .enumerate()
    {
        let id = format!("suite-f4-{i}");
        let err = w
            .repo()
            .claim_firing(&firing(&id, "suite-odd", at(12, 0), detail.clone()))
            .await
            .expect_err("not an object");
        assert!(
            matches!(&err, CadenceError::BadRequest(m) if m.contains("detail")),
            "{adapter}: {detail}: {err}"
        );
        assert_eq!(w.detail(&id).await, None, "{adapter}: {detail}");
    }
    assert_eq!(last(w, "suite-odd").await, None, "{adapter}");
}

/// An outcome for a firing never claimed is answered and changes nothing:
/// there is no firing to merge into, and none is created.
async fn an_outcome_for_an_unclaimed_firing_writes_nothing<W: World>(w: &W, adapter: &str) {
    w.repo()
        .record_outcome("suite-never", &outcome(0, 1, None))
        .await
        .expect("record_outcome answers");
    assert_eq!(w.detail("suite-never").await, None, "{adapter}");
}

// ----- the pin: the Rust check IS the table's check ---------------------------

/// Every `'<literal>'::text` in a constraint definition, as Postgres
/// prints it back (`pg_get_constraintdef`).
fn literals(def: &str) -> Vec<String> {
    def.split('\'')
        .collect::<Vec<_>>()
        .chunks(2)
        .filter_map(|pair| match pair {
            [_, lit] => Some((*lit).to_string()),
            _ => None,
        })
        .collect()
}

async fn constraint(pool: &sqlx::PgPool, name: &str) -> String {
    sqlx::query_scalar(
        "SELECT pg_get_constraintdef(oid) FROM pg_constraint \
         WHERE conrelid = 'cadence_rules'::regclass AND conname = $1",
    )
    .bind(name)
    .fetch_optional(pool)
    .await
    .expect("read pg_constraint")
    .unwrap_or_else(|| panic!("cadence_rules carries no constraint {name}"))
}

/// Would the live table take this row? A raw INSERT, bypassing both
/// adapters, rolled back whatever it answers.
async fn table_admits(pool: &sqlx::PgPool, row: &CadenceRuleRow) -> bool {
    let mut tx = pool.begin().await.expect("begin");
    let got = sqlx::query(
        "INSERT INTO cadence_rules (name, version, status, verb, basis, every_minutes, \
         at_times, min_dock_depth, cooldown_minutes, cadence, anchor_date, business_calendar) \
         VALUES ($1, 1, 'active', $2, $3, $4, $5, $6, $7, $8, $9, $10)",
    )
    .bind(&row.name)
    .bind(&row.verb)
    .bind(&row.basis)
    .bind(row.every_minutes)
    .bind(&row.at_times)
    .bind(row.min_dock_depth)
    .bind(row.cooldown_minutes)
    .bind(&row.cadence)
    .bind(row.anchor_date)
    .bind(&row.business_calendar)
    .execute(&mut *tx)
    .await;
    tx.rollback().await.expect("rollback");
    got.is_ok()
}

/// A FACT THAT LIVES TWICE (CLAUDE.md §9a): what `cadence_rules` admits is
/// stated by its CHECKs in SQL and by `cadence::check_rule` in Rust, and
/// the in-memory adapter answers by the Rust. Pinned BOTH ways against the
/// live table the migrations build:
/// - the LISTS: the verb check's literals are exactly `CONDUCTOR_VERBS`
///   and its pattern is `^open:` plus the kebab class `check_rule`
///   applies; the basis check's literals are exactly the bases of
///   `BASIS_COLUMNS` — a verb or basis added on either side alone goes
///   red naming it;
/// - the VERDICTS: every row this suite states an answer for, plus edge
///   rows between them, gets the same verdict from `check_rule` as from
///   a raw INSERT that bypasses both adapters — so a parameter group
///   loosened or tightened on one side alone goes red naming the row.
#[tokio::test(flavor = "multi_thread")]
async fn the_rule_check_is_the_tables_check() {
    let db = boss_testing::TestDb::new().await;

    let verb_def = constraint(&db.pool, "cadence_rules_verb_check").await;
    let (patterns, verbs): (Vec<String>, Vec<String>) = literals(&verb_def)
        .into_iter()
        .partition(|l| l.starts_with('^'));
    assert_eq!(
        verbs.iter().map(String::as_str).collect::<BTreeSet<_>>(),
        CONDUCTOR_VERBS.into_iter().collect::<BTreeSet<_>>(),
        "the table's conductor verbs are CONDUCTOR_VERBS: {verb_def}"
    );
    assert_eq!(
        patterns,
        vec![format!("^{OPEN_VERB_PREFIX}[a-z0-9-]+$")],
        "the table's open pattern is OPEN_VERB_PREFIX plus the kebab class check_rule \
         applies: {verb_def}"
    );
    let basis_def = constraint(&db.pool, "cadence_rules_basis_check").await;
    assert_eq!(
        literals(&basis_def)
            .iter()
            .map(String::as_str)
            .collect::<BTreeSet<_>>(),
        BASIS_COLUMNS
            .iter()
            .map(|(b, _, _)| *b)
            .collect::<BTreeSet<_>>(),
        "the table's bases are BASIS_COLUMNS': {basis_def}"
    );

    let edges: Vec<CadenceRuleRow> = vec![
        CadenceRuleRow {
            verb: "open:café".into(),
            ..wall("edge-accent", 10)
        },
        CadenceRuleRow {
            verb: "open:a_b".into(),
            ..wall("edge-underscore", 10)
        },
        CadenceRuleRow {
            verb: "Board".into(),
            ..wall("edge-caps", 10)
        },
        CadenceRuleRow {
            verb: "open:x".into(),
            ..wall("edge-one-char", 10)
        },
        wall("edge-negative", -5),
        depth("edge-negative-cooldown", 2, -1),
        CadenceRuleRow {
            at_times: Some(json!(["06:00"])),
            ..wall("edge-wall-times", 10)
        },
        CadenceRuleRow {
            every_minutes: Some(10),
            ..depth("edge-depth-every", 2, 30)
        },
        CadenceRuleRow {
            cadence: Some("daily".into()),
            ..clock("edge-clock-cadence", &["06:00"])
        },
        CadenceRuleRow {
            anchor_date: NaiveDate::from_ymd_opt(2026, 1, 1),
            ..depth("edge-depth-anchor", 2, 30)
        },
        CadenceRuleRow {
            every_minutes: Some(10),
            ..calendar("edge-calendar-every", "daily", "06:00")
        },
        CadenceRuleRow {
            at_times: Some(json!("06:00")),
            ..calendar("edge-calendar-scalar", "daily", "06:00")
        },
        CadenceRuleRow {
            at_times: Some(json!([])),
            ..calendar("edge-calendar-empty", "daily", "06:00")
        },
        clock("edge-clock-empty", &[]),
        clock("edge-clock-three", &["01:00", "02:00", "03:00"]),
    ];
    let corpus = admitted_rows()
        .into_iter()
        .chain(refused_rows().into_iter().map(|(row, _)| row))
        .chain(edges);

    let mut disagree = Vec::new();
    for row in corpus {
        let rust = check_rule(&row);
        let table = table_admits(&db.pool, &row).await;
        if rust.is_ok() != table {
            disagree.push(format!(
                "{}: check_rule says {rust:?}, the table {}",
                row.name,
                if table { "admits it" } else { "refuses it" }
            ));
        }
    }
    assert!(
        disagree.is_empty(),
        "check_rule and cadence_rules' CHECKs disagree on:\n{}",
        disagree.join("\n")
    );
}

// ----- two overlapping publishes (Postgres only) ---------------------------

/// Sessions of this database currently waiting on a lock.
async fn lock_waiters(pool: &sqlx::PgPool) -> i64 {
    sqlx::query_scalar(
        "SELECT count(*) FROM pg_stat_activity
         WHERE datname = current_database() AND wait_event_type = 'Lock'",
    )
    .fetch_one(pool)
    .await
    .expect("read pg_stat_activity")
}

/// Wait until `n` sessions wait on a lock, or `racer` has finished —
/// which it does at once when it takes no lock at all, and then the
/// assertions after the release say so rather than a timeout.
async fn until_waiting<T>(pool: &sqlx::PgPool, n: i64, racer: &tokio::task::JoinHandle<T>) {
    for _ in 0..500 {
        if lock_waiters(pool).await >= n || racer.is_finished() {
            return;
        }
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
    panic!("{n} session(s) never reached a lock and the racer never finished");
}

/// AN OVERTAKEN OLDER PUBLISH IS REFUSED, NOT LANDED OVER THE NEWER ONE
/// (backlog 4541d511). The newer publish's `v5` is held before its
/// commit — a third transaction owns an uncommitted row at the
/// (name, version) key its insert writes, so the insert waits — and the
/// older publish, declared `v4` against the live `v3`, is observed
/// WAITING before the hold is released. Let through, it must read the
/// `v5` that committed ahead of it and answer the 409.
///
/// The shape this adapter had, `MAX(version)` over rows locked
/// `FOR UPDATE` in ONE statement, waited on `v3`'s row lock and then
/// re-read `v3` alone: `v5` was inserted after that statement's
/// snapshot, so it answered `3`, the older publish passed its floor and
/// retired the committed `v5` — a silent downgrade of the train's
/// schedule. The floor is now `declared_version::lock_and_read_newest`,
/// the step-plugin registry's: an advisory lock on the lineage, then
/// the read in a statement that starts after it is held
/// (`pg_an_older_seed_waiting_on_a_newer_one_is_refused` there).
#[tokio::test(flavor = "multi_thread")]
async fn pg_an_older_publish_waiting_on_a_newer_one_is_refused() {
    let db = boss_testing::TestDb::new().await;
    let repo = PgCadence::new(db.pool.clone());
    let name = "suite-concurrent";
    repo.publish_declared(declared(wall(name, 10), 3), &actor(), at(6, 0))
        .await
        .expect("v3 is live");

    let mut hold = db.pool.begin().await.expect("hold tx");
    sqlx::query(
        "INSERT INTO cadence_rules (name, version, status, verb, basis, every_minutes)
         VALUES ($1, 5, 'draft', 'reconcile', 'wall', 10)",
    )
    .bind(name)
    .execute(&mut *hold)
    .await
    .expect("hold the newer publish's key");

    let newer_repo = PgCadence::new(db.pool.clone());
    let newer = tokio::spawn(async move {
        newer_repo
            .publish_declared(declared(wall(name, 5), 5), &actor(), at(7, 0))
            .await
    });
    until_waiting(&db.pool, 1, &newer).await;
    assert!(
        !newer.is_finished(),
        "case error: the newer publish must be held before its commit"
    );

    let older_repo = PgCadence::new(db.pool.clone());
    let older = tokio::spawn(async move {
        older_repo
            .publish_declared(declared(wall(name, 4), 4), &actor(), at(8, 0))
            .await
    });
    until_waiting(&db.pool, 2, &older).await;

    hold.rollback().await.expect("release the newer publish");
    newer
        .await
        .expect("newer task")
        .expect("the newer publish lands");
    match older.await.expect("older task") {
        Err(CadenceError::Conflict(msg)) => assert!(
            msg.contains("(v5)"),
            "the refusal names the version that overtook it: {msg}"
        ),
        other => panic!("the older publish must be refused behind the newer, got {other:?}"),
    }
    let versions: Vec<(i32, WorkflowStatus)> = repo
        .live_versions(name)
        .await
        .expect("live_versions")
        .into_iter()
        .map(|s| (s.version, s.status))
        .collect();
    assert_eq!(
        versions,
        vec![(3, WorkflowStatus::Retired), (5, WorkflowStatus::Active)],
        "the newer publish's row stays live"
    );
}
