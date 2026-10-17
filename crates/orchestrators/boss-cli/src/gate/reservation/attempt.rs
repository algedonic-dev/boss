//! One durable sending claim and one non-serializable, single-use permit.
//! A restarted process can reconcile evidence, never reconstruct this permit.
use super::*;

pub(super) const KEY: &str = model::ATTEMPT;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Proof {
    pub file_id: String,
    pub sha256: String,
    pub size_bytes: u64,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(super) enum State {
    Sending,
    NotStarted,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Attempt {
    pub contract_version: u32,
    pub id: Uuid,
    pub actor: String,
    pub intent_sha256: String,
    pub intent_file_id: String,
    pub reservation_generation: Uuid,
    pub previous: Option<Uuid>,
    pub state: State,
    pub no_child: Option<Proof>,
}

pub(super) fn read(metadata: &Value) -> Result<Option<Attempt>> {
    let Some(value) = metadata.get(KEY) else {
        return Ok(None);
    };
    let attempt: Attempt =
        serde_json::from_value(value.clone()).context("native attempt is unreadable")?;
    if attempt.contract_version != 1
        || attempt.id.is_nil()
        || attempt.actor.is_empty()
        || attempt.reservation_generation.is_nil()
        || attempt.previous.is_some_and(|id| id.is_nil())
        || attempt.intent_sha256.len() != 64
        || attempt.intent_file_id.is_empty()
        || matches!(attempt.state, State::Sending) != attempt.no_child.is_none()
    {
        bail!("native attempt identity/state is malformed; no send is authorized");
    }
    let protected = metadata
        .pointer("/_guarded_metadata/fields")
        .and_then(Value::as_array)
        .is_some_and(|fields| fields.iter().any(|field| field.as_str() == Some(KEY)));
    if !protected {
        bail!("native attempt lacks its persistent guarded binding");
    }
    Ok(Some(attempt))
}

pub(super) struct Claim {
    command: Command,
    attempt: Attempt,
    intent: LaunchIntent,
}

/// Deliberately neither Clone nor Deserialize. Only the originating
/// unsent invocation can obtain one from its own exact receipt.
pub(super) struct SendPermit {
    attempt: Attempt,
}

impl Claim {
    pub fn originate(metadata: &Value, intent: &LaunchIntent, actor: String) -> Result<Self> {
        if metadata.get("gate_orphan_claim").is_some() {
            bail!(
                "an original guarded negative decision owns this packet; no native permit may be originated"
            );
        }
        if metadata.get(model::OBSERVATION).is_some() {
            bail!("prior native acceptance is recorded; no resend");
        }
        let previous = read(metadata)?;
        let has_intent = metadata.get(model::INTENT).is_some();
        if has_intent && metadata.get(model::INTENT) != Some(&serde_json::to_value(intent)?) {
            bail!("native intent changed; no send is authorized");
        }
        if has_intent && previous.is_none() {
            bail!("legacy immutable intent has no attempt proof; outcome UNKNOWN");
        }
        if !has_intent && previous.is_some() {
            bail!("native attempt has no immutable intent");
        }
        if let Some(previous) = &previous {
            if previous.state != State::NotStarted {
                bail!("native sending outcome UNKNOWN; a stored claim is not a send permit");
            }
            if previous.intent_sha256 != intent.sha256 || previous.intent_file_id != intent.file_id
            {
                bail!("pre-spawn proof belongs to a different immutable request");
            }
        }
        let generation = reservation_generation(metadata)?;
        let reservation = model::read(metadata)
            .map_err(anyhow::Error::msg)?
            .context("no reservation")?;
        if reservation.consumed {
            bail!("native reservation is already consumed");
        }
        let attempt = Attempt {
            contract_version: 1,
            id: Uuid::new_v4(),
            actor,
            intent_sha256: intent.sha256.clone(),
            intent_file_id: intent.file_id.clone(),
            reservation_generation: generation,
            previous: previous.as_ref().map(|old| old.id),
            state: State::Sending,
            no_child: None,
        };
        let mut expected = std::collections::BTreeMap::new();
        for key in [
            model::INTENT,
            KEY,
            model::KEY,
            "gate_orphan_claim",
            "gate_submission",
        ] {
            expected.insert(
                key.to_owned(),
                boss_jobs::conditional_metadata::Expected {
                    present: metadata.get(key).is_some(),
                    value: metadata.get(key).cloned().unwrap_or(Value::Null),
                },
            );
        }
        for key in [
            "branch",
            "sha",
            "agent_run",
            "mode",
            "hold",
            "runner",
            "worktree",
        ]
        .into_iter()
        .map(str::to_owned)
        .chain(
            ParkIntent::clear_patch()
                .as_object()
                .into_iter()
                .flat_map(|fields| fields.keys().cloned())
                .collect::<Vec<_>>(),
        ) {
            expected.insert(
                key.clone(),
                boss_jobs::conditional_metadata::Expected {
                    present: metadata.get(&key).is_some(),
                    value: metadata.get(&key).cloned().unwrap_or(Value::Null),
                },
            );
        }
        let mut patch = serde_json::Map::from_iter([(KEY.into(), serde_json::to_value(&attempt)?)]);
        if !has_intent {
            patch.insert(model::INTENT.into(), serde_json::to_value(intent)?);
        }
        let command = Command {
            contract_version: 2,
            idempotency_key: Uuid::new_v4(),
            expected,
            patch,
            immutable_keys: if has_intent {
                vec![]
            } else {
                vec![model::INTENT.into()]
            },
            guarded_keys: if previous.is_none() {
                vec![KEY.into()]
            } else {
                vec![]
            },
        };
        command.validate().map_err(anyhow::Error::msg)?;
        Ok(Self {
            command,
            attempt,
            intent: intent.clone(),
        })
    }

    pub fn command(&self) -> &Command {
        &self.command
    }

    pub fn confirm(self, receipt: &Receipt) -> Result<SendPermit> {
        if receipt.contract_version != 2
            || receipt.command != self.command
            || receipt.actor != self.attempt.actor
            || receipt.job_id.to_string() != self.intent.packet
            || receipt.event_id.is_nil()
            || receipt.receipt_event_id.is_none_or(|id| id.is_nil())
            || receipt.job.metadata.get(KEY) != Some(&serde_json::to_value(&self.attempt)?)
            || receipt.job.metadata.get(model::INTENT) != Some(&serde_json::to_value(&self.intent)?)
        {
            bail!("native claim receipt does not confirm this uninterrupted unsent invocation");
        }
        Ok(SendPermit {
            attempt: self.attempt,
        })
    }
}

impl SendPermit {
    pub fn spend(self) -> Attempt {
        self.attempt
    }
}

pub(super) fn no_child_command(
    metadata: &Value,
    attempt: &Attempt,
    proof: Proof,
) -> Result<Command> {
    if read(metadata)?.as_ref() != Some(attempt) || attempt.state != State::Sending {
        bail!("only the exact live sending owner can record its proved spawn refusal");
    }
    let mut next = attempt.clone();
    next.state = State::NotStarted;
    next.no_child = Some(proof);
    let mut expected = std::collections::BTreeMap::new();
    for key in [KEY, model::INTENT, model::KEY] {
        expected.insert(
            key.to_owned(),
            boss_jobs::conditional_metadata::Expected {
                present: metadata.get(key).is_some(),
                value: metadata.get(key).cloned().unwrap_or(Value::Null),
            },
        );
    }
    Ok(Command {
        contract_version: 2,
        idempotency_key: Uuid::new_v4(),
        expected,
        patch: serde_json::Map::from_iter([(KEY.into(), serde_json::to_value(next)?)]),
        immutable_keys: vec![],
        guarded_keys: vec![],
    })
}
