//! The moves record answers the same on both `MovesStore` adapters —
//! reliability mechanism C of design 3036296f, "adapters agree"
//! (`boss_testing::adapters_agree!`), on the next port in the census
//! order after the sensor registry and the surface-opens record (backlog
//! be459ab9): `MovesStore`, which `InMemoryMoves` and `PgMoves`
//! implement.
//!
//! WHY THIS PORT. It is the yard's record of every move a packet made on
//! the IT map (design e765b3fc car M1): the mover loop reads the audit
//! log through it and writes a row per move, and the regions read counts
//! the routes taken through it to say which ones no protocol declares.
//! Every unit test of that logic (`moves.rs`) asks `InMemoryMoves`;
//! production is answered by `PgMoves`.
//!
//! THE FIRST RUN FOUND TWO DISAGREEMENTS, fixed in this car in
//! production code:
//!
//! - `crossings` ordered its routes by `ORDER BY 1, 2, 3, 4` in Postgres
//!   — NULL (off the map) LAST, and region names in the database's
//!   locale, which ignores `-` at first level and folds case — while the
//!   in-memory adapter's `BTreeMap` key orders `None` FIRST and strings
//!   by byte. The order is observable: `with_undeclared` lists a
//!   troubled region's undeclared routes in it, so the same record read
//!   two ways named its routes in two orders. The one order is the
//!   in-memory one, because it is the `Ord` Rust derives for the key
//!   (`Option<String>` × 4) — stated once, by the type, with no code to
//!   drift — and Postgres can say it in one clause per column
//!   (`COLLATE "C" NULLS FIRST`), the fix backlog 2987fb2d named for
//!   the other tables. Case
//!   `crossings_are_off_the_map_first_then_byte_order`.
//! - A NEGATIVE `limit` was refused by Postgres (`LIMIT must not be
//!   negative`) and answered as an empty page by the double, which read
//!   it through `usize::try_from(..).unwrap_or(0)`. An empty page is a
//!   confident wrong answer to a caller bug; both now refuse. Case
//!   `a_negative_limit_is_refused_not_answered_empty`.
//!
//! THE AUDIT LOG HALF, and why its cases are relative. `log_head` and
//! `causes` read `audit_log`, whose `id` and `created_at` Postgres
//! assigns and which the migrations seed with rows of their own. So each
//! case reads the head FIRST and states everything after it: the head
//! advances by one per event appended, a slice is named by the seqs the
//! adapter itself answered (never by a guessed id), and an event's
//! instant is held between two wall readings taken around its append.
//! The double holds only the events that name a packet (`push_event`
//! takes a `Cause`), so the suite appends only such events on both — the
//! Postgres filter that drops the rest is `yard_moves_pg.rs`'s case, and
//! a double of `audit_log` is not what this port's in-memory adapter is.
//!
//! THE RECORD HALF'S SEQ is `BIGSERIAL` in Postgres, and a sequence
//! advances on a skipped `ON CONFLICT` and on a rolled-back insert, where
//! the double's does not. The port promises only the record's ORDER, so
//! the cases hold seqs to strictly increasing and never to a value.
//!
//! THE SHAPE, the other suites': each case states its answer and each
//! adapter is held to that stated answer, not merely to the other one.
//! Instants are whole seconds, so Postgres's microseconds and the
//! double's nanoseconds compare equal.

use async_trait::async_trait;
use boss_jobs::moves::{Cause, InMemoryMoves, Move, MovesStore, PgMoves, Recorded, RouteCount};
use chrono::{DateTime, Duration, TimeZone, Utc};
use sqlx::PgPool;
use uuid::Uuid;

boss_testing::adapters_agree! {
    adapters {
        in_memory => (InMemoryMoves::new(), ()),
        postgres => {
            let db = boss_testing::TestDb::new().await;
            (
                PgLog {
                    moves: PgMoves::new(db.pool.clone()),
                    pool: db.pool.clone(),
                },
                db,
            )
        },
    }
    cases {
        an_empty_record_has_no_moves_and_no_seq,
        the_log_head_advances_by_one_per_event_appended,
        causes_are_the_half_open_slice_oldest_first_bounded_by_the_limit,
        a_negative_limit_is_refused_not_answered_empty,
        a_replayed_batch_writes_nothing_twice_and_reads_back_in_order,
        a_duplicate_inside_one_batch_is_recorded_once,
        a_move_that_goes_nowhere_refuses_its_whole_batch,
        crossings_count_each_route_in_the_window_and_an_exit_by_its_off_ramp,
        crossings_are_off_the_map_first_then_byte_order,
    }
}

// ----- the log each adapter reads --------------------------------------------

/// An adapter the suite can append an audit event to: the double's
/// `push_event`, or a row in the Postgres `audit_log` the adapter reads.
#[async_trait]
trait Logged: Send + Sync {
    fn moves(&self) -> &dyn MovesStore;
    /// Append one event naming `packet` — a job event names it as `id`,
    /// a step event as `job_id` — and answer its `event_id`.
    async fn append(&self, kind: &str, packet: &str) -> Uuid;
}

#[async_trait]
impl Logged for InMemoryMoves {
    fn moves(&self) -> &dyn MovesStore {
        self
    }

    async fn append(&self, kind: &str, packet: &str) -> Uuid {
        let event_id = Uuid::new_v4();
        let seq = self.log_head().await.expect("head answers") + 1;
        self.push_event(Cause {
            seq,
            event_id,
            at: Utc::now(),
            kind: kind.into(),
            packet: packet.into(),
        });
        event_id
    }
}

struct PgLog {
    moves: PgMoves,
    pool: PgPool,
}

#[async_trait]
impl Logged for PgLog {
    fn moves(&self) -> &dyn MovesStore {
        &self.moves
    }

    async fn append(&self, kind: &str, packet: &str) -> Uuid {
        let event_id = Uuid::new_v4();
        let payload = if kind.starts_with("jobs.job.") {
            serde_json::json!({ "id": packet })
        } else {
            serde_json::json!({ "job_id": packet, "id": "step-1" })
        };
        sqlx::query(
            "INSERT INTO audit_log (event_id, timestamp, source, kind, payload) \
             VALUES ($1, NOW(), 'jobs', $2, $3)",
        )
        .bind(event_id)
        .bind(kind)
        .bind(payload)
        .execute(&self.pool)
        .await
        .expect("the audit log takes the row");
        event_id
    }
}

// ----- fixtures --------------------------------------------------------------

fn at(m: u32, s: u32) -> DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 9, 30, 3, m, s).unwrap()
}

fn moved(n: u128, packet: &str, from: Option<&str>, to: Option<&str>) -> Move {
    Move {
        at: at(0, n as u32),
        packet: packet.into(),
        kind: "pr-train".into(),
        label: "PR train 2026-09-30 03:00".into(),
        from: from.map(str::to_string),
        to: to.map(str::to_string),
        declared: Some(n.is_multiple_of(2)),
        cause_event_id: Uuid::from_u128(n),
        cause_seq: n as i64,
        cause_kind: "jobs.job.updated".into(),
        handoff_from: (n == 1).then(|| "gate-1".to_string()),
        lineage: Some("boarded_jobs".into()),
        aboard: vec!["car-1".into(), "car-2".into()],
        terminal: to.is_none().then(|| "merge-lost".to_string()),
    }
}

fn rows_of(back: &[Recorded]) -> Vec<Move> {
    back.iter().map(|r| r.r#move.clone()).collect()
}

fn increasing(back: &[Recorded]) -> bool {
    back.windows(2).all(|w| w[0].seq < w[1].seq)
}

/// A route as `crossings` answers it: its two ends, and on an exit the
/// kind and terminal it left by.
fn route(
    ends: (Option<&str>, Option<&str>),
    by: (Option<&str>, Option<&str>),
    moves: i64,
    last_at: DateTime<Utc>,
) -> RouteCount {
    let own = |s: Option<&str>| s.map(str::to_string);
    RouteCount {
        from: own(ends.0),
        to: own(ends.1),
        kind: own(by.0),
        terminal: own(by.1),
        moves,
        last_at,
    }
}

// ----- cases -----------------------------------------------------------------

async fn an_empty_record_has_no_moves_and_no_seq<L: Logged>(l: &L, adapter: &str) {
    let m = l.moves();
    assert_eq!(m.latest_seq().await.unwrap(), 0, "{adapter}: no seq");
    assert!(
        m.since(0, 10).await.unwrap().is_empty(),
        "{adapter}: no rows"
    );
    assert!(
        m.crossings(at(0, 0) - Duration::days(3650))
            .await
            .unwrap()
            .is_empty(),
        "{adapter}: no routes"
    );
}

async fn the_log_head_advances_by_one_per_event_appended<L: Logged>(l: &L, adapter: &str) {
    let before = l.moves().log_head().await.unwrap();
    assert!(before >= 0, "{adapter}: a head is never negative");
    l.append("jobs.job.created", "car-1").await;
    l.append("jobs.step.updated", "car-1").await;
    l.append("step.done.task", "gate-1").await;
    assert_eq!(
        l.moves().log_head().await.unwrap() - before,
        3,
        "{adapter}: the head is the newest event"
    );
}

async fn causes_are_the_half_open_slice_oldest_first_bounded_by_the_limit<L: Logged>(
    l: &L,
    adapter: &str,
) {
    let m = l.moves();
    let before = m.log_head().await.unwrap();
    // Whole seconds below the first append and above the last, so the
    // trigger's microseconds and the double's nanoseconds both fall
    // inside.
    let floor = Utc
        .timestamp_opt(Utc::now().timestamp() - 5, 0)
        .single()
        .unwrap();
    let appended = [
        ("jobs.job.created", "car-1"),
        ("jobs.step.updated", "car-1"),
        ("step.done.task", "gate-1"),
        ("jobs.job.updated", "train-1"),
    ];
    let mut ids = Vec::new();
    for (kind, packet) in appended {
        ids.push(l.append(kind, packet).await);
    }
    let ceiling = Utc::now() + Duration::seconds(5);
    let head = m.log_head().await.unwrap();

    let all = m.causes(before, head, 100).await.unwrap();
    assert_eq!(
        all.iter()
            .map(|c| (c.kind.as_str(), c.packet.as_str(), c.event_id))
            .collect::<Vec<_>>(),
        appended
            .iter()
            .zip(&ids)
            .map(|((k, p), id)| (*k, *p, *id))
            .collect::<Vec<_>>(),
        "{adapter}: every event of the slice, oldest first, naming its packet"
    );
    assert!(
        all.windows(2).all(|w| w[0].seq < w[1].seq),
        "{adapter}: the log's own order"
    );
    assert!(
        all.iter().all(|c| c.seq > before && c.seq <= head),
        "{adapter}: inside the slice"
    );
    assert!(
        all.iter().all(|c| c.at >= floor && c.at <= ceiling),
        "{adapter}: each instant is when it was written, {floor}..{ceiling}: {:?}",
        all.iter().map(|c| c.at).collect::<Vec<_>>()
    );

    // `after` is exclusive and `upto` inclusive, named by the seqs the
    // adapter itself answered.
    assert_eq!(
        m.causes(all[0].seq, all[2].seq, 100).await.unwrap(),
        all[1..3].to_vec(),
        "{adapter}: (after, upto]"
    );
    assert_eq!(
        m.causes(before, head, 2).await.unwrap(),
        all[..2].to_vec(),
        "{adapter}: the limit keeps the OLDEST"
    );
    assert!(
        m.causes(before, head, 0).await.unwrap().is_empty(),
        "{adapter}: a zero limit"
    );
    assert!(
        m.causes(head, head, 100).await.unwrap().is_empty(),
        "{adapter}: an empty slice"
    );
}

async fn a_negative_limit_is_refused_not_answered_empty<L: Logged>(l: &L, adapter: &str) {
    let m = l.moves();
    l.append("jobs.job.created", "car-1").await;
    m.record(&[moved(1, "train-1", Some("dock"), Some("track"))])
        .await
        .unwrap();
    let head = m.log_head().await.unwrap();
    assert!(
        m.causes(0, head, -1).await.is_err(),
        "{adapter}: a negative page of the log is a caller's bug, not an empty page"
    );
    assert!(
        m.since(0, -1).await.is_err(),
        "{adapter}: a negative page of the record is a caller's bug, not an empty page"
    );
}

async fn a_replayed_batch_writes_nothing_twice_and_reads_back_in_order<L: Logged>(
    l: &L,
    adapter: &str,
) {
    let m = l.moves();
    let batch = [
        moved(1, "train-1", Some("dock"), Some("track")),
        moved(2, "train-1", Some("track"), Some("shed")),
        moved(3, "train-2", None, Some("track")),
        moved(4, "train-2", Some("track"), None),
    ];
    assert_eq!(m.record(&batch).await.unwrap(), 4, "{adapter}: four new");
    assert_eq!(
        m.record(&batch).await.unwrap(),
        0,
        "{adapter}: the same causes, the same packets"
    );

    let back = m.since(0, 10).await.unwrap();
    assert_eq!(
        rows_of(&back),
        batch,
        "{adapter}: every column round-trips, in the order recorded"
    );
    assert!(increasing(&back), "{adapter}: the record's own order");
    assert_eq!(
        m.latest_seq().await.unwrap(),
        back[3].seq,
        "{adapter}: the newest seq"
    );
    assert_eq!(
        rows_of(&m.since(back[0].seq, 10).await.unwrap()),
        batch[1..],
        "{adapter}: after a seq, exclusive"
    );
    assert_eq!(
        rows_of(&m.since(0, 2).await.unwrap()),
        batch[..2],
        "{adapter}: the limit keeps the OLDEST"
    );
    assert!(
        m.since(back[3].seq, 10).await.unwrap().is_empty(),
        "{adapter}: nothing after the newest"
    );
}

async fn a_duplicate_inside_one_batch_is_recorded_once<L: Logged>(l: &L, adapter: &str) {
    let m = l.moves();
    let once = moved(1, "train-1", Some("dock"), Some("track"));
    // The same cause moving a second packet is a second move.
    let other = Move {
        packet: "car-1".into(),
        ..once.clone()
    };
    assert_eq!(
        m.record(&[once.clone(), once.clone(), other.clone()])
            .await
            .unwrap(),
        2,
        "{adapter}: once per cause and packet"
    );
    assert_eq!(
        rows_of(&m.since(0, 10).await.unwrap()),
        [once, other],
        "{adapter}"
    );
}

async fn a_move_that_goes_nowhere_refuses_its_whole_batch<L: Logged>(l: &L, adapter: &str) {
    let m = l.moves();
    let good = moved(1, "train-1", Some("dock"), Some("track"));
    for nowhere in [
        moved(2, "train-2", Some("dock"), Some("dock")),
        moved(3, "train-3", None, None),
    ] {
        assert!(
            m.record(&[good.clone(), nowhere.clone()]).await.is_err(),
            "{adapter}: {:?} -> {:?} is refused",
            nowhere.from,
            nowhere.to
        );
    }
    assert!(
        m.since(0, 10).await.unwrap().is_empty(),
        "{adapter}: a refused batch writes none of its moves"
    );
    assert_eq!(m.latest_seq().await.unwrap(), 0, "{adapter}");
}

async fn crossings_count_each_route_in_the_window_and_an_exit_by_its_off_ramp<L: Logged>(
    l: &L,
    adapter: &str,
) {
    let m = l.moves();
    let exit_without_a_kind = Move {
        kind: String::new(),
        ..moved(6, "train-4", Some("shed"), None)
    };
    m.record(&[
        // Before the window: not counted.
        moved(1, "train-0", Some("dock"), Some("track")),
        // The window opens AT second 2, inclusive.
        moved(2, "train-1", Some("dock"), Some("track")),
        moved(3, "train-2", Some("dock"), Some("track")),
        // An exit is counted per kind and terminal; two terminals of
        // the same route are two rows.
        moved(4, "train-1", Some("track"), None),
        Move {
            terminal: Some("merged".into()),
            ..moved(5, "train-2", Some("track"), None)
        },
        // An exit whose kind is empty counts under no kind.
        exit_without_a_kind,
        // A route onto the map, and a move between regions carries no
        // terminal even when the row does.
        Move {
            terminal: Some("stray".into()),
            ..moved(7, "train-5", None, Some("track"))
        },
    ])
    .await
    .unwrap();

    // Compared as a set: the ORDER is the next case's.
    let mut taken = m.crossings(at(0, 2)).await.unwrap();
    taken.sort_by(|a, b| {
        (&a.from, &a.to, &a.kind, &a.terminal).cmp(&(&b.from, &b.to, &b.kind, &b.terminal))
    });
    assert_eq!(
        taken,
        [
            route((None, Some("track")), (None, None), 1, at(0, 7)),
            route((Some("dock"), Some("track")), (None, None), 2, at(0, 3)),
            route(
                (Some("shed"), None),
                (None, Some("merge-lost")),
                1,
                at(0, 6)
            ),
            route(
                (Some("track"), None),
                (Some("pr-train"), Some("merge-lost")),
                1,
                at(0, 4)
            ),
            route(
                (Some("track"), None),
                (Some("pr-train"), Some("merged")),
                1,
                at(0, 5)
            ),
        ],
        "{adapter}: each route, how many took it, and the newest instant"
    );
    assert!(
        m.crossings(at(0, 8)).await.unwrap().is_empty(),
        "{adapter}: after the window"
    );
}

async fn crossings_are_off_the_map_first_then_byte_order<L: Logged>(l: &L, adapter: &str) {
    let m = l.moves();
    // Each pair below sorts one way by byte and the other in a locale
    // that ignores `-` and folds case (backlog 2987fb2d): `gates-a-z`
    // before `gates-ab` by byte, after it by locale; `Shed` before
    // `dock` by byte, after it by locale.
    m.record(&[
        moved(1, "p1", Some("gates-ab"), Some("track")),
        moved(2, "p2", Some("gates-a-z"), Some("track")),
        moved(3, "p3", Some("dock"), Some("Shed")),
        moved(4, "p4", Some("Shed"), Some("dock")),
        moved(5, "p5", None, Some("dock")),
        moved(6, "p6", Some("dock"), None),
    ])
    .await
    .unwrap();
    let taken = m.crossings(at(0, 0)).await.unwrap();
    assert_eq!(
        taken
            .iter()
            .map(|c| (c.from.as_deref(), c.to.as_deref()))
            .collect::<Vec<_>>(),
        [
            (None, Some("dock")),
            (Some("Shed"), Some("dock")),
            (Some("dock"), None),
            (Some("dock"), Some("Shed")),
            (Some("gates-a-z"), Some("track")),
            (Some("gates-ab"), Some("track")),
        ],
        "{adapter}: off the map first, then byte order — the Ord of the key"
    );
}
