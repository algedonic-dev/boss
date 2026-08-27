//! Port: what persistence must do for the content service.

use async_trait::async_trait;
use boss_core::actor::ActorId;
use boss_core::publisher::EventStamp;
use chrono::{DateTime, NaiveDate, Utc};
use uuid::Uuid;

use crate::error::ContentError;
use crate::types::{
    Bulletin, BulletinDraft, BulletinPatch, ManualPatch, ManualSection, ManualSectionDraft,
    ManualSectionVersion, UserContext,
};

#[async_trait]
pub trait ContentRepository: Send + Sync {
    /// Return the live bulletins for this user as of `today`, sorted
    /// priority-desc then posted_on-desc. Excludes expired rows and
    /// (when `include_dismissed=false`) rows the user already
    /// dismissed.
    async fn list_bulletins_for(
        &self,
        user: &UserContext,
        today: NaiveDate,
        include_dismissed: bool,
    ) -> Result<Vec<Bulletin>, ContentError>;

    /// Return every bulletin, active or expired, for the admin surface.
    /// No audience filter — this is the HR-author view.
    async fn list_all_bulletins(&self) -> Result<Vec<Bulletin>, ContentError>;

    async fn get_bulletin(&self, id: Uuid) -> Result<Option<Bulletin>, ContentError>;

    /// OUTBOX (phase 2): records `content.bulletin.created` (full
    /// row state) in-tx — only when the INSERT actually inserted (a
    /// double-submit retry collapses on ON CONFLICT (id) and records
    /// nothing).
    async fn create_bulletin(
        &self,
        draft: BulletinDraft,
        actor_id: &str,
    ) -> Result<Bulletin, ContentError> {
        let stamp = EventStamp::new("content", ActorId::Automation("platform".into()));
        self.create_bulletin_at(draft, actor_id, stamp.timestamp, &stamp)
            .await
    }
    async fn create_bulletin_at(
        &self,
        draft: BulletinDraft,
        actor_id: &str,
        now: DateTime<Utc>,
        stamp: &EventStamp,
    ) -> Result<Bulletin, ContentError>;

    /// Records `content.bulletin.updated` (full row state) in-tx.
    async fn update_bulletin(
        &self,
        id: Uuid,
        patch: BulletinPatch,
    ) -> Result<Bulletin, ContentError> {
        let stamp = EventStamp::new("content", ActorId::Automation("platform".into()));
        self.update_bulletin_at(id, patch, stamp.timestamp, &stamp)
            .await
    }
    async fn update_bulletin_at(
        &self,
        id: Uuid,
        patch: BulletinPatch,
        now: DateTime<Utc>,
        stamp: &EventStamp,
    ) -> Result<Bulletin, ContentError>;

    /// Records `content.bulletin.deleted` (`{id, deleted_at}`) in-tx
    /// after the row actually deleted.
    async fn delete_bulletin(&self, id: Uuid) -> Result<(), ContentError> {
        let stamp = EventStamp::new("content", ActorId::Automation("platform".into()));
        self.delete_bulletin_at(id, stamp.timestamp, &stamp).await
    }
    async fn delete_bulletin_at(
        &self,
        id: Uuid,
        now: DateTime<Utc>,
        stamp: &EventStamp,
    ) -> Result<(), ContentError>;

    /// Mark a bulletin dismissed for one employee. Idempotent — second
    /// call is a no-op. Records `content.bulletin.dismissed` in-tx —
    /// only when the dismissal row actually inserted (a repeat
    /// dismissal records nothing).
    async fn dismiss_bulletin(&self, id: Uuid, employee_id: &str) -> Result<(), ContentError> {
        let stamp = EventStamp::new("content", ActorId::Automation("platform".into()));
        self.dismiss_bulletin_at(id, employee_id, stamp.timestamp, &stamp)
            .await
    }
    async fn dismiss_bulletin_at(
        &self,
        id: Uuid,
        employee_id: &str,
        now: DateTime<Utc>,
        stamp: &EventStamp,
    ) -> Result<(), ContentError>;

    // --- Manual ---------------------------------------------------------

    /// List every published section visible to this user, sorted by
    /// (parent_slug, sort_order, title). Intended for tree rendering —
    /// the client reassembles the hierarchy from `parent_slug` links.
    async fn manual_tree(&self, user: &UserContext) -> Result<Vec<ManualSection>, ContentError>;

    async fn get_section(
        &self,
        slug: &str,
        user: &UserContext,
    ) -> Result<Option<ManualSection>, ContentError>;

    /// Insert a new section. Creates the v1 history snapshot as part
    /// of the insert.
    async fn create_section(
        &self,
        draft: ManualSectionDraft,
        editor_id: &str,
    ) -> Result<ManualSection, ContentError>;

    /// Apply a patch, bumping `current_version` and writing an
    /// append-only history row with the pre-patch state.
    async fn update_section(
        &self,
        slug: &str,
        patch: ManualPatch,
        editor_id: &str,
    ) -> Result<ManualSection, ContentError>;

    /// Version history for a section, newest first.
    async fn section_history(&self, slug: &str) -> Result<Vec<ManualSectionVersion>, ContentError>;
}
