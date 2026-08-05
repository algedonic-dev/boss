//! The Views port — what the domain needs from storage.

use async_trait::async_trait;

use crate::error::ViewsError;
use crate::types::{View, ViewInput, ViewResults};

/// Storage for View definitions.
#[async_trait]
pub trait ViewsRepo: Send + Sync {
    /// Views visible to `viewer_id`: everything they own, plus every
    /// shared View regardless of owner.
    async fn list_for_viewer(&self, viewer_id: &str) -> Result<Vec<View>, ViewsError>;

    async fn get(&self, id: &str) -> Result<View, ViewsError>;

    async fn create(&self, input: &ViewInput) -> Result<View, ViewsError>;

    async fn replace(&self, id: &str, input: &ViewInput) -> Result<View, ViewsError>;

    async fn delete(&self, id: &str) -> Result<(), ViewsError>;
}

/// Running a View against the information layer.
///
/// Separate from [`ViewsRepo`] because they are different concerns
/// with different failure modes: one stores a definition, the other
/// reads the projections. A test can hold a real repo and a stub
/// resolver, which is the combination most View logic wants.
#[async_trait]
pub trait ViewResolver: Send + Sync {
    async fn resolve(&self, view: &View, limit: usize) -> Result<ViewResults, ViewsError>;
}
