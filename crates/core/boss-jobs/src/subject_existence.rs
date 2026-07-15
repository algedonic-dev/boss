//! Subject-existence validation for `POST /api/jobs`.
//!
//! Closes the "you can create a Job pointing at an account that
//! doesn't exist" hole captured in the create-Job UX walkthrough.
//! When wired in, the handler asks the relevant upstream service
//! ("does account `acc-bigseed-9999` exist?") before persisting the
//! Job, returning `400 Bad Request` for ghost ids instead of letting
//! them land as dangling references.
//!
//! Same opt-in shape as `CalendarClient` / `SubjectKindsClient`:
//! `JobsApiState::subject_existence: Option<Arc<dyn SubjectExistenceCheck>>`.
//! `None` skips the check (the existing test path; in-memory
//! adapters don't have upstream services to ask).

use async_trait::async_trait;
use boss_core::job::Subject;

#[derive(Debug, thiserror::Error)]
pub enum SubjectExistenceError {
    #[error("subject not found: {0}")]
    NotFound(String),
    /// The upstream service couldn't be reached or returned a shape
    /// we couldn't interpret. Treated as fail-open: the handler logs
    /// a warning and lets the create proceed (better to have a few
    /// unverified ids than to wedge the create-Job path on every
    /// network blip).
    #[error("upstream unavailable: {0}")]
    Unavailable(String),
}

#[async_trait]
pub trait SubjectExistenceCheck: Send + Sync {
    /// Return `Ok(())` if the subject exists, `NotFound` if it
    /// definitively doesn't, `Unavailable` if we couldn't tell.
    async fn check(&self, subject: &Subject) -> Result<(), SubjectExistenceError>;
}

/// HTTP-backed implementation, driven by a kind → URL-template map
/// instead of a hardcoded match (registries over hardcoded paths).
/// Each platform kind with a per-id GET endpoint registers a
/// template; `{id}` is replaced with the url-encoded subject id and
/// existence is 200-vs-404. `account` keeps its special case — its
/// endpoint returns `{exists: bool}` instead of 404ing.
///
/// Kinds without a template (vendor, campaign, tenant-defined kinds)
/// skip the check — the handler falls through to its own validation.
/// Custom kinds are already validated against the SubjectKind
/// registry upstream of this trait. Tenant deployments can extend
/// the map via [`with_endpoint`](Self::with_endpoint) without
/// touching core code.
pub struct ReqwestSubjectExistenceCheck {
    client: reqwest::Client,
    people_base: String,
    /// kind → URL template containing one `{id}` placeholder.
    endpoints: std::collections::HashMap<String, String>,
}

impl ReqwestSubjectExistenceCheck {
    pub fn new(
        people_base: impl Into<String>,
        assets_base: impl Into<String>,
        locations_base: impl Into<String>,
        inventory_base: impl Into<String>,
    ) -> Self {
        let people_base = people_base.into();
        let endpoints = std::collections::HashMap::from([
            (
                "employee".to_string(),
                format!("{people_base}/api/people/{{id}}"),
            ),
            (
                "location".to_string(),
                format!("{}/api/locations/{{id}}", locations_base.into()),
            ),
            (
                "asset".to_string(),
                format!("{}/api/assets/{{id}}", assets_base.into()),
            ),
            (
                "purchase_order".to_string(),
                format!(
                    "{}/api/inventory/purchase-orders/{{id}}",
                    inventory_base.into()
                ),
            ),
        ]);
        Self {
            client: reqwest::Client::builder()
                .timeout(std::time::Duration::from_secs(5))
                .build()
                .expect("reqwest client"),
            people_base,
            endpoints,
        }
    }

    /// Register (or override) the existence endpoint for a Subject
    /// kind. `template` must contain one `{id}` placeholder.
    pub fn with_endpoint(mut self, kind: impl Into<String>, template: impl Into<String>) -> Self {
        self.endpoints.insert(kind.into(), template.into());
        self
    }

    /// The operator-tier `x-boss-user` header that loopback service-
    /// to-service calls use across Boss. Mirrors what `boss-gateway`
    /// sends for its own bootstrap calls.
    fn loopback_header() -> reqwest::header::HeaderMap {
        let mut h = reqwest::header::HeaderMap::new();
        let user = serde_json::json!({
            "id": "system-jobs",
            "role": "system",
            "access_tier": "operator",
            "territory_account_ids": [],
            "direct_report_ids": [],
            "department": null,
        })
        .to_string();
        if let Ok(v) = reqwest::header::HeaderValue::from_str(&user) {
            h.insert("x-boss-user", v);
        }
        h
    }

    async fn http_exists(&self, url: &str) -> Result<bool, SubjectExistenceError> {
        let resp = self
            .client
            .get(url)
            .headers(Self::loopback_header())
            .send()
            .await
            .map_err(|e| SubjectExistenceError::Unavailable(e.to_string()))?;
        let status = resp.status();
        if status.is_success() {
            Ok(true)
        } else if status.as_u16() == 404 {
            Ok(false)
        } else {
            Err(SubjectExistenceError::Unavailable(format!(
                "{} {}",
                url, status
            )))
        }
    }

    async fn account_exists(&self, id: &str) -> Result<bool, SubjectExistenceError> {
        let url = format!("{}/api/people/accounts/{}/exists", self.people_base, id);
        let resp = self
            .client
            .get(&url)
            .headers(Self::loopback_header())
            .send()
            .await
            .map_err(|e| SubjectExistenceError::Unavailable(e.to_string()))?;
        if !resp.status().is_success() {
            return Err(SubjectExistenceError::Unavailable(format!(
                "{} {}",
                url,
                resp.status()
            )));
        }
        let body: serde_json::Value = resp
            .json()
            .await
            .map_err(|e| SubjectExistenceError::Unavailable(e.to_string()))?;
        Ok(body
            .get("exists")
            .and_then(|v| v.as_bool())
            .unwrap_or(false))
    }
}

#[async_trait]
impl SubjectExistenceCheck for ReqwestSubjectExistenceCheck {
    async fn check(&self, subject: &Subject) -> Result<(), SubjectExistenceError> {
        // Subject is a (kind, id) tuple. `account` is special
        // (its endpoint answers `{exists: bool}`); every other kind
        // resolves through the endpoint map. Kinds without an entry
        // (vendor, campaign, tenant-defined kinds) short-circuit —
        // their existence is checked elsewhere or not at all.
        let id = subject.id.clone();
        let kind = subject.kind.as_str();
        let exists = if kind == "account" {
            self.account_exists(&id).await?
        } else if let Some(template) = self.endpoints.get(kind) {
            let url = template.replace("{id}", &id);
            self.http_exists(&url).await?
        } else {
            return Ok(());
        };
        if exists {
            Ok(())
        } else {
            Err(SubjectExistenceError::NotFound(format!("{kind}:{id}")))
        }
    }
}

/// The uniform adapter (subject-model design R1, approved
/// 2026-07-15): one indexed lookup against the `subjects` identity
/// table, for EVERY kind — platform, tenant-defined, and the
/// previously unreachable ones (vendor, campaign, product, …).
/// Replaces the five-endpoint HTTP prober: no per-kind URL
/// templates, no fall-through kinds, no cross-service fan-out.
/// A storage error maps to `Unavailable`, which the create handler
/// fails CLOSED on (Q2: abort by default).
#[cfg(feature = "postgres")]
pub struct PgSubjectExistence {
    pool: sqlx::PgPool,
}

#[cfg(feature = "postgres")]
impl PgSubjectExistence {
    pub fn new(pool: sqlx::PgPool) -> Self {
        Self { pool }
    }
}

#[cfg(feature = "postgres")]
#[async_trait]
impl SubjectExistenceCheck for PgSubjectExistence {
    async fn check(&self, subject: &Subject) -> Result<(), SubjectExistenceError> {
        let exists: bool =
            sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM subjects WHERE kind = $1 AND id = $2)")
                .bind(&subject.kind)
                .bind(&subject.id)
                .fetch_one(&self.pool)
                .await
                .map_err(|e| SubjectExistenceError::Unavailable(e.to_string()))?;
        if exists {
            Ok(())
        } else {
            Err(SubjectExistenceError::NotFound(format!(
                "{}/{}",
                subject.kind, subject.id
            )))
        }
    }
}

#[cfg(test)]
pub mod test_helpers {
    //! In-memory implementation for unit tests. Pre-populated with
    //! a known-good id set; everything else is NotFound.

    use super::*;
    use std::collections::{HashMap, HashSet};
    use std::sync::Mutex;

    /// Kind-keyed id sets — mirrors the production endpoint map, so
    /// tests can seed any Subject kind (including tenant-defined
    /// ones) without growing this struct.
    pub struct InMemorySubjectExistenceCheck {
        pub sets: Mutex<HashMap<String, HashSet<String>>>,
    }

    impl InMemorySubjectExistenceCheck {
        pub fn new() -> Self {
            Self {
                sets: Mutex::new(HashMap::new()),
            }
        }

        /// Seed an existing subject of any kind.
        pub fn with_subject(self, kind: &str, id: &str) -> Self {
            self.sets
                .lock()
                .unwrap()
                .entry(kind.to_string())
                .or_default()
                .insert(id.to_string());
            self
        }

        pub fn with_account(self, id: &str) -> Self {
            self.with_subject("account", id)
        }
        pub fn with_employee(self, id: &str) -> Self {
            self.with_subject("employee", id)
        }
        pub fn with_asset(self, id: &str) -> Self {
            self.with_subject("asset", id)
        }
    }

    impl Default for InMemorySubjectExistenceCheck {
        fn default() -> Self {
            Self::new()
        }
    }

    #[async_trait]
    impl SubjectExistenceCheck for InMemorySubjectExistenceCheck {
        async fn check(&self, subject: &Subject) -> Result<(), SubjectExistenceError> {
            // Kinds the test never seeded behave like production
            // kinds without an endpoint entry: the check passes
            // through. Seeded kinds answer from their id set.
            let sets = self.sets.lock().unwrap();
            match sets.get(subject.kind.as_str()) {
                None => Ok(()),
                Some(set) if set.contains(&subject.id) => Ok(()),
                Some(_) => Err(SubjectExistenceError::NotFound(subject.id.clone())),
            }
        }
    }
}
