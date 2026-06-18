//! Postgres adapter for [`LocationRepository`]. Queries the
//! single `locations` table.

use async_trait::async_trait;
use boss_core::primitives::Location;
use chrono::{DateTime, Utc};
use serde_json::Value;
use sqlx::PgPool;

use crate::port::{LocationError, LocationRepository};

pub struct PgLocations {
    pool: PgPool,
}

impl PgLocations {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }
}

/// Row shape mirroring the `locations` table. Kept private so
/// `boss-core::Location` doesn't need a `sqlx::FromRow` derive
/// (which would force `sqlx` into `boss-core`'s dep graph).
#[derive(sqlx::FromRow)]
struct LocationRow {
    id: String,
    name: String,
    kind: String,
    parent_id: Option<String>,
    timezone: String,
    latitude: Option<f64>,
    longitude: Option<f64>,
    address: Option<String>,
    account_id: Option<String>,
    metadata: Value,
    retired_at: Option<DateTime<Utc>>,
}

impl From<LocationRow> for Location {
    fn from(r: LocationRow) -> Self {
        Self {
            id: r.id,
            name: r.name,
            kind: r.kind,
            parent_id: r.parent_id,
            timezone: r.timezone,
            latitude: r.latitude,
            longitude: r.longitude,
            address: r.address,
            account_id: r.account_id,
            metadata: r.metadata,
            retired_at: r.retired_at,
        }
    }
}

const SELECT_COLUMNS: &str = "id, name, kind, parent_id, timezone, latitude, longitude, \
                              address, account_id, metadata, retired_at";

#[async_trait]
impl LocationRepository for PgLocations {
    async fn get(&self, id: &str) -> Result<Option<Location>, LocationError> {
        let sql = format!("SELECT {SELECT_COLUMNS} FROM locations WHERE id = $1");
        let row: Option<LocationRow> = sqlx::query_as(&sql)
            .bind(id)
            .fetch_optional(&self.pool)
            .await
            .map_err(|e| LocationError::Storage(e.to_string()))?;
        Ok(row.map(Into::into))
    }

    async fn exists_active(&self, id: &str) -> Result<bool, LocationError> {
        let exists: bool = sqlx::query_scalar(
            "SELECT EXISTS(SELECT 1 FROM locations WHERE id = $1 AND retired_at IS NULL)",
        )
        .bind(id)
        .fetch_one(&self.pool)
        .await
        .map_err(|e| LocationError::Storage(e.to_string()))?;
        Ok(exists)
    }

    async fn list_for_kind(&self, kind: &str) -> Result<Vec<Location>, LocationError> {
        let sql = format!(
            "SELECT {SELECT_COLUMNS} FROM locations \
             WHERE kind = $1 AND retired_at IS NULL \
             ORDER BY name ASC"
        );
        let rows: Vec<LocationRow> = sqlx::query_as(&sql)
            .bind(kind)
            .fetch_all(&self.pool)
            .await
            .map_err(|e| LocationError::Storage(e.to_string()))?;
        Ok(rows.into_iter().map(Into::into).collect())
    }

    async fn children_of(&self, parent_id: Option<&str>) -> Result<Vec<Location>, LocationError> {
        // Distinct queries for the IS NULL vs. = $1 cases keep the
        // SQL planner-friendly and avoid the `parent_id IS NOT
        // DISTINCT FROM $1` shape that prevents index use.
        let rows: Vec<LocationRow> = if let Some(pid) = parent_id {
            let sql = format!(
                "SELECT {SELECT_COLUMNS} FROM locations \
                 WHERE retired_at IS NULL AND parent_id = $1 \
                 ORDER BY name ASC"
            );
            sqlx::query_as(&sql).bind(pid).fetch_all(&self.pool).await
        } else {
            let sql = format!(
                "SELECT {SELECT_COLUMNS} FROM locations \
                 WHERE retired_at IS NULL AND parent_id IS NULL \
                 ORDER BY name ASC"
            );
            sqlx::query_as(&sql).fetch_all(&self.pool).await
        }
        .map_err(|e| LocationError::Storage(e.to_string()))?;
        Ok(rows.into_iter().map(Into::into).collect())
    }
}
