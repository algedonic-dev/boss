//! Postgres coverage for the moves record (design e765b3fc §3, car M1):
//! the migration's table and its once-per-cause key, and the two reads
//! of `audit_log` the mover makes through boss-events.
//!
//! WHAT MUST HOLD: a replayed batch writes nothing twice (idempotence,
//! the unique key on `(cause_event_id, packet)`); the record reads back
//! in its own order; and the mover's log read returns exactly the
//! packet-naming rows of its slice — a step's under `job_id`, a job's
//! under `id` — and nothing else.

use boss_jobs::moves::{Move, MovesStore, PgMoves, mover_version};
use boss_testing::TestDb;
use chrono::{DateTime, Utc};
use uuid::Uuid;

fn t(s: &str) -> DateTime<Utc> {
    DateTime::parse_from_rfc3339(s).unwrap().into()
}

fn moved(
    n: u128,
    packet: &str,
    from: Option<&str>,
    to: Option<&str>,
    declared: Option<bool>,
) -> Move {
    Move {
        dispatch: None,
        at: t("2026-09-26T03:00:00Z") + chrono::Duration::seconds(n as i64),
        packet: packet.into(),
        kind: "pr-train".into(),
        label: "PR train 2026-09-26 03:04".into(),
        from: from.map(str::to_string),
        to: to.map(str::to_string),
        declared,
        cause_event_id: Uuid::from_u128(n),
        cause_seq: n as i64,
        cause_kind: "jobs.job.updated".into(),
        handoff_from: None,
        lineage: Some("boarded_jobs".into()),
        aboard: vec!["car-1".into(), "car-2".into()],
        // An exit names the terminal it left by (backlog e23005c0).
        terminal: to.is_none().then(|| "merge-lost".to_string()),
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn a_replayed_batch_writes_nothing_twice_and_reads_back_in_order() {
    let db = TestDb::new().await;
    let store = PgMoves::new(db.pool.clone());
    assert_eq!(store.latest_seq().await.unwrap(), 0, "an empty record");

    let batch = [
        moved(1, "train-1", Some("dock"), Some("track"), Some(true)),
        moved(2, "train-1", Some("track"), Some("shed"), Some(false)),
        moved(3, "train-2", None, Some("track"), None),
        moved(4, "train-2", Some("track"), None, Some(true)),
    ];
    assert_eq!(store.record(&batch).await.unwrap(), 4);
    assert_eq!(
        store.record(&batch).await.unwrap(),
        0,
        "the same causes, the same packets"
    );

    let back = store.since(0, 10).await.unwrap();
    assert_eq!(back.len(), 4);
    assert!(
        back.windows(2).all(|w| w[0].seq < w[1].seq),
        "the record's own order"
    );
    let moves: Vec<Move> = back.iter().map(|r| r.r#move.clone()).collect();
    assert_eq!(
        moves, batch,
        "every column round-trips, aboard and the terminal included"
    );
    assert_eq!(store.latest_seq().await.unwrap(), back[3].seq);
    assert_eq!(store.since(back[0].seq, 10).await.unwrap().len(), 3);

    // Every route taken in the window, counted, declared or not — onto
    // the map included (a NULL end groups like any other); whether each
    // is declared is judged at read time against the derived routes.
    // An exit is counted per kind and terminal; a move between regions
    // by its route alone.
    let taken = store.crossings(t("2026-09-26T00:00:00Z")).await.unwrap();
    assert_eq!(
        taken
            .iter()
            .map(|r| (
                r.from.as_deref(),
                r.to.as_deref(),
                r.kind.as_deref(),
                r.terminal.as_deref(),
                r.moves
            ))
            .collect::<Vec<_>>(),
        // Off the map first, as the in-memory key orders it (backlog
        // be459ab9's adapters-agree suite holds the two to one order).
        [
            (None, Some("track"), None, None, 1),
            (Some("dock"), Some("track"), None, None, 1),
            (Some("track"), None, Some("pr-train"), Some("merge-lost"), 1),
            (Some("track"), Some("shed"), None, None, 1),
        ]
    );
    assert!(
        store
            .crossings(t("2026-09-26T04:00:00Z"))
            .await
            .unwrap()
            .is_empty(),
        "outside the window"
    );

    // A move to the place it came from is refused by the table itself.
    let nowhere = moved(9, "train-3", Some("dock"), Some("dock"), Some(false));
    assert!(store.record(&[nowhere]).await.is_err());
}

/// A RECORDED MOVE NAMES THE MOVER THAT READ IT (backlog c8d22e63): the
/// adapter writes `mover_version()` into the column and reads it back,
/// and a row written the way every writer before the stamp wrote it —
/// no `mover_version` at all — reads back `None`, stays in the record,
/// and is not counted toward the undeclared band.
#[tokio::test(flavor = "multi_thread")]
async fn a_recorded_move_names_its_mover_and_an_unstamped_row_reads_back_unknown() {
    let db = TestDb::new().await;
    let store = PgMoves::new(db.pool.clone());

    // The pre-stamp writer's own INSERT, column list and all.
    let old = moved(1, "train-1", Some("shed"), Some("arrivals"), Some(false));
    sqlx::query(
        "INSERT INTO yard_moves (at, packet, kind, label, from_region, to_region, \
         declared, cause_event_id, cause_seq, cause_kind, handoff_from, lineage, aboard, terminal) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14)",
    )
    .bind(old.at)
    .bind(&old.packet)
    .bind(&old.kind)
    .bind(&old.label)
    .bind(&old.from)
    .bind(&old.to)
    .bind(old.declared)
    .bind(old.cause_event_id)
    .bind(old.cause_seq)
    .bind(&old.cause_kind)
    .bind(&old.handoff_from)
    .bind(&old.lineage)
    .bind(serde_json::json!(old.aboard))
    .bind(&old.terminal)
    .execute(&db.pool)
    .await
    .unwrap();
    let new = moved(2, "train-2", Some("shed"), Some("arrivals"), Some(false));
    assert_eq!(store.record(std::slice::from_ref(&new)).await.unwrap(), 1);

    let column: Vec<(i64, Option<String>)> =
        sqlx::query_as("SELECT seq, mover_version FROM yard_moves ORDER BY seq")
            .fetch_all(&db.pool)
            .await
            .unwrap();
    assert_eq!(
        column.iter().map(|(_, v)| v.clone()).collect::<Vec<_>>(),
        [None, Some(mover_version())],
        "the column holds what the adapter wrote, and NULL where nothing did"
    );

    let back = store.since(0, 10).await.unwrap();
    assert_eq!(
        back.iter().map(|r| r.r#move.clone()).collect::<Vec<_>>(),
        [old, new],
        "both rows are held"
    );
    assert_eq!(back[0].mover_version, None, "unknown, never guessed");
    assert_eq!(back[1].mover_version, Some(mover_version()));
    // On `/api/yard/moves` the stamped row carries its mover and the
    // unstamped one carries no key at all (its correction-ledger hash
    // is the one it always had).
    let page = serde_json::to_value(&back).unwrap();
    assert!(page[0].get("mover_version").is_none(), "{page}");
    assert_eq!(page[1]["mover_version"], mover_version());

    let taken = store.crossings(t("2026-09-26T00:00:00Z")).await.unwrap();
    assert_eq!(
        taken
            .iter()
            .map(|r| (r.from.as_deref(), r.to.as_deref(), r.moves))
            .collect::<Vec<_>>(),
        [(Some("shed"), Some("arrivals"), 1)],
        "the band counts the current mover's row alone"
    );
}

async fn log(pool: &sqlx::PgPool, kind: &str, payload: serde_json::Value) {
    sqlx::query(
        "INSERT INTO audit_log (event_id, timestamp, source, kind, payload) \
         VALUES ($1, NOW(), 'jobs', $2, $3)",
    )
    .bind(Uuid::new_v4())
    .bind(kind)
    .bind(payload)
    .execute(pool)
    .await
    .unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn the_mover_reads_the_packet_naming_rows_of_its_slice_and_nothing_else() {
    let db = TestDb::new().await;
    let store = PgMoves::new(db.pool.clone());
    let before = store.log_head().await.unwrap();

    log(
        &db.pool,
        "jobs.job.created",
        serde_json::json!({ "id": "car-1" }),
    )
    .await;
    log(
        &db.pool,
        "jobs.step.updated",
        serde_json::json!({ "job_id": "car-1", "id": "step-9" }),
    )
    .await;
    log(
        &db.pool,
        "step.done.task",
        serde_json::json!({ "job_id": "gate-1" }),
    )
    .await;
    // Neither of these names a packet the mover places.
    log(
        &db.pool,
        "jobs.kind.published",
        serde_json::json!({ "id": "wf-1" }),
    )
    .await;
    log(
        &db.pool,
        "people.employee.updated",
        serde_json::json!({ "id": "emp-1" }),
    )
    .await;
    let head = store.log_head().await.unwrap();
    assert_eq!(head - before, 5, "the head is the log's newest id");

    let causes = store.causes(before, head, 100).await.unwrap();
    let named: Vec<(&str, &str)> = causes
        .iter()
        .map(|c| (c.kind.as_str(), c.packet.as_str()))
        .collect();
    assert_eq!(
        named,
        [
            ("jobs.job.created", "car-1"),
            ("jobs.step.updated", "car-1"),
            ("step.done.task", "gate-1"),
        ],
        "a step's event names its job, not the step"
    );
    assert!(causes.windows(2).all(|w| w[0].seq < w[1].seq));
    assert_eq!(
        store.causes(before, head, 1).await.unwrap().len(),
        1,
        "the limit bounds the page"
    );
    assert!(
        store.causes(head, head, 100).await.unwrap().is_empty(),
        "an empty slice"
    );
}
