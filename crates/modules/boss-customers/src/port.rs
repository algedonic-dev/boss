//! Port (trait) for the customers domain. Adapters: PgCustomers
//! (postgres) + InMemoryCustomers (tests).

use async_trait::async_trait;
use chrono::{DateTime, Utc};

use crate::types::Customer;

#[derive(Debug, thiserror::Error)]
pub enum CustomersError {
    #[error("not found: {0}")]
    NotFound(String),
    #[error("storage: {0}")]
    Storage(String),
    #[error("invalid: {0}")]
    Invalid(String),
}

/// Refuse a customer Postgres cannot store: a NUL byte in any text field
/// or anywhere inside the metadata (TEXT and JSONB both reject one).
/// Both adapters call this before writing, so the refusal is `Invalid`
/// naming the field on each — until the adapters-agree suite (backlog
/// be459ab9) Postgres answered with its encoding error as `Storage` (a
/// 500) and the double stored the byte.
pub fn refuse_nul(customer: &Customer) -> Result<(), CustomersError> {
    fn json_holds_nul(v: &serde_json::Value) -> bool {
        match v {
            serde_json::Value::String(s) => s.contains('\0'),
            serde_json::Value::Array(a) => a.iter().any(json_holds_nul),
            serde_json::Value::Object(o) => {
                o.iter().any(|(k, v)| k.contains('\0') || json_holds_nul(v))
            }
            _ => false,
        }
    }
    let text = [
        ("id", Some(customer.id.as_str())),
        ("name", Some(customer.name.as_str())),
        ("email", customer.email.as_deref()),
        ("phone", customer.phone.as_deref()),
    ];
    let field = text
        .iter()
        .find(|(_, v)| v.is_some_and(|s| s.contains('\0')))
        .map(|(f, _)| *f)
        .or_else(|| json_holds_nul(&customer.metadata).then_some("metadata"));
    match field {
        Some(f) => Err(CustomersError::Invalid(format!(
            "{f} holds a NUL byte, which cannot be stored"
        ))),
        None => Ok(()),
    }
}

#[async_trait]
pub trait CustomersRepository: Send + Sync {
    /// Create a customer. Idempotent on `id` (ON CONFLICT DO
    /// NOTHING): re-POSTing an existing id reports `inserted =
    /// false` and emits nothing. A DIFFERENT id carrying an
    /// already-registered email is a caller bug and comes back
    /// `Invalid` (the partial unique index on `types::email_key`: ASCII
    /// whitespace trimmed, ASCII letters lowered, nothing else folded —
    /// backlog e1b08aaa).
    ///
    /// The Pg adapter does the whole birth in ONE transaction:
    /// domain row + `subjects` identity row (Q1 write-through) +
    /// `customers.customer.created` outbox event (#118); the double
    /// records the same fact (`events::customer_created`).
    ///
    /// The row is stored with `created_at = now` at the column's
    /// precision, the microsecond. A NUL byte anywhere is refused
    /// `Invalid` naming the field (`refuse_nul`).
    async fn create_customer_at(
        &self,
        customer: &Customer,
        now: DateTime<Utc>,
    ) -> Result<bool, CustomersError>;

    /// One customer; `None` for an id nobody holds — including one
    /// holding a NUL byte, which no stored id can.
    async fn get_customer(&self, id: &str) -> Result<Option<Customer>, CustomersError>;

    /// All customers, newest first; a tie on `created_at` in BYTE order
    /// of `id` (backlog 2987fb2d: never the database's locale).
    async fn list_customers(&self) -> Result<Vec<Customer>, CustomersError>;
}
