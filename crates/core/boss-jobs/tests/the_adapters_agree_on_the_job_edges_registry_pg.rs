//! The job-edges registry answers the same on both `JobEdgesRegistry`
//! adapters — reliability mechanism C of design 3036296f, "adapters
//! agree" (`boss_testing::adapters_agree!`), on the ninth port it
//! reaches (backlog be459ab9), beside the station registry: both are
//! routing, and this one decides which metadata fields the write path
//! ref-checks as links to other packets.
//!
//! WHY IT EXISTS. The port is read-only and its rows are born in
//! migrations, so the two adapters are two COPIES of one roster:
//! `PgJobEdges` reads the `job_edges` table the migrations seed, and
//! `InMemoryJobEdges` hardcodes a list "kept in deliberate agreement"
//! with them. Until this file the agreement was pinned edge by edge —
//! six `*_matches_the_migration_that_seeds_it` tests in
//! `job_edges::tests`, each reading one migration — and the whole
//! roster was stated for Postgres alone (`job_edges_pg.rs`). This case
//! states the whole roster once, every field of every row, and holds
//! both adapters to it: an edge added to one copy and not the other, a
//! description reworded in one, or a dial left `warn`, fails here
//! naming the row. It replaces the Postgres-only roster test, whose
//! reasons ride on the rows below.
//!
//! Its first run found one disagreement, fixed in this car: the double
//! described `waiting_on` with the first sentence of migration 110's
//! description only, dropping "Boards render the wait; the dispatcher
//! clears it when the blocker closes." — the half that says who clears
//! the wait. None of the six edge-by-edge pins covered that edge.
//!
//! What is NOT compared: the ORDER of the listing. The port promises
//! none (`list` says nothing of it); Postgres happens to sort by
//! `(source_kind, field_path)` and the double answers in declaration
//! order, and every consumer resolves fields by lookup. So each answer
//! is sorted here before it is compared, and the case says so rather
//! than inventing a promise a caller could come to lean on.

use boss_jobs::job_edges::{InMemoryJobEdges, JobEdgeSpec, JobEdgesRegistry, PgJobEdges};

boss_testing::adapters_agree! {
    adapters {
        in_memory => (InMemoryJobEdges, ()),
        postgres => {
            let db = boss_testing::TestDb::new().await;
            (PgJobEdges::new(db.pool.clone()), db)
        },
    }
    cases {
        every_declared_edge_is_listed_whole_and_aborts,
    }
}

fn edge(source_kind: &str, field_path: &str, field_kind: &str, description: &str) -> JobEdgeSpec {
    JobEdgeSpec {
        source_kind: source_kind.into(),
        field_path: field_path.into(),
        field_kind: field_kind.into(),
        // EVERY declared edge aborts on an unresolvable reference. The
        // fifth edge once landed `warn` while every other row said
        // `abort` and the double hardcoded `abort` for all of them: the
        // column DEFAULT was `warn` and 105 was a one-time UPDATE of the
        // rows existing then. 202608291630 made the DEFAULT `abort`;
        // this is what catches it being weakened again.
        on_missing: "abort".into(),
        description: description.into(),
    }
}

/// The declared roster, sorted by `(source_kind, field_path)`.
fn declared() -> Vec<JobEdgeSpec> {
    vec![
        // '*' applies to every kind. `waiting_on` (migration 110) is the
        // BLOCKING edge — a wait the dispatcher clears on close. The
        // three RELATION edges (design c0d2787a) carry no behaviour:
        // they record that two packets are related, so the fact is
        // resolvable and queryable instead of living in whatever key the
        // author reached for; '*' because a relationship is not a
        // property of a kind. There is deliberately no "prerequisite"
        // relation — `waiting_on` already is one.
        edge(
            "*",
            "duplicate_of",
            "job_id",
            "The packet this one restates; the duplicate is the one that closes",
        ),
        edge(
            "*",
            "occasioned_by",
            "job_id",
            "The packet whose work brought this one into being — provenance only; it gates nothing",
        ),
        edge(
            "*",
            "supersedes",
            "job_id",
            "The packet this one replaces, whose conclusion no longer holds",
        ),
        edge(
            "*",
            "waiting_on",
            "job_id",
            "The Job whose closure this Job waits on. Boards render the wait; the dispatcher clears it when the blocker closes.",
        ),
        // The feedback or backlog item a design decides (5f0b2661) —
        // followed on publish by
        // complete-feedback-design-review-on-design-doc-published.
        edge(
            "design-doc",
            "answers",
            "job_id",
            "The user-feedback or backlog-item this design decides — publishing the design completes the design-review step of that packet",
        ),
        // A design doc's revision chain (87f5bc84 Q5).
        edge(
            "design-doc",
            "translated_from",
            "job_id",
            "The design-doc packet this one revises — the previous link in the chain",
        ),
        // The gate's own park intent (89faab68), undeclared until the
        // census COUNTED it. On `gate-run`, not '*': one verb writes it
        // onto one kind.
        edge(
            "gate-run",
            "park_backlog_item",
            "job_id",
            "The backlog/feedback Job the car this gate parks will answer",
        ),
        edge(
            "pr-train",
            "boarded_jobs",
            "job_id_list",
            "The ship-a-change passengers this train carried",
        ),
        // Every OTHER item a car answers (a994f533) — a list, each
        // element ref-checked, followed on merge like `backlog_item`.
        edge(
            "ship-a-change",
            "also_answers",
            "job_id_list",
            "Every other backlog/feedback Job this change answers — each closes on merge, as backlog_item does",
        ),
        edge(
            "ship-a-change",
            "backlog_item",
            "job_id",
            "The backlog/feedback Job this change answers",
        ),
        // The car this car must land BEHIND (d3320278).
        edge(
            "ship-a-change",
            "boards_after",
            "job_id",
            "The car this one must land behind — the dock will not board it until that car has landed",
        ),
        // An item a car is ONE PIECE of (e1325456): declared so the value
        // is ref-checked, and followed by no rule, so it records
        // provenance without closing the item.
        edge(
            "ship-a-change",
            "partial_item",
            "job_id",
            "An item this change is ONE PIECE of — provenance only; it does not close on merge",
        ),
        edge(
            "ship-a-change",
            "train",
            "job_id",
            "The pr-train Job this change boarded",
        ),
    ]
}

/// Both adapters list exactly the declared roster — every field of
/// every row, nothing more, nothing less — compared in a fixed order
/// the port does not promise (see the file header).
async fn every_declared_edge_is_listed_whole_and_aborts<R: JobEdgesRegistry>(
    registry: &R,
    adapter: &str,
) {
    let mut listed = registry.list().await.expect("list answers");
    listed.sort_by(|a, b| {
        (a.source_kind.as_str(), a.field_path.as_str())
            .cmp(&(b.source_kind.as_str(), b.field_path.as_str()))
    });
    let want = declared();
    let missing: Vec<_> = want.iter().filter(|e| !listed.contains(e)).collect();
    let extra: Vec<_> = listed.iter().filter(|e| !want.contains(e)).collect();
    assert!(
        missing.is_empty() && extra.is_empty(),
        "{adapter}: declared but not listed as declared: {missing:#?}\n\
         listed but not declared: {extra:#?}"
    );
    assert_eq!(listed, want, "{adapter}: one row per edge");
}
