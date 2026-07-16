//! The customer birth contract (Q4b): one transaction lands the
//! domain row, the `subjects` identity row, and the
//! `customers.customer.created` outbox event; the rebuilder
//! reproduces the row — email and phone included — from the log
//! alone. Plus the R3 mint semantics: the same email always lands on
//! the same row, and a different id claiming a registered email is
//! rejected, not absorbed.

#![cfg(feature = "postgres")]

use boss_customers::PgCustomers;
use boss_customers::port::{CustomersError, CustomersRepository};
use boss_customers::types::{Customer, id_from_email};
use boss_testing::TestDb;
use chrono::{TimeZone, Utc};

fn customer(id: &str, name: &str, email: Option<&str>) -> Customer {
    Customer {
        id: id.into(),
        name: name.into(),
        email: email.map(str::to_string),
        phone: None,
        metadata: serde_json::json!({"source": "shop"}),
        created_at: None,
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn create_lands_row_identity_and_outbox_event_atomically() {
    let db = TestDb::new().await;
    let repo = PgCustomers::new(db.pool.clone());
    let now = Utc.with_ymd_and_hms(2026, 7, 16, 12, 0, 0).unwrap();
    let id = id_from_email("pat@example.com");

    let inserted = repo
        .create_customer_at(&customer(&id, "Pat", Some("pat@example.com")), now)
        .await
        .unwrap();
    assert!(inserted);

    let (rows, identities, events): (i64, i64, i64) = (
        sqlx::query_scalar("SELECT count(*) FROM customers WHERE id = $1")
            .bind(&id)
            .fetch_one(&db.pool)
            .await
            .unwrap(),
        sqlx::query_scalar("SELECT count(*) FROM subjects WHERE kind = 'customer' AND id = $1")
            .bind(&id)
            .fetch_one(&db.pool)
            .await
            .unwrap(),
        sqlx::query_scalar(
            "SELECT count(*) FROM event_outbox WHERE kind = 'customers.customer.created' \
             AND payload->>'id' = $1",
        )
        .bind(&id)
        .fetch_one(&db.pool)
        .await
        .unwrap(),
    );
    assert_eq!((rows, identities, events), (1, 1, 1));

    // Same id re-created (the /shop re-checkout): no-op, no event.
    let again = repo
        .create_customer_at(&customer(&id, "Pat Again", Some("pat@example.com")), now)
        .await
        .unwrap();
    assert!(!again);
    let events_after: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM event_outbox WHERE kind = 'customers.customer.created'",
    )
    .fetch_one(&db.pool)
    .await
    .unwrap();
    assert_eq!(events_after, 1, "re-create must not emit");

    // A DIFFERENT id claiming the same email is a caller bug —
    // rejected by the partial unique index, surfaced as Invalid.
    match repo
        .create_customer_at(
            &customer("cust-explicit", "Impostor", Some("PAT@example.com")),
            now,
        )
        .await
    {
        Err(CustomersError::Invalid(_)) => {}
        other => panic!("duplicate email under a new id must be Invalid, got {other:?}"),
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn rebuild_reproduces_customers_with_contact_from_the_log_alone() {
    let db = TestDb::new().await;

    for (id, name, email) in [
        ("cust-aaa", "Alpha", "a@example.com"),
        ("cust-bbb", "Beta", "b@example.com"),
        ("cust-aaa", "Alpha Renamed", "a@example.com"),
    ] {
        sqlx::query(
            "INSERT INTO audit_log (event_id, timestamp, source, kind, payload) \
             VALUES (gen_random_uuid(), '2026-07-16T00:00:00Z'::timestamptz, 'test', \
                     'customers.customer.created', $1)",
        )
        .bind(serde_json::json!({"id": id, "name": name, "email": email, "phone": "555-0100"}))
        .execute(&db.pool)
        .await
        .unwrap();
    }

    let n = boss_customers::rebuild::rebuild_customers(&db.pool)
        .await
        .expect("rebuild must tolerate repeated events per id");
    assert_eq!(n, 2);

    let (name, email, phone): (String, Option<String>, Option<String>) =
        sqlx::query_as("SELECT name, email, phone FROM customers WHERE id = 'cust-aaa'")
            .fetch_one(&db.pool)
            .await
            .unwrap();
    assert_eq!(name, "Alpha Renamed", "newest event wins");
    assert_eq!(email.as_deref(), Some("a@example.com"));
    assert_eq!(phone.as_deref(), Some("555-0100"));
}
