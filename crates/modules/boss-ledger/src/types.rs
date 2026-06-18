//! Ledger domain types.
//!
//! `JournalEntryDraft` is what rules produce — a *proposed* entry keyed by
//! account codes (not UUIDs). The postgres layer resolves codes to account
//! UUIDs at insert time and writes the balanced rows.
//!
//! Amounts are integer cents (`i64`). Session 2 of the monetary-units
//! migration removed the `rust_decimal::Decimal` layer here — the schema
//! columns `debit_cents` / `credit_cents` are `BIGINT` now, and cents is
//! what flows through rules end-to-end.

use std::borrow::Cow;

use chrono::NaiveDate;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use uuid::Uuid;

/// Account classification. Controls which financial statement the balance
/// flows into (assets/liabilities/equity → balance sheet, revenue/expense
/// → income statement).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum AccountKind {
    Asset,
    Liability,
    Equity,
    Revenue,
    Expense,
}

/// Which side a balance "lives on" by convention. Assets and expenses are
/// debit-normal; liabilities, equity, and revenue are credit-normal.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum NormalSide {
    Debit,
    Credit,
}

/// Stable identifier for an account in the chart — rules target codes,
/// not UUIDs, so the chart can be reorganized without touching rule code.
///
/// Most rules use `&'static str` literals (e.g. `"1100"`), but manual
/// journal entries carry codes from an HTTP request body — hence `Cow`.
pub type AccountCode = Cow<'static, str>;

/// Reference to a financial fact, borrowed from the domain crate's
/// transaction. `id` must be the same UUID written to `financial_facts`
/// so the journal entry's `fact_id` column resolves.
pub struct FactRef<'a> {
    pub id: Uuid,
    pub kind: &'a str,
    pub happened_on: NaiveDate,
    pub payload: &'a Value,
}

/// A proposed journal entry produced by a rule. Not yet persisted; the
/// postgres layer resolves account codes to UUIDs and writes the rows.
#[derive(Debug, Clone, PartialEq)]
pub struct JournalEntryDraft {
    pub posted_on: NaiveDate,
    pub memo: Option<String>,
    pub lines: Vec<JournalLineDraft>,
}

/// One debit-or-credit line in a draft entry. Exactly one of `debit_cents`
/// or `credit_cents` is nonzero. Amounts are integer cents.
#[derive(Debug, Clone, PartialEq)]
pub struct JournalLineDraft {
    pub account_code: AccountCode,
    pub debit_cents: i64,
    pub credit_cents: i64,
    pub memo: Option<String>,
    pub sort_order: i16,
}

impl JournalLineDraft {
    pub fn debit(account_code: impl Into<AccountCode>, amount_cents: i64, sort_order: i16) -> Self {
        Self {
            account_code: account_code.into(),
            debit_cents: amount_cents,
            credit_cents: 0,
            memo: None,
            sort_order,
        }
    }

    pub fn credit(
        account_code: impl Into<AccountCode>,
        amount_cents: i64,
        sort_order: i16,
    ) -> Self {
        Self {
            account_code: account_code.into(),
            debit_cents: 0,
            credit_cents: amount_cents,
            memo: None,
            sort_order,
        }
    }
}

impl JournalEntryDraft {
    /// Sum of debit amounts across all lines (cents).
    pub fn total_debits(&self) -> i64 {
        self.lines.iter().map(|l| l.debit_cents).sum()
    }

    /// Sum of credit amounts across all lines (cents).
    pub fn total_credits(&self) -> i64 {
        self.lines.iter().map(|l| l.credit_cents).sum()
    }

    /// Double-entry invariant: debits and credits must balance to the cent.
    pub fn is_balanced(&self) -> bool {
        self.total_debits() == self.total_credits()
    }
}
