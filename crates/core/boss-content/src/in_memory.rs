//! In-memory adapter — fast integration tests, no DB required.

use std::collections::{HashMap, HashSet};
use std::sync::Mutex;

use async_trait::async_trait;
use chrono::{NaiveDate, Utc};
use uuid::Uuid;

use crate::error::ContentError;
use crate::port::ContentRepository;
use crate::types::{
    Bulletin, BulletinDraft, BulletinPatch, ManualPatch, ManualSection, ManualSectionDraft,
    ManualSectionVersion, UserContext,
};

#[derive(Default)]
pub struct InMemoryContent {
    state: Mutex<State>,
}

#[derive(Default)]
struct State {
    bulletins: HashMap<Uuid, Bulletin>,
    dismissals: HashSet<(Uuid, String)>, // (bulletin_id, employee_id)
    manual: HashMap<String, ManualSection>, // slug → section
    manual_history: Vec<ManualSectionVersion>,
}

impl InMemoryContent {
    pub fn new() -> Self {
        Self::default()
    }
}

#[async_trait]
impl ContentRepository for InMemoryContent {
    async fn list_bulletins_for(
        &self,
        user: &UserContext,
        today: NaiveDate,
        include_dismissed: bool,
    ) -> Result<Vec<Bulletin>, ContentError> {
        let state = self.state.lock().map_err(poisoned)?;
        let mut out: Vec<Bulletin> = state
            .bulletins
            .values()
            .filter(|b| b.expires_on.is_none_or(|d| d >= today))
            .filter(|b| b.audience.matches(user))
            .map(|b| {
                let dismissed = state.dismissals.contains(&(b.id, user.id.clone()));
                Bulletin {
                    dismissed_by_viewer: dismissed,
                    ..b.clone()
                }
            })
            .filter(|b| include_dismissed || !b.dismissed_by_viewer)
            .collect();
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
        let state = self.state.lock().map_err(poisoned)?;
        let mut out: Vec<Bulletin> = state.bulletins.values().cloned().collect();
        out.sort_by_key(|b| std::cmp::Reverse(b.posted_on));
        Ok(out)
    }

    async fn get_bulletin(&self, id: Uuid) -> Result<Option<Bulletin>, ContentError> {
        let state = self.state.lock().map_err(poisoned)?;
        Ok(state.bulletins.get(&id).cloned())
    }

    async fn create_bulletin_at(
        &self,
        draft: BulletinDraft,
        actor_id: &str,
        now: chrono::DateTime<Utc>,
    ) -> Result<Bulletin, ContentError> {
        if draft.title.trim().is_empty() {
            return Err(ContentError::Validation("title is required".into()));
        }
        if draft.body.trim().is_empty() {
            return Err(ContentError::Validation("body is required".into()));
        }
        let id = draft.id.unwrap_or_else(Uuid::new_v4);
        let mut state = self.state.lock().map_err(poisoned)?;
        // Match the Pg adapter's ON CONFLICT DO NOTHING semantics:
        // if the id already exists, return the existing row.
        if let Some(existing) = state.bulletins.get(&id) {
            return Ok(existing.clone());
        }
        let bulletin = Bulletin {
            id,
            title: draft.title,
            body: draft.body,
            actor_id: actor_id.to_string(),
            posted_on: draft.posted_on.unwrap_or_else(|| now.date_naive()),
            expires_on: draft.expires_on,
            priority: draft.priority,
            audience: draft.audience,
            created_at: now,
            updated_at: now,
            dismissed_by_viewer: false,
        };
        state.bulletins.insert(bulletin.id, bulletin.clone());
        Ok(bulletin)
    }

    async fn update_bulletin_at(
        &self,
        id: Uuid,
        patch: BulletinPatch,
        now: chrono::DateTime<Utc>,
    ) -> Result<Bulletin, ContentError> {
        let mut state = self.state.lock().map_err(poisoned)?;
        let existing = state
            .bulletins
            .get_mut(&id)
            .ok_or_else(|| ContentError::NotFound(format!("bulletin {id}")))?;
        if let Some(title) = patch.title {
            if title.trim().is_empty() {
                return Err(ContentError::Validation("title must not be empty".into()));
            }
            existing.title = title;
        }
        if let Some(body) = patch.body {
            if body.trim().is_empty() {
                return Err(ContentError::Validation("body must not be empty".into()));
            }
            existing.body = body;
        }
        if let Some(expires) = patch.expires_on {
            existing.expires_on = expires;
        }
        if let Some(priority) = patch.priority {
            existing.priority = priority;
        }
        if let Some(audience) = patch.audience {
            existing.audience = audience;
        }
        existing.updated_at = now;
        Ok(existing.clone())
    }

    async fn delete_bulletin(&self, id: Uuid) -> Result<(), ContentError> {
        let mut state = self.state.lock().map_err(poisoned)?;
        if state.bulletins.remove(&id).is_none() {
            return Err(ContentError::NotFound(format!("bulletin {id}")));
        }
        state.dismissals.retain(|(bid, _)| *bid != id);
        Ok(())
    }

    async fn dismiss_bulletin_at(
        &self,
        id: Uuid,
        employee_id: &str,
        _now: chrono::DateTime<Utc>,
    ) -> Result<(), ContentError> {
        let mut state = self.state.lock().map_err(poisoned)?;
        if !state.bulletins.contains_key(&id) {
            return Err(ContentError::NotFound(format!("bulletin {id}")));
        }
        state.dismissals.insert((id, employee_id.to_string()));
        Ok(())
    }

    async fn manual_tree(&self, user: &UserContext) -> Result<Vec<ManualSection>, ContentError> {
        let state = self.state.lock().map_err(poisoned)?;
        let mut out: Vec<ManualSection> = state
            .manual
            .values()
            .filter(|s| s.published)
            .filter(|s| s.audience.matches(user))
            .cloned()
            .collect();
        out.sort_by(|a, b| {
            a.parent_slug
                .cmp(&b.parent_slug)
                .then(a.sort_order.cmp(&b.sort_order))
                .then(a.title.cmp(&b.title))
        });
        Ok(out)
    }

    async fn get_section(
        &self,
        slug: &str,
        user: &UserContext,
    ) -> Result<Option<ManualSection>, ContentError> {
        let state = self.state.lock().map_err(poisoned)?;
        Ok(state
            .manual
            .get(slug)
            .filter(|s| s.published && s.audience.matches(user))
            .cloned())
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
        let mut state = self.state.lock().map_err(poisoned)?;
        if state.manual.contains_key(&draft.slug) {
            return Err(ContentError::Validation(format!(
                "slug '{}' already exists",
                draft.slug
            )));
        }
        if let Some(ref parent) = draft.parent_slug
            && !state.manual.contains_key(parent)
        {
            return Err(ContentError::Validation(format!(
                "parent slug '{parent}' not found"
            )));
        }
        let now = Utc::now();
        let section = ManualSection {
            id: Uuid::new_v4(),
            slug: draft.slug.clone(),
            parent_slug: draft.parent_slug,
            title: draft.title.clone(),
            body: draft.body.clone(),
            sort_order: draft.sort_order,
            audience: draft.audience.clone(),
            current_version: 1,
            published: draft.published,
            created_at: now,
            updated_at: now,
        };
        state.manual.insert(section.slug.clone(), section.clone());
        state.manual_history.push(ManualSectionVersion {
            section_id: section.id,
            version: 1,
            title: section.title.clone(),
            body: section.body.clone(),
            audience: section.audience.clone(),
            edited_by: editor_id.to_string(),
            edited_at: now,
            reason: Some("initial version".into()),
        });
        Ok(section)
    }

    async fn update_section(
        &self,
        slug: &str,
        patch: ManualPatch,
        editor_id: &str,
    ) -> Result<ManualSection, ContentError> {
        let mut state = self.state.lock().map_err(poisoned)?;
        let existing = state
            .manual
            .get_mut(slug)
            .ok_or_else(|| ContentError::NotFound(format!("section {slug}")))?;
        if let Some(title) = patch.title {
            if title.trim().is_empty() {
                return Err(ContentError::Validation("title must not be empty".into()));
            }
            existing.title = title;
        }
        if let Some(body) = patch.body {
            existing.body = body;
        }
        if let Some(audience) = patch.audience {
            existing.audience = audience;
        }
        if let Some(sort_order) = patch.sort_order {
            existing.sort_order = sort_order;
        }
        if let Some(published) = patch.published {
            existing.published = published;
        }
        existing.current_version += 1;
        existing.updated_at = Utc::now();
        let snapshot = ManualSectionVersion {
            section_id: existing.id,
            version: existing.current_version,
            title: existing.title.clone(),
            body: existing.body.clone(),
            audience: existing.audience.clone(),
            edited_by: editor_id.to_string(),
            edited_at: existing.updated_at,
            reason: patch.reason,
        };
        let snapshot_out = existing.clone();
        state.manual_history.push(snapshot);
        Ok(snapshot_out)
    }

    async fn section_history(&self, slug: &str) -> Result<Vec<ManualSectionVersion>, ContentError> {
        let state = self.state.lock().map_err(poisoned)?;
        let section = state
            .manual
            .get(slug)
            .ok_or_else(|| ContentError::NotFound(format!("section {slug}")))?;
        let mut out: Vec<ManualSectionVersion> = state
            .manual_history
            .iter()
            .filter(|v| v.section_id == section.id)
            .cloned()
            .collect();
        out.sort_by_key(|v| std::cmp::Reverse(v.version));
        Ok(out)
    }
}

fn poisoned<T>(_: std::sync::PoisonError<T>) -> ContentError {
    ContentError::Storage("in-memory lock poisoned".into())
}
