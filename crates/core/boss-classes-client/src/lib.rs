//! HTTP client port for reaching the `boss-classes` registry service.
//!
//! Other services need to validate Class codes (e.g. `employees.role`,
//! `accounts.account_type`) against the registry before accepting
//! writes, and to load tag-driven role lists at startup (which roles
//! are executives, which carry global read). Trait + reqwest adapter
//! live here so any consumer (boss-people, boss-accounts, boss-jobs,
//! …) can call the same canonical contract.

use async_trait::async_trait;
use boss_core::http_client::{self, HttpClientError, ServiceLabel};
use boss_core::primitives::{Class, ClassRef};

/// Fetch employee Classes from the registry and seed
/// [`boss_core::roles`]' executive cache from the rows whose
/// `metadata.is_executive` is true. Returns the count seeded so
/// callers can log a startup banner.
///
/// Tolerant of registry failures: on transport error the cache stays
/// uninitialised and `is_executive` returns false for every role
/// (platform-admin + audit-readonly still grant global read). This
/// matches the broader rollout posture — services boot even when a
/// downstream registry is briefly unreachable.
pub async fn seed_executive_role_cache(
    client: &dyn ClassesClient,
) -> Result<usize, ClassesClientError> {
    let classes = client.list_for_subject_kind("employee").await?;
    let codes = executive_role_codes(&classes);
    let count = codes.len();
    boss_core::roles::init_executive_roles(codes);
    Ok(count)
}

/// Filter a class list to codes whose `metadata.is_executive` is true.
/// Pairs with [`boss_core::roles::init_executive_roles`]: services
/// call `client.list_for_subject_kind("employee")` at startup, pass
/// the result here, and feed the returned codes into the cache.
///
/// `metadata` is JSON; a missing key or non-bool value is treated as
/// `false`. Retired Classes are excluded so an executive role that
/// gets retired stops carrying global read on the next service boot.
pub fn executive_role_codes(classes: &[Class]) -> Vec<String> {
    classes
        .iter()
        .filter(|c| c.retired_at.is_none())
        .filter(|c| {
            c.metadata
                .get("is_executive")
                .and_then(|v| v.as_bool())
                .unwrap_or(false)
        })
        .map(|c| c.code.clone())
        .collect()
}

/// Sister of [`seed_executive_role_cache`] for the broad-account-access
/// role set. Fetches employee Classes, filters by
/// `metadata.broad_account_access`, and seeds the in-process cache.
/// Services that don't seed fall back to the hardcoded union at
/// `boss_core::roles::DEFAULT_BROAD_ACCOUNT_ACCESS_ROLES`.
pub async fn seed_broad_account_access_role_cache(
    client: &dyn ClassesClient,
) -> Result<usize, ClassesClientError> {
    let classes = client.list_for_subject_kind("employee").await?;
    let codes = broad_account_access_role_codes(&classes);
    let count = codes.len();
    boss_core::roles::init_broad_account_access_roles(codes);
    Ok(count)
}

/// Filter a class list to codes whose `metadata.broad_account_access`
/// is true. Same `metadata` shape as [`executive_role_codes`].
pub fn broad_account_access_role_codes(classes: &[Class]) -> Vec<String> {
    classes
        .iter()
        .filter(|c| c.retired_at.is_none())
        .filter(|c| {
            c.metadata
                .get("broad_account_access")
                .and_then(|v| v.as_bool())
                .unwrap_or(false)
        })
        .map(|c| c.code.clone())
        .collect()
}

/// Service-name marker for the shared [`HttpClientError`]. Keeps the
/// `Display` text reading `"classes service unreachable: …"`.
#[derive(Debug)]
pub struct Classes;
impl ServiceLabel for Classes {
    const NAME: &'static str = "classes";
}

/// Transport error for the Classes client. Alias of the shared
/// [`HttpClientError`] so existing constructors and matches keep
/// compiling.
pub type ClassesClientError = HttpClientError<Classes>;

/// Existence + listing questions other services ask the Class
/// registry. v1 exposes:
///
/// - `class_exists` — hot-path validation that replaces closed-enum
///   CHECK constraints on writes.
/// - `list_for_subject_kind` — startup-cache primitive for tag-driven
///   role lookups (e.g. which `employee` Classes are executive).
#[async_trait]
pub trait ClassesClient: Send + Sync {
    /// True iff a non-retired Class with the given key exists in the
    /// registry.
    async fn class_exists(&self, class_ref: &ClassRef) -> Result<bool, ClassesClientError>;

    /// All non-retired Classes for a `subject_kind`. Used at service
    /// startup to seed in-process caches that read metadata tags
    /// (e.g. `metadata.is_executive`).
    async fn list_for_subject_kind(
        &self,
        subject_kind: &str,
    ) -> Result<Vec<Class>, ClassesClientError>;
}

/// Production `ClassesClient` that calls the boss-classes HTTP API
/// over reqwest. 5-second timeout per call so an unresponsive
/// registry can't wedge a write indefinitely.
pub struct ReqwestClassesClient {
    base_url: String,
    http: reqwest::Client,
}

impl ReqwestClassesClient {
    pub fn new(base_url: impl Into<String>) -> Self {
        let (base_url, http) = http_client::base(base_url);
        Self { base_url, http }
    }
}

#[async_trait]
impl ClassesClient for ReqwestClassesClient {
    async fn class_exists(&self, class_ref: &ClassRef) -> Result<bool, ClassesClientError> {
        let url = format!(
            "{}/api/classes/{}/{}/exists",
            self.base_url, class_ref.subject_kind, class_ref.code
        );
        http_client::get_exists(&self.http, &url).await
    }

    async fn list_for_subject_kind(
        &self,
        subject_kind: &str,
    ) -> Result<Vec<Class>, ClassesClientError> {
        let url = format!("{}/api/classes?subject_kind={subject_kind}", self.base_url);
        http_client::get_json(&self.http, &url).await
    }
}

/// Test fake — accepts a fixed allow-list of `(subject_kind, code)`
/// pairs and optionally returns a fixture list. Use
/// `FakeClassesClient::permissive()` to accept everything,
/// `FakeClassesClient::with(...)` to gate to specific codes, or
/// `FakeClassesClient::with_classes(...)` to drive `list_for_subject_kind`.
pub struct FakeClassesClient {
    permissive: bool,
    allowed: Vec<ClassRef>,
    classes: Vec<Class>,
}

impl FakeClassesClient {
    /// Accept any `class_exists` query. Use in tests that don't care
    /// about registry state.
    pub fn permissive() -> Self {
        Self {
            permissive: true,
            allowed: Vec::new(),
            classes: Vec::new(),
        }
    }

    /// Only accept queries whose `class_ref` is in `allowed`.
    pub fn with(allowed: Vec<ClassRef>) -> Self {
        Self {
            permissive: false,
            allowed,
            classes: Vec::new(),
        }
    }

    /// Drive `list_for_subject_kind` with a fixture vector. Use to
    /// pin executive-role tagging in service-startup tests.
    pub fn with_classes(classes: Vec<Class>) -> Self {
        Self {
            permissive: true,
            allowed: Vec::new(),
            classes,
        }
    }
}

#[async_trait]
impl ClassesClient for FakeClassesClient {
    async fn class_exists(&self, class_ref: &ClassRef) -> Result<bool, ClassesClientError> {
        if self.permissive {
            return Ok(true);
        }
        Ok(self.allowed.iter().any(|c| c == class_ref))
    }

    async fn list_for_subject_kind(
        &self,
        subject_kind: &str,
    ) -> Result<Vec<Class>, ClassesClientError> {
        Ok(self
            .classes
            .iter()
            .filter(|c| c.subject_kind == subject_kind)
            .cloned()
            .collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn permissive_fake_accepts_anything() {
        let c = FakeClassesClient::permissive();
        assert!(
            c.class_exists(&ClassRef::new("employee", "ceo"))
                .await
                .unwrap()
        );
        assert!(
            c.class_exists(&ClassRef::new("anything", "totally-bogus"))
                .await
                .unwrap()
        );
    }

    #[tokio::test]
    async fn restricted_fake_gates_on_allow_list() {
        let c = FakeClassesClient::with(vec![
            ClassRef::new("employee", "ceo"),
            ClassRef::new("employee", "service-tech"),
        ]);
        assert!(
            c.class_exists(&ClassRef::new("employee", "ceo"))
                .await
                .unwrap()
        );
        assert!(
            !c.class_exists(&ClassRef::new("employee", "no-such"))
                .await
                .unwrap()
        );
        assert!(
            !c.class_exists(&ClassRef::new("account", "ceo"))
                .await
                .unwrap(),
            "subject_kind matters too"
        );
    }

    fn class(kind: &str, code: &str, metadata: serde_json::Value, retired: bool) -> Class {
        Class {
            subject_kind: kind.into(),
            code: code.into(),
            display_name: code.into(),
            parent_code: None,
            member_attribute: Some("role".into()),
            metadata,
            sort_order: 0,
            retired_at: if retired {
                Some(chrono::Utc::now())
            } else {
                None
            },
        }
    }

    #[test]
    fn executive_role_codes_filters_on_metadata_flag() {
        let classes = vec![
            class(
                "employee",
                "ceo",
                serde_json::json!({ "is_executive": true }),
                false,
            ),
            class(
                "employee",
                "service-tech",
                serde_json::json!({ "is_executive": false }),
                false,
            ),
            class(
                "employee",
                "head-of-sales",
                serde_json::json!({ "department": "sales", "is_executive": true }),
                false,
            ),
            class("employee", "no-metadata", serde_json::Value::Null, false),
            class(
                "employee",
                "retired-cto",
                serde_json::json!({ "is_executive": true }),
                true,
            ),
        ];
        let codes = executive_role_codes(&classes);
        assert_eq!(codes, vec!["ceo".to_string(), "head-of-sales".to_string()]);
    }

    #[tokio::test]
    async fn fixture_list_filters_by_subject_kind() {
        let make = |kind: &str, code: &str| Class {
            subject_kind: kind.into(),
            code: code.into(),
            display_name: code.into(),
            parent_code: None,
            member_attribute: Some("role".into()),
            metadata: serde_json::Value::Null,
            sort_order: 0,
            retired_at: None,
        };
        let c = FakeClassesClient::with_classes(vec![
            make("employee", "ceo"),
            make("employee", "service-tech"),
            make("account", "distributor"),
        ]);
        let employee = c.list_for_subject_kind("employee").await.unwrap();
        assert_eq!(employee.len(), 2);
        let account = c.list_for_subject_kind("account").await.unwrap();
        assert_eq!(account.len(), 1);
        assert_eq!(account[0].code, "distributor");
    }
}
