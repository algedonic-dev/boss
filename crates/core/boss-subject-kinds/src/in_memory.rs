//! In-memory `SubjectKindRepository` for tests + dev fallback.

use async_trait::async_trait;

use crate::port::{SubjectKind, SubjectKindError, SubjectKindRepository};

pub struct InMemorySubjectKinds {
    rows: Vec<SubjectKind>,
}

impl InMemorySubjectKinds {
    pub fn new(rows: Vec<SubjectKind>) -> Self {
        Self { rows }
    }
}

#[async_trait]
impl SubjectKindRepository for InMemorySubjectKinds {
    async fn get(&self, kind: &str) -> Result<Option<SubjectKind>, SubjectKindError> {
        Ok(self.rows.iter().find(|r| r.kind == kind).cloned())
    }

    async fn exists_active(&self, kind: &str) -> Result<bool, SubjectKindError> {
        Ok(self
            .rows
            .iter()
            .any(|r| r.kind == kind && r.retired_at.is_none()))
    }

    async fn list_active(&self) -> Result<Vec<SubjectKind>, SubjectKindError> {
        let mut out: Vec<SubjectKind> = self
            .rows
            .iter()
            .filter(|r| r.retired_at.is_none())
            .cloned()
            .collect();
        out.sort_by(|a, b| {
            a.sort_order
                .cmp(&b.sort_order)
                .then_with(|| a.kind.cmp(&b.kind))
        });
        Ok(out)
    }

    async fn children_of(&self, parent_kind: &str) -> Result<Vec<SubjectKind>, SubjectKindError> {
        let mut out: Vec<SubjectKind> = self
            .rows
            .iter()
            .filter(|r| r.retired_at.is_none() && r.parent_kind.as_deref() == Some(parent_kind))
            .cloned()
            .collect();
        out.sort_by(|a, b| a.kind.cmp(&b.kind));
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sk(kind: &str, label: &str, parent: Option<&str>) -> SubjectKind {
        SubjectKind {
            kind: kind.into(),
            label: label.into(),
            parent_kind: parent.map(String::from),
            description: None,
            owning_team: "platform".into(),
            metadata: serde_json::json!({}),
            sort_order: 0,
            retired_at: None,
        }
    }

    #[tokio::test]
    async fn list_active_filters_retired() {
        let mut retired = sk("legacy", "Legacy", None);
        retired.retired_at = Some(chrono::Utc::now());
        let repo = InMemorySubjectKinds::new(vec![sk("asset", "Asset", None), retired]);
        let rows = repo.list_active().await.unwrap();
        let ids: Vec<&str> = rows.iter().map(|r| r.kind.as_str()).collect();
        assert_eq!(ids, vec!["asset"]);
    }

    #[tokio::test]
    async fn exists_active_distinguishes_retired_from_unknown() {
        let mut retired = sk("legacy", "Legacy", None);
        retired.retired_at = Some(chrono::Utc::now());
        let repo = InMemorySubjectKinds::new(vec![sk("vendor", "Vendor", None), retired]);
        assert!(repo.exists_active("vendor").await.unwrap());
        assert!(!repo.exists_active("legacy").await.unwrap());
        assert!(!repo.exists_active("unknown").await.unwrap());
    }

    #[tokio::test]
    async fn children_of_walks_parent_kind() {
        let repo = InMemorySubjectKinds::new(vec![
            sk("account", "Account", None),
            sk("medical-practice", "Medical Practice", Some("account")),
            sk("wholesale-customer", "Wholesale Customer", Some("account")),
            sk("vendor", "Vendor", None),
        ]);
        let kids = repo.children_of("account").await.unwrap();
        let names: Vec<&str> = kids.iter().map(|k| k.kind.as_str()).collect();
        assert_eq!(names, vec!["medical-practice", "wholesale-customer"]);
    }
}
