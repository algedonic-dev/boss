//! In-memory `ClassRepository` impl. Used by tests and for service
//! startup before the postgres feature is wired in.

use async_trait::async_trait;
use boss_core::primitives::{Class, ClassRef};
use std::sync::RwLock;

use crate::port::{ClassError, ClassRepository};

/// Trivial in-memory store. Holds a snapshot of `Class` rows; lookups
/// are linear scans because the registry is tiny (≤ 100 rows in
/// account) and tests don't need indexing.
#[derive(Debug, Default)]
pub struct InMemoryClasses {
    rows: RwLock<Vec<Class>>,
}

impl InMemoryClasses {
    pub fn new(rows: Vec<Class>) -> Self {
        Self {
            rows: RwLock::new(rows),
        }
    }
}

#[async_trait]
impl ClassRepository for InMemoryClasses {
    async fn list_for_subject_kind(&self, subject_kind: &str) -> Result<Vec<Class>, ClassError> {
        let rows = self.rows.read().expect("rwlock poisoned");
        let mut out: Vec<Class> = rows
            .iter()
            .filter(|c| c.subject_kind == subject_kind && c.retired_at.is_none())
            .cloned()
            .collect();
        out.sort_by(|a, b| a.sort_order.cmp(&b.sort_order).then(a.code.cmp(&b.code)));
        Ok(out)
    }

    async fn get(&self, class_ref: &ClassRef) -> Result<Option<Class>, ClassError> {
        let rows = self.rows.read().expect("rwlock poisoned");
        Ok(rows
            .iter()
            .find(|c| c.subject_kind == class_ref.subject_kind && c.code == class_ref.code)
            .cloned())
    }

    async fn exists_active(&self, class_ref: &ClassRef) -> Result<bool, ClassError> {
        let rows = self.rows.read().expect("rwlock poisoned");
        Ok(rows.iter().any(|c| {
            c.subject_kind == class_ref.subject_kind
                && c.code == class_ref.code
                && c.retired_at.is_none()
        }))
    }

    async fn batch_upsert(&self, incoming: &[Class]) -> Result<u64, ClassError> {
        // Mirror the Postgres `ON CONFLICT (subject_kind, code) DO
        // NOTHING`: a row whose composite key already exists is left
        // untouched; only genuinely-new rows are appended. Returns the
        // count actually inserted.
        let mut rows = self.rows.write().expect("rwlock poisoned");
        let mut inserted: u64 = 0;
        for r in incoming {
            let exists = rows
                .iter()
                .any(|c| c.subject_kind == r.subject_kind && c.code == r.code);
            if !exists {
                rows.push(r.clone());
                inserted += 1;
            }
        }
        Ok(inserted)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{TimeZone, Utc};
    use serde_json::json;

    fn employee(code: &str, sort: i32, retired: bool) -> Class {
        Class {
            subject_kind: "employee".into(),
            code: code.into(),
            display_name: code.to_uppercase(),
            parent_code: None,
            member_attribute: Some("role".into()),
            metadata: json!({}),
            sort_order: sort,
            retired_at: if retired {
                Some(Utc.with_ymd_and_hms(2026, 1, 1, 0, 0, 0).unwrap())
            } else {
                None
            },
        }
    }

    #[tokio::test]
    async fn list_returns_only_active_sorted() {
        let repo = InMemoryClasses::new(vec![
            employee("ceo", 10, false),
            employee("retired-role", 5, true),
            employee("service-tech", 31, false),
            employee("sales-rep", 22, false),
        ]);

        let out = repo.list_for_subject_kind("employee").await.unwrap();
        let codes: Vec<&str> = out.iter().map(|c| c.code.as_str()).collect();
        assert_eq!(codes, vec!["ceo", "sales-rep", "service-tech"]);
    }

    #[tokio::test]
    async fn list_filters_by_subject_kind() {
        let repo = InMemoryClasses::new(vec![
            employee("ceo", 10, false),
            Class {
                subject_kind: "account".into(),
                code: "account".into(),
                display_name: "Account".into(),
                parent_code: None,
                member_attribute: Some("account_type".into()),
                metadata: json!({}),
                sort_order: 0,
                retired_at: None,
            },
        ]);

        let employee_only = repo.list_for_subject_kind("employee").await.unwrap();
        assert_eq!(employee_only.len(), 1);
        assert_eq!(employee_only[0].code, "ceo");

        let account_only = repo.list_for_subject_kind("account").await.unwrap();
        assert_eq!(account_only.len(), 1);
        assert_eq!(account_only[0].code, "account");
    }

    #[tokio::test]
    async fn get_returns_retired_rows() {
        // Audit / history surfaces need to resolve old codes even
        // after retirement.
        let repo = InMemoryClasses::new(vec![employee("retired-role", 5, true)]);
        let r = repo
            .get(&ClassRef::new("employee", "retired-role"))
            .await
            .unwrap();
        assert!(r.is_some());
        assert!(r.unwrap().retired_at.is_some());
    }

    #[tokio::test]
    async fn exists_active_excludes_retired() {
        let repo = InMemoryClasses::new(vec![
            employee("ceo", 10, false),
            employee("retired-role", 5, true),
        ]);
        assert!(
            repo.exists_active(&ClassRef::new("employee", "ceo"))
                .await
                .unwrap()
        );
        assert!(
            !repo
                .exists_active(&ClassRef::new("employee", "retired-role"))
                .await
                .unwrap(),
            "retired classes are not considered active"
        );
        assert!(
            !repo
                .exists_active(&ClassRef::new("employee", "no-such-code"))
                .await
                .unwrap()
        );
    }
}
