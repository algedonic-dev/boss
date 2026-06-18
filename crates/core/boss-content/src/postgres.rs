//! Postgres adapter.

use async_trait::async_trait;
use chrono::NaiveDate;
use sqlx::{PgPool, Row};
use uuid::Uuid;

use crate::error::ContentError;
use crate::port::ContentRepository;
use crate::types::{
    Audience, Bulletin, BulletinDraft, BulletinPatch, BulletinPriority, ManualPatch, ManualSection,
    ManualSectionDraft, ManualSectionVersion, UserContext,
};

pub struct PgContent {
    pool: PgPool,
}

impl PgContent {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }

    /// Fetch a section by slug regardless of audience/published flags.
    /// Used after writes so the author always sees the row they just
    /// saved even if its audience excludes them.
    async fn fetch_section_raw(&self, slug: &str) -> Result<Option<ManualSection>, ContentError> {
        let row = sqlx::query(
            "SELECT id, slug, parent_slug, title, body, sort_order, audience, \
                    current_version, published, created_at, updated_at \
             FROM manual_sections WHERE slug = $1",
        )
        .bind(slug)
        .fetch_optional(&self.pool)
        .await
        .map_err(store)?;
        row.as_ref().map(row_to_section).transpose()
    }
}

#[async_trait]
impl ContentRepository for PgContent {
    async fn list_bulletins_for(
        &self,
        user: &UserContext,
        today: NaiveDate,
        include_dismissed: bool,
    ) -> Result<Vec<Bulletin>, ContentError> {
        // Fetch all active rows; filter by audience + dismissal in
        // Rust. Audience can't be expressed cleanly in SQL without
        // teaching it about the `{all,departments,roles}` shape;
        // doing it in Rust keeps the predicate logic in one place
        // and is O(n) over bulletins (small).
        let rows = sqlx::query(
            "SELECT b.id, b.title, b.body, b.actor_id, b.posted_on, \
                    b.expires_on, b.priority, b.audience, \
                    b.created_at, b.updated_at, \
                    (d.employee_id IS NOT NULL) AS dismissed \
             FROM bulletins b \
             LEFT JOIN bulletin_dismissals d \
                    ON d.bulletin_id = b.id AND d.employee_id = $1 \
             WHERE b.expires_on IS NULL OR b.expires_on >= $2",
        )
        .bind(&user.id)
        .bind(today)
        .fetch_all(&self.pool)
        .await
        .map_err(|e| ContentError::Storage(e.to_string()))?;

        let mut out = Vec::with_capacity(rows.len());
        for row in rows {
            let b = row_to_bulletin(&row)?;
            if !b.audience.matches(user) {
                continue;
            }
            if !include_dismissed && b.dismissed_by_viewer {
                continue;
            }
            out.push(b);
        }
        out.sort_by(|a, b| {
            a.priority
                .sort_key()
                .cmp(&b.priority.sort_key())
                .then(b.posted_on.cmp(&a.posted_on))
                .then(b.created_at.cmp(&a.created_at))
        });
        Ok(out)
    }

    async fn list_all_bulletins(&self) -> Result<Vec<Bulletin>, ContentError> {
        let rows = sqlx::query(
            "SELECT id, title, body, actor_id, posted_on, expires_on, \
                    priority, audience, created_at, updated_at, \
                    false AS dismissed \
             FROM bulletins \
             ORDER BY posted_on DESC, created_at DESC",
        )
        .fetch_all(&self.pool)
        .await
        .map_err(|e| ContentError::Storage(e.to_string()))?;
        rows.into_iter().map(|r| row_to_bulletin(&r)).collect()
    }

    async fn get_bulletin(&self, id: Uuid) -> Result<Option<Bulletin>, ContentError> {
        let row = sqlx::query(
            "SELECT id, title, body, actor_id, posted_on, expires_on, \
                    priority, audience, created_at, updated_at, \
                    false AS dismissed \
             FROM bulletins WHERE id = $1",
        )
        .bind(id)
        .fetch_optional(&self.pool)
        .await
        .map_err(|e| ContentError::Storage(e.to_string()))?;
        row.as_ref().map(row_to_bulletin).transpose()
    }

    async fn create_bulletin_at(
        &self,
        draft: BulletinDraft,
        actor_id: &str,
        now: chrono::DateTime<chrono::Utc>,
    ) -> Result<Bulletin, ContentError> {
        if draft.title.trim().is_empty() {
            return Err(ContentError::Validation("title is required".into()));
        }
        if draft.body.trim().is_empty() {
            return Err(ContentError::Validation("body is required".into()));
        }
        // Client-supplied id takes precedence so retries (network
        // double-submit, browser double-click) land on the same
        // row via ON CONFLICT DO NOTHING. Without that, every retry
        // creates a duplicate bulletin.
        let id = draft.id.unwrap_or_else(Uuid::new_v4);
        let posted_on = draft.posted_on.unwrap_or_else(|| now.date_naive());
        sqlx::query(
            "INSERT INTO bulletins \
                (id, title, body, actor_id, posted_on, expires_on, priority, audience, \
                 created_at, updated_at) \
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $9) \
             ON CONFLICT (id) DO NOTHING",
        )
        .bind(id)
        .bind(&draft.title)
        .bind(&draft.body)
        .bind(actor_id)
        .bind(posted_on)
        .bind(draft.expires_on)
        .bind(draft.priority.as_str())
        .bind(&draft.audience.0)
        .bind(now)
        .execute(&self.pool)
        .await
        .map_err(|e| ContentError::Storage(e.to_string()))?;
        self.get_bulletin(id)
            .await?
            .ok_or_else(|| ContentError::Storage("just-created bulletin vanished".into()))
    }

    async fn update_bulletin_at(
        &self,
        id: Uuid,
        patch: BulletinPatch,
        now: chrono::DateTime<chrono::Utc>,
    ) -> Result<Bulletin, ContentError> {
        // Pull the current row so we can merge the patch without a
        // dynamic UPDATE builder. Small rows, small fields.
        let current = self
            .get_bulletin(id)
            .await?
            .ok_or_else(|| ContentError::NotFound(format!("bulletin {id}")))?;

        let title = match patch.title {
            Some(t) if t.trim().is_empty() => {
                return Err(ContentError::Validation("title must not be empty".into()));
            }
            Some(t) => t,
            None => current.title,
        };
        let body = match patch.body {
            Some(b) if b.trim().is_empty() => {
                return Err(ContentError::Validation("body must not be empty".into()));
            }
            Some(b) => b,
            None => current.body,
        };
        let expires_on = match patch.expires_on {
            Some(v) => v,
            None => current.expires_on,
        };
        let priority = patch.priority.unwrap_or(current.priority);
        let audience = patch.audience.unwrap_or(current.audience);

        sqlx::query(
            "UPDATE bulletins SET \
                 title = $2, body = $3, expires_on = $4, \
                 priority = $5, audience = $6, updated_at = $7 \
             WHERE id = $1",
        )
        .bind(id)
        .bind(&title)
        .bind(&body)
        .bind(expires_on)
        .bind(priority.as_str())
        .bind(&audience.0)
        .bind(now)
        .execute(&self.pool)
        .await
        .map_err(|e| ContentError::Storage(e.to_string()))?;

        self.get_bulletin(id)
            .await?
            .ok_or_else(|| ContentError::Storage("bulletin vanished mid-update".into()))
    }

    async fn delete_bulletin(&self, id: Uuid) -> Result<(), ContentError> {
        let res = sqlx::query("DELETE FROM bulletins WHERE id = $1")
            .bind(id)
            .execute(&self.pool)
            .await
            .map_err(|e| ContentError::Storage(e.to_string()))?;
        if res.rows_affected() == 0 {
            return Err(ContentError::NotFound(format!("bulletin {id}")));
        }
        Ok(())
    }

    async fn dismiss_bulletin_at(
        &self,
        id: Uuid,
        employee_id: &str,
        now: chrono::DateTime<chrono::Utc>,
    ) -> Result<(), ContentError> {
        // Idempotent: ON CONFLICT DO NOTHING so repeat dismissals are
        // no-ops. Validate the bulletin exists first so a stale client
        // gets 404 rather than a silent success.
        let exists: Option<(Uuid,)> = sqlx::query_as("SELECT id FROM bulletins WHERE id = $1")
            .bind(id)
            .fetch_optional(&self.pool)
            .await
            .map_err(|e| ContentError::Storage(e.to_string()))?;
        if exists.is_none() {
            return Err(ContentError::NotFound(format!("bulletin {id}")));
        }
        sqlx::query(
            "INSERT INTO bulletin_dismissals (bulletin_id, employee_id, dismissed_at) \
             VALUES ($1, $2, $3) ON CONFLICT DO NOTHING",
        )
        .bind(id)
        .bind(employee_id)
        .bind(now)
        .execute(&self.pool)
        .await
        .map_err(|e| ContentError::Storage(e.to_string()))?;
        Ok(())
    }

    async fn manual_tree(&self, user: &UserContext) -> Result<Vec<ManualSection>, ContentError> {
        let rows = sqlx::query(
            "SELECT id, slug, parent_slug, title, body, sort_order, audience, \
                    current_version, published, created_at, updated_at \
             FROM manual_sections \
             WHERE published = true \
             ORDER BY parent_slug NULLS FIRST, sort_order, title",
        )
        .fetch_all(&self.pool)
        .await
        .map_err(store)?;
        let all: Vec<ManualSection> = rows.iter().map(row_to_section).collect::<Result<_, _>>()?;
        Ok(all
            .into_iter()
            .filter(|s| s.audience.matches(user))
            .collect())
    }

    async fn get_section(
        &self,
        slug: &str,
        user: &UserContext,
    ) -> Result<Option<ManualSection>, ContentError> {
        let row = sqlx::query(
            "SELECT id, slug, parent_slug, title, body, sort_order, audience, \
                    current_version, published, created_at, updated_at \
             FROM manual_sections WHERE slug = $1",
        )
        .bind(slug)
        .fetch_optional(&self.pool)
        .await
        .map_err(store)?;
        let Some(row) = row else { return Ok(None) };
        let section = row_to_section(&row)?;
        if !section.published || !section.audience.matches(user) {
            return Ok(None);
        }
        Ok(Some(section))
    }

    async fn create_section(
        &self,
        draft: ManualSectionDraft,
        editor_id: &str,
    ) -> Result<ManualSection, ContentError> {
        if draft.slug.trim().is_empty() {
            return Err(ContentError::Validation("slug is required".into()));
        }
        if draft.title.trim().is_empty() {
            return Err(ContentError::Validation("title is required".into()));
        }
        let id = Uuid::new_v4();
        let mut tx = self.pool.begin().await.map_err(store)?;
        sqlx::query(
            "INSERT INTO manual_sections \
                (id, slug, parent_slug, title, body, sort_order, audience, \
                 current_version, published) \
             VALUES ($1, $2, $3, $4, $5, $6, $7, 1, $8)",
        )
        .bind(id)
        .bind(&draft.slug)
        .bind(&draft.parent_slug)
        .bind(&draft.title)
        .bind(&draft.body)
        .bind(draft.sort_order)
        .bind(&draft.audience.0)
        .bind(draft.published)
        .execute(&mut *tx)
        .await
        .map_err(|e| ContentError::Validation(e.to_string()))?;
        sqlx::query(
            "INSERT INTO manual_section_history \
                (section_id, version, title, body, audience, edited_by, reason) \
             VALUES ($1, 1, $2, $3, $4, $5, $6)",
        )
        .bind(id)
        .bind(&draft.title)
        .bind(&draft.body)
        .bind(&draft.audience.0)
        .bind(editor_id)
        .bind("initial version")
        .execute(&mut *tx)
        .await
        .map_err(store)?;
        tx.commit().await.map_err(store)?;

        self.fetch_section_raw(&draft.slug)
            .await?
            .ok_or_else(|| ContentError::Storage("just-created section vanished".into()))
    }

    async fn update_section(
        &self,
        slug: &str,
        patch: ManualPatch,
        editor_id: &str,
    ) -> Result<ManualSection, ContentError> {
        let mut tx = self.pool.begin().await.map_err(store)?;
        let row = sqlx::query(
            "SELECT id, slug, parent_slug, title, body, sort_order, audience, \
                    current_version, published, created_at, updated_at \
             FROM manual_sections WHERE slug = $1 FOR UPDATE",
        )
        .bind(slug)
        .fetch_optional(&mut *tx)
        .await
        .map_err(store)?
        .ok_or_else(|| ContentError::NotFound(format!("section {slug}")))?;
        let current = row_to_section(&row)?;

        let title = match patch.title {
            Some(t) if t.trim().is_empty() => {
                return Err(ContentError::Validation("title must not be empty".into()));
            }
            Some(t) => t,
            None => current.title.clone(),
        };
        let body = patch.body.unwrap_or(current.body.clone());
        let audience = patch.audience.unwrap_or(current.audience.clone());
        let sort_order = patch.sort_order.unwrap_or(current.sort_order);
        let published = patch.published.unwrap_or(current.published);
        let new_version = current.current_version + 1;

        sqlx::query(
            "UPDATE manual_sections SET \
                title = $2, body = $3, audience = $4, sort_order = $5, \
                published = $6, current_version = $7, updated_at = NOW() \
             WHERE id = $1",
        )
        .bind(current.id)
        .bind(&title)
        .bind(&body)
        .bind(&audience.0)
        .bind(sort_order)
        .bind(published)
        .bind(new_version)
        .execute(&mut *tx)
        .await
        .map_err(store)?;
        sqlx::query(
            "INSERT INTO manual_section_history \
                (section_id, version, title, body, audience, edited_by, reason) \
             VALUES ($1, $2, $3, $4, $5, $6, $7)",
        )
        .bind(current.id)
        .bind(new_version)
        .bind(&title)
        .bind(&body)
        .bind(&audience.0)
        .bind(editor_id)
        .bind(patch.reason.as_deref())
        .execute(&mut *tx)
        .await
        .map_err(store)?;
        tx.commit().await.map_err(store)?;

        self.fetch_section_raw(slug)
            .await?
            .ok_or_else(|| ContentError::Storage("section vanished mid-update".into()))
    }

    async fn section_history(&self, slug: &str) -> Result<Vec<ManualSectionVersion>, ContentError> {
        let id: Option<(Uuid,)> = sqlx::query_as("SELECT id FROM manual_sections WHERE slug = $1")
            .bind(slug)
            .fetch_optional(&self.pool)
            .await
            .map_err(store)?;
        let Some((section_id,)) = id else {
            return Err(ContentError::NotFound(format!("section {slug}")));
        };
        let rows = sqlx::query(
            "SELECT section_id, version, title, body, audience, edited_by, \
                    edited_at, reason \
             FROM manual_section_history \
             WHERE section_id = $1 \
             ORDER BY version DESC",
        )
        .bind(section_id)
        .fetch_all(&self.pool)
        .await
        .map_err(store)?;
        rows.iter().map(row_to_version).collect()
    }
}

fn row_to_bulletin(row: &sqlx::postgres::PgRow) -> Result<Bulletin, ContentError> {
    let priority_str: String = row
        .try_get("priority")
        .map_err(|e| ContentError::Storage(e.to_string()))?;
    let priority = BulletinPriority::parse(&priority_str)
        .ok_or_else(|| ContentError::Storage(format!("unknown priority `{priority_str}`")))?;
    let audience_value: serde_json::Value = row
        .try_get("audience")
        .map_err(|e| ContentError::Storage(e.to_string()))?;
    Ok(Bulletin {
        id: row.try_get("id").map_err(store)?,
        title: row.try_get("title").map_err(store)?,
        body: row.try_get("body").map_err(store)?,
        actor_id: row.try_get("actor_id").map_err(store)?,
        posted_on: row.try_get("posted_on").map_err(store)?,
        expires_on: row.try_get("expires_on").map_err(store)?,
        priority,
        audience: Audience(audience_value),
        created_at: row.try_get("created_at").map_err(store)?,
        updated_at: row.try_get("updated_at").map_err(store)?,
        dismissed_by_viewer: row.try_get::<bool, _>("dismissed").unwrap_or(false),
    })
}

fn store(e: sqlx::Error) -> ContentError {
    ContentError::Storage(e.to_string())
}

fn row_to_section(row: &sqlx::postgres::PgRow) -> Result<ManualSection, ContentError> {
    let audience_value: serde_json::Value = row.try_get("audience").map_err(store)?;
    Ok(ManualSection {
        id: row.try_get("id").map_err(store)?,
        slug: row.try_get("slug").map_err(store)?,
        parent_slug: row.try_get("parent_slug").map_err(store)?,
        title: row.try_get("title").map_err(store)?,
        body: row.try_get("body").map_err(store)?,
        sort_order: row.try_get("sort_order").map_err(store)?,
        audience: Audience(audience_value),
        current_version: row.try_get("current_version").map_err(store)?,
        published: row.try_get("published").map_err(store)?,
        created_at: row.try_get("created_at").map_err(store)?,
        updated_at: row.try_get("updated_at").map_err(store)?,
    })
}

fn row_to_version(row: &sqlx::postgres::PgRow) -> Result<ManualSectionVersion, ContentError> {
    let audience_value: serde_json::Value = row.try_get("audience").map_err(store)?;
    Ok(ManualSectionVersion {
        section_id: row.try_get("section_id").map_err(store)?,
        version: row.try_get("version").map_err(store)?,
        title: row.try_get("title").map_err(store)?,
        body: row.try_get("body").map_err(store)?,
        audience: Audience(audience_value),
        edited_by: row.try_get("edited_by").map_err(store)?,
        edited_at: row.try_get("edited_at").map_err(store)?,
        reason: row.try_get("reason").map_err(store)?,
    })
}
