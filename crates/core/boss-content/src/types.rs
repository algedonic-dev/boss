//! Domain types for HR content.
//!
//! Audience model (per Q5): JSONB predicate that filters at query
//! time against a `UserContext` (department, role). `{all: true}` is
//! the open audience. Combined keys AND together; arrays within a
//! key OR. Unknown keys are ignored so new dimensions can land
//! without breaking existing rows.

use chrono::{DateTime, NaiveDate, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use uuid::Uuid;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum BulletinPriority {
    #[default]
    Normal,
    Pinned,
    Urgent,
}

impl BulletinPriority {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Normal => "normal",
            Self::Pinned => "pinned",
            Self::Urgent => "urgent",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "normal" => Some(Self::Normal),
            "pinned" => Some(Self::Pinned),
            "urgent" => Some(Self::Urgent),
            _ => None,
        }
    }

    /// Sort key — lower comes first on the board.
    pub fn sort_key(self) -> u8 {
        match self {
            Self::Urgent => 0,
            Self::Pinned => 1,
            Self::Normal => 2,
        }
    }
}

/// A live bulletin in its persisted shape.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Bulletin {
    pub id: Uuid,
    pub title: String,
    pub body: String,
    pub actor_id: String,
    pub posted_on: NaiveDate,
    pub expires_on: Option<NaiveDate>,
    pub priority: BulletinPriority,
    pub audience: Audience,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    /// Populated for reads on behalf of a specific user — true if
    /// the caller has already dismissed the bulletin. List endpoints
    /// filter these out by default; admin views include them.
    #[serde(default)]
    pub dismissed_by_viewer: bool,
}

/// Input shape for creating a bulletin. `id` is optional — client
/// can supply a UUID so a double-submit (network retry, double-
/// click) lands on the same row via the ON CONFLICT (id) DO NOTHING
/// path. When omitted the server generates a fresh UUID per call.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BulletinDraft {
    #[serde(default)]
    pub id: Option<Uuid>,
    pub title: String,
    pub body: String,
    /// ISO date, optional — defaults to today.
    pub posted_on: Option<NaiveDate>,
    pub expires_on: Option<NaiveDate>,
    #[serde(default)]
    pub priority: BulletinPriority,
    #[serde(default)]
    pub audience: Audience,
}

/// Partial update. Any `Some` field replaces the current value.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct BulletinPatch {
    pub title: Option<String>,
    pub body: Option<String>,
    pub expires_on: Option<Option<NaiveDate>>,
    pub priority: Option<BulletinPriority>,
    pub audience: Option<Audience>,
}

/// The caller identity used for audience filtering. Provided by the
/// gateway via the `X-Boss-User` header; field names mirror `boss_policy_client::User`
/// so the shared header payload deserializes cleanly here too.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct UserContext {
    pub id: String,
    #[serde(default)]
    pub role: String,
    #[serde(default)]
    pub department: Option<String>,
}

/// Audience predicate. Stored as JSONB; the empty object matches
/// nothing, `{"all": true}` matches everyone. Designed to be
/// forward-compatible — unknown keys are ignored.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Audience(pub Value);

impl Default for Audience {
    fn default() -> Self {
        Self(serde_json::json!({ "all": true }))
    }
}

impl Audience {
    pub fn all() -> Self {
        Self::default()
    }

    /// True if this audience matches the given user. Evaluation:
    /// - `{"all": true}` → always true
    /// - `{"departments": [...]}` → user's department ∈ list
    /// - `{"roles": [...]}` → user's role ∈ list
    /// - combined keys AND together; a missing user attribute
    ///   (null department) fails the check for that key.
    ///
    /// Unknown keys are ignored.
    pub fn matches(&self, user: &UserContext) -> bool {
        let obj = match self.0.as_object() {
            Some(o) => o,
            None => return false,
        };
        if obj.get("all").and_then(|v| v.as_bool()) == Some(true) {
            return true;
        }
        let mut matched = false;
        if let Some(depts) = obj.get("departments").and_then(|v| v.as_array()) {
            let user_dept = user.department.as_deref();
            let in_list = depts
                .iter()
                .any(|v| v.as_str() == user_dept && user_dept.is_some());
            if !in_list {
                return false;
            }
            matched = true;
        }
        if let Some(roles) = obj.get("roles").and_then(|v| v.as_array()) {
            let in_list = roles.iter().any(|v| v.as_str() == Some(&user.role));
            if !in_list {
                return false;
            }
            matched = true;
        }
        matched
    }
}

/// A section of the company manual. Slugs are path-like strings
/// (`benefits/time-off/vacation`) and `parent_slug` defines the tree.
/// Each `PUT` bumps `current_version` and writes a `ManualSectionVersion`
/// snapshot.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ManualSection {
    pub id: Uuid,
    pub slug: String,
    pub parent_slug: Option<String>,
    pub title: String,
    pub body: String,
    pub sort_order: i32,
    pub audience: Audience,
    pub current_version: i32,
    pub published: bool,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// A frozen snapshot of a section at a specific version.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ManualSectionVersion {
    pub section_id: Uuid,
    pub version: i32,
    pub title: String,
    pub body: String,
    pub audience: Audience,
    pub edited_by: String,
    pub edited_at: DateTime<Utc>,
    pub reason: Option<String>,
}

/// Input shape for creating a new section. `id` is server-generated;
/// `current_version` starts at 1.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ManualSectionDraft {
    pub slug: String,
    pub parent_slug: Option<String>,
    pub title: String,
    pub body: String,
    #[serde(default)]
    pub sort_order: i32,
    #[serde(default)]
    pub audience: Audience,
    #[serde(default = "default_published")]
    pub published: bool,
}

fn default_published() -> bool {
    true
}

/// Patch for a section update. Each `PUT` to a section writes an
/// append-only history row with the prior state, then applies the patch.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ManualPatch {
    pub title: Option<String>,
    pub body: Option<String>,
    pub audience: Option<Audience>,
    pub sort_order: Option<i32>,
    pub published: Option<bool>,
    /// Optional free-text "why this change" captured in history.
    pub reason: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn user(role: &str, department: Option<&str>) -> UserContext {
        UserContext {
            id: "emp-test".into(),
            role: role.into(),
            department: department.map(str::to_string),
        }
    }

    #[test]
    fn all_audience_matches_everyone() {
        let a = Audience::all();
        assert!(a.matches(&user("sales-rep", Some("sales"))));
        assert!(a.matches(&user("service-tech", None)));
    }

    #[test]
    fn department_scoped_only_matches_listed() {
        let a = Audience(serde_json::json!({ "departments": ["sales", "service"] }));
        assert!(a.matches(&user("x", Some("sales"))));
        assert!(a.matches(&user("x", Some("service"))));
        assert!(!a.matches(&user("x", Some("finance"))));
        assert!(!a.matches(&user("x", None)));
    }

    #[test]
    fn role_scoped_only_matches_listed() {
        let a = Audience(serde_json::json!({ "roles": ["service-mgr"] }));
        assert!(a.matches(&user("service-mgr", None)));
        assert!(!a.matches(&user("service-tech", None)));
    }

    #[test]
    fn combined_keys_and_together() {
        let a = Audience(serde_json::json!({
            "departments": ["sales"],
            "roles": ["sales-rep", "sales-mgr"]
        }));
        assert!(a.matches(&user("sales-rep", Some("sales"))));
        assert!(!a.matches(&user("sales-rep", Some("service"))));
        assert!(!a.matches(&user("cto", Some("sales"))));
    }

    #[test]
    fn unknown_keys_ignored() {
        let a = Audience(serde_json::json!({
            "departments": ["sales"],
            "future_key": ["whatever"]
        }));
        assert!(a.matches(&user("x", Some("sales"))));
    }

    #[test]
    fn empty_audience_matches_nothing() {
        let a = Audience(serde_json::json!({}));
        assert!(!a.matches(&user("x", Some("sales"))));
    }
}
