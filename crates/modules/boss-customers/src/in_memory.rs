//! In-memory adapter — the test double (no mocks; a real
//! implementation of the port, minus durability). Held to Postgres by
//! `tests/the_adapters_agree_on_the_customer_registry_pg.rs` (backlog
//! be459ab9).

use std::collections::BTreeMap;
use std::sync::Mutex;

use async_trait::async_trait;
use chrono::{DateTime, DurationRound, TimeDelta, Utc};

use crate::port::{CustomersError, CustomersRepository, refuse_nul};
use crate::types::{Customer, email_key};

#[derive(Default)]
pub struct InMemoryCustomers {
    rows: Mutex<BTreeMap<String, Customer>>,
    recorded: Mutex<Vec<boss_core::event::Event>>,
}

impl InMemoryCustomers {
    pub fn new() -> Self {
        Self::default()
    }

    /// Events the create path recorded — test visibility (the
    /// in-memory analogue of the Pg adapter's in-tx outbox write).
    pub fn recorded_events(&self) -> Vec<boss_core::event::Event> {
        self.recorded.lock().map(|v| v.clone()).unwrap_or_default()
    }
}

#[async_trait]
impl CustomersRepository for InMemoryCustomers {
    async fn create_customer_at(
        &self,
        customer: &Customer,
        now: DateTime<Utc>,
    ) -> Result<bool, CustomersError> {
        refuse_nul(customer)?;
        let mut rows = self.rows.lock().unwrap();
        if rows.contains_key(&customer.id) {
            return Ok(false);
        }
        // The Pg adapter's partial unique index is on `email_key` (backlog
        // e1b08aaa): ASCII whitespace trimmed, ASCII letters lowered,
        // nothing else folded. Until then the index was `lower(email)`
        // under the database's locale and this folded full Unicode.
        if let Some(email) = customer.email.as_deref() {
            let needle = email_key(email);
            if rows
                .values()
                .any(|c| c.email.as_deref().is_some_and(|e| email_key(e) == needle))
            {
                return Err(CustomersError::Invalid(format!(
                    "email already registered: {email}"
                )));
            }
        }
        // The column keeps microseconds; so does the double, or one
        // write reads back unequal across adapters (backlog be459ab9).
        let at = now
            .duration_trunc(TimeDelta::microseconds(1))
            .unwrap_or(now);
        let mut stored = customer.clone();
        stored.created_at = Some(at);
        rows.insert(customer.id.clone(), stored);
        if let Ok(mut recorded) = self.recorded.lock() {
            recorded.push(crate::events::customer_created(customer, now));
        }
        Ok(true)
    }

    async fn get_customer(&self, id: &str) -> Result<Option<Customer>, CustomersError> {
        Ok(self.rows.lock().unwrap().get(id).cloned())
    }

    async fn list_customers(&self) -> Result<Vec<Customer>, CustomersError> {
        let rows = self.rows.lock().unwrap();
        let mut all: Vec<Customer> = rows.values().cloned().collect();
        all.sort_by(|a, b| b.created_at.cmp(&a.created_at).then(a.id.cmp(&b.id)));
        Ok(all)
    }
}
