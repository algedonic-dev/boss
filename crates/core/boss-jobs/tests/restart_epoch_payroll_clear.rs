//! Regression: the demo-loop reset (restart-epoch) must clear the
//! directly-written payroll projection tables.
//!
//! `payroll_runs` is written straight by the ledger payroll-synthesize
//! endpoint and no audit-log rebuilder owns it, so the per-service
//! replay in restart-epoch leaves it behind. Across epoch loops it then
//! accumulates prior-cycle rows; synthesize's calendar-dated key
//! (`payroll-YYYYMMDD`) collides with the stale row, dedups, and the new
//! cycle's payroll never reaches the GL (6100 shows one period instead of
//! ~26/yr). `clear_epoch_payroll_state` — which restart-epoch now calls
//! between the audit_log trim and the projection rebuild — fixes that.
//!
//! Driven against Postgres directly: the in-memory adapter has no
//! payroll_runs table, so this gap is Postgres-only.

#![cfg(feature = "postgres")]

use boss_jobs::postgres::clear_epoch_payroll_state;
use boss_testing::TestDb;

#[tokio::test]
async fn restart_epoch_clears_payroll_runs_and_lines() {
    let db = TestDb::new().await;

    // A prior-cycle payroll run + a line (the FK child) — exactly the
    // rows the epoch loop leaves behind. net = gross - withheld per the
    // table CHECK constraints.
    sqlx::query(
        "INSERT INTO payroll_runs \
         (id, run_date, period_start, period_end, gross_cents, employer_tax_cents, \
          withheld_cents, net_cents, employee_count, provider, status) \
         VALUES ('payroll-20250410', '2025-04-10', '2025-03-28', '2025-04-10', \
                 978129, 146719, 215188, 762941, 407, 'in-house', 'posted')",
    )
    .execute(&db.pool)
    .await
    .expect("seed payroll_runs");
    sqlx::query(
        "INSERT INTO payroll_run_lines \
         (run_id, employee_id, gross_cents, withheld_cents, net_cents, department, role) \
         VALUES ('payroll-20250410', 'emp-cto', 2400, 528, 1872, 'executive', 'cto')",
    )
    .execute(&db.pool)
    .await
    .expect("seed payroll_run_lines");

    let runs_before: i64 = sqlx::query_scalar("SELECT count(*) FROM payroll_runs")
        .fetch_one(&db.pool)
        .await
        .unwrap();
    assert_eq!(
        runs_before, 1,
        "precondition: a stale prior-cycle run exists"
    );

    // The reset clear (what restart-epoch invokes).
    clear_epoch_payroll_state(&db.pool)
        .await
        .expect("clear_epoch_payroll_state ok");

    let runs: i64 = sqlx::query_scalar("SELECT count(*) FROM payroll_runs")
        .fetch_one(&db.pool)
        .await
        .unwrap();
    let lines: i64 = sqlx::query_scalar("SELECT count(*) FROM payroll_run_lines")
        .fetch_one(&db.pool)
        .await
        .unwrap();
    assert_eq!(runs, 0, "restart-epoch must clear payroll_runs");
    assert_eq!(
        lines, 0,
        "restart-epoch must clear payroll_run_lines (FK child)"
    );
}
