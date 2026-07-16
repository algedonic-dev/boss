//! Postgres adapter. The create path is the crate's contract in one
//! transaction: domain row + `subjects` identity row (Q1
//! write-through) + `customers.customer.created` outbox event
//! (#118 transactional outbox). All three land or none do.

use async_trait::async_trait;
use boss_core::event::Event;
use chrono::{DateTime, Utc};
use sqlx::PgPool;

use crate::port::{CustomersError, CustomersRepository};
use crate::types::Customer;

pub struct PgCustomers {
    pool: PgPool,
}

impl PgCustomers {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }
}

fn storage(e: impl std::fmt::Display) -> CustomersError {
    CustomersError::Storage(e.to_string())
}

#[async_trait]
impl CustomersRepository for PgCustomers {
    async fn create_customer_at(
        &self,
        customer: &Customer,
        now: DateTime<Utc>,
    ) -> Result<bool, CustomersError> {
        let mut tx = self.pool.begin().await.map_err(storage)?;
        let insert = sqlx::query(
            "INSERT INTO customers (id, name, email, phone, metadata, created_at) \
             VALUES ($1, $2, $3, $4, $5, $6) \
             ON CONFLICT (id) DO NOTHING",
        )
        .bind(&customer.id)
        .bind(&customer.name)
        .bind(&customer.email)
        .bind(&customer.phone)
        .bind(&customer.metadata)
        .bind(now)
        .execute(&mut *tx)
        .await;
        let inserted = match insert {
            Ok(r) => r.rows_affected() > 0,
            // A DIFFERENT id carrying an already-registered email
            // trips the partial unique index — a caller bug, not a
            // storage fault. (Same-id re-creates are absorbed by the
            // ON CONFLICT above and never reach here.)
            Err(e) if e.to_string().contains("customers_email") => {
                return Err(CustomersError::Invalid(format!(
                    "email already registered: {}",
                    customer.email.as_deref().unwrap_or("")
                )));
            }
            Err(e) => return Err(storage(e)),
        };

        if inserted {
            boss_subject_kinds::subjects::record_subject_in_tx(
                &mut tx,
                "customer",
                &customer.id,
                Some(&customer.name),
            )
            .await
            .map_err(CustomersError::Storage)?;

            // email/phone ride the payload: the log is the system of
            // record, and a column the log doesn't carry is a column
            // every rebuild silently loses (live-vs-rebuilt
            // divergence). This adds no exposure the system doesn't
            // already have — /shop writes customer_email into Job
            // metadata, which flows through jobs.job.created events
            // today.
            let payload = serde_json::json!({
                "id": customer.id,
                "name": customer.name,
                "email": customer.email,
                "phone": customer.phone,
                "metadata": customer.metadata,
            });
            let event = Event::new("boss-customers", "customers.customer.created", payload, now);
            boss_events::outbox::record_event_in_tx(&mut tx, &event)
                .await
                .map_err(CustomersError::Storage)?;
        }

        tx.commit().await.map_err(storage)?;
        Ok(inserted)
    }

    async fn get_customer(&self, id: &str) -> Result<Option<Customer>, CustomersError> {
        let row = sqlx::query_as::<_, CustomerRow>(
            "SELECT id, name, email, phone, metadata, created_at \
             FROM customers WHERE id = $1",
        )
        .bind(id)
        .fetch_optional(&self.pool)
        .await
        .map_err(storage)?;
        Ok(row.map(Customer::from))
    }

    async fn list_customers(&self) -> Result<Vec<Customer>, CustomersError> {
        let rows = sqlx::query_as::<_, CustomerRow>(
            "SELECT id, name, email, phone, metadata, created_at \
             FROM customers ORDER BY created_at DESC, id",
        )
        .fetch_all(&self.pool)
        .await
        .map_err(storage)?;
        Ok(rows.into_iter().map(Customer::from).collect())
    }
}

#[derive(sqlx::FromRow)]
struct CustomerRow {
    id: String,
    name: String,
    email: Option<String>,
    phone: Option<String>,
    metadata: serde_json::Value,
    created_at: DateTime<Utc>,
}

impl From<CustomerRow> for Customer {
    fn from(r: CustomerRow) -> Self {
        Customer {
            id: r.id,
            name: r.name,
            email: r.email,
            phone: r.phone,
            metadata: r.metadata,
            created_at: Some(r.created_at),
        }
    }
}
