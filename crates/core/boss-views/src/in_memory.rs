//! In-memory `ViewsRepo` — the test adapter.
//!
//! Prefer this over mocks: View logic (visibility filtering, filter
//! validation on save) is exercised through the real port.

use std::collections::HashMap;
use std::sync::Mutex;

use async_trait::async_trait;
use chrono::{DateTime, Utc};

use crate::error::ViewsError;
use crate::filter;
use crate::port::ViewsRepo;
use crate::types::{View, ViewInput, Visibility};

pub struct InMemoryViewsRepo {
    rows: Mutex<HashMap<String, View>>,
    /// Monotonic id source. Deterministic ids keep tests readable and
    /// keep this adapter free of a clock or RNG dependency.
    next: Mutex<u64>,
    now: DateTime<Utc>,
}

impl InMemoryViewsRepo {
    pub fn new(now: DateTime<Utc>) -> Self {
        Self {
            rows: Mutex::new(HashMap::new()),
            next: Mutex::new(1),
            now,
        }
    }

    fn mint_id(&self) -> Result<String, ViewsError> {
        let mut n = self
            .next
            .lock()
            .map_err(|_| ViewsError::Storage("id lock poisoned".into()))?;
        let id = format!("view-{n}");
        *n += 1;
        Ok(id)
    }
}

#[async_trait]
impl ViewsRepo for InMemoryViewsRepo {
    async fn list_for_viewer(&self, viewer_id: &str) -> Result<Vec<View>, ViewsError> {
        let rows = self
            .rows
            .lock()
            .map_err(|_| ViewsError::Storage("lock poisoned".into()))?;
        let mut out: Vec<View> = rows
            .values()
            .filter(|v| v.owner_id == viewer_id || v.visibility == Visibility::Shared)
            .cloned()
            .collect();
        out.sort_by(|a, b| a.id.cmp(&b.id));
        Ok(out)
    }

    async fn get(&self, id: &str) -> Result<View, ViewsError> {
        let rows = self
            .rows
            .lock()
            .map_err(|_| ViewsError::Storage("lock poisoned".into()))?;
        rows.get(id)
            .cloned()
            .ok_or_else(|| ViewsError::NotFound(id.to_string()))
    }

    async fn create(&self, input: &ViewInput) -> Result<View, ViewsError> {
        filter::compile(&input.filter)?;
        let id = self.mint_id()?;
        let view = View {
            id: id.clone(),
            owner_id: input.owner_id.clone(),
            title: input.title.clone(),
            source: input.source,
            filter: input.filter.clone(),
            columns: input.columns.clone(),
            layout: input.layout,
            visibility: input.visibility,
            created_at: self.now,
            updated_at: self.now,
        };
        let mut rows = self
            .rows
            .lock()
            .map_err(|_| ViewsError::Storage("lock poisoned".into()))?;
        rows.insert(id, view.clone());
        Ok(view)
    }

    async fn replace(&self, id: &str, input: &ViewInput) -> Result<View, ViewsError> {
        filter::compile(&input.filter)?;
        let mut rows = self
            .rows
            .lock()
            .map_err(|_| ViewsError::Storage("lock poisoned".into()))?;
        let existing = rows
            .get(id)
            .cloned()
            .ok_or_else(|| ViewsError::NotFound(id.to_string()))?;
        let updated = View {
            id: existing.id,
            owner_id: input.owner_id.clone(),
            title: input.title.clone(),
            source: input.source,
            filter: input.filter.clone(),
            columns: input.columns.clone(),
            layout: input.layout,
            visibility: input.visibility,
            // Creation time survives a replace: it records when the
            // View came into being, which an edit does not change.
            created_at: existing.created_at,
            updated_at: self.now,
        };
        rows.insert(id.to_string(), updated.clone());
        Ok(updated)
    }

    async fn delete(&self, id: &str) -> Result<(), ViewsError> {
        let mut rows = self
            .rows
            .lock()
            .map_err(|_| ViewsError::Storage("lock poisoned".into()))?;
        rows.remove(id)
            .map(|_| ())
            .ok_or_else(|| ViewsError::NotFound(id.to_string()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{ViewLayout, ViewSource};

    fn repo() -> InMemoryViewsRepo {
        InMemoryViewsRepo::new(DateTime::from_timestamp(1_700_000_000, 0).expect("valid ts"))
    }

    fn input(owner: &str, title: &str, visibility: Visibility) -> ViewInput {
        ViewInput {
            owner_id: owner.to_string(),
            title: title.to_string(),
            source: ViewSource::Jobs,
            filter: String::new(),
            columns: vec![],
            layout: ViewLayout::Table,
            visibility,
        }
    }

    #[tokio::test]
    async fn a_private_view_is_visible_only_to_its_owner() {
        let r = repo();
        r.create(&input("alice", "Mine", Visibility::Private))
            .await
            .unwrap();

        assert_eq!(r.list_for_viewer("alice").await.unwrap().len(), 1);
        assert_eq!(r.list_for_viewer("bob").await.unwrap().len(), 0);
    }

    #[tokio::test]
    async fn a_shared_view_is_visible_to_everyone() {
        // Q4: sharing takes no promotion Job. Marking it shared IS the
        // act of sharing.
        let r = repo();
        r.create(&input("alice", "Ours", Visibility::Shared))
            .await
            .unwrap();

        assert_eq!(r.list_for_viewer("bob").await.unwrap().len(), 1);
    }

    #[tokio::test]
    async fn a_bad_filter_is_rejected_on_create_not_on_read() {
        let r = repo();
        let mut bad = input("alice", "Broken", Visibility::Private);
        bad.filter = "status =".to_string();

        let err = r.create(&bad).await.unwrap_err();
        assert!(matches!(err, ViewsError::InvalidFilter(_)));
        // And nothing was stored, so nobody can open it later.
        assert_eq!(r.list_for_viewer("alice").await.unwrap().len(), 0);
    }

    #[tokio::test]
    async fn replace_keeps_created_at_and_moves_updated_at() {
        let r = repo();
        let made = r
            .create(&input("alice", "First", Visibility::Private))
            .await
            .unwrap();

        let updated = r
            .replace(&made.id, &input("alice", "Second", Visibility::Shared))
            .await
            .unwrap();

        assert_eq!(updated.title, "Second");
        assert_eq!(updated.visibility, Visibility::Shared);
        assert_eq!(updated.created_at, made.created_at);
    }

    #[tokio::test]
    async fn deleting_a_missing_view_is_not_found() {
        let r = repo();
        assert!(matches!(
            r.delete("view-nope").await.unwrap_err(),
            ViewsError::NotFound(_)
        ));
    }
}
