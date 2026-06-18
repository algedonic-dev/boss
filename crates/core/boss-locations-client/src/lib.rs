//! HTTP client port for reaching the `boss-locations` registry service.
//!
//! Other services need to validate Location ids (e.g.
//! `employees.location`, future `*_location_id` columns) against the
//! registry before accepting writes — same shape as
//! `boss-classes-client`. Trait + reqwest adapter live here so any
//! consumer (boss-people, boss-accounts, …) can call the same
//! canonical contract.

use async_trait::async_trait;
use boss_core::http_client::{self, HttpClientError, ServiceLabel};

/// Service-name marker for the shared [`HttpClientError`]. Keeps the
/// `Display` text reading `"locations service unreachable: …"`.
#[derive(Debug)]
pub struct Locations;
impl ServiceLabel for Locations {
    const NAME: &'static str = "locations";
}

/// Transport error for the Locations client. Alias of the shared
/// [`HttpClientError`] so existing `LocationsClientError::Unreachable`
/// constructors and matches keep compiling.
pub type LocationsClientError = HttpClientError<Locations>;

/// Existence question other services ask the Locations registry on
/// writes that reference a Location id. v1 only exposes the
/// `exists_active` check — that's what every downstream validator
/// needs. Read methods (`get`, `list_for_kind`, `children_of`) can be
/// added when a consumer has a concrete need.
#[async_trait]
pub trait LocationsClient: Send + Sync {
    /// True iff a non-retired Location with the given id exists in
    /// the registry. The hot-path validation primitive that replaces
    /// the closed `WorkLocation` enum CHECK constraint today defends.
    async fn location_exists(&self, id: &str) -> Result<bool, LocationsClientError>;
}

/// Production `LocationsClient` that calls the boss-locations HTTP
/// API over reqwest. 5-second timeout per call so an unresponsive
/// registry can't wedge a write indefinitely.
pub struct ReqwestLocationsClient {
    base_url: String,
    http: reqwest::Client,
}

impl ReqwestLocationsClient {
    pub fn new(base_url: impl Into<String>) -> Self {
        let (base_url, http) = http_client::base(base_url);
        Self { base_url, http }
    }
}

#[async_trait]
impl LocationsClient for ReqwestLocationsClient {
    async fn location_exists(&self, id: &str) -> Result<bool, LocationsClientError> {
        let url = format!("{}/api/locations/{}/exists", self.base_url, id);
        http_client::get_exists(&self.http, &url).await
    }
}

/// Test fake — accepts a fixed allow-list of Location ids. Use
/// `FakeLocationsClient::permissive()` to accept everything, or
/// `FakeLocationsClient::with(...)` to gate to specific ids.
pub struct FakeLocationsClient {
    permissive: bool,
    allowed: Vec<String>,
}

impl FakeLocationsClient {
    /// Accept any `location_exists` query. Use in tests that don't
    /// care about registry state.
    pub fn permissive() -> Self {
        Self {
            permissive: true,
            allowed: Vec::new(),
        }
    }

    /// Only accept queries whose id is in `allowed`.
    pub fn with(allowed: Vec<String>) -> Self {
        Self {
            permissive: false,
            allowed,
        }
    }
}

#[async_trait]
impl LocationsClient for FakeLocationsClient {
    async fn location_exists(&self, id: &str) -> Result<bool, LocationsClientError> {
        if self.permissive {
            return Ok(true);
        }
        Ok(self.allowed.iter().any(|i| i == id))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn permissive_fake_accepts_anything() {
        let c = FakeLocationsClient::permissive();
        assert!(c.location_exists("loc-hq").await.unwrap());
        assert!(c.location_exists("totally-bogus").await.unwrap());
    }

    #[tokio::test]
    async fn restricted_fake_gates_on_allow_list() {
        let c = FakeLocationsClient::with(vec!["loc-hq".into(), "loc-remote-default".into()]);
        assert!(c.location_exists("loc-hq").await.unwrap());
        assert!(!c.location_exists("loc-bogus").await.unwrap());
    }
}
