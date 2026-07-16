//! In-memory adapter — the test double (no mocks; a real
//! implementation of the port, minus durability).

use std::collections::BTreeMap;
use std::sync::Mutex;

use async_trait::async_trait;
use chrono::{DateTime, Utc};

use crate::port::{CustomersError, CustomersRepository};
use crate::types::Customer;

#[derive(Default)]
pub struct InMemoryCustomers {
    rows: Mutex<BTreeMap<String, Customer>>,
}

impl InMemoryCustomers {
    pub fn new() -> Self {
        Self::default()
    }
}

#[async_trait]
impl CustomersRepository for InMemoryCustomers {
    async fn create_customer_at(
        &self,
        customer: &Customer,
        now: DateTime<Utc>,
    ) -> Result<bool, CustomersError> {
        let mut rows = self.rows.lock().unwrap();
        if rows.contains_key(&customer.id) {
            return Ok(false);
        }
        // Mirror the Pg adapter's email-uniqueness semantics.
        if let Some(email) = customer.email.as_deref() {
            let needle = email.trim().to_lowercase();
            if rows.values().any(|c| {
                c.email
                    .as_deref()
                    .is_some_and(|e| e.trim().to_lowercase() == needle)
            }) {
                return Err(CustomersError::Invalid(format!(
                    "email already registered: {email}"
                )));
            }
        }
        let mut stored = customer.clone();
        stored.created_at = Some(now);
        rows.insert(customer.id.clone(), stored);
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
