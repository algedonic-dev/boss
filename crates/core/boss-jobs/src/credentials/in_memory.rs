//! In-memory adapter for `CredentialsRegistry` — the port-level test
//! double. Mirrors the Pg semantics that matter: `list` is ordered by
//! id (byte order), an unknown rotation target is `UnknownCredential`,
//! the install phase stamps `rotated_at` with the event's own instant,
//! and a batch naming a `rotation_policy` the schema refuses lands
//! nothing. The adapters-agree suite
//! (`the_adapters_agree_on_the_credentials_registry_pg.rs`) holds both
//! adapters to each of those.

use async_trait::async_trait;
use std::sync::Mutex;

use boss_core::event::Event;
use boss_core::publisher::EventStamp;

use super::port::{CredentialsError, CredentialsRegistry, declared_event};
use super::types::{
    CredentialInput, CredentialRow, CredentialsBatchOutcome, ROTATION_POLICIES, RotationPhase,
};

/// The registry row a declaration lands as: JSON arrays for the two
/// list columns (the Pg adapter's `scopes`/`consumers` are JSONB),
/// `rotated_at` NULL — a declaration never claims a rotation instant.
fn row_of(c: &CredentialInput) -> Result<CredentialRow, CredentialsError> {
    Ok(CredentialRow {
        id: c.id.clone(),
        kind: c.kind.clone(),
        issuer: c.issuer.clone(),
        principal: c.principal.clone(),
        scopes: serde_json::to_value(&c.scopes)
            .map_err(|e| CredentialsError::Storage(e.to_string()))?,
        storage_location: c.storage_location.clone(),
        consumers: serde_json::to_value(&c.consumers)
            .map_err(|e| CredentialsError::Storage(e.to_string()))?,
        rotation_policy: c.rotation_policy.clone(),
        rotated_at: None,
        notes: c.notes.clone(),
    })
}

#[derive(Default)]
pub struct InMemoryCredentials {
    state: Mutex<State>,
}

#[derive(Default)]
struct State {
    rows: Vec<CredentialRow>,
    events: Vec<Event>,
    receipts: Vec<super::receipt::RotationReceipt>,
}

impl InMemoryCredentials {
    pub fn new(rows: Vec<CredentialRow>) -> Self {
        Self {
            state: Mutex::new(State {
                rows,
                ..State::default()
            }),
        }
    }

    /// Every rotation event recorded through this adapter, in order —
    /// what a Pg deployment would find on the outbox.
    pub fn recorded_events(&self) -> Result<Vec<Event>, CredentialsError> {
        Ok(self
            .state
            .lock()
            .map_err(|_| CredentialsError::Storage("credential state lock poisoned".into()))?
            .events
            .clone())
    }
}

#[async_trait]
impl CredentialsRegistry for InMemoryCredentials {
    async fn record_delivery(
        &self,
        context: &super::runner_delivery::ResolvedDelivery,
        stamp: &EventStamp,
    ) -> Result<super::receipt::RotationOutcome, CredentialsError> {
        use super::receipt::RotationOutcome;
        context.validate_actor(stamp)?;
        let mut state = self
            .state
            .lock()
            .map_err(|_| CredentialsError::Storage("credential state lock poisoned".into()))?;
        let id = &context.credential_id;
        let row = state
            .rows
            .iter()
            .find(|row| &row.id == id)
            .ok_or_else(|| CredentialsError::UnknownCredential(id.clone()))?;
        if row.kind != "ops-runner-credential" {
            return Err(CredentialsError::ObservationConflict);
        }
        let outcome = super::runner_delivery::prepare_receipt(context, &state.receipts, stamp)?;
        if let RotationOutcome::Recorded { receipt } = &outcome {
            state
                .events
                .push(receipt.event(stamp, context.evidence(), RotationPhase::Verified)?);
            state.receipts.push(receipt.clone());
        }
        Ok(outcome)
    }

    async fn delivery_receipt(
        &self,
        id: &str,
        attempt: uuid::Uuid,
    ) -> Result<Option<super::receipt::RotationReceipt>, CredentialsError> {
        let state = self
            .state
            .lock()
            .map_err(|_| CredentialsError::Storage("credential state lock poisoned".into()))?;
        Ok(state
            .receipts
            .iter()
            .find(|receipt| {
                receipt.credential_id == id
                    && receipt.phase == "verified"
                    && receipt.observation_id == format!("{attempt}:delivered")
            })
            .cloned())
    }

    async fn restore_rotation(&self, event: &Event) -> Result<(), CredentialsError> {
        use super::receipt::RotationReceipt;
        let receipt = RotationReceipt::from_event(event)?;
        let phase = RotationPhase::ALL
            .into_iter()
            .find(|p| p.event_kind() == event.kind)
            .ok_or(CredentialsError::ObservationConflict)?;
        let id = receipt
            .as_ref()
            .map(|r| r.credential_id.as_str())
            .or_else(|| {
                event
                    .payload
                    .get("credential_id")
                    .and_then(serde_json::Value::as_str)
            })
            .ok_or(CredentialsError::ObservationConflict)?
            .to_owned();
        let mut state = self
            .state
            .lock()
            .map_err(|_| CredentialsError::Storage("credential state lock poisoned".into()))?;
        if !state.rows.iter().any(|r| r.id == id) {
            return Err(CredentialsError::UnknownCredential(id));
        }
        if let Some(receipt) = receipt {
            if let Some(original) = state.receipts.iter().find(|r| {
                r.credential_id == receipt.credential_id
                    && r.phase == receipt.phase
                    && r.observation_id == receipt.observation_id
            }) {
                if *original != receipt {
                    return Err(CredentialsError::ObservationConflict);
                }
            } else {
                state.receipts.push(receipt);
            }
        }
        if phase == RotationPhase::Installed {
            for row in state.rows.iter_mut().filter(|r| r.id == id) {
                row.rotated_at = Some(event.timestamp);
            }
        }
        Ok(())
    }
    async fn list(&self) -> Result<Vec<CredentialRow>, CredentialsError> {
        let mut rows = self
            .state
            .lock()
            .map_err(|_| CredentialsError::Storage("credential state lock poisoned".into()))?
            .rows
            .clone();
        rows.sort_by(|a, b| a.id.cmp(&b.id));
        Ok(rows)
    }

    async fn get(&self, id: &str) -> Result<Option<CredentialRow>, CredentialsError> {
        Ok(self
            .state
            .lock()
            .map_err(|_| CredentialsError::Storage("credential state lock poisoned".into()))?
            .rows
            .iter()
            .find(|r| r.id == id)
            .cloned())
    }

    async fn publish(
        &self,
        tenant_id: &str,
        declared: &[CredentialInput],
        stamp: &EventStamp,
    ) -> Result<CredentialsBatchOutcome, CredentialsError> {
        // The schema's CHECK on `rotation_policy` runs on every declared
        // row — a held id too, since a CHECK is met before the conflict
        // is — and refuses the batch's whole transaction. Judged here
        // before anything is written, so a refused batch lands nothing,
        // as it lands nothing there; until backlog be459ab9's
        // adapters-agree suite (2026-09-29) the double took any spelling.
        if let Some(bad) = declared
            .iter()
            .find(|c| !ROTATION_POLICIES.contains(&c.rotation_policy.as_str()))
        {
            return Err(CredentialsError::Storage(format!(
                "credential {}: rotation_policy {:?} is none of {ROTATION_POLICIES:?}",
                bad.id, bad.rotation_policy
            )));
        }
        let mut state = self
            .state
            .lock()
            .map_err(|_| CredentialsError::Storage("credential state lock poisoned".into()))?;
        let State { rows, events, .. } = &mut *state;
        let mut inserted = 0;
        for c in declared {
            if rows.iter().any(|r| r.id == c.id) {
                continue;
            }
            rows.push(row_of(c)?);
            events.push(declared_event(stamp, tenant_id, c)?);
            inserted += 1;
        }
        Ok(CredentialsBatchOutcome {
            received: declared.len(),
            inserted,
        })
    }

    async fn record_rotation(
        &self,
        id: &str,
        phase: RotationPhase,
        mut evidence: serde_json::Value,
        stamp: &EventStamp,
    ) -> Result<super::receipt::RotationOutcome, CredentialsError> {
        use super::receipt::{RotationOutcome, RotationReceipt, observation};
        let observation_id = observation(&mut evidence)?;
        if observation_id.is_some() {
            evidence["credential_id"] = serde_json::json!(id);
        }
        let mut state = self
            .state
            .lock()
            .map_err(|_| CredentialsError::Storage("credential state lock poisoned".into()))?;
        if !state.rows.iter().any(|r| r.id == id) {
            return Err(CredentialsError::UnknownCredential(id.to_string()));
        }
        if let Some(observation_id) = &observation_id
            && let Some(receipt) = state.receipts.iter().find(|r| {
                r.credential_id == id
                    && r.phase == phase.as_str()
                    && r.observation_id == *observation_id
            })
        {
            return if receipt.matches(&evidence, stamp) {
                Ok(RotationOutcome::Replayed {
                    receipt: receipt.clone(),
                })
            } else {
                Err(CredentialsError::ObservationConflict)
            };
        }
        let receipt = observation_id
            .map(|observation_id| RotationReceipt::new(id, phase, observation_id, &evidence, stamp))
            .transpose()?;
        let event = match &receipt {
            Some(receipt) => receipt.event(stamp, evidence, phase)?,
            None => stamp.event(phase.event_kind(), evidence),
        };
        if phase == RotationPhase::Installed {
            for row in state.rows.iter_mut().filter(|r| r.id == id) {
                row.rotated_at = Some(stamp.timestamp);
            }
        }
        state.events.push(event);
        Ok(match receipt {
            Some(receipt) => {
                state.receipts.push(receipt.clone());
                RotationOutcome::Recorded { receipt }
            }
            None => RotationOutcome::LegacyRecorded,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    pub(crate) fn row(id: &str) -> CredentialRow {
        CredentialRow {
            id: id.into(),
            kind: "forgejo-access-token".into(),
            issuer: "forgejo (10.20.0.15)".into(),
            principal: "user david".into(),
            scopes: json!(["write:repository"]),
            storage_location: "k8s Secret boss-dev/boss-dev-forge-token key token".into(),
            consumers: json!([{ "kind": "secret-mount", "location": "/etc/boss-train/forge.token" }]),
            rotation_policy: "on-demand".into(),
            rotated_at: None,
            notes: String::new(),
        }
    }

    fn stamp() -> EventStamp {
        EventStamp::new(
            "jobs",
            boss_core::actor::ActorId::Automation("rule:broker-test".into()),
        )
    }

    #[tokio::test]
    async fn list_is_ordered_by_id_whatever_the_insert_order() {
        let repo = InMemoryCredentials::new(vec![row("zeta"), row("alpha")]);
        let ids: Vec<String> = repo
            .list()
            .await
            .unwrap()
            .into_iter()
            .map(|r| r.id)
            .collect();
        assert_eq!(ids, vec!["alpha", "zeta"]);
    }

    #[tokio::test]
    async fn get_answers_the_row_or_none() {
        let repo = InMemoryCredentials::new(vec![row("boss-dev-forge-token")]);
        let got = repo.get("boss-dev-forge-token").await.unwrap().unwrap();
        assert_eq!(got.kind, "forgejo-access-token");
        assert!(
            repo.get("no-such-credential").await.unwrap().is_none(),
            "an unknown id is None, not an error — the HTTP door owns the 404"
        );
    }

    #[tokio::test]
    async fn an_empty_registry_lists_empty() {
        let repo = InMemoryCredentials::default();
        assert!(repo.list().await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn a_recorded_phase_becomes_an_event_of_its_kind() {
        let repo = InMemoryCredentials::new(vec![row("boss-dev-forge-token")]);
        repo.record_rotation(
            "boss-dev-forge-token",
            RotationPhase::Minted,
            json!({ "token_name": "boss-dev-forge-token-7ee101aa" }),
            &stamp(),
        )
        .await
        .unwrap();
        let events = repo.recorded_events().unwrap();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].kind, "credential.minted");
        assert_eq!(events[0].source, "jobs");
        assert_eq!(
            events[0].payload["token_name"],
            "boss-dev-forge-token-7ee101aa"
        );
        assert_eq!(
            events[0].payload["_actor"], "automation:rule:broker-test",
            "the stamp's actor rides the payload exactly as EventStamp injects it"
        );
    }

    #[tokio::test]
    async fn only_the_install_phase_stamps_rotated_at() {
        let repo = InMemoryCredentials::new(vec![row("boss-dev-forge-token")]);
        let s = stamp();
        repo.record_rotation("boss-dev-forge-token", RotationPhase::Minted, json!({}), &s)
            .await
            .unwrap();
        assert!(
            repo.get("boss-dev-forge-token")
                .await
                .unwrap()
                .unwrap()
                .rotated_at
                .is_none(),
            "a mint changes nothing installed — rotated_at waits for the install"
        );
        repo.record_rotation(
            "boss-dev-forge-token",
            RotationPhase::Installed,
            json!({}),
            &s,
        )
        .await
        .unwrap();
        assert_eq!(
            repo.get("boss-dev-forge-token")
                .await
                .unwrap()
                .unwrap()
                .rotated_at,
            Some(s.timestamp),
            "the row bind and the event share ONE instant (stamp.timestamp)"
        );
    }

    fn declaration(id: &str) -> CredentialInput {
        CredentialInput {
            id: id.into(),
            kind: "stripe-restricted-key".into(),
            issuer: "stripe (the operator's dashboard)".into(),
            principal: "the company's Stripe account".into(),
            scopes: vec!["charges: read".into()],
            storage_location:
                "k8s Secret boss/boss-credential-broker-root key stripe-restricted-read".into(),
            consumers: vec![super::super::types::Consumer {
                kind: "env".into(),
                location: "dispatcher env BOSS_BROKER_STRIPE_KEY".into(),
            }],
            rotation_policy: "on-demand".into(),
            notes: "declared by the tenant".into(),
        }
    }

    /// The declaration door (backlog ee368d0c): insert-if-absent by id,
    /// one `credential.declared` per row INSERTED carrying `declared_by`
    /// and the tenant, none for a kept row — and the kept row keeps
    /// what the rotation path wrote (`rotated_at`), because a
    /// declaration is knowledge about the credential, not its history.
    #[tokio::test]
    async fn a_declaration_inserts_absent_rows_keeps_held_ones_and_leaves_one_fact_per_insert() {
        let repo = InMemoryCredentials::new(vec![row("boss-dev-forge-token")]);
        let s = stamp();
        repo.record_rotation(
            "boss-dev-forge-token",
            RotationPhase::Installed,
            json!({}),
            &s,
        )
        .await
        .unwrap();
        let mut held = declaration("boss-dev-forge-token");
        held.notes = "a declaration must not erase the rotation's book-keeping".into();
        let out = repo
            .publish("acme", &[held, declaration("stripe-restricted-read")], &s)
            .await
            .unwrap();
        assert_eq!((out.received, out.inserted), (2, 1));
        let kept = repo.get("boss-dev-forge-token").await.unwrap().unwrap();
        assert_eq!(
            kept.rotated_at,
            Some(s.timestamp),
            "the held row is untouched"
        );
        assert_eq!(kept.notes, "", "the held row's notes are untouched");
        let new = repo.get("stripe-restricted-read").await.unwrap().unwrap();
        assert_eq!(new.scopes, json!(["charges: read"]));
        assert!(
            new.rotated_at.is_none(),
            "a declaration claims no rotation instant"
        );
        let declared: Vec<_> = repo
            .recorded_events()
            .unwrap()
            .into_iter()
            .filter(|e| e.kind == "credential.declared")
            .collect();
        assert_eq!(
            declared.len(),
            1,
            "one fact per inserted row, none for the kept one"
        );
        assert_eq!(declared[0].payload["id"], "stripe-restricted-read");
        assert_eq!(declared[0].payload["tenant_id"], "acme");
        assert_eq!(
            declared[0].payload["declared_by"], declared[0].payload["_actor"],
            "declared_by and the stamp's actor are one value"
        );
        // A second publish inserts nothing and records nothing.
        let again = repo
            .publish("acme", &[declaration("stripe-restricted-read")], &s)
            .await
            .unwrap();
        assert_eq!((again.received, again.inserted), (1, 0));
        assert_eq!(
            repo.recorded_events()
                .unwrap()
                .iter()
                .filter(|e| e.kind == "credential.declared")
                .count(),
            1
        );
    }

    #[tokio::test]
    async fn a_rotation_against_an_unknown_credential_is_refused_loudly() {
        let repo = InMemoryCredentials::new(vec![]);
        let err = repo
            .record_rotation("ghost", RotationPhase::Minted, json!({}), &stamp())
            .await
            .unwrap_err();
        assert!(matches!(err, CredentialsError::UnknownCredential(id) if id == "ghost"));
        assert!(
            repo.recorded_events().unwrap().is_empty(),
            "no event may detach from the row it annotates"
        );
    }

    #[tokio::test]
    async fn replaying_one_phase_observation_preserves_its_original_event_and_rotation_instant() {
        let repo = InMemoryCredentials::new(vec![row("boss-dev-forge-token")]);
        let evidence = json!({
            "observation_id": "917e55fc-f19b-43aa-951f-7d763805b172",
            "job_id": "a2dc6dfb-ea69-4bfb-9867-f9d509fb752b",
            "host": "forge",
            "last_eight": "fixture1"
        });
        let first = stamp();
        repo.record_rotation(
            "boss-dev-forge-token",
            RotationPhase::Installed,
            evidence.clone(),
            &first,
        )
        .await
        .unwrap();
        let original = repo.recorded_events().unwrap()[0].clone();
        repo.record_rotation(
            "boss-dev-forge-token",
            RotationPhase::Installed,
            evidence,
            &stamp(),
        )
        .await
        .unwrap();
        let events = repo.recorded_events().unwrap();
        assert_eq!(
            events.len(),
            1,
            "one durable observation must leave one event despite lost acknowledgement"
        );
        assert_eq!(events[0].id, original.id);
        assert_eq!(
            repo.get("boss-dev-forge-token")
                .await
                .unwrap()
                .unwrap()
                .rotated_at,
            Some(first.timestamp)
        );
    }

    #[tokio::test]
    async fn conflicting_phase_observation_refuses_without_a_second_event_or_rotation_change() {
        let repo = InMemoryCredentials::new(vec![row("boss-dev-forge-token")]);
        let observation = "917e55fc-f19b-43aa-951f-7d763805b172";
        let first = stamp();
        repo.record_rotation(
            "boss-dev-forge-token",
            RotationPhase::Installed,
            json!({"observation_id": observation, "last_eight": "original"}),
            &first,
        )
        .await
        .unwrap();
        let conflict = repo
            .record_rotation(
                "boss-dev-forge-token",
                RotationPhase::Installed,
                json!({"observation_id": observation, "last_eight": "different"}),
                &stamp(),
            )
            .await;
        assert!(
            conflict.is_err(),
            "same observation with different evidence must refuse"
        );
        assert_eq!(repo.recorded_events().unwrap().len(), 1);
        assert_eq!(
            repo.get("boss-dev-forge-token")
                .await
                .unwrap()
                .unwrap()
                .rotated_at,
            Some(first.timestamp)
        );
    }

    #[tokio::test]
    async fn distinct_phase_observations_record_distinct_effects() {
        let repo = InMemoryCredentials::new(vec![row("boss-dev-forge-token")]);
        for id in [
            "917e55fc-f19b-43aa-951f-7d763805b172",
            "ac275a7b-faf2-4769-ab46-9d2f5e50ae39",
        ] {
            repo.record_rotation(
                "boss-dev-forge-token",
                RotationPhase::Installed,
                json!({"observation_id": id, "last_eight": "fixture1"}),
                &stamp(),
            )
            .await
            .unwrap();
        }
        assert_eq!(
            repo.recorded_events().unwrap().len(),
            2,
            "different observations remain separate facts"
        );
    }

    #[tokio::test]
    async fn owner_restoration_retains_the_receipt_without_reemitting_the_event() {
        let original = InMemoryCredentials::new(vec![row("fixture")]);
        let evidence = json!({"observation_id":"restore-1","measured":9.0});
        original
            .record_rotation(
                "fixture",
                RotationPhase::Installed,
                evidence.clone(),
                &stamp(),
            )
            .await
            .unwrap();
        let event = original.recorded_events().unwrap()[0].clone();
        let restored = InMemoryCredentials::new(vec![row("fixture")]);
        restored.restore_rotation(&event).await.unwrap();
        restored.restore_rotation(&event).await.unwrap();
        let replay = restored
            .record_rotation("fixture", RotationPhase::Installed, evidence, &stamp())
            .await
            .unwrap();
        assert!(
            matches!(replay, super::super::receipt::RotationOutcome::Replayed { receipt } if receipt.event_id == event.id)
        );
        assert!(
            restored.recorded_events().unwrap().is_empty(),
            "restoration and replay emit nothing"
        );
        assert_eq!(
            restored.get("fixture").await.unwrap().unwrap().rotated_at,
            Some(event.timestamp)
        );
        let mut damaged = event.clone();
        damaged.payload["measured"] = json!(10);
        assert!(restored.restore_rotation(&damaged).await.is_err());
    }

    #[tokio::test]
    async fn legacy_callers_cannot_supply_an_owner_receipt() {
        let repo = InMemoryCredentials::new(vec![row("fixture")]);
        assert!(
            repo.record_rotation(
                "fixture",
                RotationPhase::Installed,
                json!({"_rotation_receipt":{"version":1}}),
                &stamp()
            )
            .await
            .is_err()
        );
        assert!(repo.recorded_events().unwrap().is_empty());
        assert!(
            repo.get("fixture")
                .await
                .unwrap()
                .unwrap()
                .rotated_at
                .is_none()
        );
    }
}
