//! The surface-opens record answers the same on both `SurfaceOpens`
//! adapters — reliability mechanism C of design 3036296f, "adapters
//! agree" (`boss_testing::adapters_agree!`), on the next port in the
//! census order after the sensor registry (backlog be459ab9):
//! `SurfaceOpens`, which `InMemorySurfaceOpens` and `PgSurfaceOpens`
//! implement.
//!
//! WHY THIS PORT. It is the count of which operator surfaces are used —
//! the roll-up the daily usage chore and the page read to say which
//! routes earn their keep (backlog 628f182b). Every door test of that
//! surface (`surface_opens/http.rs`) asks `InMemorySurfaceOpens`;
//! production is answered by `PgSurfaceOpens`. The double calls its
//! `roll_up` "the stated twin of the Pg adapter's GROUP BY / ORDER BY",
//! and the Postgres adapter calls its ORDER BY "the in-memory adapter's
//! sort, stated once there and once here" — a fact that lives twice
//! (CLAUDE.md §9a) with a comment asking the two to agree, and no test.
//!
//! The first run found them disagreeing, fixed in this car in production
//! code: the roll-up promises "actor-ordered, then most-opened first",
//! route breaking a tie, and Postgres sorted `actor_id` and `route` by
//! the database's locale, which ignores `-` at first level and folds
//! case — so `emp-ab` served before `emp-a-z` there and after it in
//! memory (backlog 2987fb2d names `surface_opens (actor_id, route)`
//! among the tables still disagreeing). Both now sort `COLLATE "C"`,
//! byte order, the double's `String::cmp`. Case
//! `the_rollup_is_actor_then_most_opened_then_route_in_byte_order`.
//!
//! THE SHAPE, the other suites': each case states its answer and each
//! adapter is held to that stated answer, not merely to the other one.
//! Instants are whole seconds, so Postgres's microseconds and the
//! double's nanoseconds compare equal. The migrations seed no open, and
//! both adapters start empty.

use boss_jobs::surface_opens::{
    InMemorySurfaceOpens, PgSurfaceOpens, Rollup, RouteCount, SurfaceOpens, SurfaceOpensError,
};
use chrono::{DateTime, TimeZone, Utc};

boss_testing::adapters_agree! {
    adapters {
        in_memory => (InMemorySurfaceOpens::new(), ()),
        postgres => {
            let db = boss_testing::TestDb::new().await;
            (PgSurfaceOpens::new(db.pool.clone()), db)
        },
    }
    cases {
        an_empty_record_rolls_up_to_no_rows,
        a_window_that_holds_nothing_is_refused_not_answered_empty,
        the_rollup_counts_each_actor_and_route_inside_a_half_open_window,
        the_rollup_is_actor_then_most_opened_then_route_in_byte_order,
        the_sweep_deletes_strictly_before_the_cutoff_and_says_how_many,
    }
}

// ----- fixtures ------------------------------------------------------------

fn at(h: u32, m: u32) -> DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 9, 29, h, m, 0).unwrap()
}

async fn open<S: SurfaceOpens>(s: &S, actor: &str, route: &str, when: DateTime<Utc>) {
    s.record(actor, route, when).await.expect("record answers");
}

fn count(actor: &str, route: &str, opens: i64, last_at: DateTime<Utc>) -> RouteCount {
    RouteCount {
        actor_id: actor.into(),
        route: route.into(),
        opens,
        last_at,
    }
}

async fn rollup<S: SurfaceOpens>(s: &S, since: DateTime<Utc>, until: DateTime<Utc>) -> Rollup {
    s.rollup(since, until).await.expect("rollup answers")
}

// ----- cases ---------------------------------------------------------------

/// Nothing recorded: the roll-up over a real window is no rows, carrying
/// its window — an answer, never an error — and a sweep deletes none.
async fn an_empty_record_rolls_up_to_no_rows<S: SurfaceOpens>(s: &S, adapter: &str) {
    assert_eq!(
        rollup(s, at(0, 0), at(23, 0)).await,
        Rollup {
            since: at(0, 0),
            until: at(23, 0),
            rows: vec![],
        },
        "{adapter}"
    );
    assert_eq!(s.sweep(at(23, 0)).await.expect("sweep"), 0, "{adapter}");
}

/// `since >= until` is a bad request on both, not an empty answer that
/// would read as a quiet day — an equal pair and a reversed one.
async fn a_window_that_holds_nothing_is_refused_not_answered_empty<S: SurfaceOpens>(
    s: &S,
    adapter: &str,
) {
    open(s, "emp-a", "/it", at(10, 0)).await;
    for (since, until) in [(at(10, 0), at(10, 0)), (at(11, 0), at(9, 0))] {
        let err = s.rollup(since, until).await.expect_err("an empty window");
        assert!(
            matches!(err, SurfaceOpensError::BadRequest(_)),
            "{adapter}: [{since}, {until}): {err}"
        );
    }
}

/// One row per (actor, route) opened inside `[since, until)` — the start
/// instant in, the end instant out — with its count and its LAST open,
/// whatever order the opens were recorded in.
async fn the_rollup_counts_each_actor_and_route_inside_a_half_open_window<S: SurfaceOpens>(
    s: &S,
    adapter: &str,
) {
    for (actor, route, when) in [
        ("emp-a", "/it", at(9, 30)),
        ("emp-a", "/it", at(8, 0)),
        ("emp-a", "/it", at(9, 0)),
        ("emp-a", "/it/codebase", at(8, 30)),
        ("emp-b", "/it", at(9, 45)),
        ("emp-a", "/it", at(7, 59)),
        ("emp-b", "/it", at(10, 0)),
    ] {
        open(s, actor, route, when).await;
    }
    let r = rollup(s, at(8, 0), at(10, 0)).await;
    assert_eq!(
        r.rows,
        vec![
            count("emp-a", "/it", 3, at(9, 30)),
            count("emp-a", "/it/codebase", 1, at(8, 30)),
            count("emp-b", "/it", 1, at(9, 45)),
        ],
        "{adapter}: 07:59 and 10:00 are outside a window [08:00, 10:00)"
    );
    assert_eq!(r.opens(), 5, "{adapter}");
}

/// The roll-up's order: actor, then most-opened first, then route — the
/// text in BYTE order (`-` before a letter, upper before lower), the only
/// order both adapters can hold. A locale order would answer two lists.
async fn the_rollup_is_actor_then_most_opened_then_route_in_byte_order<S: SurfaceOpens>(
    s: &S,
    adapter: &str,
) {
    for (actor, route, n) in [
        ("emp-b", "/it", 1),
        ("emp-ab", "/it", 1),
        ("emp-a-z", "/it/ab", 1),
        ("emp-a-z", "/it/a-z", 1),
        ("emp-a-z", "/it/B", 1),
        ("emp-a-z", "/it/zz", 2),
    ] {
        for i in 0..n {
            open(s, actor, route, at(9, i)).await;
        }
    }
    let rows: Vec<(String, String, i64)> = rollup(s, at(0, 0), at(23, 0))
        .await
        .rows
        .into_iter()
        .map(|r| (r.actor_id, r.route, r.opens))
        .collect();
    let want: Vec<(String, String, i64)> = [
        ("emp-a-z", "/it/zz", 2),
        ("emp-a-z", "/it/B", 1),
        ("emp-a-z", "/it/a-z", 1),
        ("emp-a-z", "/it/ab", 1),
        ("emp-ab", "/it", 1),
        ("emp-b", "/it", 1),
    ]
    .into_iter()
    .map(|(a, r, n)| (a.to_string(), r.to_string(), n))
    .collect();
    assert_eq!(rows, want, "{adapter}: every actor and route byte for byte");
}

/// The sweep deletes every open strictly BEFORE the cutoff — the cutoff
/// instant is kept — and answers how many; what stays still rolls up.
async fn the_sweep_deletes_strictly_before_the_cutoff_and_says_how_many<S: SurfaceOpens>(
    s: &S,
    adapter: &str,
) {
    for when in [at(7, 0), at(7, 59), at(8, 0), at(9, 0)] {
        open(s, "emp-a", "/it", when).await;
    }
    assert_eq!(s.sweep(at(8, 0)).await.expect("sweep"), 2, "{adapter}");
    assert_eq!(
        rollup(s, at(0, 0), at(23, 0)).await.rows,
        vec![count("emp-a", "/it", 2, at(9, 0))],
        "{adapter}: 08:00 stays"
    );
    assert_eq!(
        s.sweep(at(8, 0)).await.expect("sweep again"),
        0,
        "{adapter}"
    );
}
