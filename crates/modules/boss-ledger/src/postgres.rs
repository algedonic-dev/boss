//! Postgres write path for the ledger projection.
//!
//! `post_fact_in_tx` is the single entry point domain crates call from
//! inside their write transaction. It:
//!
//! 1. Evaluates the active rule for the fact (RuleSet v1 today; a hardcoded
//!    dispatch for now — v2 will look up the active row in
//!    `gl_rule_versions` at startup).
//! 2. Auto-creates the monthly `gl_periods` row if one doesn't yet exist.
//! 3. Resolves draft account codes to chart UUIDs.
//! 4. Inserts `gl_journal_entries` + `gl_journal_lines` rows.
//! 5. The deferred trigger checks the double-entry invariant at commit.
//!
//! Idempotency: `gl_journal_entries` has a `UNIQUE (fact_id, rule_version_id)`
//! constraint. A re-post of the same fact is a no-op.

use chrono::{Datelike, NaiveDate};
use sqlx::{Postgres, Transaction};
use uuid::Uuid;

use crate::error::LedgerError;
use crate::rules::{BossRuleSet, evaluate, is_gl_inert};
use crate::types::{FactRef, JournalEntryDraft};

/// Fixed UUID of the active BOSS RuleSet — matches the seed in
/// `schema/40-ledger.sql`. A future shape change introduces a sibling
/// `RULE_SET_V2_ID` + RuleSet impl alongside this one and historical
/// rows stay pinned to their original `rule_version_id`.
pub const RULE_SET_ID: Uuid = Uuid::from_u128(0x0000_0000_0000_0000_0000_0000_0000_0001);

fn evaluate_active(fact: &FactRef<'_>) -> Result<(JournalEntryDraft, Uuid), LedgerError> {
    let draft = evaluate(&BossRuleSet, fact)?;
    Ok((draft, RULE_SET_ID))
}

/// Project a fact to journal entries in the given transaction. Called by
/// domain crates after writing the fact row. Idempotent — re-posting the
/// same fact is a no-op thanks to `UNIQUE (fact_id, rule_version_id)`.
pub async fn post_fact_in_tx(
    tx: &mut Transaction<'_, Postgres>,
    fact: &FactRef<'_>,
) -> Result<(), LedgerError> {
    // GL-inert kinds (dedup/audit-only facts like
    // `finance.inventory.received`) post NO journal entry. Skip BEFORE
    // evaluating a RuleSet — the rebuild + replay-check stages iterate
    // every fact row, and an inert kind has no RuleSet arm; without this
    // guard it would hit `UnknownFactKind` and fail the whole rebuild.
    if is_gl_inert(fact.kind) {
        return Ok(());
    }
    let (draft, rule_version_id) = evaluate_active(fact)?;

    // Early-return if a row already exists for this (fact, ruleset). Saves
    // a period-lookup and chart-lookup on replay.
    let existing: Option<(Uuid,)> = sqlx::query_as(
        "SELECT id FROM gl_journal_entries \
         WHERE fact_id = $1 AND rule_version_id = $2",
    )
    .bind(fact.id)
    .bind(rule_version_id)
    .fetch_optional(&mut **tx)
    .await
    .map_err(|e| LedgerError::Storage(e.to_string()))?;
    if existing.is_some() {
        return Ok(());
    }

    let period_id = ensure_period_for(tx, draft.posted_on).await?;

    // Reject writes to a locked period up-front with a clear error. The
    // DB trigger is defense-in-depth, but we'd rather surface a clean
    // `LockedPeriod` than a Postgres RAISE.
    let period_status: String = sqlx::query_scalar("SELECT status FROM gl_periods WHERE id = $1")
        .bind(period_id)
        .fetch_one(&mut **tx)
        .await
        .map_err(|e| LedgerError::Storage(e.to_string()))?;
    if period_status == "locked" {
        return Err(LedgerError::LockedPeriod {
            period_id,
            happened_on: draft.posted_on,
        });
    }
    let account_ids = resolve_account_codes(tx, &draft).await?;

    // Invariant: 1000 Cash must not go negative. Real-world a bank
    // would refuse the transfer; in
    // the books a JE that drives cash below zero is recording a
    // payment that could never have happened. Compute the net
    // delta on 1000 from this draft; query current balance; reject
    // if posting would land 1000 below 0.
    //
    // This surfaces the model imbalance (payroll burn > revenue
    // collection): when the brewery runs out of cash, the next
    // payroll JE 422s instead of silently overdrawing.
    let cash_delta_cents: i64 = draft
        .lines
        .iter()
        .filter(|l| &*l.account_code == "1000")
        .map(|l| l.debit_cents - l.credit_cents)
        .sum();
    if cash_delta_cents < 0 {
        let current_cash_cents: i64 = sqlx::query_scalar(
            "SELECT COALESCE(SUM(l.debit_cents - l.credit_cents), 0)::bigint \
             FROM gl_journal_lines l \
             JOIN gl_accounts a ON a.id = l.account_id \
             WHERE a.code = '1000'",
        )
        .fetch_one(&mut **tx)
        .await
        .map_err(|e| LedgerError::Storage(e.to_string()))?;
        let proposed_cash_cents = current_cash_cents + cash_delta_cents;
        if proposed_cash_cents < 0 {
            return Err(LedgerError::InvalidPayload {
                kind: fact.kind.to_string(),
                reason: format!(
                    "would drive 1000 Cash to ${:.2} (current ${:.2}, draft delta ${:.2}); \
                     real-world the bank refuses the transfer. Increase opening capital, \
                     collect more revenue, or add a Line of Credit.",
                    proposed_cash_cents as f64 / 100.0,
                    current_cash_cents as f64 / 100.0,
                    cash_delta_cents as f64 / 100.0,
                ),
            });
        }
    }

    insert_entry(
        tx,
        fact.id,
        rule_version_id,
        period_id,
        &draft,
        &account_ids,
    )
    .await?;
    Ok(())
}

/// Auto-create the monthly period containing `posted_on` if it doesn't
/// exist. Returns the period id.
async fn ensure_period_for(
    tx: &mut Transaction<'_, Postgres>,
    posted_on: NaiveDate,
) -> Result<Uuid, LedgerError> {
    let starts_on = NaiveDate::from_ymd_opt(posted_on.year(), posted_on.month(), 1)
        .expect("first of month always valid");
    let ends_on = match posted_on.month() {
        12 => NaiveDate::from_ymd_opt(posted_on.year() + 1, 1, 1),
        m => NaiveDate::from_ymd_opt(posted_on.year(), m + 1, 1),
    }
    .and_then(|d| d.pred_opt())
    .expect("last of month always valid");

    let existing: Option<(Uuid,)> =
        sqlx::query_as("SELECT id FROM gl_periods WHERE kind = 'month' AND starts_on = $1")
            .bind(starts_on)
            .fetch_optional(&mut **tx)
            .await
            .map_err(|e| LedgerError::Storage(e.to_string()))?;
    if let Some((id,)) = existing {
        return Ok(id);
    }

    let id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO gl_periods (id, kind, starts_on, ends_on, status) \
         VALUES ($1, 'month', $2, $3, 'open') \
         ON CONFLICT (kind, starts_on) DO NOTHING",
    )
    .bind(id)
    .bind(starts_on)
    .bind(ends_on)
    .execute(&mut **tx)
    .await
    .map_err(|e| LedgerError::Storage(e.to_string()))?;

    // Conflict path: someone else created the row concurrently. Read it
    // back rather than assuming our insert won.
    let (id,): (Uuid,) =
        sqlx::query_as("SELECT id FROM gl_periods WHERE kind = 'month' AND starts_on = $1")
            .bind(starts_on)
            .fetch_one(&mut **tx)
            .await
            .map_err(|e| LedgerError::Storage(e.to_string()))?;
    Ok(id)
}

/// Resolve all account codes in the draft to UUIDs in one query.
async fn resolve_account_codes(
    tx: &mut Transaction<'_, Postgres>,
    draft: &JournalEntryDraft,
) -> Result<std::collections::HashMap<String, Uuid>, LedgerError> {
    let codes: Vec<String> = draft
        .lines
        .iter()
        .map(|l| l.account_code.to_string())
        .collect();
    let rows: Vec<(String, Uuid)> =
        sqlx::query_as("SELECT code, id FROM gl_accounts WHERE code = ANY($1)")
            .bind(&codes)
            .fetch_all(&mut **tx)
            .await
            .map_err(|e| LedgerError::Storage(e.to_string()))?;
    let map: std::collections::HashMap<String, Uuid> = rows.into_iter().collect();
    for code in &codes {
        if !map.contains_key(code) {
            return Err(LedgerError::UnknownAccount(code.clone()));
        }
    }
    Ok(map)
}

/// Insert a journal entry directly into an explicit period, bypassing
/// the monthly auto-assignment in `ensure_period_for`. Used only by
/// the year-end close handler: closing entries are dated on Dec 31
/// but must live in the yearly period, not December's monthly one.
///
/// Caller is responsible for the `financial_facts` insert + the
/// outer transaction boundary. The function resolves account codes
/// and posts lines in the same tx. Uses the currently-active rule
/// version.
pub async fn insert_closing_entry(
    tx: &mut Transaction<'_, Postgres>,
    fact_id: Uuid,
    yearly_period_id: Uuid,
    draft: &JournalEntryDraft,
) -> Result<(), LedgerError> {
    let (rule_version_id,): (Uuid,) =
        sqlx::query_as("SELECT id FROM gl_rule_versions WHERE is_active = true")
            .fetch_one(&mut **tx)
            .await
            .map_err(|e| LedgerError::Storage(e.to_string()))?;
    let account_ids = resolve_account_codes(tx, draft).await?;
    insert_entry(
        tx,
        fact_id,
        rule_version_id,
        yearly_period_id,
        draft,
        &account_ids,
    )
    .await
}

async fn insert_entry(
    tx: &mut Transaction<'_, Postgres>,
    fact_id: Uuid,
    rule_version_id: Uuid,
    period_id: Uuid,
    draft: &JournalEntryDraft,
    account_ids: &std::collections::HashMap<String, Uuid>,
) -> Result<(), LedgerError> {
    let entry_id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO gl_journal_entries \
            (id, fact_id, rule_version_id, posted_on, period_id, memo) \
         VALUES ($1, $2, $3, $4, $5, $6) \
         ON CONFLICT (fact_id, rule_version_id) DO NOTHING",
    )
    .bind(entry_id)
    .bind(fact_id)
    .bind(rule_version_id)
    .bind(draft.posted_on)
    .bind(period_id)
    .bind(&draft.memo)
    .execute(&mut **tx)
    .await
    .map_err(|e| LedgerError::Storage(e.to_string()))?;

    for line in &draft.lines {
        let account_id = account_ids[line.account_code.as_ref()];
        sqlx::query(
            "INSERT INTO gl_journal_lines \
                (id, journal_entry_id, account_id, debit_cents, credit_cents, currency, memo, sort_order) \
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8)",
        )
        .bind(Uuid::new_v4())
        .bind(entry_id)
        .bind(account_id)
        .bind(line.debit_cents)
        .bind(line.credit_cents)
        .bind("USD")
        .bind(&line.memo)
        .bind(line.sort_order)
        .execute(&mut **tx)
        .await
        .map_err(|e| LedgerError::Storage(e.to_string()))?;
    }
    Ok(())
}
