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

#[async_trait]
pub trait CustomersRepository: Send + Sync {
    /// Create a customer. Idempotent on `id` (ON CONFLICT DO
    /// NOTHING): re-POSTing an existing id reports `inserted =
    /// false` and emits nothing. A DIFFERENT id carrying an
    /// already-registered email is a caller bug and comes back
    /// `Invalid` (the partial unique index on lower(email)).
    ///
    /// The Pg adapter does the whole birth in ONE transaction:
    /// domain row + `subjects` identity row (Q1 write-through) +
    /// `customers.customer.created` outbox event (#118).
    async fn create_customer_at(
        &self,
        customer: &Customer,
        now: DateTime<Utc>,
    ) -> Result<bool, CustomersError>;

    async fn get_customer(&self, id: &str) -> Result<Option<Customer>, CustomersError>;

    /// All customers, newest first.
    async fn list_customers(&self) -> Result<Vec<Customer>, CustomersError>;
}
