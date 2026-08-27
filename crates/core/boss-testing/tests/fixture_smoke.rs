//! The shared-fixture smoke: does the test harness itself still stand up?
//!
//! WHY THIS EXISTS, AND WHY IT RUNS FIRST. Measured across the forge's
//! whole CI history on 2026-08-15 (106 runs, 36 trains): 79% of train
//! reds surfaced only in `test` — 23 of 29 stage failures, against 5 in
//! `fast` and 1 in `locomotive`. A train that goes green first time
//! reaches main in 0.54h; one that reds takes 1.91h.
//!
//! The expensive reds were not a crate's own logic failing. They were
//! the SHARED fixture failing, which reds every DB-backed crate at once:
//!
//!   * a registry migration that INSERTed the new active row BEFORE
//!     retiring the old one, against a plain partial unique index — so
//!     the schema load died and every DB-backed test in the workspace
//!     aborted, nowhere near the migration;
//!   * a `TestDb` guard that refused to run against a server hosting a
//!     database named `boss` — which CI's own Postgres service is
//!     created as — so all 11 `boss-accounts` tests panicked at the
//!     same line of the harness.
//!
//! Neither is visible to `infra/gate.sh -p <crate>`, the per-crate gate
//! an agent runs before pushing a car. That gate answers "did I break
//! my crate". These break everyone's fixture, and the blast radius is
//! the whole consist: today's trains carry 12-13 cars, so one fixture
//! defect blocks a dozen unrelated changes for a full CI cycle.
//!
//! So this test builds the fixture once and asserts it is real, and
//! `infra/gate.sh` runs it FIRST — in car mode (`-p <crate>`) as well as
//! the full run. That placement is the point. Detecting a fixture break
//! sooner in CI would still be detecting it on a train, after it has
//! already blocked a dozen cars; running it unscoped in the gate an
//! agent runs before pushing means the break is named to the person who
//! caused it, before the car is ever parked.
//!
//! It deliberately does NOT live as its own step in the CI workflow:
//! `gate_sh.rs` pins the test job against inlining checks beside
//! `infra/gate.sh`, because the gate having two definitions is its own
//! recurring bug class (CLAUDE.md §9a). One definition, run everywhere.
//!
//! Keep it cheap and keep it about the FIXTURE. Anything that tests a
//! crate's behaviour belongs in that crate.

/// The whole shared fixture, end to end: the harness agrees to run, the
/// schema directory loads in order, and the tables the rest of the suite
/// assumes are actually there.
#[tokio::test(flavor = "multi_thread")]
async fn the_shared_fixture_stands_up() {
    // `TestDb::new` panics if the harness refuses the server, or if any
    // schema file fails to apply — so arriving at the next line already
    // covers both of the failures above.
    let db = boss_testing::TestDb::new().await;

    // What that does NOT cover is a schema that loads but is empty: the
    // file list is generated from the schema directory, and a filter or
    // ordering bug there would apply nothing and raise no error. Name a
    // few core tables so that stays loud. These are the four primitives
    // plus the registry every tenant reads.
    for table in ["audit_log", "jobs", "steps", "workflows", "classes"] {
        let exists: bool = sqlx::query_scalar(
            "SELECT EXISTS (SELECT 1 FROM information_schema.tables \
             WHERE table_schema = 'public' AND table_name = $1)",
        )
        .bind(table)
        .fetch_one(&db.pool)
        .await
        .unwrap_or_else(|e| panic!("probing for table `{table}`: {e}"));

        assert!(
            exists,
            "core table `{table}` is missing from a freshly loaded schema — \
             the schema file list applied without error but did not create it"
        );
    }
}
