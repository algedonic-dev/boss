//! A customer email is unique under ONE key — `types::email_key`, trim
//! ASCII whitespace and lower ASCII letters — in the id mint, both
//! adapters and the `customers_email` index (backlog e1b08aaa,
//! 2026-10-01). The adapters-agree suite holds the two adapters to each
//! other; this file holds the INDEX to the Rust rule, which only a real
//! database can answer:
//! - the migration file carries `SQL_EMAIL_KEY` verbatim;
//! - Postgres evaluates `SQL_EMAIL_KEY` to `email_key` byte for byte,
//!   non-ASCII case and non-ASCII space included — no locale consulted;
//! - run against the table shape every deployment held before it, the
//!   migration REFUSES while rows the old index allowed are one address
//!   under the new key, naming each group by id, and changes nothing;
//! - with no such rows it replaces the index, which then refuses a
//!   padded second address.

use boss_customers::types::{SQL_EMAIL_KEY, email_key};
use boss_testing::TestDb;
use sqlx::PgPool;

const MIGRATION: &str =
    "infra/postgres/schema/20261001070511-a-customer-email-is-one-key-everywhere.sql";

fn migration_sql() -> String {
    std::fs::read_to_string(boss_testing::repo_root().join(MIGRATION))
        .unwrap_or_else(|e| panic!("read {MIGRATION}: {e}"))
}

/// Inputs that tell the candidate rules apart: ASCII padding of every
/// kind Rust trims, a vertical tab and a no-break space it does not,
/// ASCII case, and non-ASCII case a locale would fold.
const INPUTS: &[&str] = &[
    "pat@example.com",
    "  Pat@Example.COM ",
    "\t\r\n\x0cB@SUITE.TEST\r\n",
    "\x0bpat@x.test",
    "\u{a0}pat@x.test\u{a0}",
    "\u{c9}LISE@x.test",
    "\u{e9}lise@x.test",
    "STRASSE@\u{df}.test",
    "",
    "   ",
];

/// Put the index back in the shape 30-customers.sql created and every
/// deployment held until this car, so the migration runs against what it
/// was written for.
async fn legacy_index(pool: &PgPool) {
    sqlx::raw_sql(
        "DROP INDEX customers_email; \
         CREATE UNIQUE INDEX customers_email ON customers(lower(email)) WHERE email IS NOT NULL;",
    )
    .execute(pool)
    .await
    .expect("legacy index");
}

async fn insert(pool: &PgPool, id: &str, email: &str) {
    sqlx::query("INSERT INTO customers (id, name, email) VALUES ($1, $1, $2)")
        .bind(id)
        .bind(email)
        .execute(pool)
        .await
        .unwrap_or_else(|e| panic!("insert {id}: {e}"));
}

async fn index_def(pool: &PgPool) -> String {
    sqlx::query_scalar("SELECT pg_get_indexdef('customers_email'::regclass)")
        .fetch_one(pool)
        .await
        .expect("the index exists")
}

/// The migration, run the way migrate.sh runs it: one transaction.
async fn migrate(pool: &PgPool) -> Result<(), sqlx::Error> {
    let mut tx = pool.begin().await?;
    sqlx::raw_sql(&migration_sql()).execute(&mut *tx).await?;
    tx.commit().await
}

#[test]
fn the_migration_carries_the_rust_constant_verbatim() {
    let sql = migration_sql();
    let uses = sql.matches(SQL_EMAIL_KEY).count();
    assert_eq!(
        uses, 2,
        "{MIGRATION} must spell SQL_EMAIL_KEY exactly, in the clash check AND the index: {SQL_EMAIL_KEY}"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn postgres_computes_the_key_the_rust_function_computes() {
    let db = TestDb::new().await;
    let query = format!("SELECT {SQL_EMAIL_KEY} FROM (SELECT $1::text AS email) AS s");
    for input in INPUTS {
        let pg: String = sqlx::query_scalar(&query)
            .bind(input)
            .fetch_one(&db.pool)
            .await
            .unwrap_or_else(|e| panic!("{input:?}: {e}"));
        assert_eq!(pg, email_key(input), "{input:?}");
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn the_migration_refuses_rows_that_are_one_address_and_changes_nothing() {
    let db = TestDb::new().await;
    legacy_index(&db.pool).await;
    // Each pair is two rows under lower(email) and one under the key.
    insert(&db.pool, "cust-pat-1", "pat@x.test").await;
    insert(&db.pool, "cust-pat-2", " Pat@x.test").await;
    insert(&db.pool, "cust-q-1", "q@x.test\n").await;
    insert(&db.pool, "cust-q-2", "Q@X.TEST").await;
    // Distinct under the key: a non-ASCII space, and a non-ASCII letter.
    // (`\u{e9}` and `\u{c9}` cannot both be seeded here: the old index's
    // lower() folds them as one under the test database's locale — the
    // locale dependence this migration removes.)
    insert(&db.pool, "cust-nbsp", "\u{a0}pat@x.test").await;
    insert(&db.pool, "cust-e-1", "\u{e9}@x.test").await;
    let before = index_def(&db.pool).await;

    let err = migrate(&db.pool)
        .await
        .expect_err("rows that are one address must refuse the index")
        .to_string();
    assert!(
        err.contains("cust-pat-1, cust-pat-2; cust-q-1, cust-q-2"),
        "the refusal names every clashing group, and only those: {err}"
    );
    assert!(
        !err.contains("cust-e-") && !err.contains("cust-nbsp"),
        "{err}"
    );
    assert!(!err.contains("x.test"), "no email is printed: {err}");

    let rows: i64 = sqlx::query_scalar("SELECT count(*) FROM customers WHERE id LIKE 'cust-%'")
        .fetch_one(&db.pool)
        .await
        .unwrap();
    assert_eq!(rows, 6, "no row merged or dropped");
    assert_eq!(index_def(&db.pool).await, before, "the old index stands");
}

#[tokio::test(flavor = "multi_thread")]
async fn without_clashes_the_migration_replaces_the_index_with_the_key() {
    let db = TestDb::new().await;
    legacy_index(&db.pool).await;
    insert(&db.pool, "cust-pat", "Pat@x.test").await;
    insert(&db.pool, "cust-e-1", "\u{e9}@x.test").await;

    migrate(&db.pool)
        .await
        .expect("no clashes: the index lands");
    // Non-ASCII case is no longer folded by a locale: a second address.
    insert(&db.pool, "cust-e-2", "\u{c9}@x.test").await;
    assert!(
        index_def(&db.pool).await.contains("translate(btrim(email"),
        "{}",
        index_def(&db.pool).await
    );
    let err = sqlx::query(
        "INSERT INTO customers (id, name, email) VALUES ('cust-pad', 'x', ' pat@X.TEST ')",
    )
    .execute(&db.pool)
    .await
    .expect_err("a padded, cased second address is the held one")
    .to_string();
    assert!(err.contains("customers_email"), "{err}");
}
