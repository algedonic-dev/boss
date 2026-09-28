//! Revenue category → GL revenue account, read off the Class registry
//! (backlog aa860c6d, 2026-09-26).
//!
//! WHY. The map lived in `seeds/revenue_accounts.toml`, a platform file
//! `include_str!`-ed into the ledger with a `BOSS_LEDGER_REVENUE_ACCOUNTS_TOML`
//! override nothing set, so every instance ran the brewery's seven rows.
//! The categories themselves were already tenant data — each is a Class
//! `(invoice, revenue_category)` that commerce gates an invoice line
//! against — so a tenant that declared its own (Algedonic, LLC:
//! sponsorship, support, hosting, measured live 2026-09-26 beside its
//! declared accounts 4200 / 4300 / 4400) had a category commerce accepted
//! and the ledger refused. The account now lives ON the Class, as
//! `metadata.gl_account`, so the category and its account are one row
//! the tenant declares (CLAUDE.md §9, §9a): no second list to keep in
//! step with the first.
//!
//! RETIRED CLASSES STILL RESOLVE. A retired category takes no new
//! invoice lines (commerce's gate), but a rebuild re-posts the invoices
//! it already carried, so the posting map reads every row, retired or
//! not — the same log must project the same entries.
//!
//! The rules stay pure: [`crate::rules::BossRuleSet`] holds one of these
//! and the posting path loads it in the posting transaction (`load`),
//! the shape the `tax_kinds` hold already has.

use std::collections::{BTreeMap, BTreeSet};

use crate::error::LedgerError;

/// The Class `subject_kind` a revenue category is declared under.
pub const SUBJECT_KIND: &str = "invoice";
/// The Class `member_attribute` a revenue category is declared under.
pub const MEMBER_ATTRIBUTE: &str = "revenue_category";
/// The Class metadata key naming the category's GL revenue account.
pub const GL_ACCOUNT_KEY: &str = "gl_account";

/// The `gl_account` a Class's metadata names, if it names a non-empty
/// string there.
pub fn gl_account_of(metadata: &serde_json::Value) -> Option<String> {
    metadata
        .get(GL_ACCOUNT_KEY)
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
}

/// Every declared revenue category and the account its Class names —
/// `None` when the Class names none, which refuses at the post BY NAME
/// rather than reading as an unknown category.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RevenueAccounts {
    by_category: BTreeMap<String, Option<String>>,
}

impl RevenueAccounts {
    /// Build from `(category code, gl_account)` rows — one per Class.
    pub fn from_classes(rows: impl IntoIterator<Item = (String, Option<String>)>) -> Self {
        Self {
            by_category: rows.into_iter().collect(),
        }
    }

    /// The revenue account `category` posts to. `kind` is the fact
    /// kind the refusal is reported against.
    pub fn account_for(&self, kind: &str, category: &str) -> Result<&str, LedgerError> {
        match self.by_category.get(category) {
            Some(Some(account)) => Ok(account.as_str()),
            Some(None) => Err(LedgerError::InvalidPayload {
                kind: kind.to_string(),
                reason: format!(
                    "revenue category `{category}` is a Class ({SUBJECT_KIND}, \
                     {MEMBER_ATTRIBUTE}) whose metadata names no `{GL_ACCOUNT_KEY}` — \
                     declare the revenue account on the Class"
                ),
            }),
            None => Err(LedgerError::InvalidPayload {
                kind: kind.to_string(),
                reason: format!(
                    "unknown revenue category `{category}` — no Class ({SUBJECT_KIND}, \
                     {MEMBER_ATTRIBUTE}) declares it"
                ),
            }),
        }
    }

    /// Every `(category, account)` pair that resolves, in category
    /// order — what commerce inverts for its per-account COGS rollup.
    pub fn iter(&self) -> impl Iterator<Item = (&str, &str)> {
        self.by_category
            .iter()
            .filter_map(|(c, a)| a.as_deref().map(|a| (c.as_str(), a)))
    }
}

/// The tenant check's judgement of its revenue-category Classes: every
/// one names a `gl_account`, and — when the tenant's chart is on hand —
/// that account is one the chart declares as a revenue account.
/// `revenue_chart` is `None` when the tenant declares no chart file:
/// its accounts may be the instance's own (design e187198f), and the
/// post refuses an undeclared code by name (`UnknownAccount`).
pub fn validate_declarations(
    rows: &[(String, Option<String>)],
    revenue_chart: Option<&BTreeSet<String>>,
) -> Result<(), String> {
    for (code, account) in rows {
        let Some(account) = account else {
            return Err(format!(
                "revenue_category Class `{code}` names no `{GL_ACCOUNT_KEY}` in its metadata — \
                 an invoice line in it could not post; name its revenue account \
                 (\"metadata\": {{\"{GL_ACCOUNT_KEY}\": \"<code>\"}})"
            ));
        };
        if let Some(chart) = revenue_chart
            && !chart.contains(account)
        {
            return Err(format!(
                "revenue_category Class `{code}` names {GL_ACCOUNT_KEY} `{account}`, which \
                 seeds/chart_of_accounts.toml does not declare as a revenue account"
            ));
        }
    }
    Ok(())
}

#[cfg(feature = "postgres")]
pub use pg::load;

#[cfg(feature = "postgres")]
mod pg {
    use super::*;

    /// Read every `(invoice, revenue_category)` Class — retired ones
    /// included, so a rebuild re-posts what they carried — into the
    /// map the rules evaluate against.
    pub async fn load<'e>(exec: impl sqlx::PgExecutor<'e>) -> Result<RevenueAccounts, LedgerError> {
        let rows: Vec<(String, serde_json::Value)> = sqlx::query_as(
            "SELECT code, metadata FROM classes \
             WHERE subject_kind = $1 AND member_attribute = $2",
        )
        .bind(SUBJECT_KIND)
        .bind(MEMBER_ATTRIBUTE)
        .fetch_all(exec)
        .await
        .map_err(|e| LedgerError::Storage(e.to_string()))?;
        Ok(RevenueAccounts::from_classes(
            rows.into_iter().map(|(code, m)| (code, gl_account_of(&m))),
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn llc() -> RevenueAccounts {
        RevenueAccounts::from_classes([
            (
                "sponsorship".to_string(),
                gl_account_of(&json!({"gl_account": "4200"})),
            ),
            (
                "support".to_string(),
                gl_account_of(&json!({"gl_account": "4300"})),
            ),
            (
                "hosting".to_string(),
                gl_account_of(&json!({"gl_account": "4400"})),
            ),
        ])
    }

    #[test]
    fn a_category_resolves_to_the_account_its_class_names() {
        assert_eq!(llc().account_for("k", "hosting").unwrap(), "4400");
        assert_eq!(llc().account_for("k", "support").unwrap(), "4300");
    }

    #[test]
    fn an_undeclared_category_still_refuses_by_name() {
        let err = llc()
            .account_for("finance.invoice.issued", "wholesale")
            .unwrap_err();
        assert!(
            err.to_string()
                .contains("unknown revenue category `wholesale`"),
            "{err}"
        );
    }

    #[test]
    fn a_class_with_no_account_refuses_naming_the_class() {
        let m = RevenueAccounts::from_classes([("hosting".to_string(), gl_account_of(&json!({})))]);
        let err = m
            .account_for("finance.invoice.issued", "hosting")
            .unwrap_err();
        assert!(
            err.to_string().contains("`hosting`") && err.to_string().contains("gl_account"),
            "{err}"
        );
    }

    #[test]
    fn a_blank_account_is_no_account() {
        assert_eq!(gl_account_of(&json!({"gl_account": "  "})), None);
        assert_eq!(gl_account_of(&json!({"gl_account": 4400})), None);
    }

    #[test]
    fn every_declared_class_must_name_a_revenue_account_the_chart_declares() {
        let chart: BTreeSet<String> = ["4200", "4300"].iter().map(|s| s.to_string()).collect();
        let rows = vec![
            ("support".to_string(), Some("4300".to_string())),
            ("hosting".to_string(), Some("4400".to_string())),
        ];
        let err = validate_declarations(&rows, Some(&chart)).unwrap_err();
        assert!(err.contains("`hosting`") && err.contains("`4400`"), "{err}");
        // Without a chart file the account cannot be judged here, but a
        // Class that names none is still refused.
        validate_declarations(&rows, None).unwrap();
        let bare = vec![("hosting".to_string(), None)];
        let err = validate_declarations(&bare, None).unwrap_err();
        assert!(
            err.contains("`hosting`") && err.contains("gl_account"),
            "{err}"
        );
    }
}
