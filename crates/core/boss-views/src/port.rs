//! The Views port — what the domain needs from storage.

use async_trait::async_trait;
use boss_policy_client::User;

use crate::error::ViewsError;
use crate::types::{View, ViewInput, ViewResults};

/// Storage for View definitions.
///
/// Every method that touches one View takes the caller's id. Ownership
/// is enforced HERE rather than in the HTTP layer so a second caller
/// of the port cannot forget it — the first version enforced nothing
/// anywhere, and `visibility: private` was a label with nothing
/// behind it.
#[async_trait]
pub trait ViewsRepo: Send + Sync {
    /// Views visible to `viewer_id`: everything they own, plus every
    /// shared View regardless of owner.
    async fn list_for_viewer(&self, viewer_id: &str) -> Result<Vec<View>, ViewsError>;

    /// One View, if this caller may see it — theirs, or shared.
    ///
    /// A View the caller may not see reports `NotFound`, not
    /// `Forbidden`: a distinct 403 confirms that someone else's
    /// private View exists at that id, which is the leak the status
    /// code was supposed to prevent.
    async fn get_for_viewer(&self, id: &str, viewer_id: &str) -> Result<View, ViewsError>;

    async fn create(&self, owner_id: &str, input: &ViewInput) -> Result<View, ViewsError>;

    /// Replace a View the caller owns. Someone else's — shared or not
    /// — is `NotFound`.
    async fn replace(
        &self,
        id: &str,
        owner_id: &str,
        input: &ViewInput,
    ) -> Result<View, ViewsError>;

    /// Delete a View the caller owns.
    async fn delete(&self, id: &str, owner_id: &str) -> Result<(), ViewsError>;
}

/// Running a View against the information layer.
///
/// Separate from [`ViewsRepo`] because they are different concerns
/// with different failure modes: one stores a definition, the other
/// reads the projections. A test can hold a real repo and a stub
/// resolver, which is the combination most View logic wants.
#[async_trait]
pub trait ViewResolver: Send + Sync {
    /// Resolve for a specific caller.
    ///
    /// The `user` is not decoration: a View reads `jobs` and
    /// `audit_log` directly, and those are policy-scoped everywhere
    /// else they are read. Without the caller here, a View is a way
    /// to read rows the caller's role cannot open through any surface
    /// — which is exactly what the first version was.
    async fn resolve(
        &self,
        view: &View,
        user: &User,
        limit: usize,
    ) -> Result<ViewResults, ViewsError>;
}
