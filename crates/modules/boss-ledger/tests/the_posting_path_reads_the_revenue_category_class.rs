//! The revenue account an invoice line credits is the one its
//! category's Class names — `metadata.gl_account` on the
//! `(invoice, revenue_category)` row the tenant declares (backlog
//! aa860c6d, 2026-09-26).
//!
//! WHAT WAS MEASURED. The category → account map was
//! `seeds/revenue_accounts.toml`, a platform file bundled into the
//! ledger with a `BOSS_LEDGER_REVENUE_ACCOUNTS_TOML` override no unit,
//! manifest or estate file set, so the live ledger ran the brewery's
//! seven rows. Algedonic, LLC declared its own categories as Classes
//! (sponsorship, support, hosting) and its own revenue accounts (4200,
//! 4300, 4400) — commerce accepted an invoice line in `hosting`, and the
//! ledger would have refused it as an unknown category. The map and its
//! override are gone; the posting path reads the Class instead.

use boss_ledger::{FactRef, LedgerError, post_fact_in_tx};
use boss_testing::TestDb;
use chrono::NaiveDate;
use serde_json::{Value, json};
use uuid::Uuid;

/// Insert an `finance.invoice.issued` fact row with one line in
/// `category` and post it, returning the posting's own verdict.
async fn post_invoice(db: &TestDb, category: &str) -> Result<Uuid, LedgerError> {
    let fact_id = Uuid::new_v4();
    let happened_on = NaiveDate::from_ymd_opt(2026, 9, 26).unwrap();
    let payload: Value = json!({
        "invoice_id": format!("inv-{fact_id}"),
        "amount_cents": 40_000,
        "currency": "USD",
        "line_items": [{"category": category, "amount_cents": 40_000, "currency": "USD"}],
    });
    let mut tx = db.pool.begin().await.unwrap();
    sqlx::query(
        "INSERT INTO financial_facts (id, kind, happened_on, payload, source_table, source_id, created_by) \
         VALUES ($1, 'finance.invoice.issued', $2, $3, 'invoices', $4, 'test')",
    )
    .bind(fact_id)
    .bind(happened_on)
    .bind(&payload)
    .bind(fact_id.to_string())
    .execute(&mut *tx)
    .await
    .unwrap();
    let fact = FactRef {
        id: fact_id,
        kind: "finance.invoice.issued",
        happened_on,
        payload: &payload,
    };
    post_fact_in_tx(&mut tx, &fact).await?;
    tx.commit().await.unwrap();
    Ok(fact_id)
}

/// The LLC's hosting business, as the LLC declared it: account 4400
/// Hosting revenue and the `hosting` Class naming it.
async fn declare_hosting(db: &TestDb, metadata: Value) {
    sqlx::query(
        "INSERT INTO gl_accounts (id, code, name, kind, normal_side) \
         VALUES ($1, '4400', 'Hosting revenue', 'revenue', 'credit') ON CONFLICT (code) DO NOTHING",
    )
    .bind(Uuid::new_v4())
    .execute(&db.pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO classes (subject_kind, code, display_name, member_attribute, metadata, sort_order) \
         VALUES ('invoice', 'hosting', 'Hosted instance', 'revenue_category', $1, 30)",
    )
    .bind(metadata)
    .execute(&db.pool)
    .await
    .unwrap();
}

/// The credit lines the fact's entry holds, as (account code, cents).
async fn credits(db: &TestDb, fact_id: Uuid) -> Vec<(String, i64)> {
    sqlx::query_as(
        "SELECT a.code, l.credit_cents FROM gl_journal_lines l \
         JOIN gl_accounts a ON a.id = l.account_id \
         JOIN gl_journal_entries e ON e.id = l.journal_entry_id \
         WHERE e.fact_id = $1 AND l.credit_cents > 0 ORDER BY a.code",
    )
    .bind(fact_id)
    .fetch_all(&db.pool)
    .await
    .unwrap()
}

#[tokio::test]
async fn a_hosting_line_posts_to_the_account_its_class_names() {
    let db = TestDb::new().await;
    declare_hosting(&db, json!({"gl_account": "4400"})).await;
    let fact_id = post_invoice(&db, "hosting").await.unwrap();
    assert_eq!(
        credits(&db, fact_id).await,
        vec![("4400".to_string(), 40_000)]
    );
}

/// A retired category takes no new lines (commerce's gate) but still
/// resolves here, so a rebuild re-posts the invoices it carried.
#[tokio::test]
async fn a_retired_category_still_posts_what_it_carried() {
    let db = TestDb::new().await;
    declare_hosting(&db, json!({"gl_account": "4400"})).await;
    sqlx::query(
        "UPDATE classes SET retired_at = now() WHERE subject_kind = 'invoice' AND code = 'hosting'",
    )
    .execute(&db.pool)
    .await
    .unwrap();
    let fact_id = post_invoice(&db, "hosting").await.unwrap();
    assert_eq!(
        credits(&db, fact_id).await,
        vec![("4400".to_string(), 40_000)]
    );
}

#[tokio::test]
async fn an_undeclared_category_still_refuses() {
    let db = TestDb::new().await;
    declare_hosting(&db, json!({"gl_account": "4400"})).await;
    // The brewery's category, on an instance that never declared it.
    match post_invoice(&db, "wholesale").await {
        Err(LedgerError::InvalidPayload { reason, .. }) => {
            assert!(
                reason.contains("unknown revenue category `wholesale`"),
                "{reason}"
            );
        }
        other => panic!("expected InvalidPayload, got {other:?}"),
    }
}

#[tokio::test]
async fn a_class_that_names_no_account_refuses_by_name() {
    let db = TestDb::new().await;
    declare_hosting(&db, json!({})).await;
    match post_invoice(&db, "hosting").await {
        Err(LedgerError::InvalidPayload { reason, .. }) => {
            assert!(
                reason.contains("`hosting`") && reason.contains("gl_account"),
                "{reason}"
            );
        }
        other => panic!("expected InvalidPayload, got {other:?}"),
    }
}

/// A Class naming an account the chart does not hold refuses at the
/// post by the account's code — the chart is the tenant's to declare.
#[tokio::test]
async fn a_class_naming_an_undeclared_account_refuses_by_the_code() {
    let db = TestDb::new().await;
    sqlx::query(
        "INSERT INTO classes (subject_kind, code, display_name, member_attribute, metadata) \
         VALUES ('invoice', 'support', 'Support engagement', 'revenue_category', '{\"gl_account\": \"4300\"}')",
    )
    .execute(&db.pool)
    .await
    .unwrap();
    match post_invoice(&db, "support").await {
        Err(LedgerError::UnknownAccount(code)) => assert_eq!(code, "4300"),
        other => panic!("expected UnknownAccount, got {other:?}"),
    }
}
