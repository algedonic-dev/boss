//! Executor provenance is verified at an authenticated launch boundary.
//! An immutable caller assertion is still an assertion.

use crate::first_record::FirstRecord;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum ExecutorProvenanceRequirement {
    #[default]
    Advisory,
    Verified,
}

/// Server-captured claim. The requirement has no serde default: an absent
/// immutable declaration must never turn into permission for advisory work.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SourceClaim {
    pub native_run: boss_core::job::JobId,
    pub source_job: boss_core::job::JobId,
    pub source_step: boss_core::job::StepId,
    pub source_slug: String,
    pub actor: boss_core::actor::ActorId,
    pub protocol_version: i32,
    pub configured_model: String,
    pub controlled_effort: Option<String>,
    pub executor_provenance: ExecutorProvenanceRequirement,
    pub source_ref: Option<String>,
    pub source_head: Option<String>,
    pub source_binding: Option<serde_json::Value>,
}

pub const EXECUTOR_BINDING_KEY: &str = "executor.binding";
pub const SOURCE_CLAIM_KEY: &str = "executor.source_claim";

pub fn reserved_key(key: &str) -> bool {
    key.starts_with("executor.")
}

/// Resolved from the pinned protocol and immutable source claim, never
/// from a caller's mutable run metadata.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExecutorExpectation {
    pub native_run: uuid::Uuid,
    pub actor: String,
    pub source_job: uuid::Uuid,
    pub source_step: uuid::Uuid,
    pub source_slug: String,
    pub protocol_version: i32,
    pub configured_model: String,
    pub controlled_effort: Option<String>,
    pub execution_handle: String,
    pub launch_boundary: String,
    pub source_claim_digest: String,
    pub source_claim_event: uuid::Uuid,
    pub source_ref: Option<String>,
    pub source_head: Option<String>,
    pub source_binding: Option<serde_json::Value>,
    /// Supplied by the repository's immutable record reader, not wire JSON.
    #[serde(skip)]
    pub source_claim: Option<FirstRecord>,
    pub executor_family: String,
    pub launched_at: chrono::DateTime<chrono::Utc>,
    pub launch_receipt_digest: String,
}

/// Authenticates the complete receipt at a host/tool boundary. Implementations
/// must not infer trust from an actor header or a JSON assurance field.
pub trait LaunchReceiptVerifier {
    fn authenticate(
        &self,
        record: &FirstRecord,
        expectation: &ExecutorExpectation,
    ) -> Result<(), AttestationError>;
}

pub struct UnavailableLaunchReceiptVerifier;

impl LaunchReceiptVerifier for UnavailableLaunchReceiptVerifier {
    fn authenticate(
        &self,
        _: &FirstRecord,
        _: &ExecutorExpectation,
    ) -> Result<(), AttestationError> {
        Err(AttestationError::UntrustedLaunchBoundary)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum AttestationError {
    #[error("verified executor requires an immutable launch receipt")]
    MissingReceipt,
    #[error("verified executor requires the original immutable source claim")]
    MissingSourceClaim,
    #[error("launch receipt has no authenticated host or tool boundary")]
    UntrustedLaunchBoundary,
    #[error("launch receipt does not match its original immutable record")]
    InvalidRecord,
    #[error("launch receipt does not bind the exact protocol claim and executor controls")]
    BindingMismatch,
}

/// Only the verifier can produce this result; it cannot be deserialized from
/// a run's metadata. Its digest is the one later verdict readers must retain.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VerifiedExecutor {
    digest: String,
    event_id: uuid::Uuid,
}

impl VerifiedExecutor {
    pub fn digest(&self) -> &str {
        &self.digest
    }

    pub fn event_id(&self) -> uuid::Uuid {
        self.event_id
    }
}

/// Decode an original claim while retaining its immutable envelope. This
/// validation is shared even by advisory consumers: a damaged old claim
/// cannot become permission to downgrade its declared requirement.
pub fn read_source_claim(record: &FirstRecord) -> Result<SourceClaim, AttestationError> {
    let canonical = boss_core::job::canonical_json_bytes(&record.value);
    if record.key != SOURCE_CLAIM_KEY
        || record.event_id.is_nil()
        || record.step_id.inner().as_uuid().is_nil()
        || record.canonical != canonical
        || record.digest != hex::encode(Sha256::digest(&canonical))
        || serde_json::from_str::<serde_json::Value>(&record.value_json)
            .ok()
            .as_ref()
            != Some(&record.value)
    {
        return Err(AttestationError::InvalidRecord);
    }
    let claim: SourceClaim = serde_json::from_value(record.value.clone())
        .map_err(|_| AttestationError::BindingMismatch)?;
    if record.job_id != claim.native_run
        || record.actor != claim.actor
        || claim.native_run.inner().as_uuid().is_nil()
        || claim.source_job.inner().as_uuid().is_nil()
        || claim.source_step.inner().as_uuid().is_nil()
        || claim.protocol_version <= 0
        || claim.source_slug.trim().is_empty()
        || claim.configured_model.trim().is_empty()
        || claim
            .controlled_effort
            .as_ref()
            .is_some_and(|effort| effort.trim().is_empty())
    {
        return Err(AttestationError::BindingMismatch);
    }
    Ok(claim)
}

/// One predicate for every consumer of a protocol's executor requirement.
/// Advisory work produces no verified result. A verified outcome requires
/// its original claim and an authenticated receipt; there is no fallback.
pub fn executor_eligibility(
    requirement: ExecutorProvenanceRequirement,
    record: Option<&FirstRecord>,
    expectation: Option<&ExecutorExpectation>,
    verifier: &dyn LaunchReceiptVerifier,
) -> Result<Option<VerifiedExecutor>, AttestationError> {
    if let Some(expectation) = expectation
        && let Some(source) = expectation.source_claim.as_ref()
    {
        let source = read_source_claim(source)?;
        if source.executor_provenance != requirement {
            return Err(AttestationError::BindingMismatch);
        }
    }
    match requirement {
        ExecutorProvenanceRequirement::Advisory => Ok(None),
        ExecutorProvenanceRequirement::Verified => verify_executor_receipt(
            record,
            expectation.ok_or(AttestationError::MissingSourceClaim)?,
            verifier,
        )
        .map(Some),
    }
}

pub fn verify_executor_receipt(
    record: Option<&FirstRecord>,
    expectation: &ExecutorExpectation,
    verifier: &dyn LaunchReceiptVerifier,
) -> Result<VerifiedExecutor, AttestationError> {
    let record = record.ok_or(AttestationError::MissingReceipt)?;
    let canonical = boss_core::job::canonical_json_bytes(&record.value);
    let parsed: serde_json::Value =
        serde_json::from_str(&record.value_json).map_err(|_| AttestationError::InvalidRecord)?;
    if record.key != EXECUTOR_BINDING_KEY
        || record.event_id.is_nil()
        || record.canonical != canonical
        || parsed != record.value
        || record.digest != hex::encode(Sha256::digest(&canonical))
    {
        return Err(AttestationError::InvalidRecord);
    }
    let binding: ExecutorExpectation = serde_json::from_value(record.value.clone())
        .map_err(|_| AttestationError::BindingMismatch)?;
    if serde_json::to_value(&binding).map_err(|_| AttestationError::BindingMismatch)?
        != serde_json::to_value(expectation).map_err(|_| AttestationError::BindingMismatch)?
        || record.job_id.to_string() != expectation.native_run.to_string()
        || expectation.native_run.is_nil()
        || expectation.source_job.is_nil()
        || expectation.source_step.is_nil()
        || expectation.protocol_version <= 0
        || [
            &expectation.actor,
            &expectation.source_slug,
            &expectation.configured_model,
            &expectation.execution_handle,
            &expectation.launch_boundary,
            &expectation.executor_family,
        ]
        .iter()
        .any(|value| value.trim().is_empty())
    {
        return Err(AttestationError::BindingMismatch);
    }
    let source = expectation
        .source_claim
        .as_ref()
        .ok_or(AttestationError::MissingSourceClaim)?;
    let source_value = read_source_claim(source)?;
    if source.key != SOURCE_CLAIM_KEY
        || source.event_id.is_nil()
        || record.step_id != source.step_id
        || source.job_id.to_string() != expectation.native_run.to_string()
        || source.digest != expectation.source_claim_digest
        || source.event_id != expectation.source_claim_event
        || source.canonical != boss_core::job::canonical_json_bytes(&source.value)
        || source.digest != hex::encode(Sha256::digest(&source.canonical))
        || source_value.native_run.to_string() != expectation.native_run.to_string()
        || source_value.source_job.to_string() != expectation.source_job.to_string()
        || source_value.source_step.to_string() != expectation.source_step.to_string()
        || source_value.source_slug != expectation.source_slug
        || source_value.actor.to_string() != expectation.actor
        || source_value.protocol_version != expectation.protocol_version
        || source_value.configured_model != expectation.configured_model
        || source_value.controlled_effort != expectation.controlled_effort
        || source_value.source_ref != expectation.source_ref
        || source_value.source_head != expectation.source_head
        || source_value.source_binding != expectation.source_binding
        || source.actor != source_value.actor
        || expectation.launched_at < source.recorded_at
        || expectation.launched_at > record.recorded_at
        || expectation.launch_receipt_digest.len() != 64
        || !expectation
            .launch_receipt_digest
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
        || serde_json::from_str::<serde_json::Value>(&source.value_json)
            .ok()
            .as_ref()
            != Some(&source.value)
        || expectation.source_binding.as_ref().is_some_and(|binding| {
            binding.get("ref").and_then(serde_json::Value::as_str)
                != expectation.source_ref.as_deref()
                || binding.get("head").and_then(serde_json::Value::as_str)
                    != expectation.source_head.as_deref()
        })
    {
        return Err(AttestationError::BindingMismatch);
    }
    verifier.authenticate(record, expectation)?;
    Ok(VerifiedExecutor {
        digest: record.digest.clone(),
        event_id: record.event_id,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_damaged_original_claim_cannot_downgrade_a_verified_executor_to_advisory() {
        let mut expected = bound_expectation();
        expected.source_claim.as_mut().unwrap().value["executor_provenance"] =
            serde_json::json!("advisory");
        assert_eq!(
            executor_eligibility(
                ExecutorProvenanceRequirement::Advisory,
                None,
                Some(&expected),
                &UnavailableLaunchReceiptVerifier
            ),
            Err(AttestationError::InvalidRecord)
        );
    }

    #[test]
    fn the_executor_namespace_cannot_be_written_as_ordinary_metadata() {
        assert!(reserved_key("executor.binding"));
        assert!(reserved_key("executor.source_claim"));
        assert!(reserved_key("executor.future_receipt"));
        assert!(!reserved_key("execution_notes"));
    }

    #[test]
    fn matching_parent_assertions_without_an_authenticated_receipt_are_not_verified() {
        assert_eq!(
            verify_executor_receipt(None, &expectation(), &UnavailableLaunchReceiptVerifier),
            Err(AttestationError::MissingReceipt)
        );
    }

    fn expectation() -> ExecutorExpectation {
        ExecutorExpectation {
            native_run: uuid::Uuid::from_u128(1),
            actor: "agent-codex".into(),
            source_job: uuid::Uuid::from_u128(2),
            source_step: uuid::Uuid::from_u128(3),
            source_slug: "review".into(),
            protocol_version: 7,
            configured_model: "gpt-6.1-sol".into(),
            controlled_effort: Some("low".into()),
            execution_handle: "process-4".into(),
            launch_boundary: "host.launch".into(),
            source_claim_digest: String::new(),
            source_claim_event: uuid::Uuid::nil(),
            source_claim: None,
            source_ref: Some("refs/heads/feat/example".into()),
            source_head: Some("1".repeat(40)),
            source_binding: None,
            executor_family: "codex.exec".into(),
            launched_at: "2020-01-01T00:00:00Z".parse().unwrap(),
            launch_receipt_digest: "a".repeat(64),
        }
    }

    fn receipt() -> FirstRecord {
        use boss_core::{
            actor::ActorId,
            job::{JobId, StepId},
            publisher::EventStamp,
        };
        FirstRecord::new(
            JobId::from_uuid(uuid::Uuid::from_u128(1)),
            StepId::from_uuid(uuid::Uuid::from_u128(4)),
            EXECUTOR_BINDING_KEY,
            &serde_json::to_value(expectation()).unwrap(),
            &EventStamp::new("authenticated-launch", ActorId::automation("launch")),
            uuid::Uuid::from_u128(5),
        )
    }

    struct TrustedTestBoundary;

    impl LaunchReceiptVerifier for TrustedTestBoundary {
        fn authenticate(
            &self,
            _: &FirstRecord,
            _: &ExecutorExpectation,
        ) -> Result<(), AttestationError> {
            Ok(())
        }
    }

    #[test]
    fn a_caller_record_with_matching_controls_still_needs_a_trusted_boundary() {
        let expected = bound_expectation();
        let record = bound_receipt(&expected);
        assert_eq!(
            verify_executor_receipt(Some(&record), &expected, &UnavailableLaunchReceiptVerifier),
            Err(AttestationError::UntrustedLaunchBoundary)
        );
    }

    #[test]
    fn a_source_claim_without_an_original_event_is_not_a_claim() {
        let mut expected = bound_expectation();
        expected.source_claim.as_mut().unwrap().event_id = uuid::Uuid::nil();
        expected.source_claim_event = uuid::Uuid::nil();
        let record = bound_receipt(&expected);
        assert_eq!(
            verify_executor_receipt(Some(&record), &expected, &TrustedTestBoundary),
            Err(AttestationError::InvalidRecord)
        );
    }

    #[test]
    fn an_authenticated_receipt_binds_every_executor_claim_field() {
        let original = bound_expectation();
        let record = bound_receipt(&original);
        assert!(verify_executor_receipt(Some(&record), &original, &TrustedTestBoundary).is_ok());
        let mut fields = vec![];
        for key in [
            "actor",
            "source_slug",
            "configured_model",
            "controlled_effort",
            "execution_handle",
            "launch_boundary",
        ] {
            let mut value = serde_json::to_value(&original).unwrap();
            value[key] = serde_json::json!("different");
            let mut changed = serde_json::from_value::<ExecutorExpectation>(value).unwrap();
            changed.source_claim = original.source_claim.clone();
            fields.push(changed);
        }
        for changed in fields {
            assert_eq!(
                verify_executor_receipt(Some(&record), &changed, &TrustedTestBoundary),
                Err(AttestationError::BindingMismatch)
            );
        }
    }

    #[test]
    fn a_receipt_from_another_run_is_refused_even_with_equal_controls() {
        let mut record = receipt();
        record.job_id = boss_core::job::JobId::from_uuid(uuid::Uuid::from_u128(99));
        assert_eq!(
            verify_executor_receipt(Some(&record), &expectation(), &TrustedTestBoundary),
            Err(AttestationError::BindingMismatch)
        );
    }

    #[test]
    fn a_modified_original_digest_is_refused() {
        let mut record = receipt();
        record.digest = "0".repeat(64);
        assert_eq!(
            verify_executor_receipt(Some(&record), &expectation(), &TrustedTestBoundary),
            Err(AttestationError::InvalidRecord)
        );
    }

    #[test]
    fn a_source_claim_never_defaults_a_missing_requirement_to_advisory() {
        let claim = serde_json::json!({
            "native_run": uuid::Uuid::from_u128(1), "source_job":uuid::Uuid::from_u128(2),
            "source_step":uuid::Uuid::from_u128(3), "source_slug":"review",
            "actor":"agent-codex", "protocol_version":7,
            "configured_model":"gpt-6.1-sol", "controlled_effort":"low"
        });
        assert!(serde_json::from_value::<SourceClaim>(claim).is_err());
    }

    #[test]
    fn a_receipt_without_the_original_source_claim_digest_is_refused() {
        assert_eq!(
            verify_executor_receipt(Some(&receipt()), &expectation(), &TrustedTestBoundary),
            Err(AttestationError::MissingSourceClaim)
        );
    }

    fn bound_expectation() -> ExecutorExpectation {
        use boss_core::{
            actor::ActorId,
            job::{JobId, StepId},
            publisher::EventStamp,
        };
        let mut expected = expectation();
        let value = serde_json::json!({"native_run":expected.native_run,"source_job":expected.source_job,"source_step":expected.source_step,"source_slug":expected.source_slug,"actor":expected.actor,"protocol_version":expected.protocol_version,"configured_model":expected.configured_model,"controlled_effort":expected.controlled_effort,"executor_provenance":"verified","source_ref":expected.source_ref,"source_head":expected.source_head,"source_binding":expected.source_binding});
        let source = FirstRecord::new(
            JobId::from_uuid(expected.native_run),
            StepId::from_uuid(uuid::Uuid::from_u128(4)),
            SOURCE_CLAIM_KEY,
            &value,
            &EventStamp::new(
                "jobs.claim",
                ActorId::RegisteredAgent(expected.actor.clone()),
            ),
            uuid::Uuid::from_u128(6),
        );
        expected.source_claim_digest = source.digest.clone();
        expected.source_claim_event = source.event_id;
        expected.launched_at = source.recorded_at;
        expected.source_claim = Some(source);
        expected
    }

    fn bound_receipt(expected: &ExecutorExpectation) -> FirstRecord {
        let mut record = receipt();
        let value = serde_json::to_value(expected).unwrap();
        let stamp = boss_core::publisher::EventStamp::new(
            "authenticated-launch",
            boss_core::actor::ActorId::automation("launch"),
        );
        record = FirstRecord::new(
            record.job_id,
            record.step_id,
            EXECUTOR_BINDING_KEY,
            &value,
            &stamp,
            record.event_id,
        );
        record
    }

    #[test]
    fn the_full_source_binding_cannot_change_after_launch() {
        let mut expected = bound_expectation();
        expected.source_binding = Some(
            serde_json::json!({"ref":expected.source_ref,"head":expected.source_head,"repo":"/declared/repo","plan":"original"}),
        );
        let record = bound_receipt(&expected);
        expected.source_binding.as_mut().unwrap()["plan"] = serde_json::json!("changed");
        assert_eq!(
            verify_executor_receipt(Some(&record), &expected, &TrustedTestBoundary),
            Err(AttestationError::BindingMismatch)
        );
    }

    #[test]
    fn a_binding_on_another_native_step_is_refused() {
        let expected = bound_expectation();
        let mut record = bound_receipt(&expected);
        record.step_id = boss_core::job::StepId::from_uuid(uuid::Uuid::from_u128(99));
        assert_eq!(
            verify_executor_receipt(Some(&record), &expected, &TrustedTestBoundary),
            Err(AttestationError::BindingMismatch)
        );
    }

    #[test]
    fn an_uncontrolled_effort_is_preserved_without_inventing_a_value() {
        let mut expected = bound_expectation();
        expected.controlled_effort = None;
        let mut source = expected.source_claim.take().unwrap();
        source.value["controlled_effort"] = serde_json::Value::Null;
        let stamp = boss_core::publisher::EventStamp::new(
            "jobs.claim",
            boss_core::actor::ActorId::RegisteredAgent(expected.actor.clone()),
        );
        source = FirstRecord::new(
            source.job_id,
            source.step_id,
            SOURCE_CLAIM_KEY,
            &source.value,
            &stamp,
            source.event_id,
        );
        expected.source_claim_digest = source.digest.clone();
        expected.launched_at = source.recorded_at;
        expected.source_claim = Some(source);
        let record = bound_receipt(&expected);
        assert!(verify_executor_receipt(Some(&record), &expected, &TrustedTestBoundary).is_ok());
    }

    #[test]
    fn a_declared_controlled_effort_cannot_be_blank() {
        let mut expected = bound_expectation();
        expected.controlled_effort = Some(" ".into());
        let mut source = expected.source_claim.take().unwrap();
        source.value["controlled_effort"] = serde_json::json!(" ");
        let stamp = boss_core::publisher::EventStamp::new(
            "jobs.claim",
            boss_core::actor::ActorId::RegisteredAgent(expected.actor.clone()),
        );
        source = FirstRecord::new(
            source.job_id,
            source.step_id,
            SOURCE_CLAIM_KEY,
            &source.value,
            &stamp,
            source.event_id,
        );
        expected.source_claim_digest = source.digest.clone();
        expected.launched_at = source.recorded_at;
        expected.source_claim = Some(source);
        let record = bound_receipt(&expected);
        assert_eq!(
            verify_executor_receipt(Some(&record), &expected, &TrustedTestBoundary),
            Err(AttestationError::BindingMismatch)
        );
    }

    #[test]
    fn a_launch_cannot_precede_its_original_source_claim() {
        let mut expected = bound_expectation();
        expected.launched_at =
            expected.source_claim.as_ref().unwrap().recorded_at - chrono::Duration::seconds(1);
        let record = bound_receipt(&expected);
        assert_eq!(
            verify_executor_receipt(Some(&record), &expected, &TrustedTestBoundary),
            Err(AttestationError::BindingMismatch)
        );
    }

    #[test]
    fn a_launch_receipt_binds_family_and_the_original_host_receipt_digest() {
        let expected = bound_expectation();
        let record = bound_receipt(&expected);
        let mut changed = expected.clone();
        changed.executor_family = "different".into();
        assert_eq!(
            verify_executor_receipt(Some(&record), &changed, &TrustedTestBoundary),
            Err(AttestationError::BindingMismatch)
        );
        changed = expected;
        changed.launch_receipt_digest = "b".repeat(64);
        assert_eq!(
            verify_executor_receipt(Some(&record), &changed, &TrustedTestBoundary),
            Err(AttestationError::BindingMismatch)
        );
    }

    #[test]
    fn the_same_eligibility_predicate_keeps_advisory_and_verified_outcomes_distinct() {
        assert_eq!(
            executor_eligibility(
                ExecutorProvenanceRequirement::Advisory,
                None,
                None,
                &UnavailableLaunchReceiptVerifier
            ),
            Ok(None)
        );
        assert_eq!(
            executor_eligibility(
                ExecutorProvenanceRequirement::Verified,
                None,
                None,
                &UnavailableLaunchReceiptVerifier
            ),
            Err(AttestationError::MissingSourceClaim)
        );
    }
}
