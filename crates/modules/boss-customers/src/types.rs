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

/// The R3 mint: `cust-<sha256(lowercased email)[..12 hex]>`.
/// Deterministic — the same buyer re-checking-out lands on the same
/// row — and carries no PII. Sim births and operator tooling may
/// pass explicit ids instead.
pub fn id_from_email(email: &str) -> String {
    let digest = Sha256::digest(email.trim().to_lowercase().as_bytes());
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
}
