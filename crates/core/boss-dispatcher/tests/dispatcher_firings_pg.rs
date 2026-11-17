//! Postgres coverage for the WRITE half of `dispatcher_firings`
//! (backlog b14afc48): the sink the rules runner hands its fired rules
//! to, against the real table.
//!
//! The two properties worth a database to check: the row lands with the
//! columns the reader selects, and a redelivered event does NOT count
//! twice. The second is the whole reason the id is deterministic — the
//! transport may present one event eight times, and "how often did this
//! rule fire" must answer about EVENTS, not deliveries.

use boss_dispatcher::rules::firings::{
    Firing, FiringSink, Outcome, PgFirings, dead_letter_id, firing_id,
};
use boss_testing::TestDb;
use chrono::{TimeZone, Utc};
use sqlx::Row;

fn firing(rule: &str, event: &str, fired_at: chrono::DateTime<Utc>) -> Firing {
    Firing {
        firing_id: firing_id(rule, event),
        rule: rule.to_string(),
        fired_on: "jobs.gate.green".to_string(),
        fired_at,
        detail: serde_json::json!({ "event_id": event, "simulated": false }),
        outcome: Outcome::Fired,
    }
}

/// Count actual SQL statements at the adapter boundary, not calls to a
/// fake port. Per-statement delay models a slow database round trip;
/// it does not establish the cause of the production backlog.
#[tokio::test(flavor = "multi_thread")]
async fn one_event_records_all_its_rules_in_one_insert() {
    let db = TestDb::new().await;
    sqlx::raw_sql(
        "CREATE TABLE firing_insert_calls (n integer NOT NULL); \
         INSERT INTO firing_insert_calls VALUES (0); \
         CREATE FUNCTION count_firing_insert() RETURNS trigger LANGUAGE plpgsql AS $$ \
         BEGIN UPDATE firing_insert_calls SET n = n + 1; \
         PERFORM pg_sleep(0.02); RETURN NULL; END $$; \
         CREATE TRIGGER count_firing_insert AFTER INSERT ON dispatcher_firings \
         FOR EACH STATEMENT EXECUTE FUNCTION count_firing_insert();",
    )
    .execute(&db.pool)
    .await
    .unwrap();
    let sink = PgFirings::new(db.pool.clone());
    let at = Utc::now();
    let rows = vec![
        firing("r1", "evt", at),
        firing("r2", "evt", at),
        firing("r3", "evt", at),
    ];
    let start = std::time::Instant::now();
    sink.record(&rows).await.unwrap();
    let elapsed = start.elapsed();
    let calls: i32 = sqlx::query_scalar("SELECT n FROM firing_insert_calls")
        .fetch_one(&db.pool)
        .await
        .unwrap();
    let count: i64 =
        sqlx::query_scalar("SELECT count(*) FROM dispatcher_firings WHERE firing_id = ANY($1)")
            .bind(rows.iter().map(|r| r.firing_id.clone()).collect::<Vec<_>>())
            .fetch_one(&db.pool)
            .await
            .unwrap();
    println!(
        "MEASURE rules=3 inserted_rows={count} actual_insert_statements={calls} injected_per_statement_ms=20 elapsed_ns={} (isolated fixture, not production cause)",
        elapsed.as_nanos()
    );
    assert_eq!(count, 3);
    assert_eq!(
        calls, 1,
        "one event must pay one INSERT round trip, not one per rule"
    );
    sink.record(&rows).await.unwrap();
    let count: i64 =
        sqlx::query_scalar("SELECT count(*) FROM dispatcher_firings WHERE firing_id = ANY($1)")
            .bind(rows.iter().map(|r| r.firing_id.clone()).collect::<Vec<_>>())
            .fetch_one(&db.pool)
            .await
            .unwrap();
    assert_eq!(count, 3, "redelivery preserves deterministic firing IDs");
}

/// 4b175523: a dead-letter on a topic that names no packet lands as an
/// `outcome = 'dead-letter'` row, beside — never instead of — a firing
/// of the same rule on the same event.
#[tokio::test(flavor = "multi_thread")]
async fn a_dead_letter_row_lands_beside_a_firing_of_the_same_event() {
    let db = TestDb::new().await;
    let sink = PgFirings::new(db.pool.clone());
    let t = Utc.with_ymd_and_hms(2026, 9, 26, 15, 0, 0).unwrap();
    let dead = Firing {
        firing_id: dead_letter_id("issue-invoice", "evt-inv"),
        outcome: Outcome::DeadLetter,
        ..firing("issue-invoice", "evt-inv", t)
    };
    sink.record(std::slice::from_ref(&dead)).await.unwrap();
    sink.record(&[dead]).await.unwrap();
    sink.record(&[firing("issue-invoice", "evt-inv", t)])
        .await
        .unwrap();
    let rows = sqlx::query(
        "SELECT firing_id, outcome FROM dispatcher_firings \
         WHERE rule_name = 'issue-invoice' ORDER BY firing_id",
    )
    .fetch_all(&db.pool)
    .await
    .unwrap();
    let got: Vec<(String, String)> = rows
        .iter()
        .map(|r| (r.get("firing_id"), r.get("outcome")))
        .collect();
    assert_eq!(
        got,
        [
            (
                "dead-letter:issue-invoice:evt-inv".to_string(),
                "dead-letter".to_string()
            ),
            (
                "dispatcher:issue-invoice:evt-inv".to_string(),
                "fired".to_string()
            ),
        ],
        "one dead-letter however often recorded, and the later delivery's firing beside it"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_firing_lands_once_however_often_its_event_is_redelivered() {
    let db = TestDb::new().await;
    let sink = PgFirings::new(db.pool.clone());
    let t = Utc.with_ymd_and_hms(2026, 9, 19, 11, 30, 0).unwrap();

    sink.record(&[firing("auto-park-on-gate-green", "evt-1", t)])
        .await
        .unwrap();
    // The same event, presented again after a NAK: the handlers re-ran,
    // but the rule fired on ONE event and the record must say so.
    sink.record(&[firing(
        "auto-park-on-gate-green",
        "evt-1",
        t + chrono::Duration::seconds(30),
    )])
    .await
    .unwrap();
    sink.record(&[firing("auto-park-on-gate-green", "evt-2", t)])
        .await
        .unwrap();

    let rows = sqlx::query(
        "SELECT firing_id, fired_on, fired_at, detail FROM dispatcher_firings \
         WHERE rule_name = $1 ORDER BY firing_id",
    )
    .bind("auto-park-on-gate-green")
    .fetch_all(&db.pool)
    .await
    .unwrap();
    assert_eq!(rows.len(), 2, "two events, two firings, three recordings");
    let first: String = rows[0].get("firing_id");
    assert_eq!(first, "dispatcher:auto-park-on-gate-green:evt-1");
    let landed: chrono::DateTime<Utc> = rows[0].get("fired_at");
    assert_eq!(
        landed, t,
        "the first recording's instant stands: a redelivery is not a later firing"
    );
    let detail: serde_json::Value = rows[0].get("detail");
    assert_eq!(
        detail.get("event_id").and_then(|v| v.as_str()),
        Some("evt-1")
    );
    let fired_on: String = rows[0].get("fired_on");
    assert_eq!(fired_on, "jobs.gate.green");
}

#[tokio::test(flavor = "multi_thread")]
async fn recording_nothing_is_not_an_error() {
    // The runner calls the sink only when something fired, but an empty
    // slice must be a no-op rather than a failed write that logs a
    // warning about a firing nobody claimed.
    let db = TestDb::new().await;
    let sink = PgFirings::new(db.pool.clone());
    sink.record(&[]).await.unwrap();
    let n: i64 = sqlx::query("SELECT count(*) AS n FROM dispatcher_firings")
        .fetch_one(&db.pool)
        .await
        .unwrap()
        .get("n");
    assert_eq!(n, 0);
}
