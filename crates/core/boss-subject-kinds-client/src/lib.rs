//! HTTP client port for reaching the `boss-subject-kinds` registry.
//!
//! Services validate a Subject's kind against the registry before
//! accepting writes — same shape as `boss-classes-client` /
//! `boss-locations-client`. Trait + reqwest adapter live here so any
//! consumer can call the same canonical contract.

use async_trait::async_trait;
use boss_core::http_client::{self, HttpClientError, ServiceLabel};

/// Service-name marker for the shared [`HttpClientError`]. Keeps the
/// `Display` text reading `"subject-kinds service unreachable: …"`.
#[derive(Debug)]
pub struct SubjectKinds;
impl ServiceLabel for SubjectKinds {
    const NAME: &'static str = "subject-kinds";
}

/// Transport error for the SubjectKinds client. Alias of the shared
/// [`HttpClientError`] so existing constructors and matches keep
/// compiling.
pub type SubjectKindsClientError = HttpClientError<SubjectKinds>;

/// Existence question other services ask the SubjectKind registry on
/// writes that reference a Subject-kind discriminator. v1 only
/// exposes the `exists_active` check — that's what every downstream
/// validator needs. Read methods can be added when a consumer has a
/// concrete need.
#[async_trait]
pub trait SubjectKindsClient: Send + Sync {
    /// True iff a non-retired Subject kind with the given slug
    /// exists in the registry.
    async fn subject_kind_exists(&self, kind: &str) -> Result<bool, SubjectKindsClientError>;
}

/// Production `SubjectKindsClient` that calls the boss-subject-kinds
/// HTTP API over reqwest. 5-second timeout per call so an
/// unresponsive registry can't wedge a write indefinitely.
pub struct ReqwestSubjectKindsClient {
    base_url: String,
    http: reqwest::Client,
}

impl ReqwestSubjectKindsClient {
    pub fn new(base_url: impl Into<String>) -> Self {
        let (base_url, http) = http_client::base(base_url);
        Self { base_url, http }
    }
}

#[async_trait]
impl SubjectKindsClient for ReqwestSubjectKindsClient {
    async fn subject_kind_exists(&self, kind: &str) -> Result<bool, SubjectKindsClientError> {
        let url = format!("{}/api/subject-kinds/{}/exists", self.base_url, kind);
        http_client::get_exists(&self.http, &url).await
    }
}

/// Test fake — accepts a fixed allow-list of subject-kind slugs.
/// Use `FakeSubjectKindsClient::permissive()` to accept everything.
pub struct FakeSubjectKindsClient {
    permissive: bool,
    allowed: Vec<String>,
}

impl FakeSubjectKindsClient {
    pub fn permissive() -> Self {
        Self {
            permissive: true,
            allowed: Vec::new(),
        }
    }

    pub fn with(allowed: Vec<String>) -> Self {
        Self {
            permissive: false,
            allowed,
        }
    }
}

#[async_trait]
impl SubjectKindsClient for FakeSubjectKindsClient {
    async fn subject_kind_exists(&self, kind: &str) -> Result<bool, SubjectKindsClientError> {
        if self.permissive {
            return Ok(true);
        }
        Ok(self.allowed.iter().any(|k| k == kind))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn permissive_fake_accepts_anything() {
        let c = FakeSubjectKindsClient::permissive();
        assert!(c.subject_kind_exists("recipe").await.unwrap());
        assert!(c.subject_kind_exists("totally-bogus").await.unwrap());
    }

    #[tokio::test]
    async fn restricted_fake_gates_on_allow_list() {
        let c = FakeSubjectKindsClient::with(vec!["asset".into(), "vendor".into()]);
        assert!(c.subject_kind_exists("asset").await.unwrap());
        assert!(!c.subject_kind_exists("recipe").await.unwrap());
    }
}
