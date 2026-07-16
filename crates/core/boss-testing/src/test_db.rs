//! `TestDb` — per-test Postgres database with the Boss schema loaded.
//!
//! Each `TestDb::new()` call creates a fresh, randomly-named database
//! (`test_boss_<uuid>`), loads the per-module `infra/postgres/schema/`
//! files (via `apply-schema.sh` / the `SCHEMA_FILES` list)
//! into it, and returns a connection pool. On `Drop`, the database is
//! dropped via a best-effort background task — if that fails (test
//! process killed, runtime already shut down), the random name prefix
//! makes orphans easy to find and clean up administratively.
//!
//! ## Prerequisites
//!
//! - A reachable Postgres instance with a role that has `CREATEDB`.
//! - The admin URL (connection string pointing at the `postgres`
//!   database or any existing database) via the
//!   `BOSS_TEST_POSTGRES_ADMIN_URL` environment variable, or the default
//!   `postgres://boss:boss@127.0.0.1/postgres`.
//!
//! ## Usage
//!
//! ```ignore
//! #[tokio::test(flavor = "multi_thread")]
//! async fn my_integration_test() {
//!     let db = boss_testing::TestDb::new().await;
//!     sqlx::query("INSERT INTO accounts (id, name) VALUES ($1, $2)")
//!         .bind("account-1")
//!         .bind("Test Account")
//!         .execute(&db.pool)
//!         .await
//!         .unwrap();
//!     // ... exercise code under test against db.pool ...
//! }
//! ```

use std::str::FromStr;

use sqlx::postgres::{PgConnectOptions, PgPool, PgPoolOptions};
use sqlx::{Connection, Executor, PgConnection};
use uuid::Uuid;

const DEFAULT_ADMIN_URL: &str = "postgres://boss:boss@127.0.0.1/postgres";
/// The per-module schema files, in manifest apply order. `include_str!`
/// is compile-time, so each file is listed explicitly (rather than reading
/// schema/manifest.txt at runtime); they're concatenated in `schema_sql`
/// so `new_without` can skip a module's file, mirroring apply-schema.sh's
/// `--without`. Keep this list in sync with infra/postgres/schema/manifest.txt.
const SCHEMA_FILES: &[(&str, &str)] = &[
    (
        "00-extensions",
        include_str!("../../../../infra/postgres/schema/00-extensions.sql"),
    ),
    (
        "01-registries",
        include_str!("../../../../infra/postgres/schema/01-registries.sql"),
    ),
    (
        "02-events",
        include_str!("../../../../infra/postgres/schema/02-events.sql"),
    ),
    (
        "03-jobs",
        include_str!("../../../../infra/postgres/schema/03-jobs.sql"),
    ),
    (
        "04-policy",
        include_str!("../../../../infra/postgres/schema/04-policy.sql"),
    ),
    (
        "05-ml",
        include_str!("../../../../infra/postgres/schema/05-ml.sql"),
    ),
    (
        "06-docs",
        include_str!("../../../../infra/postgres/schema/06-docs.sql"),
    ),
    (
        "07-content",
        include_str!("../../../../infra/postgres/schema/07-content.sql"),
    ),
    (
        "08-gateway",
        include_str!("../../../../infra/postgres/schema/08-gateway.sql"),
    ),
    (
        "09-clock",
        include_str!("../../../../infra/postgres/schema/09-clock.sql"),
    ),
    (
        "10-people",
        include_str!("../../../../infra/postgres/schema/10-people.sql"),
    ),
    (
        "20-catalog",
        include_str!("../../../../infra/postgres/schema/20-catalog.sql"),
    ),
    (
        "21-assets",
        include_str!("../../../../infra/postgres/schema/21-assets.sql"),
    ),
    (
        "22-accounts",
        include_str!("../../../../infra/postgres/schema/22-accounts.sql"),
    ),
    (
        "23-commerce",
        include_str!("../../../../infra/postgres/schema/23-commerce.sql"),
    ),
    (
        "24-inventory",
        include_str!("../../../../infra/postgres/schema/24-inventory.sql"),
    ),
    (
        "25-products",
        include_str!("../../../../infra/postgres/schema/25-products.sql"),
    ),
    (
        "26-messages",
        include_str!("../../../../infra/postgres/schema/26-messages.sql"),
    ),
    (
        "27-shipping",
        include_str!("../../../../infra/postgres/schema/27-shipping.sql"),
    ),
    (
        "28-scheduling",
        include_str!("../../../../infra/postgres/schema/28-scheduling.sql"),
    ),
    (
        "29-campaigns",
        include_str!("../../../../infra/postgres/schema/29-campaigns.sql"),
    ),
    (
        "40-ledger",
        include_str!("../../../../infra/postgres/schema/40-ledger.sql"),
    ),
    (
        "41-dispatcher",
        include_str!("../../../../infra/postgres/schema/41-dispatcher.sql"),
    ),
    (
        "99-search",
        include_str!("../../../../infra/postgres/schema/99-search.sql"),
    ),
];

/// Concatenate the schema files in manifest order, omitting any whose name
/// contains an entry in `without`.
fn schema_sql(without: &[&str]) -> String {
    SCHEMA_FILES
        .iter()
        .filter(|(name, _)| !without.iter().any(|w| name.contains(w)))
        .map(|(_, sql)| *sql)
        .collect::<Vec<_>>()
        .join("\n")
}

pub struct TestDb {
    pub pool: PgPool,
    db_name: String,
    admin_url: String,
}

impl TestDb {
    /// Create a fresh database, load the full Boss schema into it, and
    /// return a pool. Panics on any setup failure — tests that need a
    /// DB can't meaningfully continue without one, so fail loud.
    pub async fn new() -> Self {
        Self::new_with(&[]).await
    }

    /// Like [`new`](Self::new) but omits the schema files whose name
    /// contains any entry in `without` (mirrors apply-schema.sh's
    /// `--without`). Lets a test prove the core + a subset of modules
    /// bootstrap with, e.g., the ledger absent: `new_without(&["ledger"])`.
    pub async fn new_without(without: &[&str]) -> Self {
        Self::new_with(without).await
    }

    async fn new_with(without: &[&str]) -> Self {
        let admin_url = std::env::var("BOSS_TEST_POSTGRES_ADMIN_URL")
            .unwrap_or_else(|_| DEFAULT_ADMIN_URL.to_string());

        let suffix = Uuid::new_v4().simple().to_string();
        let db_name = format!("test_boss_{}", &suffix[..12]);

        let admin_opts = PgConnectOptions::from_str(&admin_url)
            .unwrap_or_else(|e| panic!("parsing BOSS_TEST_POSTGRES_ADMIN_URL: {e}"));

        let mut admin = PgConnection::connect_with(&admin_opts)
            .await
            .unwrap_or_else(|e| panic!("connecting to admin db at {admin_url}: {e}"));

        admin
            .execute(format!(r#"CREATE DATABASE "{db_name}""#).as_str())
            .await
            .unwrap_or_else(|e| panic!("CREATE DATABASE {db_name}: {e}"));

        // Test sessions write audit_log events directly (via
        // PgAuditWriter / seed helpers) without running the projection
        // pipeline that populates the soft-FK parent tables (accounts,
        // vendors, …). The `audit_log_check_refs` BEFORE INSERT trigger
        // would therefore reject every invoice / vendor-invoice event.
        // Disable the check at the DB level — the same escape hatch
        // bundle-import uses (`audit_log.ref_check = 'off'`, see
        // boss-rebuild). The soft-FK *integrity scan*
        // (`check_audit_log_integrity`) runs independently and stays
        // under test.
        admin
            .execute(
                format!(r#"ALTER DATABASE "{db_name}" SET audit_log.ref_check = 'off'"#).as_str(),
            )
            .await
            .unwrap_or_else(|e| panic!("disable ref_check on {db_name}: {e}"));

        // Close admin connection before opening the test DB.
        drop(admin);

        let test_opts = admin_opts.clone().database(&db_name);
        let pool = PgPoolOptions::new()
            .max_connections(5)
            .connect_with(test_opts)
            .await
            .unwrap_or_else(|e| panic!("connecting to test db {db_name}: {e}"));

        let schema = schema_sql(without);
        sqlx::raw_sql(&schema)
            .execute(&pool)
            .await
            .unwrap_or_else(|e| panic!("loading schema into {db_name}: {e}"));

        Self {
            pool,
            db_name,
            admin_url,
        }
    }

    /// Database name, primarily for debugging.
    pub fn name(&self) -> &str {
        &self.db_name
    }
}

impl Drop for TestDb {
    fn drop(&mut self) {
        // Best-effort cleanup. If we're inside a tokio runtime we
        // schedule a background drop of the database. If not, the
        // orphan persists and must be cleaned by `DROP DATABASE
        // test_boss_*` administratively.
        let db_name = std::mem::take(&mut self.db_name);
        let admin_url = std::mem::take(&mut self.admin_url);
        if db_name.is_empty() {
            return;
        }
        if let Ok(handle) = tokio::runtime::Handle::try_current() {
            handle.spawn(async move {
                drop_database(&admin_url, &db_name).await;
            });
        }
    }
}

async fn drop_database(admin_url: &str, db_name: &str) {
    let Ok(opts) = PgConnectOptions::from_str(admin_url) else {
        return;
    };
    let Ok(mut conn) = PgConnection::connect_with(&opts).await else {
        return;
    };
    // Terminate other sessions holding connections to the test db,
    // otherwise DROP DATABASE fails with "database is being accessed
    // by other users". WITH (FORCE) would also work on Postgres 13+.
    let _ = conn
        .execute(
            format!(
                "SELECT pg_terminate_backend(pid) FROM pg_stat_activity \
                 WHERE datname = '{db_name}' AND pid <> pg_backend_pid()"
            )
            .as_str(),
        )
        .await;
    let _ = conn
        .execute(format!(r#"DROP DATABASE IF EXISTS "{db_name}""#).as_str())
        .await;
}
