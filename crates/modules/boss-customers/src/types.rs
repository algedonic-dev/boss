//! Domain types for DTC customers.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

/// One end-consumer. Thin: identity + contact. Purchase history is
/// derivable from the Jobs/invoices that reference the customer —
/// no just-in-case rollup columns.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Customer {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub email: Option<String>,
    #[serde(default)]
    pub phone: Option<String>,
    /// Free-form: source channel, marketing consent, …
    #[serde(default = "default_metadata")]
    pub metadata: serde_json::Value,
    #[serde(default)]
    pub created_at: Option<DateTime<Utc>>,
}

fn default_metadata() -> serde_json::Value {
    serde_json::json!({})
}

/// The one key an email is unique under (backlog e1b08aaa): surrounding
/// ASCII whitespace trimmed, ASCII letters lowered, every other byte
/// kept as sent. The id mint, both adapters and the `customers_email`
/// index all answer with this rule and no other.
///
/// WHY ASCII ONLY. Until e1b08aaa there were three rules: the mint
/// trimmed Unicode whitespace and folded full Unicode case, the index was
/// `lower(email)` — no trim, and case folded by the DATABASE'S LOCALE —
/// and the double folded full Unicode case. So `' pat@x'` and `'pat@x'`
/// landed as two rows for one person under explicit ids, and whether
/// `É` and `é` were one address depended on how the cluster was
/// initdb'd. ASCII folding is the rule both sides compute identically
/// with no locale at all, and it never merges two addresses a mail
/// server could tell apart: every domain on the wire is ASCII
/// (IDNA), and a local part's non-ASCII case is the mailbox's own
/// business.
pub fn email_key(email: &str) -> String {
    email
        .trim_matches(|c: char| c.is_ascii_whitespace())
        .to_ascii_lowercase()
}

/// [`email_key`] spelled in SQL over a column named `email`, for the
/// index the migration builds. `char::is_ascii_whitespace` is exactly
/// space, tab, line feed, form feed and carriage return, so `btrim` is
/// handed those five; `translate` lowers A-Z and nothing else, with no
/// collation to consult. The migration file carries this text verbatim
/// and `tests/an_email_is_one_key_everywhere_pg.rs` holds it to the
/// file and to the Rust function on a real database.
pub const SQL_EMAIL_KEY: &str = "translate(btrim(email, E' \\t\\n\\f\\r'), \
     'ABCDEFGHIJKLMNOPQRSTUVWXYZ', 'abcdefghijklmnopqrstuvwxyz')";

/// The R3 mint: `cust-<sha256(email_key(email))[..12 hex]>`.
/// Deterministic — the same buyer re-checking-out lands on the same
/// row — and carries no PII. Sim births and operator tooling may
/// pass explicit ids instead.
pub fn id_from_email(email: &str) -> String {
    let digest = Sha256::digest(email_key(email).as_bytes());
    let hex: String = digest.iter().take(6).map(|b| format!("{b:02x}")).collect();
    format!("cust-{hex}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn id_from_email_is_deterministic_and_case_insensitive() {
        let a = id_from_email("Pat@Example.com");
        let b = id_from_email("  pat@example.com ");
        assert_eq!(a, b);
        assert!(a.starts_with("cust-"));
        assert_eq!(a.len(), "cust-".len() + 12);
    }

    #[test]
    fn the_key_trims_ascii_whitespace_and_lowers_ascii_letters_only() {
        assert_eq!(
            email_key(" \t\r\n\x0cPat@Example.COM \n"),
            "pat@example.com"
        );
        // Non-ASCII case is kept as sent, and so is non-ASCII space: a
        // no-break space is not trimmed.
        assert_eq!(email_key("\u{c9}lise@x.test"), "\u{c9}lise@x.test");
        assert_ne!(
            email_key("\u{c9}lise@x.test"),
            email_key("\u{e9}lise@x.test")
        );
        assert_eq!(email_key("\u{a0}pat@x.test"), "\u{a0}pat@x.test");
        // Vertical tab is not ASCII whitespace to Rust, so not to SQL.
        assert_eq!(email_key("\x0bpat@x.test"), "\x0bpat@x.test");
    }

    #[test]
    fn the_mint_is_the_key_hashed() {
        assert_eq!(id_from_email(" B@x.test"), id_from_email("b@x.test"));
        assert_ne!(
            id_from_email("\u{c9}lise@x.test"),
            id_from_email("\u{e9}lise@x.test"),
            "non-ASCII case is not folded by the mint either"
        );
        assert_ne!(
            id_from_email("\u{a0}b@x.test"),
            id_from_email("b@x.test"),
            "nor is non-ASCII space trimmed"
        );
    }
}
