//! Postgres adapter for the Marketing Asset KB.

use sqlx::{PgPool, Row, postgres::PgRow};

use super::types::{MarketingAsset, NewMarketingAsset, UpdateMarketingAsset};
use crate::port::KbError;

pub struct PgMarketingAssets {
    pool: PgPool,
}

impl PgMarketingAssets {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }

    pub async fn create(&self, new: NewMarketingAsset) -> Result<MarketingAsset, KbError> {
        let tags = serde_json::to_value(&new.tags).unwrap_or(serde_json::json!([]));
        let skus = serde_json::to_value(&new.linked_device_skus).unwrap_or(serde_json::json!([]));
        let accounts =
            serde_json::to_value(&new.linked_account_ids).unwrap_or(serde_json::json!([]));
        let campaigns =
            serde_json::to_value(&new.linked_campaign_ids).unwrap_or(serde_json::json!([]));
        let mut tx = self
            .pool
            .begin()
            .await
            .map_err(|e| KbError::Storage(e.to_string()))?;
        // Identity write-through (subject-model R1, Q1).
        boss_subject_kinds::subjects::record_subject_in_tx(
            &mut tx,
            "marketing-asset",
            &new.id,
            Some(&new.title),
        )
        .await
        .map_err(KbError::Storage)?;
        let row = sqlx::query(
            "INSERT INTO marketing_assets \
                (id, title, kind, description, file_url, tags, \
                 linked_device_skus, linked_account_ids, linked_campaign_ids, \
                 owner_id, brand_reviewed_by, brand_reviewed_at, supersedes_id) \
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13) \
             ON CONFLICT (id) DO UPDATE SET \
                title = EXCLUDED.title, \
                kind = EXCLUDED.kind, \
                description = EXCLUDED.description, \
                file_url = EXCLUDED.file_url, \
                tags = EXCLUDED.tags, \
                linked_device_skus = EXCLUDED.linked_device_skus, \
                linked_account_ids = EXCLUDED.linked_account_ids, \
                linked_campaign_ids = EXCLUDED.linked_campaign_ids, \
                owner_id = EXCLUDED.owner_id, \
                brand_reviewed_by = EXCLUDED.brand_reviewed_by, \
                brand_reviewed_at = EXCLUDED.brand_reviewed_at, \
                supersedes_id = EXCLUDED.supersedes_id, \
                updated_at = NOW() \
             RETURNING *",
        )
        .bind(&new.id)
        .bind(&new.title)
        .bind(new.kind.as_deref())
        .bind(new.description.as_deref())
        .bind(new.file_url.as_deref())
        .bind(&tags)
        .bind(&skus)
        .bind(&accounts)
        .bind(&campaigns)
        .bind(new.owner_id.as_deref())
        .bind(new.brand_reviewed_by.as_deref())
        .bind(new.brand_reviewed_at)
        .bind(new.supersedes_id.as_deref())
        .fetch_one(&mut *tx)
        .await
        .map_err(store)?;
        tx.commit()
            .await
            .map_err(|e| KbError::Storage(e.to_string()))?;
        Ok(row_to_asset(&row))
    }

    pub async fn get(&self, id: &str) -> Result<Option<MarketingAsset>, KbError> {
        let row = sqlx::query("SELECT * FROM marketing_assets WHERE id = $1")
            .bind(id)
            .fetch_optional(&self.pool)
            .await
            .map_err(store)?;
        Ok(row.as_ref().map(row_to_asset))
    }

    /// List assets, optionally filtered by kind + retired status.
    /// Ordered most-recent first.
    pub async fn list(
        &self,
        kind: Option<&str>,
        include_retired: bool,
        limit: i64,
    ) -> Result<Vec<MarketingAsset>, KbError> {
        let rows = sqlx::query(
            "SELECT * FROM marketing_assets \
             WHERE ($1::text IS NULL OR kind = $1) \
               AND ($2 OR retired_at IS NULL) \
             ORDER BY created_at DESC \
             LIMIT $3",
        )
        .bind(kind)
        .bind(include_retired)
        .bind(limit.clamp(1, 1000))
        .fetch_all(&self.pool)
        .await
        .map_err(store)?;
        Ok(rows.iter().map(row_to_asset).collect())
    }

    pub async fn update(
        &self,
        id: &str,
        patch: UpdateMarketingAsset,
    ) -> Result<Option<MarketingAsset>, KbError> {
        // Fetch + merge + overwrite. Cleaner than a dynamic SQL
        // UPDATE — the row is small, race window is tiny, and the
        // audit trail of the UPDATE shows the full new state.
        let Some(existing) = self.get(id).await? else {
            return Ok(None);
        };
        let merged = NewMarketingAsset {
            id: existing.id.clone(),
            title: patch.title.unwrap_or(existing.title.clone()),
            kind: existing.kind.clone(),
            description: patch.description.or(existing.description.clone()),
            file_url: patch.file_url.or(existing.file_url.clone()),
            tags: patch.tags.unwrap_or_else(|| existing.tags.clone()),
            linked_device_skus: patch
                .linked_device_skus
                .unwrap_or_else(|| existing.linked_device_skus.clone()),
            linked_account_ids: patch
                .linked_account_ids
                .unwrap_or_else(|| existing.linked_account_ids.clone()),
            linked_campaign_ids: patch
                .linked_campaign_ids
                .unwrap_or_else(|| existing.linked_campaign_ids.clone()),
            owner_id: patch.owner_id.or(existing.owner_id.clone()),
            brand_reviewed_by: patch
                .brand_reviewed_by
                .or(existing.brand_reviewed_by.clone()),
            brand_reviewed_at: patch.brand_reviewed_at.or(existing.brand_reviewed_at),
            supersedes_id: existing.supersedes_id.clone(),
        };
        self.create(merged).await.map(Some)
    }

    /// Soft-retire an asset. Keeps the row queryable; the default
    /// list endpoint filters retired rows out via `include_retired`.
    pub async fn retire(&self, id: &str) -> Result<Option<MarketingAsset>, KbError> {
        let row = sqlx::query(
            "UPDATE marketing_assets \
                SET retired_at = NOW(), updated_at = NOW() \
              WHERE id = $1 AND retired_at IS NULL \
              RETURNING *",
        )
        .bind(id)
        .fetch_optional(&self.pool)
        .await
        .map_err(store)?;
        Ok(row.as_ref().map(row_to_asset))
    }

    /// Walk the `supersedes_id` chain back to the root. Returns the
    /// chain in oldest → newest order, with the newest being the
    /// current version. Cheap: chain length is at most a handful.
    pub async fn history(&self, id: &str) -> Result<Vec<MarketingAsset>, KbError> {
        let rows = sqlx::query(
            "WITH RECURSIVE chain AS ( \
                 SELECT * FROM marketing_assets WHERE id = $1 \
                 UNION \
                 SELECT m.* FROM marketing_assets m \
                 JOIN chain c ON c.supersedes_id = m.id \
             ) \
             SELECT * FROM chain ORDER BY created_at ASC",
        )
        .bind(id)
        .fetch_all(&self.pool)
        .await
        .map_err(store)?;
        Ok(rows.iter().map(row_to_asset).collect())
    }
}

fn store(e: sqlx::Error) -> KbError {
    KbError::Storage(e.to_string())
}

fn row_to_asset(row: &PgRow) -> MarketingAsset {
    let as_strings = |col: &str| -> Vec<String> {
        row.try_get::<serde_json::Value, _>(col)
            .ok()
            .and_then(|v| serde_json::from_value(v).ok())
            .unwrap_or_default()
    };
    MarketingAsset {
        id: row.get("id"),
        title: row.get("title"),
        kind: row.try_get("kind").ok(),
        description: row.try_get("description").ok(),
        file_url: row.try_get("file_url").ok(),
        tags: as_strings("tags"),
        linked_device_skus: as_strings("linked_device_skus"),
        linked_account_ids: as_strings("linked_account_ids"),
        linked_campaign_ids: as_strings("linked_campaign_ids"),
        owner_id: row.try_get("owner_id").ok(),
        brand_reviewed_by: row.try_get("brand_reviewed_by").ok(),
        brand_reviewed_at: row.try_get("brand_reviewed_at").ok(),
        supersedes_id: row.try_get("supersedes_id").ok(),
        retired_at: row.try_get("retired_at").ok(),
        created_at: row.get("created_at"),
        updated_at: row.get("updated_at"),
    }
}
