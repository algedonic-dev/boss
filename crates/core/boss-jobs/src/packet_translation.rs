//! Source expectations for atomic packet translation (design 429cf74a).
//! This checks a snapshot only. It supplies no lock or write authority: an
//! adapter must check it inside the transaction shared with step completion,
//! successor admission and source termination. No writer uses it yet.
use std::collections::BTreeMap;

use boss_core::job::{Job, JobId, JobStatus, Step, StepId, StepStatus};
use serde::{Deserialize, Serialize};

use crate::conditional_metadata::Expected;

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
                    || matches!(expected.status, StepStatus::Completed | StepStatus::Skipped)
                    || self.steps[..index]
                        .iter()
                        .any(|other| other.id == expected.id)
            })
        {
            return Err("translation requires unique nonterminal source step expectations");
        }
        if self.metadata.iter().any(|(key, expected)| {
            key.is_empty() || (!expected.present && !expected.value.is_null())
        }) {
            return Err("an absent expected metadata key has no value");
        }
        Ok(())
    }

    pub fn matches(&self, job: &Job, steps: &[Step]) -> bool {
        self.validate().is_ok()
            && job.id == self.source
            && job.kind == self.workflow_kind
            && job.workflow_version == self.workflow_version
            && job.status == JobStatus::Open
            && job.metadata.is_object()
            && self.steps.iter().all(|expected| {
                let mut found = steps.iter().filter(|step| step.id == expected.id);
                found.next().is_some_and(|step| {
                    step.job_id == self.source && step.status == expected.status
                }) && found.next().is_none()
            })
            && self.metadata.iter().all(|(key, expected)| {
                match (job.metadata.get(key), expected.present) {
                    (None, false) => true,
                    (Some(value), true) => value == &expected.value,
                    _ => false,
                }
            })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use boss_core::job::{Priority, Subject};
    use serde_json::json;

    fn fixture() -> (TranslationBasis, Job, Vec<Step>) {
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
        let basis = TranslationBasis {
            source: job.id,
            workflow_kind: job.kind.clone(),
            workflow_version: job.workflow_version,
            steps: vec![StepExpectation {
                id: step.id,
                status: step.status,
            }],
            metadata: BTreeMap::from([(
                "successor".into(),
                Expected {
                    present: false,
                    value: json!(null),
                },
            )]),
        };
        (basis, job, vec![step])
    }

    #[test]
    fn approval_after_snapshot_refuses_the_old_basis() {
        let (basis, job, mut steps) = fixture();
        assert!(basis.matches(&job, &steps));
        steps[0].status = StepStatus::Completed;
        assert!(!basis.matches(&job, &steps));
    }

    #[test]
    fn missing_foreign_and_duplicate_step_evidence_refuses() {
        let (basis, job, steps) = fixture();
        assert!(!basis.matches(&job, &[]));
        let mut foreign = steps.clone();
        foreign[0].job_id = JobId::new();
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
