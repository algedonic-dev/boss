//! Source expectations for atomic packet translation (design 429cf74a).
//! This checks a snapshot only. It supplies no lock or write authority: an
//! adapter must check it inside the transaction shared with step completion,
//! successor admission and source termination. No writer uses it yet.
use std::collections::BTreeMap;

use boss_core::job::{Job, JobId, JobStatus, Step, StepId, StepStatus};
use serde::{Deserialize, Serialize};

use crate::conditional_metadata::{Expected, expectations_match, validate_expectations};
use crate::port::StepVersion;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TranslationBasis {
    pub source: JobId,
    pub workflow_kind: String,
    pub workflow_version: i32,
    pub steps: Vec<StepExpectation>,
    pub metadata: BTreeMap<String, Expected>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StepExpectation {
    pub id: StepId,
    pub status: StepStatus,
    // Missing is not an expectation of nobody, just as for completion.
    #[serde(deserialize_with = "crate::conditional_completion::present_holder")]
    pub assignee_id: Option<String>,
    /// The scoped token from the versioned read; mandatory for translation.
    pub version: String,
}

impl TranslationBasis {
    pub fn validate(&self) -> Result<(), &'static str> {
        if self.source.inner().as_uuid().is_nil()
            || self.workflow_kind.trim().is_empty()
            || self.workflow_version < 1
        {
            return Err("translation requires a source and pinned workflow identity");
        }
        if self.steps.is_empty()
            || self.steps.iter().enumerate().any(|(index, expected)| {
                expected.id.inner().as_uuid().is_nil()
                    || expected.version.trim().is_empty()
                    || expected
                        .assignee_id
                        .as_deref()
                        .is_some_and(|id| id.trim().is_empty())
                    || matches!(expected.status, StepStatus::Completed | StepStatus::Skipped)
                    || self.steps[..index]
                        .iter()
                        .any(|other| other.id == expected.id)
            })
        {
            return Err("translation requires unique nonterminal source step expectations");
        }
        validate_expectations(&self.metadata)
    }

    pub fn matches(&self, job: &Job, steps: &[(Step, StepVersion)]) -> bool {
        self.validate().is_ok()
            && job.id == self.source
            && job.kind == self.workflow_kind
            && job.workflow_version == self.workflow_version
            && job.status == JobStatus::Open
            && job.metadata.is_object()
            && self.steps.iter().all(|expected| {
                let mut found = steps.iter().filter(|(step, _)| step.id == expected.id);
                found.next().is_some_and(|(step, version)| {
                    step.job_id == self.source
                        && step.status == expected.status
                        && step.assignee_id == expected.assignee_id
                        && version.scoped_token(&step.job_id, &step.id) == expected.version
                }) && found.next().is_none()
            })
            && expectations_match(&self.metadata, &job.metadata)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use boss_core::job::{Priority, Subject};
    use serde_json::json;

    fn fixture() -> (TranslationBasis, Job, Vec<(Step, StepVersion)>) {
        let mut job = Job::new(
            "fixture",
            Subject::new("custom", "fixture"),
            "Source",
            "agent-test",
            Priority::Standard,
            chrono::NaiveDate::from_ymd_opt(2026, 11, 18).unwrap(),
        );
        job.status = JobStatus::Open;
        let mut step = Step::new(job.id, "sign-off", "Authority", 0);
        step.status = StepStatus::Ready;
        let version = StepVersion::new(1);
        let basis = TranslationBasis {
            source: job.id,
            workflow_kind: job.kind.clone(),
            workflow_version: job.workflow_version,
            steps: vec![StepExpectation {
                id: step.id,
                status: step.status,
                assignee_id: step.assignee_id.clone(),
                version: version.scoped_token(&step.job_id, &step.id),
            }],
            metadata: BTreeMap::from([(
                "successor".into(),
                Expected {
                    present: false,
                    value: json!(null),
                },
            )]),
        };
        (basis, job, vec![(step, version)])
    }

    #[test]
    fn a_source_step_metadata_write_invalidates_its_observed_version() {
        let (basis, job, mut steps) = fixture();
        steps[0].0.metadata = json!({"source_sha": "new-head"});
        steps[0].1 = StepVersion::new(2);
        assert!(!basis.matches(&job, &steps));
    }

    #[test]
    fn omitted_or_blank_step_guards_are_not_wildcards() {
        let (basis, _, _) = fixture();
        let wire = serde_json::to_value(&basis.steps[0]).unwrap();
        for field in ["assignee_id", "version"] {
            let mut missing = wire.clone();
            missing.as_object_mut().unwrap().remove(field);
            assert!(
                serde_json::from_value::<StepExpectation>(missing).is_err(),
                "{field}"
            );
        }
        for holder in [None, Some("   ")] {
            let mut invalid = basis.clone();
            if let Some(holder) = holder {
                invalid.steps[0].assignee_id = Some(holder.into());
            } else {
                invalid.steps[0].version.clear();
            }
            assert!(invalid.validate().is_err());
        }
    }

    #[test]
    fn conditional_writes_and_translation_share_metadata_expectations() {
        let (mut basis, mut job, steps) = fixture();
        for expected in [
            Expected {
                present: false,
                value: json!(null),
            },
            Expected {
                present: true,
                value: json!(null),
            },
            Expected {
                present: true,
                value: json!({"nested": [1, 2]}),
            },
        ] {
            basis.metadata.insert("successor".into(), expected);
            let command = crate::conditional_metadata::Command {
                contract_version: 1,
                idempotency_key: uuid::Uuid::new_v4(),
                expected: basis.metadata.clone(),
                patch: serde_json::Map::from_iter([("successor".into(), json!("next"))]),
                immutable_keys: vec![],
                guarded_keys: vec![],
            };
            assert!(command.validate().is_ok());
            for metadata in [
                json!({}),
                json!({"successor": null}),
                json!({"successor": {"nested": [1, 2]}}),
                json!({"successor": "other"}),
            ] {
                job.metadata = metadata;
                assert_eq!(basis.matches(&job, &steps), command.matches(&job));
            }
        }
    }

    #[test]
    fn reassignment_after_snapshot_refuses_the_old_basis() {
        let (basis, job, mut steps) = fixture();
        steps[0].0.assignee_id = Some("another-executor".into());
        assert!(!basis.matches(&job, &steps));
    }

    #[test]
    fn the_step_expectation_carries_an_explicit_holder_and_version() {
        let (basis, _, _) = fixture();
        let wire = json!({"id": basis.steps[0].id, "status": "ready",
                          "assignee_id": null, "version": "observed-row-token"});
        let expected: StepExpectation = serde_json::from_value(wire.clone())
            .expect("translation guards carry the holder and row version");
        assert_eq!(serde_json::to_value(expected).unwrap(), wire);
    }

    #[test]
    fn a_nonobject_source_metadata_never_matches_an_absence_guard() {
        let (basis, mut job, steps) = fixture();
        for metadata in [json!(null), json!([]), json!("unreadable"), json!(17)] {
            job.metadata = metadata;
            assert!(!basis.matches(&job, &steps));
        }
    }

    #[test]
    fn approval_after_snapshot_refuses_the_old_basis() {
        let (basis, job, mut steps) = fixture();
        assert!(basis.matches(&job, &steps));
        steps[0].0.status = StepStatus::Completed;
        assert!(!basis.matches(&job, &steps));
    }

    #[test]
    fn missing_foreign_and_duplicate_step_evidence_refuses() {
        let (basis, job, steps) = fixture();
        assert!(!basis.matches(&job, &[]));
        let mut foreign = steps.clone();
        foreign[0].0.job_id = JobId::new();
        assert!(!basis.matches(&job, &foreign));
        assert!(!basis.matches(&job, &[steps[0].clone(), steps[0].clone()]));
    }

    #[test]
    fn source_identity_protocol_and_status_are_part_of_the_basis() {
        let (basis, job, steps) = fixture();
        let mut changed = job.clone();
        changed.id = JobId::new();
        assert!(!basis.matches(&changed, &steps));
        changed = job.clone();
        changed.workflow_version += 1;
        assert!(!basis.matches(&changed, &steps));
        changed = job.clone();
        changed.kind.push_str("-other");
        assert!(!basis.matches(&changed, &steps));
        changed = job.clone();
        changed.status = JobStatus::Closed;
        assert!(!basis.matches(&changed, &steps));
    }

    #[test]
    fn absence_is_distinct_from_explicit_null() {
        let (mut basis, mut job, steps) = fixture();
        job.metadata = json!({"successor": null});
        assert!(!basis.matches(&job, &steps));
        basis.metadata.get_mut("successor").unwrap().present = true;
        assert!(basis.matches(&job, &steps));
        job.metadata = json!({"successor": "different"});
        assert!(!basis.matches(&job, &steps));
    }

    #[test]
    fn invalid_or_terminal_expectations_never_match() {
        let (basis, job, steps) = fixture();
        let mut invalid = basis.clone();
        invalid.steps.clear();
        assert!(invalid.validate().is_err());
        assert!(!invalid.matches(&job, &steps));
        invalid = basis.clone();
        invalid.steps.push(invalid.steps[0].clone());
        assert!(invalid.validate().is_err());
        for status in [StepStatus::Completed, StepStatus::Skipped] {
            invalid = basis.clone();
            invalid.steps[0].status = status;
            assert!(invalid.validate().is_err());
        }
        invalid = basis.clone();
        invalid.workflow_version = 0;
        assert!(invalid.validate().is_err());
        invalid = basis.clone();
        invalid.workflow_kind.clear();
        assert!(invalid.validate().is_err());
        invalid = basis;
        invalid.metadata.get_mut("successor").unwrap().value = json!(1);
        assert!(invalid.validate().is_err());
    }
}
