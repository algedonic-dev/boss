//! The station registry answers the same on both `StationRegistry`
//! adapters — reliability mechanism C of design 3036296f, "adapters
//! agree" (`boss_testing::adapters_agree!`), on the eighth port it
//! reaches (backlog be459ab9; after the jobs store's list filters, the
//! agent-run log, the Workflow registry, the Class registry, the policy
//! store, and the credentials and agents registries).
//!
//! WHY THIS PORT NEXT. It is routing: a station is the queue a packet
//! waits in, and its row decides who may claim from it. Every door test
//! of `/api/stations` and the station flow asks `InMemoryStations`;
//! production is answered by `PgStations`. `stations::tests` states the
//! contract of the one and `stations_pg.rs` of the other, each in its
//! own words; this file is the one statement both are held to — every
//! method of the port, and every write judged by the fact it leaves.
//!
//! The first run found two disagreements, both in the Postgres adapter
//! against the port's own words, each fixed in this car (production
//! changes to `PgStations`):
//! - ORDER. `list_active` promises "name-ordered". Postgres sorted by
//!   the database's locale, which ignores `-` at first level, so
//!   `suite-ab` listed before `suite-a-z` there and after it in memory.
//!   It now sorts `COLLATE "C"` — byte order, the order the double
//!   holds and the only one both can; the Workflow and credentials
//!   registries' suites found and fixed the same defect the same way.
//!   Case `list_active_is_every_live_name_in_byte_order`.
//! - A WINDOW THE COLUMN CANNOT HOLD. `terminal_window_days` is a `u32`
//!   on the row and an INT column. Postgres clamped a window past
//!   `i32::MAX` to `i32::MAX` on the way in, while the write answered —
//!   and its `draft_saved` / `published` fact recorded — the window it
//!   was handed, and the double stored it as handed: three answers to
//!   one question, where the port promises the fact's payload IS the
//!   stored row. Both adapters now refuse it as `Invalid` before writing
//!   anything (`stations::window_column`, one check for both). Case
//!   `a_window_the_column_cannot_hold_is_refused_whole`.
//!
//! THE SHAPE, the other suites': each case states its answer — as
//! `(version, status)` pairs, names, or whole rows — and each adapter is
//! held to that stated answer, not merely to the other one. Every write
//! is also judged by the FACTS it leaves: the `jobs.station.*` events the
//! in-memory adapter collects and Postgres stages on `event_outbox` in
//! the row's own transaction, because the port promises the event
//! "atomically with the row", payload = the row written. Instants are
//! whole seconds, so Postgres's microseconds and the double's
//! nanoseconds compare equal.
//!
//! Rows the world starts with: the migrations seed the SDLC stations
//! (`loading-dock`, `design-review`, the watchlist, …) straight into
//! every Postgres database, and the in-memory adapter starts empty, so
//! every name this file writes starts `suite-`, and an unfiltered read
//! (`list_active`, the outbox) is judged over those names only.

use boss_core::actor::ActorId;
use boss_core::job::{JobStatus, StepStatus};
use boss_jobs::events::{STATION_DRAFT_SAVED, STATION_PUBLISHED, STATION_RETIRED};
use boss_jobs::registry::WorkflowStatus;
use boss_jobs::station_queue::{DisciplineKey, StationPredicate, StepMatch};
use boss_jobs::{
    InMemoryStations, PgStations, StationCapability, StationError, StationKind, StationLens,
    StationRegistry, StationSpec, StationUpstream,
};
use chrono::{DateTime, TimeZone, Utc};
use serde_json::Value;

/// The registry under test and the facts its writes left, read the way
/// each adapter keeps them.
trait World {
    type R: StationRegistry;
    fn repo(&self) -> &Self::R;
    /// Every `jobs.station.*` fact about a `suite-` station recorded so
    /// far, in the order recorded: `(kind, payload)`.
    async fn facts(&self) -> Vec<(String, Value)>;
}

fn is_suite_fact(kind: &str, payload: &Value) -> bool {
    kind.starts_with("jobs.station.")
        && payload["name"]
            .as_str()
            .is_some_and(|n| n.starts_with("suite-"))
}

struct InMemory(InMemoryStations);

impl World for InMemory {
    type R = InMemoryStations;
    fn repo(&self) -> &InMemoryStations {
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
    repo: PgStations,
    pool: sqlx::PgPool,
}

impl World for Postgres {
    type R = PgStations;
    fn repo(&self) -> &PgStations {
        &self.repo
    }
    async fn facts(&self) -> Vec<(String, Value)> {
        let rows: Vec<(String, Value)> = sqlx::query_as(
            "SELECT kind, payload FROM event_outbox WHERE kind LIKE 'jobs.station.%' ORDER BY id",
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
        in_memory => (InMemory(InMemoryStations::new()), ()),
        postgres => {
            let db = boss_testing::TestDb::new().await;
            let world = Postgres { repo: PgStations::new(db.pool.clone()), pool: db.pool.clone() };
            (world, db)
        },
    }
    cases {
        a_fresh_registry_holds_no_suite_station,
        a_draft_lands_whole_at_the_next_version_and_is_not_live,
        publish_promotes_the_newest_draft_and_retires_the_active,
        a_publish_with_no_draft_refuses_and_records_nothing,
        an_unviable_draft_refuses_publish_and_flips_nothing,
        retire_is_idempotent_and_keeps_history_readable,
        list_active_is_every_live_name_in_byte_order,
        a_declared_version_lands_as_declared_and_is_never_overwritten,
        a_declared_version_at_or_below_the_newest_is_refused,
        a_window_the_column_cannot_hold_is_refused_whole,
    }
}

// ----- fixtures ------------------------------------------------------------

/// A whole-second instant, `secs` after a fixed origin.
fn at(secs: i64) -> DateTime<Utc> {
    Utc.timestamp_opt(1_790_000_000 + secs, 0)
        .single()
        .expect("a representable instant")
}

fn author() -> ActorId {
    ActorId::Human("emp-cto".into())
}

/// A minimal VIABLE station: a batch queue over one Workflow kind.
fn viable(name: &str) -> StationSpec {
    StationSpec::draft(
        name,
        format!("The {name} queue"),
        StationKind::Batch,
        StationPredicate {
            kind: Some("ship-a-change".into()),
            ..Default::default()
        },
        at(0),
    )
}

/// A viable station with EVERY optional field set, so a column the Pg
/// adapter drops or a JSON shape it bends shows up as a whole-row
/// difference.
fn whole(name: &str) -> StationSpec {
    let mut s = StationSpec::draft(
        name,
        format!("The {name} queue, every field declared"),
        StationKind::Group,
        StationPredicate {
            kind: Some("backlog-item".into()),
            status: Some(JobStatus::Closed),
            tags_any: vec!["suite".into(), "reliability".into()],
            metadata_present: vec!["area".into()],
            metadata_absent: vec!["train".into()],
            metadata_equals: [("software_tier".to_string(), "core".to_string())].into(),
            step: Some(StepMatch {
                slug: Some("build".into()),
                status_in: vec![StepStatus::Ready, StepStatus::Active],
                ..Default::default()
            }),
        },
        at(0),
    );
    s.discipline = vec![DisciplineKey::Due, DisciplineKey::Age];
    s.wip_limit = Some(7);
    // A terminal status is viable only with a retention window.
    s.terminal_window_days = Some(14);
    s.capability = Some(StationCapability {
        roles: vec!["platform-admin".into(), "engineering-agent".into()],
        models: vec!["opus-5[1m]".into()],
    });
    s.rollup_parent = Some("suite-department".into());
    s.upstream = Some(StationUpstream {
        label: "FEEDBACK".into(),
        href: "/system/feedback".into(),
    });
    s.lens = Some(StationLens {
        eyebrow: Some("Suite · Stations".into()),
        title: "Every field".into(),
        subtitle: Some("held to one answer".into()),
        panels: vec!["throughput".into(), "ageing".into()],
        with_steps: true,
    });
    s
}

/// An UNVIABLE station: a key demanded present and absent at once, so
/// no packet can ever match it.
fn unviable(name: &str) -> StationSpec {
    let mut s = viable(name);
    s.predicate.metadata_present = vec!["branch".into()];
    s.predicate.metadata_absent = vec!["branch".into()];
    s
}

/// `spec` as a row of the registry: at `version`, in `status`, stamped
/// `created_at`.
fn row(spec: &StationSpec, version: i32, status: WorkflowStatus, created: i64) -> StationSpec {
    StationSpec {
        version,
        status,
        created_at: at(created),
        ..spec.clone()
    }
}

async fn draft<W: World>(w: &W, spec: StationSpec, secs: i64) -> StationSpec {
    w.repo()
        .create_draft(spec, &author(), at(secs))
        .await
        .expect("a draft lands")
}

async fn publish<W: World>(w: &W, name: &str) -> StationSpec {
    w.repo()
        .publish(name, &author(), at(9_000))
        .await
        .expect("a viable draft publishes")
}

/// `(version, status)` for every row of `name`, oldest first.
async fn history<W: World>(w: &W, name: &str) -> Vec<(i32, WorkflowStatus)> {
    w.repo()
        .list_versions(name)
        .await
        .expect("list_versions answers")
        .into_iter()
        .map(|s| (s.version, s.status))
        .collect()
}

/// `(kind, name, version)` for each fact.
async fn fact_rows<W: World>(w: &W) -> Vec<(String, String, i64)> {
    w.facts()
        .await
        .into_iter()
        .map(|(kind, p)| {
            let name = p["name"].as_str().unwrap_or("").to_string();
            let version = p["version"].as_i64().unwrap_or(-1);
            (kind, name, version)
        })
        .collect()
}

fn facts(v: &[(&str, &str, i64)]) -> Vec<(String, String, i64)> {
    v.iter()
        .map(|(k, n, ver)| (k.to_string(), n.to_string(), *ver))
        .collect()
}

/// The spec a fact's payload describes, and the actor it carries.
fn fact_spec(payload: &Value) -> (StationSpec, String) {
    let mut p = payload.clone();
    let actor = p
        .as_object_mut()
        .and_then(|m| m.remove("_actor"))
        .and_then(|a| a.as_str().map(String::from))
        .unwrap_or_default();
    let spec = serde_json::from_value(p).expect("a station fact's payload is a station row");
    (spec, actor)
}

fn is_not_found(r: &Result<StationSpec, StationError>) -> bool {
    matches!(r, Err(StationError::NotFound(_)))
}

// ----- cases -----------------------------------------------------------------

/// Nothing written: no suite station answers any read, none is listed,
/// and no fact names one. A miss is `NotFound` naming the station, never
/// a storage error.
async fn a_fresh_registry_holds_no_suite_station<W: World>(w: &W, adapter: &str) {
    let got = w.repo().get_active("suite-nothing").await;
    assert!(is_not_found(&got), "{adapter}: {got:?}");
    let got = w.repo().get_version("suite-nothing", 1).await;
    assert!(is_not_found(&got), "{adapter}: {got:?}");
    assert!(history(w, "suite-nothing").await.is_empty(), "{adapter}");
    let listed = w.repo().list_active().await.expect("list_active answers");
    assert!(
        !listed.iter().any(|s| s.name.starts_with("suite-")),
        "{adapter}: {listed:?}"
    );
    assert!(w.facts().await.is_empty(), "{adapter}");
}

/// A draft lands at max(version)+1 — whatever version and status the
/// caller put on it — stamped with the `now` it was handed, and reads
/// back WHOLE through `get_version` and `list_versions`. It is not live:
/// `get_active` misses and `list_active` omits it. It records one
/// `draft_saved` whose payload is the row stored, signed by the author.
async fn a_draft_lands_whole_at_the_next_version_and_is_not_live<W: World>(w: &W, adapter: &str) {
    let mut asked = whole("suite-whole");
    asked.version = 7;
    asked.status = WorkflowStatus::Active;
    let stored = draft(w, asked, 60).await;
    let want = row(&whole("suite-whole"), 1, WorkflowStatus::Draft, 60);
    assert_eq!(
        stored, want,
        "{adapter}: the write answers the row it stored"
    );
    assert_eq!(
        w.repo()
            .get_version("suite-whole", 1)
            .await
            .expect("get_version answers"),
        want,
        "{adapter}: every field reads back as written"
    );
    assert_eq!(
        w.repo()
            .list_versions("suite-whole")
            .await
            .expect("list_versions"),
        vec![want.clone()],
        "{adapter}"
    );

    let second = draft(w, whole("suite-whole"), 120).await;
    assert_eq!(
        (second.version, second.status),
        (2, WorkflowStatus::Draft),
        "{adapter}"
    );
    let got = w.repo().get_active("suite-whole").await;
    assert!(
        is_not_found(&got),
        "{adapter}: a draft is not live: {got:?}"
    );
    let listed = w.repo().list_active().await.expect("list_active");
    assert!(
        !listed.iter().any(|s| s.name == "suite-whole"),
        "{adapter}: {listed:?}"
    );

    let recorded = w.facts().await;
    assert_eq!(
        fact_rows(w).await,
        facts(&[
            (STATION_DRAFT_SAVED, "suite-whole", 1),
            (STATION_DRAFT_SAVED, "suite-whole", 2),
        ]),
        "{adapter}"
    );
    let (spec, actor) = fact_spec(&recorded[0].1);
    assert_eq!(spec, want, "{adapter}: the fact is the row stored");
    assert_eq!(actor, "emp-cto", "{adapter}: signed by the author");
}

/// Publish promotes the NEWEST draft and retires whatever was active;
/// an older draft is left a draft. The promoted row keeps the instant
/// its draft was saved — a publish moves status, not history. Each
/// publish records one `published` whose payload is the promoted row.
async fn publish_promotes_the_newest_draft_and_retires_the_active<W: World>(w: &W, adapter: &str) {
    draft(w, viable("suite-flow"), 60).await;
    let first = publish(w, "suite-flow").await;
    assert_eq!(
        first,
        row(&viable("suite-flow"), 1, WorkflowStatus::Active, 60),
        "{adapter}"
    );
    draft(w, viable("suite-flow"), 120).await;
    let mut newest = viable("suite-flow");
    newest.title = "The newest draft".into();
    draft(w, newest.clone(), 180).await;

    let promoted = publish(w, "suite-flow").await;
    let want = row(&newest, 3, WorkflowStatus::Active, 180);
    assert_eq!(promoted, want, "{adapter}: the newest draft, as saved");
    assert_eq!(
        w.repo().get_active("suite-flow").await.expect("get_active"),
        want,
        "{adapter}"
    );
    assert_eq!(
        history(w, "suite-flow").await,
        vec![
            (1, WorkflowStatus::Retired),
            (2, WorkflowStatus::Draft),
            (3, WorkflowStatus::Active),
        ],
        "{adapter}"
    );

    let recorded = w.facts().await;
    assert_eq!(
        fact_rows(w).await,
        facts(&[
            (STATION_DRAFT_SAVED, "suite-flow", 1),
            (STATION_PUBLISHED, "suite-flow", 1),
            (STATION_DRAFT_SAVED, "suite-flow", 2),
            (STATION_DRAFT_SAVED, "suite-flow", 3),
            (STATION_PUBLISHED, "suite-flow", 3),
        ]),
        "{adapter}: the retirement a publish causes is inside its published fact"
    );
    let (spec, actor) = fact_spec(&recorded[4].1);
    assert_eq!(spec, want, "{adapter}: the fact is the promoted row");
    assert_eq!(actor, "emp-cto", "{adapter}: signed by the author");
}

/// Publish with nothing to promote — a name never written, or one whose
/// only row is already live — is `NotFound`, flips nothing and records
/// nothing.
async fn a_publish_with_no_draft_refuses_and_records_nothing<W: World>(w: &W, adapter: &str) {
    let got = w.repo().publish("suite-ghost", &author(), at(60)).await;
    assert!(is_not_found(&got), "{adapter}: {got:?}");

    draft(w, viable("suite-live"), 60).await;
    publish(w, "suite-live").await;
    let before = fact_rows(w).await;
    let got = w.repo().publish("suite-live", &author(), at(120)).await;
    assert!(is_not_found(&got), "{adapter}: {got:?}");
    assert_eq!(
        history(w, "suite-live").await,
        vec![(1, WorkflowStatus::Active)],
        "{adapter}"
    );
    assert_eq!(fact_rows(w).await, before, "{adapter}: no fact");
}

/// A draft the viability lint refuses cannot publish: `Unviable` with
/// its problems, the live row stays live, the draft stays a draft, and
/// no `published` fact is recorded.
async fn an_unviable_draft_refuses_publish_and_flips_nothing<W: World>(w: &W, adapter: &str) {
    draft(w, viable("suite-guarded"), 60).await;
    publish(w, "suite-guarded").await;
    draft(w, unviable("suite-guarded"), 120).await;
    let before = fact_rows(w).await;

    let got = w.repo().publish("suite-guarded", &author(), at(180)).await;
    match got {
        Err(StationError::Unviable(problems)) => {
            assert!(!problems.is_empty(), "{adapter}: the refusal names why")
        }
        other => panic!("{adapter}: an unviable draft must be refused: {other:?}"),
    }
    assert_eq!(
        history(w, "suite-guarded").await,
        vec![(1, WorkflowStatus::Active), (2, WorkflowStatus::Draft)],
        "{adapter}"
    );
    assert_eq!(fact_rows(w).await, before, "{adapter}: no fact");
}

/// Retire flips the live row to retired and records one `retired`
/// carrying it. Retiring again, or retiring a name with no live row, is
/// `Ok` and SILENT. History stays readable by version.
async fn retire_is_idempotent_and_keeps_history_readable<W: World>(w: &W, adapter: &str) {
    draft(w, viable("suite-gone"), 60).await;
    publish(w, "suite-gone").await;
    for _ in 0..2 {
        w.repo()
            .retire("suite-gone", &author(), at(120))
            .await
            .expect("retire answers");
    }
    w.repo()
        .retire("suite-never", &author(), at(120))
        .await
        .expect("retiring an unknown name is a no-op");

    let got = w.repo().get_active("suite-gone").await;
    assert!(is_not_found(&got), "{adapter}: {got:?}");
    let want = row(&viable("suite-gone"), 1, WorkflowStatus::Retired, 60);
    assert_eq!(
        w.repo()
            .get_version("suite-gone", 1)
            .await
            .expect("history is readable"),
        want,
        "{adapter}"
    );
    let recorded = w.facts().await;
    assert_eq!(
        fact_rows(w).await,
        facts(&[
            (STATION_DRAFT_SAVED, "suite-gone", 1),
            (STATION_PUBLISHED, "suite-gone", 1),
            (STATION_RETIRED, "suite-gone", 1),
        ]),
        "{adapter}: one retirement, one fact"
    );
    let (spec, _) = fact_spec(&recorded[2].1);
    assert_eq!(spec, want, "{adapter}: the fact is the retired row");
}

/// `list_active` is every live station, "name-ordered" as the port
/// says — in BYTE order (`-` sorts before a letter), the order the
/// double holds and the only one both adapters can: a listing whose
/// order depends on the database's locale answers two questions. A
/// draft-only name and a retired one are not listed; a name with a live
/// row AND a newer draft is listed once, at its live version.
async fn list_active_is_every_live_name_in_byte_order<W: World>(w: &W, adapter: &str) {
    for name in ["suite-b", "suite-ab", "suite-a-z"] {
        draft(w, viable(name), 60).await;
        publish(w, name).await;
    }
    draft(w, viable("suite-draft-only"), 60).await;
    draft(w, viable("suite-retired"), 60).await;
    publish(w, "suite-retired").await;
    w.repo()
        .retire("suite-retired", &author(), at(120))
        .await
        .expect("retire");
    draft(w, viable("suite-b"), 180).await;

    let listed: Vec<(String, i32)> = w
        .repo()
        .list_active()
        .await
        .expect("list_active")
        .into_iter()
        .filter(|s| s.name.starts_with("suite-"))
        .map(|s| (s.name, s.version))
        .collect();
    let one = |n: &str| (n.to_string(), 1);
    assert_eq!(
        listed,
        vec![one("suite-a-z"), one("suite-ab"), one("suite-b")],
        "{adapter}: every live name, byte for byte"
    );
}

/// `publish_declared` — the platform bundle's write — lands the row LIVE
/// at the version it declares, stamped `now`, retiring any live row of
/// the name; the next draft counts on from it. A declared version that
/// is already held is `Conflict` and overwrites nothing; an unviable
/// declaration is `Unviable` and lands nothing. Only a landed row
/// records a `published`.
async fn a_declared_version_lands_as_declared_and_is_never_overwritten<W: World>(
    w: &W,
    adapter: &str,
) {
    let mut v3 = viable("suite-bundle");
    v3.version = 3;
    let landed = w
        .repo()
        .publish_declared(v3.clone(), &author(), at(60))
        .await
        .expect("a declared version lands");
    let want3 = row(&v3, 3, WorkflowStatus::Active, 60);
    assert_eq!(landed, want3, "{adapter}");
    assert_eq!(
        w.repo()
            .get_active("suite-bundle")
            .await
            .expect("get_active"),
        want3,
        "{adapter}"
    );

    let next = draft(w, viable("suite-bundle"), 120).await;
    assert_eq!(next.version, 4, "{adapter}: drafts count on from it");

    let mut v5 = viable("suite-bundle");
    v5.version = 5;
    v5.title = "The bundle's next version".into();
    w.repo()
        .publish_declared(v5.clone(), &author(), at(180))
        .await
        .expect("a newer declared version lands");
    assert_eq!(
        history(w, "suite-bundle").await,
        vec![
            (3, WorkflowStatus::Retired),
            (4, WorkflowStatus::Draft),
            (5, WorkflowStatus::Active),
        ],
        "{adapter}"
    );
    let before = fact_rows(w).await;

    let mut again = viable("suite-bundle");
    again.version = 3;
    again.title = "an attempt to overwrite v3".into();
    let got = w.repo().publish_declared(again, &author(), at(240)).await;
    assert!(
        matches!(got, Err(StationError::Conflict(_))),
        "{adapter}: {got:?}"
    );
    let mut bad = unviable("suite-bundle");
    bad.version = 6;
    let got = w.repo().publish_declared(bad, &author(), at(240)).await;
    assert!(
        matches!(got, Err(StationError::Unviable(_))),
        "{adapter}: {got:?}"
    );

    assert_eq!(
        w.repo()
            .get_version("suite-bundle", 3)
            .await
            .expect("v3 is held"),
        row(&v3, 3, WorkflowStatus::Retired, 60),
        "{adapter}: the held row is untouched"
    );
    assert_eq!(
        history(w, "suite-bundle").await,
        vec![
            (3, WorkflowStatus::Retired),
            (4, WorkflowStatus::Draft),
            (5, WorkflowStatus::Active),
        ],
        "{adapter}: nothing landed"
    );
    assert_eq!(
        fact_rows(w).await,
        before,
        "{adapter}: no fact for a refusal"
    );
    assert_eq!(
        before,
        facts(&[
            (STATION_PUBLISHED, "suite-bundle", 3),
            (STATION_DRAFT_SAVED, "suite-bundle", 4),
            (STATION_PUBLISHED, "suite-bundle", 5),
        ]),
        "{adapter}"
    );
}

/// THE FLOOR (backlog df793bd7, the station twin of 1cd85e94). A
/// declared version at or below the newest the lineage holds — the live
/// row, or a draft above it — is `Conflict`: it retires nothing, writes
/// nothing, records nothing. Before, only the EXACT (name, version) was
/// refused, so a bundle's `v4` under a live `v5` retired `v5` and went
/// live in its place — the write the seed's classify, a read taken
/// before it, cannot rule out when two converges overlap.
async fn a_declared_version_at_or_below_the_newest_is_refused<W: World>(w: &W, adapter: &str) {
    let declared = |version: i32| {
        let mut s = viable("suite-floor");
        s.version = version;
        s.title = format!("The floor at v{version}");
        s
    };
    w.repo()
        .publish_declared(declared(5), &author(), at(60))
        .await
        .expect("a declared version lands");
    for below in [4, 1] {
        let got = w
            .repo()
            .publish_declared(declared(below), &author(), at(120))
            .await;
        assert!(
            matches!(got, Err(StationError::Conflict(_))),
            "{adapter}: v{below} under v5: {got:?}"
        );
    }
    let d = draft(w, viable("suite-floor"), 180).await;
    assert_eq!(d.version, 6, "{adapter}");
    let got = w
        .repo()
        .publish_declared(declared(6), &author(), at(240))
        .await;
    assert!(
        matches!(got, Err(StationError::Conflict(_))),
        "{adapter}: v6 is the draft's: {got:?}"
    );

    assert_eq!(
        w.repo()
            .get_active("suite-floor")
            .await
            .expect("get_active"),
        row(&declared(5), 5, WorkflowStatus::Active, 60),
        "{adapter}: the live row is untouched"
    );
    assert_eq!(
        history(w, "suite-floor").await,
        vec![(5, WorkflowStatus::Active), (6, WorkflowStatus::Draft)],
        "{adapter}"
    );
    assert_eq!(
        fact_rows(w).await,
        facts(&[
            (STATION_PUBLISHED, "suite-floor", 5),
            (STATION_DRAFT_SAVED, "suite-floor", 6),
        ]),
        "{adapter}: a refusal records no fact"
    );
    w.repo()
        .publish_declared(declared(7), &author(), at(300))
        .await
        .expect("above the whole lineage lands");
}

/// `terminal_window_days` is a `u32` on the row and an `INT` column in
/// Postgres. A window past `i32::MAX` days cannot be stored as written,
/// so both writes that store one — a draft and a declared version —
/// refuse it as `Invalid`, landing no row and recording no fact. The
/// alternative, storing some other number, leaves a row that differs
/// from the value the write answered and the fact it recorded.
async fn a_window_the_column_cannot_hold_is_refused_whole<W: World>(w: &W, adapter: &str) {
    let mut wide = whole("suite-wide");
    wide.terminal_window_days = Some(u32::try_from(i32::MAX).expect("fits") + 1);
    let got = w.repo().create_draft(wide.clone(), &author(), at(60)).await;
    assert!(
        matches!(got, Err(StationError::Invalid(_))),
        "{adapter}: {got:?}"
    );
    wide.version = 2;
    let got = w.repo().publish_declared(wide, &author(), at(60)).await;
    assert!(
        matches!(got, Err(StationError::Invalid(_))),
        "{adapter}: {got:?}"
    );
    assert!(history(w, "suite-wide").await.is_empty(), "{adapter}");
    assert!(w.facts().await.is_empty(), "{adapter}");

    // The widest window the column holds lands, and reads back as written.
    let mut widest = whole("suite-wide");
    widest.terminal_window_days = Some(u32::try_from(i32::MAX).expect("fits"));
    let stored = draft(w, widest.clone(), 120).await;
    assert_eq!(
        w.repo()
            .get_version("suite-wide", stored.version)
            .await
            .expect("get_version"),
        row(&widest, 1, WorkflowStatus::Draft, 120),
        "{adapter}"
    );
}
