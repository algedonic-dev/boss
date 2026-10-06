//! The step-plugin registry answers the same on both
//! `StepPluginRegistry` adapters — reliability mechanism C of design
//! 3036296f, "adapters agree" (`boss_testing::adapters_agree!`), on the
//! next port in the census order after the departments registry
//! (backlog be459ab9).
//!
//! WHY THIS PORT. A step plugin is the surface a step renders through,
//! and its active version is snapshotted onto every step born under it
//! (`the_adapters_agree_on_a_steps_plugin_version_pg.rs` holds the JOBS
//! store to that; this file holds the REGISTRY it reads). Every door test
//! of `/api/step-plugins` and the platform bundle's seed asks
//! `InMemoryStepPlugins`; production is answered by `PgStepPlugins`. This
//! file is the one statement both are held to — every method of the
//! port, and every write judged by the fact it leaves.
//!
//! The first run found one disagreement, in the Postgres adapter against
//! the port's own shape, fixed in this car (a production change to
//! `PgStepPlugins::list_active`):
//! - ORDER. `list_active` answers kind-ordered on both adapters (the
//!   double sorts by kind, Postgres said `ORDER BY kind`). Postgres
//!   sorted by the database's locale, which ignores `-` at first level,
//!   so `suite-ab` listed before `suite-a-z` there and after it in
//!   memory. It now sorts `COLLATE "C"` — byte order, the order the
//!   double holds and the only one both can; the Workflow, credentials,
//!   station and departments registries' suites found and fixed the same
//!   defect the same way. Case `list_active_is_every_live_kind_in_byte_order`.
//!
//! THE SHAPE, the station suite's, since this port is its twin: each case
//! states its answer — as `(version, status)` pairs, kinds, or whole
//! rows — and each adapter is held to that stated answer, not merely to
//! the other one. Every write is also judged by the FACTS it leaves: the
//! `jobs.step_plugin.*` events the in-memory adapter collects and
//! Postgres stages on `event_outbox` in the row's own transaction,
//! because the port promises the event "atomically with the row",
//! payload = the row written. Instants are whole seconds, so Postgres's
//! microseconds and the double's nanoseconds compare equal.
//!
//! THE VERSION FLOOR (backlog 1cd85e94). `publish_declared` retired any
//! live row of the kind whatever its version, so the only thing between
//! a stale bundle and a downgrade was the seed's classify — a read
//! taken BEFORE the write. Two converges carrying different bundles
//! overlap: the older classifies `v4` as a publish over a live `v3`, the
//! newer lands `v5`, and the older's write then retired `v5` for `v4`,
//! silently. The port now enforces its own floor inside the write — a
//! declared version at or below the newest the lineage holds is
//! `Conflict` — and the overlap is planted two ways: in sequence
//! through both adapters (case
//! `an_older_seed_overtaken_by_a_newer_one_downgrades_nothing`), and as
//! two Postgres transactions in flight at once
//! (`pg_an_older_seed_waiting_on_a_newer_one_is_refused`), because a
//! floor read under the wrong lock passes the first and fails the
//! second.
//!
//! Rows the world starts with: the migrations seed the platform's plugins
//! (`sign-off`, `answer-question`, …) straight into every Postgres
//! database, and the in-memory adapter starts empty, so every kind this
//! file writes starts `suite-`, and an unfiltered read (`list_active`,
//! the outbox) is judged over those kinds only.

use boss_core::actor::ActorId;
use boss_jobs::events::{STEP_PLUGIN_DRAFT_SAVED, STEP_PLUGIN_PUBLISHED, STEP_PLUGIN_RETIRED};
use boss_jobs::registry::WorkflowStatus;
use boss_jobs::step_plugin_seed::{SeedOutcome, StepPluginSeedError, seed_step_plugins};
use boss_jobs::{
    InMemoryStepPlugins, PgStepPlugins, StepPluginError, StepPluginRegistry, StepPluginSpec,
};
use chrono::{DateTime, TimeZone, Utc};
use serde_json::{Value, json};
use uuid::Uuid;

/// The registry under test and the facts its writes left, read the way
/// each adapter keeps them.
trait World {
    type R: StepPluginRegistry;
    fn repo(&self) -> &Self::R;
    /// Every `jobs.step_plugin.*` fact about a `suite-` kind recorded so
    /// far, in the order recorded: `(kind, payload)`.
    async fn facts(&self) -> Vec<(String, Value)>;
}

fn is_suite_fact(kind: &str, payload: &Value) -> bool {
    kind.starts_with("jobs.step_plugin.")
        && payload["kind"]
            .as_str()
            .is_some_and(|k| k.starts_with("suite-"))
}

struct InMemory(InMemoryStepPlugins);

impl World for InMemory {
    type R = InMemoryStepPlugins;
    fn repo(&self) -> &InMemoryStepPlugins {
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
    repo: PgStepPlugins,
    pool: sqlx::PgPool,
}

impl World for Postgres {
    type R = PgStepPlugins;
    fn repo(&self) -> &PgStepPlugins {
        &self.repo
    }
    async fn facts(&self) -> Vec<(String, Value)> {
        let rows: Vec<(String, Value)> = sqlx::query_as(
            "SELECT kind, payload FROM event_outbox WHERE kind LIKE 'jobs.step\\_plugin.%' ORDER BY id",
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
        in_memory => (InMemory(InMemoryStepPlugins::new()), ()),
        postgres => {
            let db = boss_testing::TestDb::new().await;
            let world = Postgres { repo: PgStepPlugins::new(db.pool.clone()), pool: db.pool.clone() };
            (world, db)
        },
    }
    cases {
        a_fresh_registry_holds_no_suite_plugin,
        a_draft_lands_whole_at_the_next_version_and_is_not_live,
        publish_promotes_the_newest_draft_and_retires_the_active,
        a_publish_with_no_draft_refuses_and_records_nothing,
        retire_is_idempotent_and_keeps_history_readable,
        list_active_is_every_live_kind_in_byte_order,
        list_active_narrows_to_one_category,
        a_declared_version_lands_as_declared_and_is_never_overwritten,
        a_declared_version_at_or_below_the_newest_is_refused,
        an_older_seed_overtaken_by_a_newer_one_downgrades_nothing,
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

/// A minimal plugin: no description, no authoring packet, an empty
/// schema.
fn plain(kind: &str) -> StepPluginSpec {
    let mut s = StepPluginSpec::draft(
        kind,
        format!("The {kind} surface"),
        "suite",
        format!("{kind}.js"),
        json!({}),
    );
    s.created_at = at(0);
    s
}

/// A plugin with EVERY optional field set and a schema carrying each JSON
/// shape, so a column the Pg adapter drops or a JSONB round trip that
/// bends a value shows up as a whole-row difference.
fn whole(kind: &str) -> StepPluginSpec {
    let mut s = plain(kind);
    s.description = Some("Every field declared — ünïcode included".into());
    s.category = "suite-review".into();
    s.owning_team = "reliability".into();
    s.authoring_job_id = Some(Uuid::from_u128(0x3036_296f_be45_9ab9));
    s.metadata_schema = json!({
        "type": "object",
        "required": ["verdict", "notes"],
        "properties": {
            "verdict": {"type": "string", "enum": ["pass", "fail"]},
            "score": {"type": "number", "minimum": 0.5, "maximum": 10},
            "notes": {"type": "string", "default": null},
            "strict": true,
        },
    });
    s
}

/// `spec` as a row of the registry: at `version`, in `status`, stamped
/// `created_at`.
fn row(
    spec: &StepPluginSpec,
    version: i32,
    status: WorkflowStatus,
    created: i64,
) -> StepPluginSpec {
    StepPluginSpec {
        version,
        status,
        created_at: at(created),
        ..spec.clone()
    }
}

async fn draft<W: World>(w: &W, spec: StepPluginSpec, secs: i64) -> StepPluginSpec {
    w.repo()
        .create_draft(spec, &author(), at(secs))
        .await
        .expect("a draft lands")
}

async fn publish<W: World>(w: &W, kind: &str) -> StepPluginSpec {
    w.repo()
        .publish(kind, &author(), at(9_000))
        .await
        .expect("a draft publishes")
}

/// `(version, status)` for every row of `kind`, oldest first.
async fn history<W: World>(w: &W, kind: &str) -> Vec<(i32, WorkflowStatus)> {
    w.repo()
        .list_versions(kind)
        .await
        .expect("list_versions answers")
        .into_iter()
        .map(|s| (s.version, s.status))
        .collect()
}

/// `(event kind, plugin kind, version)` for each fact.
async fn fact_rows<W: World>(w: &W) -> Vec<(String, String, i64)> {
    w.facts()
        .await
        .into_iter()
        .map(|(kind, p)| {
            let plugin = p["kind"].as_str().unwrap_or("").to_string();
            let version = p["version"].as_i64().unwrap_or(-1);
            (kind, plugin, version)
        })
        .collect()
}

fn facts(v: &[(&str, &str, i64)]) -> Vec<(String, String, i64)> {
    v.iter()
        .map(|(k, n, ver)| (k.to_string(), n.to_string(), *ver))
        .collect()
}

/// The spec a fact's payload describes, and the actor it carries.
fn fact_spec(payload: &Value) -> (StepPluginSpec, String) {
    let mut p = payload.clone();
    let actor = p
        .as_object_mut()
        .and_then(|m| m.remove("_actor"))
        .and_then(|a| a.as_str().map(String::from))
        .unwrap_or_default();
    let spec = serde_json::from_value(p).expect("a step-plugin fact's payload is a plugin row");
    (spec, actor)
}

fn is_not_found<T: std::fmt::Debug>(r: &Result<T, StepPluginError>) -> bool {
    matches!(r, Err(StepPluginError::NotFound(_)))
}

// ----- cases -----------------------------------------------------------------

/// Nothing written: no suite plugin answers any read, none is listed, and
/// no fact names one. A miss is `NotFound`, never a storage error.
async fn a_fresh_registry_holds_no_suite_plugin<W: World>(w: &W, adapter: &str) {
    let got = w.repo().get_active("suite-nothing").await;
    assert!(is_not_found(&got), "{adapter}: {got:?}");
    let got = w.repo().get_version("suite-nothing", 1).await;
    assert!(is_not_found(&got), "{adapter}: {got:?}");
    assert!(history(w, "suite-nothing").await.is_empty(), "{adapter}");
    let listed = w.repo().list_active(None).await.expect("list_active");
    assert!(
        !listed.iter().any(|s| s.kind.starts_with("suite-")),
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
            .expect("get_version"),
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
    let listed = w.repo().list_active(None).await.expect("list_active");
    assert!(
        !listed.iter().any(|s| s.kind == "suite-whole"),
        "{adapter}: {listed:?}"
    );

    let recorded = w.facts().await;
    assert_eq!(
        fact_rows(w).await,
        facts(&[
            (STEP_PLUGIN_DRAFT_SAVED, "suite-whole", 1),
            (STEP_PLUGIN_DRAFT_SAVED, "suite-whole", 2),
        ]),
        "{adapter}"
    );
    let (spec, actor) = fact_spec(&recorded[0].1);
    assert_eq!(spec, want, "{adapter}: the fact is the row stored");
    assert_eq!(actor, "emp-cto", "{adapter}: signed by the author");
}

/// Publish promotes the NEWEST draft and retires whatever was active; an
/// older draft is left a draft. The promoted row keeps the instant its
/// draft was saved — a publish moves status, not history. Each publish
/// records one `published` whose payload is the promoted row.
async fn publish_promotes_the_newest_draft_and_retires_the_active<W: World>(w: &W, adapter: &str) {
    draft(w, plain("suite-flow"), 60).await;
    let first = publish(w, "suite-flow").await;
    assert_eq!(
        first,
        row(&plain("suite-flow"), 1, WorkflowStatus::Active, 60),
        "{adapter}"
    );
    draft(w, plain("suite-flow"), 120).await;
    let mut newest = plain("suite-flow");
    newest.label = "The newest draft".into();
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
            (STEP_PLUGIN_DRAFT_SAVED, "suite-flow", 1),
            (STEP_PLUGIN_PUBLISHED, "suite-flow", 1),
            (STEP_PLUGIN_DRAFT_SAVED, "suite-flow", 2),
            (STEP_PLUGIN_DRAFT_SAVED, "suite-flow", 3),
            (STEP_PLUGIN_PUBLISHED, "suite-flow", 3),
        ]),
        "{adapter}: the retirement a publish causes is inside its published fact"
    );
    let (spec, actor) = fact_spec(&recorded[4].1);
    assert_eq!(spec, want, "{adapter}: the fact is the promoted row");
    assert_eq!(actor, "emp-cto", "{adapter}: signed by the author");
}

/// Publish with nothing to promote — a kind never written, or one whose
/// only row is already live — is `NotFound`, flips nothing and records
/// nothing.
async fn a_publish_with_no_draft_refuses_and_records_nothing<W: World>(w: &W, adapter: &str) {
    let got = w.repo().publish("suite-ghost", &author(), at(60)).await;
    assert!(is_not_found(&got), "{adapter}: {got:?}");

    draft(w, plain("suite-live"), 60).await;
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

/// Retire flips the live row to retired and records one `retired`
/// carrying it. Retiring again, or retiring a kind with no live row, is
/// `Ok` and SILENT. History stays readable by version.
async fn retire_is_idempotent_and_keeps_history_readable<W: World>(w: &W, adapter: &str) {
    draft(w, plain("suite-gone"), 60).await;
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
        .expect("retiring an unknown kind is a no-op");

    let got = w.repo().get_active("suite-gone").await;
    assert!(is_not_found(&got), "{adapter}: {got:?}");
    let want = row(&plain("suite-gone"), 1, WorkflowStatus::Retired, 60);
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
            (STEP_PLUGIN_DRAFT_SAVED, "suite-gone", 1),
            (STEP_PLUGIN_PUBLISHED, "suite-gone", 1),
            (STEP_PLUGIN_RETIRED, "suite-gone", 1),
        ]),
        "{adapter}: one retirement, one fact"
    );
    let (spec, _) = fact_spec(&recorded[2].1);
    assert_eq!(spec, want, "{adapter}: the fact is the retired row");
}

/// `list_active` is every live plugin ordered by kind — in BYTE order
/// (`-` sorts before a letter), the order the double holds and the only
/// one both adapters can: a listing whose order depends on the database's
/// locale answers two questions. A draft-only kind and a retired one are
/// not listed; a kind with a live row AND a newer draft is listed once,
/// at its live version.
async fn list_active_is_every_live_kind_in_byte_order<W: World>(w: &W, adapter: &str) {
    for kind in ["suite-b", "suite-ab", "suite-a-z"] {
        draft(w, plain(kind), 60).await;
        publish(w, kind).await;
    }
    draft(w, plain("suite-draft-only"), 60).await;
    draft(w, plain("suite-retired"), 60).await;
    publish(w, "suite-retired").await;
    w.repo()
        .retire("suite-retired", &author(), at(120))
        .await
        .expect("retire");
    draft(w, plain("suite-b"), 180).await;

    let listed: Vec<(String, i32)> = w
        .repo()
        .list_active(None)
        .await
        .expect("list_active")
        .into_iter()
        .filter(|s| s.kind.starts_with("suite-"))
        .map(|s| (s.kind, s.version))
        .collect();
    let one = |k: &str| (k.to_string(), 1);
    assert_eq!(
        listed,
        vec![one("suite-a-z"), one("suite-ab"), one("suite-b")],
        "{adapter}: every live kind, byte for byte"
    );
}

/// `list_active(Some(category))` answers exactly the live plugins of that
/// category, in the same byte order; a category nothing is filed under
/// answers empty, not everything. The two kinds differ around a `-`, so
/// they sort differently by byte and by locale: the filtered branch is
/// its own SQL statement, and kinds that sort the same both ways let its
/// `COLLATE "C"` be deleted with every case still green (review run
/// 550600ac).
async fn list_active_narrows_to_one_category<W: World>(w: &W, adapter: &str) {
    for (kind, category) in [
        ("suite-review-ab", "suite-review"),
        ("suite-review-a-z", "suite-review"),
        ("suite-other", "suite-other"),
    ] {
        let mut s = plain(kind);
        s.category = category.into();
        draft(w, s, 60).await;
        publish(w, kind).await;
    }
    let kinds = |v: Vec<StepPluginSpec>| -> Vec<String> { v.into_iter().map(|s| s.kind).collect() };
    assert_eq!(
        kinds(
            w.repo()
                .list_active(Some("suite-review"))
                .await
                .expect("list_active")
        ),
        vec![
            "suite-review-a-z".to_string(),
            "suite-review-ab".to_string()
        ],
        "{adapter}"
    );
    assert!(
        w.repo()
            .list_active(Some("suite-no-such-category"))
            .await
            .expect("list_active")
            .is_empty(),
        "{adapter}: a filter nothing matches answers empty"
    );
}

/// `publish_declared` — the platform bundle's write — lands the row LIVE
/// at the version it declares, stamped `now`, retiring any live row of
/// the kind; the next draft counts on from it. A declared version that
/// is already held is `Conflict` and overwrites nothing. Only a landed
/// row records a `published`.
async fn a_declared_version_lands_as_declared_and_is_never_overwritten<W: World>(
    w: &W,
    adapter: &str,
) {
    let mut v3 = whole("suite-bundle");
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

    let next = draft(w, plain("suite-bundle"), 120).await;
    assert_eq!(next.version, 4, "{adapter}: drafts count on from it");

    let mut v5 = plain("suite-bundle");
    v5.version = 5;
    v5.label = "The bundle's next version".into();
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

    let mut again = plain("suite-bundle");
    again.version = 3;
    again.label = "an attempt to overwrite v3".into();
    let got = w.repo().publish_declared(again, &author(), at(240)).await;
    assert!(
        matches!(got, Err(StepPluginError::Conflict(_))),
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
        w.repo()
            .get_active("suite-bundle")
            .await
            .expect("get_active")
            .version,
        5,
        "{adapter}: a refusal retires nothing"
    );
    assert_eq!(
        fact_rows(w).await,
        before,
        "{adapter}: no fact for a refusal"
    );
    assert_eq!(
        before,
        facts(&[
            (STEP_PLUGIN_PUBLISHED, "suite-bundle", 3),
            (STEP_PLUGIN_DRAFT_SAVED, "suite-bundle", 4),
            (STEP_PLUGIN_PUBLISHED, "suite-bundle", 5),
        ]),
        "{adapter}"
    );
}

/// `spec` as a bundle row declared at `version`, its JS named for it so
/// two versions of one kind differ in the column a downgrade would
/// change under David.
fn declared(kind: &str, version: i32) -> StepPluginSpec {
    let mut s = plain(kind);
    s.version = version;
    s.status = WorkflowStatus::Active;
    s.frontend_url = format!("{kind}-v{version}.js");
    s
}

fn seeder() -> ActorId {
    ActorId::Automation("platform-workflow-seed".into())
}

fn is_conflict<T: std::fmt::Debug>(r: &Result<T, StepPluginError>) -> bool {
    matches!(r, Err(StepPluginError::Conflict(_)))
}

/// THE FLOOR ITSELF. A declared version at or below the newest version
/// the lineage holds — the live row, or a row above it in any status —
/// is `Conflict`: it retires nothing, writes nothing, records nothing.
/// Before backlog 1cd85e94 only the EXACT (kind, version) was refused,
/// so `v4` under a live `v5` retired `v5` and went live in its place.
async fn a_declared_version_at_or_below_the_newest_is_refused<W: World>(w: &W, adapter: &str) {
    let v5 = declared("suite-floor", 5);
    w.repo()
        .publish_declared(v5.clone(), &seeder(), at(60))
        .await
        .expect("a declared version lands");
    let before = fact_rows(w).await;

    for below in [4, 1] {
        let got = w
            .repo()
            .publish_declared(declared("suite-floor", below), &seeder(), at(120))
            .await;
        assert!(is_conflict(&got), "{adapter}: v{below} under v5: {got:?}");
    }
    // Above the live row, below a DRAFT: the draft's number is spent
    // (an author's next publish promotes it), so the bundle may not
    // declare under it either — the floor is the whole lineage, as the
    // cadence registry's is.
    let draft6 = draft(w, plain("suite-floor"), 180).await;
    assert_eq!(draft6.version, 6, "{adapter}");
    let got = w
        .repo()
        .publish_declared(declared("suite-floor", 6), &seeder(), at(240))
        .await;
    assert!(is_conflict(&got), "{adapter}: v6 is the draft's: {got:?}");

    assert_eq!(
        w.repo()
            .get_active("suite-floor")
            .await
            .expect("get_active"),
        row(&v5, 5, WorkflowStatus::Active, 60),
        "{adapter}: the live row is untouched"
    );
    assert_eq!(
        history(w, "suite-floor").await,
        vec![(5, WorkflowStatus::Active), (6, WorkflowStatus::Draft)],
        "{adapter}: no row was written below the floor"
    );
    let mut want = before;
    want.push((STEP_PLUGIN_DRAFT_SAVED.to_string(), "suite-floor".into(), 6));
    assert_eq!(
        fact_rows(w).await,
        want,
        "{adapter}: a refusal records no fact"
    );

    let v7 = w
        .repo()
        .publish_declared(declared("suite-floor", 7), &seeder(), at(300))
        .await
        .expect("above the whole lineage lands");
    assert_eq!(v7.version, 7, "{adapter}");
}

/// A registry whose FIRST declared write is overtaken: before it goes
/// through, a newer converge runs its whole seed of `newer` — the
/// overlap of two converges carrying different bundles, planted at the
/// one point that matters, between the older seed's classify and its
/// write.
struct Overtaken<'a, R> {
    inner: &'a R,
    newer: std::sync::Mutex<Option<Vec<StepPluginSpec>>>,
}

#[async_trait::async_trait]
impl<R: StepPluginRegistry> StepPluginRegistry for Overtaken<'_, R> {
    async fn get_active(&self, kind: &str) -> Result<StepPluginSpec, StepPluginError> {
        self.inner.get_active(kind).await
    }
    async fn get_version(
        &self,
        kind: &str,
        version: i32,
    ) -> Result<StepPluginSpec, StepPluginError> {
        self.inner.get_version(kind, version).await
    }
    async fn list_active(
        &self,
        category: Option<&str>,
    ) -> Result<Vec<StepPluginSpec>, StepPluginError> {
        self.inner.list_active(category).await
    }
    async fn list_versions(&self, kind: &str) -> Result<Vec<StepPluginSpec>, StepPluginError> {
        self.inner.list_versions(kind).await
    }
    async fn create_draft(
        &self,
        spec: StepPluginSpec,
        actor: &ActorId,
        now: DateTime<Utc>,
    ) -> Result<StepPluginSpec, StepPluginError> {
        self.inner.create_draft(spec, actor, now).await
    }
    async fn publish(
        &self,
        kind: &str,
        actor: &ActorId,
        now: DateTime<Utc>,
    ) -> Result<StepPluginSpec, StepPluginError> {
        self.inner.publish(kind, actor, now).await
    }
    async fn retire(
        &self,
        kind: &str,
        actor: &ActorId,
        now: DateTime<Utc>,
    ) -> Result<(), StepPluginError> {
        self.inner.retire(kind, actor, now).await
    }
    async fn publish_declared(
        &self,
        spec: StepPluginSpec,
        actor: &ActorId,
        now: DateTime<Utc>,
    ) -> Result<StepPluginSpec, StepPluginError> {
        let newer = self.newer.lock().ok().and_then(|mut n| n.take());
        if let Some(newer) = newer {
            seed_step_plugins(self.inner, &newer, actor, now, false)
                .await
                .expect("the newer converge lands whole");
        }
        self.inner.publish_declared(spec, actor, now).await
    }
}

/// TWO OVERLAPPING SEEDS (the downgrade the review of be459ab9 found).
/// `v3` is live. An older converge's seed classifies its bundle row `v4`
/// as a publish over `v3`; before it writes, a newer converge's seed
/// lands `v5`. The older write is refused `Conflict` — the seed fails
/// naming the kind — and `v5` stays live: the plugin David sees is the
/// newer one, and no `published` fact claims otherwise.
async fn an_older_seed_overtaken_by_a_newer_one_downgrades_nothing<W: World>(w: &W, adapter: &str) {
    let v3 = declared("suite-overtaken", 3);
    let v4 = declared("suite-overtaken", 4);
    let v5 = declared("suite-overtaken", 5);
    w.repo()
        .publish_declared(v3, &seeder(), at(60))
        .await
        .expect("v3 is live");

    let overtaken = Overtaken {
        inner: w.repo(),
        newer: std::sync::Mutex::new(Some(vec![v5.clone()])),
    };
    let older = seed_step_plugins(&overtaken, &[v4], &seeder(), at(120), false).await;
    match older {
        Err(StepPluginSeedError::Registry {
            row,
            error: StepPluginError::Conflict(_),
            ..
        }) => assert_eq!(
            row, "suite-overtaken",
            "{adapter}: the refusal names the kind"
        ),
        other => panic!("{adapter}: the overtaken seed must be refused, got {other:?}"),
    }

    assert_eq!(
        w.repo()
            .get_active("suite-overtaken")
            .await
            .expect("get_active"),
        row(&v5, 5, WorkflowStatus::Active, 120),
        "{adapter}: the newer bundle's row stays live"
    );
    assert_eq!(
        history(w, "suite-overtaken").await,
        vec![(3, WorkflowStatus::Retired), (5, WorkflowStatus::Active)],
        "{adapter}: v4 was never written"
    );
    assert_eq!(
        fact_rows(w).await,
        facts(&[
            (STEP_PLUGIN_PUBLISHED, "suite-overtaken", 3),
            (STEP_PLUGIN_PUBLISHED, "suite-overtaken", 5),
        ]),
        "{adapter}"
    );
}

// ----- Postgres: the overlap as two transactions in flight ---------------------

/// Sessions of this database blocked on a lock right now.
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

/// THE SAME OVERLAP, CONCURRENT. The newer seed's `v5` write is held
/// before its commit — a third transaction owns an uncommitted row at
/// the key its insert writes, so the insert waits — and the older
/// seed, which classified `v4` against the still-live `v3`, is observed
/// WAITING on it before the hold is released. Let through, the older
/// write must read the `v5` that committed ahead of it and refuse.
///
/// This is the case the cadence registry's floor shape did not pass
/// (until backlog 4541d511 moved it onto this module's floor):
/// `MAX(version)` over rows locked `FOR UPDATE` in ONE statement waits
/// on `v3`'s lock, then re-reads `v3` alone — a row the lock holder
/// INSERTED is not in the snapshot that statement began with — so it
/// answered `3`, and the older write retired the committed `v5` (run
/// against that shape while this car was built). The floor is read in
/// a statement that STARTS after the lineage's lock is held.
#[tokio::test(flavor = "multi_thread")]
async fn pg_an_older_seed_waiting_on_a_newer_one_is_refused() {
    let db = boss_testing::TestDb::new().await;
    let repo = PgStepPlugins::new(db.pool.clone());
    let kind = "suite-concurrent";
    repo.publish_declared(declared(kind, 3), &seeder(), at(60))
        .await
        .expect("v3 is live");

    let mut hold = db.pool.begin().await.expect("hold tx");
    sqlx::query(
        "INSERT INTO step_plugins
            (kind, version, status, label, category, metadata_schema, frontend_url, owning_team)
         VALUES ($1, 5, 'draft', 'the hold', 'suite', '{}', 'hold.js', 'suite')",
    )
    .bind(kind)
    .execute(&mut *hold)
    .await
    .expect("hold the newer seed's key");

    let newer_repo = PgStepPlugins::new(db.pool.clone());
    let newer = tokio::spawn(async move {
        seed_step_plugins(&newer_repo, &[declared(kind, 5)], &seeder(), at(120), false).await
    });
    until_waiting(&db.pool, 1, &newer).await;
    assert!(
        !newer.is_finished(),
        "case error: the newer seed must be held before its commit"
    );

    let older_repo = PgStepPlugins::new(db.pool.clone());
    let older = tokio::spawn(async move {
        seed_step_plugins(&older_repo, &[declared(kind, 4)], &seeder(), at(180), false).await
    });
    until_waiting(&db.pool, 2, &older).await;

    hold.rollback().await.expect("release the newer seed");
    let report = newer
        .await
        .expect("newer task")
        .expect("the newer seed lands");
    assert_eq!(
        report.rows[0].outcome,
        SeedOutcome::Published { superseded: 3 }
    );
    match older.await.expect("older task") {
        Err(StepPluginSeedError::Registry {
            error: StepPluginError::Conflict(_),
            ..
        }) => {}
        other => panic!("the older seed must be refused behind the newer, got {other:?}"),
    }
    assert_eq!(
        repo.get_active(kind).await.expect("get_active").version,
        5,
        "the newer seed's row stays live"
    );
    let versions: Vec<(i32, WorkflowStatus)> = repo
        .list_versions(kind)
        .await
        .expect("list_versions")
        .into_iter()
        .map(|s| (s.version, s.status))
        .collect();
    assert_eq!(
        versions,
        vec![(3, WorkflowStatus::Retired), (5, WorkflowStatus::Active)]
    );
}
