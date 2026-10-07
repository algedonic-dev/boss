//! A host acknowledgment is authenticated context plus an original owner
//! observation. Caller text alone is neither (approved 104a93d5).
use boss_core::publisher::EventStamp;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use super::{CredentialsError, receipt::RotationReceipt};

pub const PURPOSE: &str = "authenticated-runner-delivery";

/// One authoritative installed-command wire shape, also read by the broker's
/// recovery validator. Values stay outside this value-free command.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InstalledCommand {
    pub secret_namespace: String,
    pub secret_name: String,
    pub secret_key: String,
    pub secret_uid: String,
    pub precondition_version: String,
    pub value_length: usize,
    pub last_eight: String,
    pub replaced_staged_for: Value,
    pub carried_to_previous: Value,
    pub dropped_previous: Value,
    pub delivery: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Witness {
    version: u32,
    job_id: uuid::Uuid,
    credential_id: String,
    namespace: String,
    name: String,
    host: String,
    uid: String,
    attempt: uuid::Uuid,
    last_eight: String,
    promoted: bool,
    commands: std::collections::BTreeMap<String, Value>,
}

/// Metadata comes from the same fixed mounted generation as the presented
/// value, never the request body. The owner still checks the installed receipt.
pub(crate) fn resolve_context(
    dir: &std::path::Path,
    presented: &str,
    host: &str,
    slot: &str,
) -> Option<ResolvedDelivery> {
    use boss_core::machine_token::{read_slot_checked, verify};
    let root = dir.canonicalize().ok()?;
    let snapshot = match std::fs::symlink_metadata(root.join("..data")) {
        Ok(meta) if meta.file_type().is_symlink() => {
            let actual = root.join("..data").canonicalize().ok()?;
            if actual.parent() != Some(root.as_path()) {
                return None;
            }
            actual
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => root,
        _ => return None,
    };
    let actual = read_slot_checked(&snapshot, &format!("{host}.{slot}")).ok()??;
    if !verify(&actual, Some(presented)) {
        return None;
    }
    let packet = read_slot_checked(&snapshot, &format!("{host}.{slot}.minted-for")).ok()??;
    let packet = uuid::Uuid::parse_str(&packet).ok()?;
    let raw = read_slot_checked(&snapshot, &format!("{host}.recovery.{packet}.witness")).ok()??;
    let witness: Witness = serde_json::from_str(&raw).ok()?;
    let last_eight: String = actual
        .chars()
        .rev()
        .take(8)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect();
    if witness.version != 1
        || witness.job_id != packet
        || witness.host != host
        || witness.credential_id.trim().is_empty()
        || witness.namespace.trim().is_empty()
        || witness.name.trim().is_empty()
        || witness.uid.trim().is_empty()
        || witness.attempt.is_nil()
        || witness.last_eight != last_eight
        || witness.last_eight.len() != 8
        || witness.promoted != (slot == "current")
        || !matches!(slot, "next" | "current")
        || witness.commands.len() != super::RotationPhase::ALL.len()
        || super::RotationPhase::ALL.iter().any(|phase| {
            witness.commands.get(phase.as_str()).is_none_or(|command| {
                command["job_id"] != packet.to_string()
                    || command["host"] != host
                    || command["observation_id"]
                        != format!("{}:{}", witness.attempt, phase.as_str())
            })
        })
    {
        return None;
    }
    let installed = witness.commands.get("installed")?.clone();
    let mut specific = installed.clone();
    let map = specific.as_object_mut()?;
    for key in ["job_id", "host", "observation_id"] {
        map.remove(key);
    }
    let command: InstalledCommand = serde_json::from_value(specific).ok()?;
    if command.secret_namespace != witness.namespace
        || command.secret_name != witness.name
        || command.secret_key != format!("{host}.next")
        || command.secret_uid != witness.uid
        || command.value_length != actual.len()
        || command.last_eight != last_eight
        || command.delivery != "off-host"
        || command.precondition_version.is_empty()
        || command.precondition_version.len() > 128
        || !command
            .precondition_version
            .bytes()
            .all(|b| b.is_ascii_digit())
        || !(command.replaced_staged_for.is_null()
            || command
                .replaced_staged_for
                .as_str()
                .is_some_and(|s| uuid::Uuid::parse_str(s).is_ok()))
        || [&command.carried_to_previous, &command.dropped_previous]
            .iter()
            .any(|value| {
                !value.is_null()
                    && !value.as_str().is_some_and(|s| {
                        s.len() == 8
                            && s.bytes()
                                .all(|b| b.is_ascii_alphanumeric() || b"-_".contains(&b))
                    })
            })
    {
        return None;
    }
    Some(ResolvedDelivery {
        credential_id: witness.credential_id,
        host: host.into(),
        request: DeliveryRequest {
            job_id: packet,
            attempt: witness.attempt,
            secret_uid: witness.uid,
        },
        installed,
    })
}

#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct DeliveryRequest {
    pub job_id: uuid::Uuid,
    pub attempt: uuid::Uuid,
    pub secret_uid: String,
}

/// Constructed only by the credential resolver from its actual mounted
/// generation. No credential value crosses this boundary.
#[derive(Clone, Debug)]
pub struct ResolvedDelivery {
    pub(crate) credential_id: String,
    pub(crate) host: String,
    pub(crate) request: DeliveryRequest,
    pub(crate) installed: Value,
}

/// Value-free expectation supplied by a consumer. It grants no authority;
/// the retained owner receipt, fetched through the owner read door, is required.
pub fn delivery_evidence(
    credential_id: &str,
    host: &str,
    request: &DeliveryRequest,
    installed: &Value,
) -> Value {
    json!({"credential_id":credential_id,"purpose":PURPOSE,
        "host":host,"job_id":request.job_id,"attempt":request.attempt,
        "secret_uid":request.secret_uid,
        "installed_observation_id":format!("{}:installed", request.attempt),
        "installed_command":installed})
}

pub fn receipt_matches_delivery(
    receipt: &RotationReceipt,
    credential_id: &str,
    host: &str,
    request: &DeliveryRequest,
    installed: &Value,
) -> bool {
    let expected = delivery_evidence(credential_id, host, request, installed);
    receipt.version == 1
        && !receipt.event_id.is_nil()
        && receipt.credential_id == credential_id
        && receipt.phase == "verified"
        && receipt.actor == crate::runner_credential::ACTOR
        && receipt.observation_id == format!("{}:delivered", request.attempt)
        && serde_json::from_str::<Value>(&receipt.evidence_json)
            .ok()
            .as_ref()
            == Some(&expected)
        && receipt.canonical == boss_core::job::canonical_json_bytes(&expected)
}

impl ResolvedDelivery {
    pub fn admits(&self, id: &str, request: &DeliveryRequest) -> bool {
        self.credential_id == id && &self.request == request
    }

    pub fn observation_id(&self) -> String {
        format!("{}:delivered", self.request.attempt)
    }

    pub fn installed_observation_id(&self) -> String {
        format!("{}:installed", self.request.attempt)
    }

    pub fn evidence(&self) -> Value {
        delivery_evidence(
            &self.credential_id,
            &self.host,
            &self.request,
            &self.installed,
        )
    }

    /// Called under the credential owner's serialization boundary. A Secret
    /// witness describes intent; this original receipt proves its owner commit.
    pub fn validate_installed(&self, receipt: &RotationReceipt) -> Result<(), CredentialsError> {
        let mut expected = self.installed.clone();
        let map = expected.as_object_mut().ok_or_else(|| {
            CredentialsError::InvalidObservation("invalid installed owner context".into())
        })?;
        map.remove("observation_id");
        map.insert("credential_id".into(), json!(self.credential_id));
        let original: Value = serde_json::from_str(&receipt.evidence_json)
            .map_err(|_| CredentialsError::ObservationConflict)?;
        if receipt.version != 1
            || receipt.event_id.is_nil()
            || receipt.credential_id != self.credential_id
            || receipt.phase != "installed"
            || receipt.observation_id != self.installed_observation_id()
            || receipt.actor.is_empty()
            || original != expected
            || receipt.canonical != boss_core::job::canonical_json_bytes(&original)
        {
            return Err(CredentialsError::ObservationConflict);
        }
        Ok(())
    }

    pub fn validate_actor(&self, stamp: &EventStamp) -> Result<(), CredentialsError> {
        if stamp.actor().to_string() != crate::runner_credential::ACTOR {
            return Err(CredentialsError::InvalidObservation(
                "delivery requires the resolved runner actor".into(),
            ));
        }
        Ok(())
    }
}

/// Both adapters call this while holding the credential owner's lock. Equal
/// instants are ambiguous, not an invented ordering between generations.
pub(crate) fn prepare_receipt(
    context: &ResolvedDelivery,
    receipts: &[RotationReceipt],
    stamp: &EventStamp,
) -> Result<super::receipt::RotationOutcome, CredentialsError> {
    use super::receipt::RotationOutcome;
    context.validate_actor(stamp)?;
    let evidence = context.evidence();
    if let Some(original) = receipts.iter().find(|receipt| {
        receipt.credential_id == context.credential_id
            && receipt.phase == "verified"
            && receipt.observation_id == context.observation_id()
    }) {
        return if original.matches(&evidence, stamp) {
            Ok(RotationOutcome::Replayed {
                receipt: original.clone(),
            })
        } else {
            Err(CredentialsError::ObservationConflict)
        };
    }
    let installed = receipts
        .iter()
        .find(|receipt| {
            receipt.credential_id == context.credential_id
                && receipt.phase == "installed"
                && receipt.observation_id == context.installed_observation_id()
        })
        .ok_or(CredentialsError::ObservationConflict)?;
    context.validate_installed(installed)?;
    if receipts.iter().any(|receipt| {
        receipt.credential_id == context.credential_id
            && receipt.phase == "installed"
            && receipt.event_id != installed.event_id
            && receipt.timestamp >= installed.timestamp
    }) {
        return Err(CredentialsError::ObservationConflict);
    }
    let receipt = RotationReceipt::new(
        &context.credential_id,
        super::RotationPhase::Verified,
        context.observation_id(),
        &evidence,
        stamp,
    )?;
    Ok(RotationOutcome::Recorded { receipt })
}

// Each invocation below is a separate owning command/transaction, including
// concurrent retries. Its stamp is minted once before that command begins.
#[cfg(test)]
fn owner_request_stamp(actor: boss_core::actor::ActorId) -> EventStamp {
    EventStamp::new("jobs", actor)
}

#[cfg(all(test, feature = "postgres"))]
mod tests {
    use super::*;
    use crate::credentials::{CredentialInput, CredentialsRegistry, PgCredentials, RotationPhase};

    #[tokio::test]
    async fn postgres_retains_one_original_authenticated_delivery() {
        let db = boss_testing::TestDb::new().await;
        let registry = PgCredentials::new(db.pool.clone());
        let id = "ops-runner-credential-forge";
        let stage =
            owner_request_stamp(boss_core::actor::ActorId::Automation("fake-broker".into()));
        registry
            .publish(
                "fake",
                &[CredentialInput {
                    id: id.into(),
                    kind: "ops-runner-credential".into(),
                    issuer: "fake-broker".into(),
                    principal: "runner:ops".into(),
                    scopes: vec![],
                    storage_location: "fake Secret".into(),
                    consumers: vec![],
                    rotation_policy: "on-demand".into(),
                    notes: String::new(),
                }],
                &stage,
            )
            .await
            .unwrap();
        let attempt = uuid::Uuid::new_v4();
        let packet = uuid::Uuid::new_v4();
        let installed = json!({"job_id":packet,"host":"forge",
            "observation_id":format!("{attempt}:installed"),"secret_uid":"fake-uid"});
        registry
            .record_rotation(id, RotationPhase::Installed, installed.clone(), &stage)
            .await
            .unwrap();
        let context = ResolvedDelivery {
            credential_id: id.into(),
            host: "forge".into(),
            request: DeliveryRequest {
                job_id: packet,
                attempt,
                secret_uid: "fake-uid".into(),
            },
            installed,
        };
        let stamp = owner_request_stamp(boss_core::actor::ActorId::Automation("ops-runner".into()));
        let retry = owner_request_stamp(stamp.actor().clone());
        let (first, second) = tokio::join!(
            registry.record_delivery(&context, &stamp),
            registry.record_delivery(&context, &retry)
        );
        use super::super::receipt::RotationOutcome;
        let (first, second) = match (first.unwrap(), second.unwrap()) {
            (
                RotationOutcome::Recorded { receipt: first },
                RotationOutcome::Replayed { receipt: second },
            )
            | (
                RotationOutcome::Replayed { receipt: second },
                RotationOutcome::Recorded { receipt: first },
            ) => (first, second),
            other => panic!("concurrent owner commands record once then replay: {other:?}"),
        };
        assert_eq!(first, second);
        assert_eq!(
            registry.delivery_receipt(id, attempt).await.unwrap(),
            Some(first)
        );
        let count: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM event_outbox WHERE payload->>'purpose'=$1")
                .bind(PURPOSE)
                .fetch_one(&db.pool)
                .await
                .unwrap();
        assert_eq!(count, 1);
    }
}

#[cfg(test)]
mod owner_order_tests {
    use super::*;
    use crate::credentials::{
        CredentialRow, CredentialsRegistry, InMemoryCredentials, RotationPhase,
    };

    #[tokio::test]
    async fn latest_install_is_required_for_new_delivery_but_original_replay_is_conserved() {
        for acknowledged in [false, true] {
            let id = "fake-runner-owner";
            let registry = InMemoryCredentials::new(vec![CredentialRow {
                id: id.into(),
                kind: "ops-runner-credential".into(),
                issuer: "fake-owner".into(),
                principal: "runner:ops".into(),
                scopes: json!([]),
                storage_location: "fake mount".into(),
                consumers: json!([]),
                rotation_policy: "on-demand".into(),
                rotated_at: None,
                notes: String::new(),
            }]);
            let attempt = uuid::Uuid::new_v4();
            let packet = uuid::Uuid::new_v4();
            let installed = json!({"job_id":packet,"host":"forge",
                "observation_id":format!("{attempt}:installed"),"secret_uid":"fake-uid"});
            let stage =
                owner_request_stamp(boss_core::actor::ActorId::Automation("fake-broker".into()));
            registry
                .record_rotation(id, RotationPhase::Installed, installed.clone(), &stage)
                .await
                .unwrap();
            let context = ResolvedDelivery {
                credential_id: id.into(),
                host: "forge".into(),
                request: DeliveryRequest {
                    job_id: packet,
                    attempt,
                    secret_uid: "fake-uid".into(),
                },
                installed,
            };
            let stamp =
                owner_request_stamp(boss_core::actor::ActorId::Automation("ops-runner".into()));
            let original = if acknowledged {
                Some(registry.record_delivery(&context, &stamp).await.unwrap())
            } else {
                None
            };
            registry
                .record_rotation(
                    id,
                    RotationPhase::Installed,
                    json!({"observation_id":format!("{}:installed", uuid::Uuid::new_v4()),
                    "job_id":uuid::Uuid::new_v4(),"host":"forge","secret_uid":"fake-uid"}),
                    &owner_request_stamp(stage.actor().clone()),
                )
                .await
                .unwrap();
            let count = registry.recorded_events().unwrap().len();
            let result = registry
                .record_delivery(&context, &owner_request_stamp(stamp.actor().clone()))
                .await;
            if let Some(super::super::receipt::RotationOutcome::Recorded { receipt: original }) =
                original
            {
                let super::super::receipt::RotationOutcome::Replayed { receipt } = result.unwrap()
                else {
                    panic!("original delivery must replay");
                };
                assert_eq!(receipt, original);
            } else {
                assert!(
                    matches!(result, Err(CredentialsError::ObservationConflict)),
                    "an unacknowledged old installation cannot become a new delivery"
                );
            }
            assert_eq!(registry.recorded_events().unwrap().len(), count);
        }
    }
}
