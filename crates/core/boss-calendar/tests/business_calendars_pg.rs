//! The business-calendar batch against the real tables (design
//! e187198f, David 2026-09-18: THE INSTANCE IS THE TRUTH).
//!
//! Measured that day: `upsert_business_calendars` was `ON CONFLICT
//! (code) DO UPDATE` plus a DELETE-and-reinsert of the closed-day set
//! (postgres.rs:397-417), and `boss tenant publish` runs at every
//! services-container start — so an operator's edit to a calendar's
//! closed days or weekend lived exactly until the next boot. The door
//! is insert-if-absent by default now: a held code is KEPT and the
//! outcome names the fields the declaration differs on; only
//! `PublishMode::Take` replaces the row (header and closed set
//! wholesale), naming each change from → to.

use std::collections::BTreeSet;
use std::sync::Arc;

use boss_calendar::{CalendarClient, PgCalendar};
use boss_core::actor::ActorId;
use boss_core::calendar::BusinessCalendar;
use boss_core::publish::PublishMode;
use boss_core::publisher::EventStamp;
use boss_testing::TestDb;
use chrono::NaiveDate;
use sqlx::postgres::PgPoolOptions;

fn day(s: &str) -> NaiveDate {
    NaiveDate::parse_from_str(s, "%Y-%m-%d").unwrap()
}

/// The stamp the door hands the adapter: the actor the policy ladder
/// resolved (backlog 05f61acf).
fn stamp() -> EventStamp {
    stamp_as("emp-keeper")
}

fn stamp_as(id: &str) -> EventStamp {
    EventStamp::new("calendar", ActorId::Human(id.into()))
}

/// Every business-calendar fact the outbox holds, oldest first, as
/// `(kind, payload)`.
async fn calendar_facts(db: &TestDb) -> Vec<(String, serde_json::Value)> {
    sqlx::query_as(
        "SELECT kind, payload FROM event_outbox WHERE kind LIKE 'business-calendar.%' \
         ORDER BY id",
    )
    .fetch_all(&db.pool)
    .await
    .unwrap()
}

fn calendar(name: &str, weekend: &[u8], closed: &[&str]) -> BusinessCalendar {
    BusinessCalendar {
        code: "acme-founder".into(),
        name: name.into(),
        weekend: weekend.iter().copied().collect(),
        closed: closed.iter().map(|d| day(d)).collect(),
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn a_held_calendar_is_kept_by_default_and_replaced_only_under_take() {
    let db = TestDb::new().await;
    let cal = PgCalendar::new(db.pool.clone());

    // First publish: the code is absent, so it lands whole.
    let out = cal
        .publish_business_calendars(
            &[calendar("founder hours", &[5, 6], &["2026-12-25"])],
            PublishMode::InsertIfAbsent,
            &stamp(),
        )
        .await
        .expect("lands");
    assert_eq!((out.received, out.inserted), (1, 1));
    assert!(out.kept.is_empty() && out.updated.is_empty(), "{out:?}");

    // An operator's edit through the database: a longer closed set
    // and a Friday weekend.
    sqlx::query("UPDATE business_calendars SET weekend = '{4,5,6}' WHERE code = 'acme-founder'")
        .execute(&db.pool)
        .await
        .unwrap();
    sqlx::query(
        "INSERT INTO business_calendar_closed_days (calendar_code, day) VALUES ('acme-founder', '2026-12-26')",
    )
    .execute(&db.pool)
    .await
    .unwrap();

    // The same file again, by default: kept, and the outcome names the
    // two fields the declaration differs on.
    let declared = calendar("founder hours", &[5, 6], &["2026-12-25"]);
    let out = cal
        .publish_business_calendars(
            std::slice::from_ref(&declared),
            PublishMode::InsertIfAbsent,
            &stamp(),
        )
        .await
        .expect("answers");
    assert_eq!((out.received, out.inserted), (1, 0));
    assert!(out.updated.is_empty(), "{out:?}");
    assert_eq!(out.kept.len(), 1, "{out:?}");
    assert_eq!(
        out.kept[0].render(),
        "acme-founder differs on weekend, closed"
    );
    let live = cal
        .get_business_calendar("acme-founder")
        .await
        .unwrap()
        .expect("still there");
    assert_eq!(
        live.weekend,
        [4u8, 5, 6].into_iter().collect::<BTreeSet<_>>()
    );
    assert_eq!(
        live.closed,
        [day("2026-12-25"), day("2026-12-26")]
            .into_iter()
            .collect::<BTreeSet<_>>(),
        "the operator's closed day survives the republish"
    );

    // Identical declaration: neither kept-differing nor updated.
    let same = calendar("founder hours", &[4, 5, 6], &["2026-12-25", "2026-12-26"]);
    let out = cal
        .publish_business_calendars(&[same], PublishMode::InsertIfAbsent, &stamp())
        .await
        .unwrap();
    assert_eq!((out.inserted, out.kept.len(), out.updated.len()), (0, 0, 0));
    assert_eq!(out.unchanged, 1);

    // Under take the declaration replaces header and closed set
    // wholesale, and each change is named from → to.
    let out = cal
        .publish_business_calendars(std::slice::from_ref(&declared), PublishMode::Take, &stamp())
        .await
        .expect("takes");
    assert_eq!((out.inserted, out.kept.len()), (0, 0));
    assert_eq!(out.updated.len(), 1, "{out:?}");
    assert_eq!(out.updated[0].id, "acme-founder");
    assert_eq!(
        out.updated[0].render(),
        "acme-founder (weekend [4,5,6] → [5,6], closed [\"2026-12-25\",\"2026-12-26\"] → [\"2026-12-25\"])"
    );
    let live = cal
        .get_business_calendar("acme-founder")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(live.weekend, [5u8, 6].into_iter().collect::<BTreeSet<_>>());
    assert_eq!(
        live.closed,
        [day("2026-12-25")].into_iter().collect::<BTreeSet<_>>()
    );
}

/// The list read `boss tenant export` writes the file from (backlog
/// e618f3ac): every code the tables hold, sorted, each with its closed
/// set — so the export of an instance is the batch's own input.
#[tokio::test(flavor = "multi_thread")]
async fn list_answers_every_code_sorted_with_its_closed_set() {
    let db = TestDb::new().await;
    let cal = PgCalendar::new(db.pool.clone());
    let mut second = calendar("Second", &[6], &["2026-11-26"]);
    second.code = "aaa-second".into();
    let first = calendar("Founder", &[5, 6], &["2026-12-25", "2026-01-01"]);
    cal.publish_business_calendars(&[first, second], PublishMode::InsertIfAbsent, &stamp())
        .await
        .unwrap();
    let rows = cal.list_business_calendars().await.unwrap();
    let codes: Vec<&str> = rows.iter().map(|c| c.code.as_str()).collect();
    assert_eq!(codes, ["aaa-second", "acme-founder"]);
    assert_eq!(
        rows[1].closed,
        [day("2026-01-01"), day("2026-12-25")]
            .into_iter()
            .collect::<BTreeSet<_>>()
    );
    assert_eq!(rows[0].weekend, [6u8].into_iter().collect::<BTreeSet<_>>());
}

/// WHO CHANGED A CALENDAR IS ON THE RECORD (backlog 05f61acf,
/// 2026-09-28; the calendar half of 06590554). Every publish that
/// changes a row stages ONE fact on the outbox in the write's own
/// transaction, signed by the stamp's actor: `business-calendar.declared`
/// for an inserted code, `business-calendar.updated` for a take that
/// replaced a held one — each change from → to, so the closed-day set
/// the take overwrote is in the log. A kept row, an identical
/// declaration and a take that restates the held row change nothing,
/// write nothing and record nothing.
#[tokio::test(flavor = "multi_thread")]
async fn each_publish_that_changes_a_calendar_stages_one_fact_signed_by_its_caller() {
    let db = TestDb::new().await;
    let cal = PgCalendar::new(db.pool.clone());
    let v1 = calendar("founder hours", &[5, 6], &["2026-12-25"]);
    cal.publish_business_calendars(
        std::slice::from_ref(&v1),
        PublishMode::InsertIfAbsent,
        &stamp_as("emp-declarer"),
    )
    .await
    .unwrap();
    let facts = calendar_facts(&db).await;
    assert_eq!(facts.len(), 1, "{facts:?}");
    assert_eq!(facts[0].0, "business-calendar.declared");
    let declared = &facts[0].1;
    assert_eq!(declared["code"], "acme-founder");
    assert_eq!(declared["name"], "founder hours");
    assert_eq!(declared["weekend"], serde_json::json!([5, 6]));
    assert_eq!(declared["closed"], serde_json::json!(["2026-12-25"]));
    assert_eq!(declared["mode"], "insert-if-absent");
    assert_eq!(declared["declared_by"], "emp-declarer");
    assert_eq!(declared["_actor"], "emp-declarer");

    // Kept, then restated: nothing written, nothing recorded.
    let v2 = calendar("founder hours", &[5, 6], &["2027-01-01"]);
    cal.publish_business_calendars(
        std::slice::from_ref(&v2),
        PublishMode::InsertIfAbsent,
        &stamp_as("emp-taker"),
    )
    .await
    .unwrap();
    let touched = || async {
        sqlx::query_scalar::<_, chrono::DateTime<chrono::Utc>>(
            "SELECT updated_at FROM business_calendars WHERE code = 'acme-founder'",
        )
        .fetch_one(&db.pool)
        .await
        .unwrap()
    };
    let before = touched().await;
    cal.publish_business_calendars(
        std::slice::from_ref(&v1),
        PublishMode::Take,
        &stamp_as("emp-taker"),
    )
    .await
    .unwrap();
    assert_eq!(calendar_facts(&db).await.len(), 1, "no change, no fact");
    // A restating take no longer rewrites the row (it used to bump
    // `updated_at` with no change; nothing reads that column).
    assert_eq!(touched().await, before, "no change, no write");

    // The take that replaces the held closed set names who took it.
    let out = cal
        .publish_business_calendars(
            std::slice::from_ref(&v2),
            PublishMode::Take,
            &stamp_as("emp-taker"),
        )
        .await
        .unwrap();
    assert_eq!(out.updated.len(), 1, "{out:?}");
    let facts = calendar_facts(&db).await;
    assert_eq!(facts.len(), 2, "{facts:?}");
    assert_eq!(facts[1].0, "business-calendar.updated");
    let updated = &facts[1].1;
    assert_eq!(updated["code"], "acme-founder");
    assert_eq!(updated["mode"], "take");
    assert_eq!(updated["closed"], serde_json::json!(["2027-01-01"]));
    assert_eq!(updated["changes"][0]["field"], "closed");
    assert_eq!(
        updated["changes"][0]["from"],
        serde_json::json!(["2026-12-25"])
    );
    assert_eq!(
        updated["changes"][0]["to"],
        serde_json::json!(["2027-01-01"])
    );
    assert_eq!(updated["updated_by"], "emp-taker");
    assert_eq!(updated["_actor"], "emp-taker");
}

/// `n` concurrent publishes of the one code under `mode`, each declaring
/// its own closed day `<month>-<i>` and signed `emp-<i>`, all joined.
async fn race(cal: &Arc<PgCalendar>, mode: PublishMode, month: &str, n: usize) {
    let racers: Vec<_> = (1..=n)
        .map(|i| {
            let cal = cal.clone();
            let day = format!("{month}-{i:02}");
            let declared = calendar("founder hours", &[5, 6], &[day.as_str()]);
            tokio::spawn(async move {
                cal.publish_business_calendars(&[declared], mode, &stamp_as(&format!("emp-{i}")))
                    .await
                    .expect("each racer publishes")
            })
        })
        .collect();
    for racer in racers {
        racer.await.expect("the racer ran");
    }
}

/// CONCURRENT PUBLISHES OF ONE CODE LEAVE ONE CHAIN OF FACTS THAT ENDS AT
/// THE ROW (adversarial review of car d768217e, F1, 2026-09-28). The
/// held row was read with no lock under READ COMMITTED, so two takes on
/// one code both read A: T1 wrote B, T2's upsert waited and wrote C, and
/// the log held A→B and A→C — the B that T2 overwrote was never a
/// `from`, which is the one thing this fact exists to record. Two
/// default publishes of an absent code both read None, both recorded
/// `declared`, and the second's upsert overwrote the first with no
/// `updated`. The realistic trigger is the boot-time tenant publish
/// racing an operator's take. Each code is now locked for the batch's
/// transaction before it is read, so the racers queue: exactly one
/// `declared`, then every `updated` names as `from` the set the one
/// before it wrote, and the last one's set is the row.
#[tokio::test(flavor = "multi_thread", worker_threads = 8)]
async fn concurrent_publishes_of_one_code_leave_one_chain_of_facts_that_ends_at_the_row() {
    let db = TestDb::new().await;
    let pool = PgPoolOptions::new()
        .max_connections(12)
        .connect(&db.url())
        .await
        .unwrap();
    let cal = Arc::new(PgCalendar::new(pool));
    let live_closed = || async {
        let row = cal
            .get_business_calendar("acme-founder")
            .await
            .unwrap()
            .expect("the code is held");
        serde_json::to_value(&row.closed).unwrap()
    };

    // Absent: eight default publishes, one of which declares it.
    race(&cal, PublishMode::InsertIfAbsent, "2027-01", 8).await;
    let facts = calendar_facts(&db).await;
    assert_eq!(facts.len(), 1, "one code declared once: {facts:?}");
    assert_eq!(facts[0].0, "business-calendar.declared");
    assert_eq!(
        facts[0].1["closed"],
        live_closed().await,
        "the declared set is the row"
    );

    // Held: eight takes, each a change, chained.
    race(&cal, PublishMode::Take, "2027-02", 8).await;
    let facts = calendar_facts(&db).await;
    assert_eq!(facts.len(), 9, "{facts:?}");
    let mut prev = facts[0].1["closed"].clone();
    for (kind, fact) in &facts[1..] {
        assert_eq!(kind, "business-calendar.updated");
        let closed = fact["changes"]
            .as_array()
            .unwrap()
            .iter()
            .find(|c| c["field"] == "closed")
            .expect("each take changed the closed set");
        assert_eq!(
            closed["from"], prev,
            "each take names the set the one before it wrote: {facts:?}"
        );
        prev = fact["closed"].clone();
    }
    assert_eq!(prev, live_closed().await, "the last fact is the row");
}
