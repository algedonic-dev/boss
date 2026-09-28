//! Postgres adapter for [`TenantPublishes`]: the `tenant_publishes` row
//! (infra/postgres/schema/20260918200200-…) and its `tenant.published`
//! outbox event in ONE transaction — the body the CLI's `PgStamps`
//! ran until backlog 42da8bd2 moved it behind this door.

use async_trait::async_trait;
use sqlx::PgPool;

use super::{Stamp, TenantPublishes, TenantPublishesError, published_event};

pub struct PgTenantPublishes {
    pool: PgPool,
}

impl PgTenantPublishes {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }
}

fn storage(what: &str, e: impl std::fmt::Display) -> TenantPublishesError {
    TenantPublishesError::Storage(format!("{what}: {e}"))
}

#[async_trait]
impl TenantPublishes for PgTenantPublishes {
    async fn record(&self, stamp: &Stamp) -> Result<(), TenantPublishesError> {
        // The row and its fact commit or abort together: the event is
        // staged on the outbox inside the row's transaction (backlog
        // dbdc4d31), so a stamp never exists without the log entry a
        // rebuilder would reproduce it from, nor the entry without
        // the row.
        let mut tx = self
            .pool
            .begin()
            .await
            .map_err(|e| storage("opening the tenant publish stamp transaction", e))?;
        sqlx::query(
            "INSERT INTO tenant_publishes \
             (tenant_id, published_at, published_by, boss_commit, took, writes) \
             VALUES ($1, $2, $3, $4, $5, $6)",
        )
        .bind(&stamp.tenant_id)
        .bind(stamp.published_at)
        .bind(&stamp.published_by)
        .bind(&stamp.boss_commit)
        .bind(&stamp.took)
        .bind(stamp.writes)
        .execute(&mut *tx)
        .await
        .map_err(|e| storage("recording the stamp in tenant_publishes", e))?;
        boss_events::outbox::record_event_in_tx(&mut tx, &published_event(stamp))
            .await
            .map_err(|e| storage("staging tenant.published on the event outbox", e))?;
        tx.commit()
            .await
            .map_err(|e| storage("committing the stamp and its event", e))?;
        Ok(())
    }
}
