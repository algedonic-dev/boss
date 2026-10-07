//! Postgres coverage for the READ half of `dispatcher_firings`
//! (backlog b14afc48).
//!
//! The world map asks one question of a dispatcher rule — when did it
//! last fire — and the answer has to come off the row the runner wrote.
//! This test writes rows the way the writer does and reads them through
//! the adapter, so the two halves are pinned to the same columns: the
//! writer lives in `boss-dispatcher` (which depends on this crate, so
//! it cannot be exercised from here) and its own round trip is
//! `boss-dispatcher/tests/dispatcher_firings_pg.rs`.
//!
//! WHAT MUST NOT BLUR: "never fired" and "could not be read". The
//! adapter answers `Ok(None)` for the first and an `Err` for the
//! second, because a read failure rendered as never-fired is how a
//! stopped machine draws healthy.

use boss_jobs::dispatcher_firings::{DispatcherFiringsRepository, PgDispatcherFirings};
use boss_testing::TestDb;
use chrono::{TimeZone, Utc};

#[tokio::test(flavor = "multi_thread")]
async fn retained_failure_pages_preserve_detail_order_filters_and_beyond_end_total() {
    use boss_jobs::dispatcher_firings::{InMemoryDispatcherFirings, RetainedDeadLetter};
    let db = TestDb::new().await;
    let at = Utc.with_ymd_and_hms(2026, 10, 3, 14, 8, 35).unwrap();
    let since = at - chrono::Duration::days(30);
    let detail = serde_json::json!({"failures":[{"handler":"mint","error":"complete retained cause"}],"context":{"unparsed":[1,true,null]}});
    let mut retained = Vec::new();
    for (id, rule, when, outcome) in [
        ("a-z", "broker", at, "dead-letter"),
        ("ab", "broker", at, "dead-letter"),
        (
            "old",
            "broker",
            since - chrono::Duration::seconds(1),
            "dead-letter",
        ),
        ("other", "different", at, "dead-letter"),
        ("success", "broker", at, "fired"),
    ] {
        sqlx::query("INSERT INTO dispatcher_firings (firing_id, rule_name, fired_on, fired_at, outcome, detail) VALUES ($1,$2,$3,$4,$5,$6)")
            .bind(id).bind(rule).bind("ops.requested").bind(when).bind(outcome).bind(&detail)
            .execute(&db.pool).await.unwrap();
        if outcome == "dead-letter" {
            retained.push(RetainedDeadLetter {
                firing_id: id.into(),
                rule: rule.into(),
                fired_on: "ops.requested".into(),
                fired_at: when,
                detail: detail.clone(),
            });
        }
    }
    let memory = InMemoryDispatcherFirings::new(Vec::new()).with_retained_dead_letters(retained);
    let postgres = PgDispatcherFirings::new(db.pool.clone());
    for repo in [&memory as &dyn DispatcherFiringsRepository, &postgres] {
        let rollup = repo.unrouted_dead_letters(since).await.unwrap();
        let broker = rollup.iter().find(|row| row.rule == "broker").unwrap();
        assert_eq!(broker.count, 2);
        let page = repo.dead_letter_page("broker", since, 1, 0).await.unwrap();
        assert_eq!(page.total, 2);
        assert_eq!(page.data.len(), 1);
        assert_eq!(page.data[0].firing_id, "ab");
        assert_eq!(page.data[0].detail, detail);
        assert_eq!(page.data[0].fired_on, "ops.requested");
        let next = repo.dead_letter_page("broker", since, 1, 1).await.unwrap();
        assert_eq!(next.total, 2);
        assert_eq!(next.data[0].firing_id, "a-z");
        let beyond = repo.dead_letter_page("broker", since, 1, 10).await.unwrap();
        assert_eq!(beyond.total, 2);
        assert!(beyond.data.is_empty());
        let absent = repo.dead_letter_page("absent", since, 1, 0).await.unwrap();
        assert_eq!(absent.total, 0);
        assert!(absent.data.is_empty());
    }
}

async fn insert(pool: &sqlx::PgPool, id: &str, rule: &str, fired_at: &str) {
    sqlx::query(
        "INSERT INTO dispatcher_firings (firing_id, rule_name, fired_on, fired_at, detail) \
         VALUES ($1, $2, $3, $4, $5) ON CONFLICT (firing_id) DO NOTHING",
    )
    .bind(id)
    .bind(rule)
    .bind("jobs.gate.green")
    .bind(fired_at.parse::<chrono::DateTime<Utc>>().unwrap())
    .bind(serde_json::json!({ "event_id": "evt-1" }))
    .execute(pool)
    .await
    .unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn the_newest_firing_of_the_named_rule_is_what_is_served() {
    let db = TestDb::new().await;
    let repo = PgDispatcherFirings::new(db.pool.clone());

    // A rule nobody has recorded a firing for: None, never an error and
    // never an invented instant.
    assert_eq!(
        repo.last_firing("auto-park-on-gate-green").await.unwrap(),
        None,
        "a rule with no row has never fired — and says so"
    );

    insert(
        &db.pool,
        "dispatcher:auto-park-on-gate-green:evt-1",
        "auto-park-on-gate-green",
        "2026-09-19T09:00:00Z",
    )
    .await;
    insert(
        &db.pool,
        "dispatcher:auto-park-on-gate-green:evt-2",
        "auto-park-on-gate-green",
        "2026-09-19T11:30:00Z",
    )
    .await;
    // Another rule's firing, newer than both: the read is per rule, so
    // it must not leak across names.
    insert(
        &db.pool,
        "dispatcher:complete-marker-on-step-ready:evt-3",
        "complete-marker-on-step-ready",
        "2026-09-19T12:00:00Z",
    )
    .await;

    let last = repo
        .last_firing("auto-park-on-gate-green")
        .await
        .unwrap()
        .expect("the rule has fired twice");
    assert_eq!(
        last.fired_at,
        Utc.with_ymd_and_hms(2026, 9, 19, 11, 30, 0).unwrap()
    );
    assert_eq!(last.firing_id, "dispatcher:auto-park-on-gate-green:evt-2");
    assert_eq!(last.fired_on, "jobs.gate.green");
}

/// The rules list's read (backlog 43c4451a): the newest firing of EVERY
/// rule, one row each, off the same columns the writer fills — an empty
/// table answering an empty list, never an error.
#[tokio::test(flavor = "multi_thread")]
async fn every_rules_newest_firing_is_one_row_per_rule() {
    let db = TestDb::new().await;
    let repo = PgDispatcherFirings::new(db.pool.clone());
    assert!(repo.last_firings().await.unwrap().is_empty());

    for (id, rule, at) in [
        ("evt-1", "auto-park-on-gate-green", "2026-09-19T09:00:00Z"),
        ("evt-2", "auto-park-on-gate-green", "2026-09-19T11:30:00Z"),
        ("evt-3", "auto-park-on-gate-green", "2026-09-19T10:00:00Z"),
        (
            "evt-4",
            "complete-marker-on-step-ready",
            "2026-09-18T12:00:00Z",
        ),
    ] {
        insert(&db.pool, &format!("dispatcher:{rule}:{id}"), rule, at).await;
    }

    let all = repo.last_firings().await.unwrap();
    assert_eq!(
        all.iter()
            .map(|f| (f.rule.as_str(), f.fired_at))
            .collect::<Vec<_>>(),
        [
            (
                "auto-park-on-gate-green",
                Utc.with_ymd_and_hms(2026, 9, 19, 11, 30, 0).unwrap()
            ),
            (
                "complete-marker-on-step-ready",
                Utc.with_ymd_and_hms(2026, 9, 18, 12, 0, 0).unwrap()
            ),
        ]
    );
    assert_eq!(all[0].fired_on, "jobs.gate.green");
}

/// 4b175523: a dead-letter on a topic that names no packet is an
/// `outcome = 'dead-letter'` row. It is NOT a firing — neither firing
/// read may answer with it, or a rule failing every event would read
/// "fired a minute ago" — and the dead-letter read counts it per rule
/// inside the window.
#[tokio::test(flavor = "multi_thread")]
async fn a_dead_letter_row_is_read_as_a_dead_letter_and_never_as_a_firing() {
    let db = TestDb::new().await;
    let repo = PgDispatcherFirings::new(db.pool.clone());
    insert(
        &db.pool,
        "dispatcher:issue-invoice:evt-1",
        "issue-invoice",
        "2026-09-26T09:00:00Z",
    )
    .await;
    for (id, at) in [
        ("evt-2", "2026-09-26T10:00:00Z"),
        ("evt-3", "2026-09-26T11:00:00Z"),
        // Outside the window asked for below.
        ("evt-0", "2026-08-01T00:00:00Z"),
    ] {
        sqlx::query(
            "INSERT INTO dispatcher_firings \
             (firing_id, rule_name, fired_on, fired_at, detail, outcome) \
             VALUES ($1, 'issue-invoice', 'commerce.invoice.issued', $2, '{}'::jsonb, 'dead-letter')",
        )
        .bind(format!("dead-letter:issue-invoice:{id}"))
        .bind(at.parse::<chrono::DateTime<Utc>>().unwrap())
        .execute(&db.pool)
        .await
        .unwrap();
    }

    let last = repo.last_firing("issue-invoice").await.unwrap().unwrap();
    assert_eq!(
        last.firing_id, "dispatcher:issue-invoice:evt-1",
        "the newest FIRING, not the newer dead-letters"
    );
    let all = repo.last_firings().await.unwrap();
    assert_eq!(all.len(), 1);
    assert_eq!(
        all[0].fired_at,
        Utc.with_ymd_and_hms(2026, 9, 26, 9, 0, 0).unwrap()
    );

    let since = Utc.with_ymd_and_hms(2026, 9, 1, 0, 0, 0).unwrap();
    let dead = repo.unrouted_dead_letters(since).await.unwrap();
    assert_eq!(dead.len(), 1, "{dead:?}");
    assert_eq!(dead[0].rule, "issue-invoice");
    assert_eq!(dead[0].count, 2, "the August one is outside the window");
    assert_eq!(
        dead[0].newest_at,
        Utc.with_ymd_and_hms(2026, 9, 26, 11, 0, 0).unwrap()
    );
}
