//! The gateway audit role, RUN against the real schema: once every
//! migration is applied, `boss_gateway_audit` can open no session and
//! holds nothing in the database (backlog 7ec7113b).
//!
//! `111-gateway-audit-events.sql` created it as a LOGIN role whose
//! password was its own name, so every database that ran 111 carried a
//! credential anyone reading the tree knew. 111 cannot be edited —
//! applied migrations are hashed, and editing one took production down
//! for an hour on 2026-08-13 — so a later migration takes the login
//! away, clears the password and revokes the three grants. The gateway
//! no longer connects as the role at all: it stages its auth events
//! through the service database URL (backlog d49b4355;
//! `boss_gateway::audit::AUDIT_SINK_URL_VAR`).
//!
//! The role is left in place rather than dropped by the migration:
//! roles are cluster-global, and a DROP is refused while ANY database on
//! the server still holds a grant to it — every parallel test database
//! here, and whatever the live server holds — so the drop is the
//! operator's act on the live database, recorded on the packet.
//!
//! Never against production: TestDb refuses a server hosting a database
//! named `boss` (test_db.rs).

use boss_testing::TestDb;
use sqlx::ConnectOptions;
use sqlx::Row;
use sqlx::postgres::PgConnectOptions;
use std::str::FromStr;

const ROLE: &str = "boss_gateway_audit";

#[tokio::test(flavor = "multi_thread")]
async fn the_gateway_audit_role_cannot_log_in_and_has_no_password() {
    let db = TestDb::new().await;
    let row = sqlx::query(
        "SELECT rolcanlogin, rolpassword IS NULL AS no_password \
         FROM pg_authid WHERE rolname = $1",
    )
    .bind(ROLE)
    .fetch_one(&db.pool)
    .await
    .unwrap_or_else(|e| panic!("reading {ROLE} from pg_authid: {e}"));
    let can_login: bool = row.get("rolcanlogin");
    let no_password: bool = row.get("no_password");
    assert!(
        !can_login,
        "{ROLE} can still LOG IN after every migration — the role 111 created \
         with a password equal to its name opens a session (backlog 7ec7113b)"
    );
    assert!(
        no_password,
        "{ROLE} still carries a password after every migration — the one 111 \
         published in the tree (backlog 7ec7113b)"
    );
}

/// The published credential, tried the way an attacker on the LAN would:
/// a connection as the role with the password 111 gave it. Which refusal
/// answers depends on the server's pg_hba: under `trust` the role's
/// NOLOGIN refuses; under scram/md5 (the mirror's CI postgres, reached
/// through a docker bridge) the cleared password fails first. Either one
/// proves the published password opens nothing; any OTHER error (a down
/// server, a bad URL) proves nothing and fails.
#[tokio::test(flavor = "multi_thread")]
async fn the_published_password_opens_no_session() {
    let db = TestDb::new().await;
    let opts = PgConnectOptions::from_str(&db.url())
        .unwrap_or_else(|e| panic!("parsing the scratch database URL: {e}"))
        .username(ROLE)
        .password(ROLE);
    let err = match opts.connect().await {
        Ok(_) => panic!(
            "connected as {ROLE} with the password migration 111 published — \
             the role must open no session (backlog 7ec7113b)"
        ),
        Err(e) => e.to_string(),
    };
    assert!(
        err.contains("not permitted to log in") || err.contains("password authentication failed"),
        "the connection was refused, but not by the role's NOLOGIN or its cleared \
         password — a refusal for another reason (a down server, a bad URL) proves \
         nothing: {err}"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn the_gateway_audit_role_holds_no_grant() {
    let db = TestDb::new().await;
    let row = sqlx::query(
        "SELECT has_table_privilege($1, 'event_outbox', 'INSERT') AS outbox, \
                has_sequence_privilege($1, 'event_outbox_id_seq', 'USAGE') AS seq, \
                has_table_privilege($1, 'audit_log_ref_checks', 'SELECT') AS rules",
    )
    .bind(ROLE)
    .fetch_one(&db.pool)
    .await
    .unwrap_or_else(|e| panic!("reading {ROLE}'s privileges: {e}"));
    for grant in ["outbox", "seq", "rules"] {
        let held: bool = row.get(grant);
        assert!(
            !held,
            "{ROLE} still holds the `{grant}` grant 111 gave it — a role nothing \
             connects as keeps nothing (backlog 7ec7113b)"
        );
    }
}
