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
        let failed = self.unconfirmed(receipt)?;
        if !failed.is_empty() {
            bail!(
                "native claim receipt does not confirm this uninterrupted unsent invocation — {}",
                failed.join("; ")
            );
        }
        Ok(SendPermit {
            attempt: self.attempt,
        })
    }

    /// Every predicate of [`Self::confirm`] the receipt fails, each named
    /// with both of its values. The predicates are unchanged; only the
    /// refusal now says which (backlog 870dbfe4: nine gate-runs died here
    /// on 2026-10-18 with a refusal naming none of the eight, and the
    /// alias-vs-canonical actor cause was re-derived from the events).
    fn unconfirmed(&self, receipt: &Receipt) -> Result<Vec<String>> {
        let mut failed = Vec::new();
        if receipt.contract_version != 2 {
            failed.push(format!(
                "contract_version: the receipt is v{}, this claim requires v2",
                receipt.contract_version
            ));
        }
        if receipt.command != self.command {
            failed.push(format!(
                "command: the receipt applied command {} and this invocation sent {}{}",
                receipt.command.idempotency_key,
                self.command.idempotency_key,
                if receipt.command.idempotency_key == self.command.idempotency_key {
                    " with a different body"
                } else {
                    ""
                }
            ));
        }
        if receipt.actor != self.attempt.actor {
            failed.push(format!(
                "actor: the server recorded this write as {:?} while the attempt claims {:?}",
                receipt.actor, self.attempt.actor
            ));
        }
        if receipt.job_id.to_string() != self.intent.packet {
            failed.push(format!(
                "packet: the receipt is for {} and this claim is for {}",
                receipt.job_id, self.intent.packet
            ));
        }
        if receipt.event_id.is_nil() {
            failed.push("event_id: the receipt names no state event".into());
        }
        if receipt.receipt_event_id.is_none_or(|id| id.is_nil()) {
            failed.push("receipt_event_id: the receipt names no receipt event".into());
        }
        if receipt.job.metadata.get(KEY) != Some(&serde_json::to_value(&self.attempt)?) {
            failed.push(format!(
                "{KEY}: the committed record {} and this invocation's is {}",
                receipt
                    .job
                    .metadata
                    .get(KEY)
                    .map_or("carries none".into(), |value| format!("is {value}")),
                serde_json::to_value(&self.attempt)?
            ));
        }
        if receipt.job.metadata.get(model::INTENT) != Some(&serde_json::to_value(&self.intent)?) {
            failed.push(format!(
                "{}: the committed record {} and this invocation's is {}",
                model::INTENT,
                receipt
                    .job
                    .metadata
                    .get(model::INTENT)
                    .map_or("carries none".into(), |value| format!("is {value}")),
                serde_json::to_value(&self.intent)?
            ));
        }
        Ok(failed)
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

#[cfg(test)]
mod tests {
    use super::*;
    use boss_core::job::{Job, JobId, Priority, Subject};

    const PACKET: &str = "66194934-4ce9-417e-ad10-ab52604c8dbc";

    fn intent() -> LaunchIntent {
        LaunchIntent {
            contract_version: 1,
            packet: PACKET.into(),
            namespace: "boss-dev".into(),
            name: "gate-661949344ce9417ead10ab52604c8dbc".into(),
            head: "67ebef47fdd8e40313b0f33c78f27a61bf664470".into(),
            mode: "--auto".into(),
            transition_generation: None,
            file_id: "9ad52d0c-0bb5-485b-a59b-fca7101b45a2".into(),
            sha256: "2d5f0f3297d1d7fe678f44f444ae3e6dfca346e757e9726927f024810880f44f".into(),
            size_bytes: 4560,
        }
    }

    /// A queued packet: its reservation, nothing native yet.
    fn queued() -> Value {
        let now = chrono::DateTime::parse_from_rfc3339("2026-10-18T16:13:58Z")
            .unwrap()
            .with_timezone(&chrono::Utc);
        Value::Object(model::renew(&json!({}), now).unwrap().patch)
    }

    /// The receipt the jobs API returns for `claim`'s exact command,
    /// written as `actor` — the post-state carrying the patch it applied.
    fn committed(claim: &Claim, actor: &str) -> Receipt {
        let mut metadata = queued();
        for (key, value) in &claim.command().patch {
            metadata[key] = value.clone();
        }
        let mut job = Job::new(
            "gate-run",
            Subject::new("custom", "test"),
            "test",
            "test",
            Priority::Standard,
            chrono::NaiveDate::from_ymd_opt(2026, 10, 18).unwrap(),
        )
        .with_id(JobId::from_uuid(Uuid::parse_str(PACKET).unwrap()));
        job.metadata = metadata;
        Receipt {
            contract_version: 2,
            job_id: job.id,
            actor: actor.into(),
            command: claim.command().clone(),
            applied_at: chrono::Utc::now(),
            event_id: Uuid::new_v4(),
            receipt_event_id: Some(Uuid::new_v4()),
            job,
        }
    }

    fn refusal(claim: Claim, receipt: &Receipt) -> String {
        match claim.confirm(receipt) {
            Ok(_) => panic!("a receipt that differs from the claim granted a send permit"),
            Err(error) => format!("{error:#}"),
        }
    }

    #[test]
    fn an_exact_receipt_still_grants_the_one_permit() {
        let claim = Claim::originate(&queued(), &intent(), "agent-claude-b".into()).unwrap();
        let receipt = committed(&claim, "agent-claude-b");
        assert!(claim.confirm(&receipt).is_ok());
    }

    /// WHY (backlog 870dbfe4). On 2026-10-18 nine gate-runs died at this
    /// refusal and its text named no predicate of the eight, so the cause
    /// — the attempt signed with the login alias `claude-b@algedonic.dev`
    /// while the server recorded the canonical `agent-claude-b` — was
    /// re-derived by hand from the packet's events, and on 2026-10-17 a
    /// second session could not tell which predicate failed when actors
    /// matched. The refusal now names each failed predicate with both of
    /// its values, and nothing it did not compare.
    #[test]
    fn a_refused_receipt_names_the_actor_it_was_written_as_and_the_actor_claimed() {
        let claim =
            Claim::originate(&queued(), &intent(), "claude-b@algedonic.dev".into()).unwrap();
        let receipt = committed(&claim, "agent-claude-b");
        let why = refusal(claim, &receipt);
        assert!(
            why.starts_with(
                "native claim receipt does not confirm this uninterrupted unsent invocation"
            ),
            "the refusal keeps the sentence every reader already searches for: {why}"
        );
        assert!(
            why.contains("actor")
                && why.contains("\"agent-claude-b\"")
                && why.contains("\"claude-b@algedonic.dev\""),
            "the actor predicate and BOTH of its values are named: {why}"
        );
        for unfailed in [
            "contract_version",
            "command",
            "packet",
            "event",
            KEY,
            model::INTENT,
        ] {
            assert!(
                !why.contains(unfailed),
                "{unfailed} did not fail and is not named: {why}"
            );
        }
    }

    #[test]
    fn each_other_failed_predicate_is_named_too() {
        let cases: [(&str, fn(&mut Receipt)); 6] = [
            ("contract_version", |r| r.contract_version = 1),
            ("command", |r| r.command.idempotency_key = Uuid::new_v4()),
            ("packet", |r| r.job_id = JobId::new()),
            ("event_id", |r| r.event_id = Uuid::nil()),
            ("receipt_event_id", |r| r.receipt_event_id = None),
            (KEY, |r| {
                r.job.metadata.as_object_mut().unwrap().remove(KEY);
            }),
        ];
        for (predicate, break_it) in cases {
            let claim = Claim::originate(&queued(), &intent(), "agent-claude-b".into()).unwrap();
            let mut receipt = committed(&claim, "agent-claude-b");
            break_it(&mut receipt);
            let why = refusal(claim, &receipt);
            assert!(
                why.contains(&format!("{predicate}:")),
                "{predicate} failed and is named: {why}"
            );
        }
        let claim = Claim::originate(&queued(), &intent(), "agent-claude-b".into()).unwrap();
        let mut receipt = committed(&claim, "agent-claude-b");
        receipt.job.metadata[model::INTENT]["head"] = json!("0".repeat(40));
        let why = refusal(claim, &receipt);
        assert!(why.contains(&format!("{}:", model::INTENT)), "{why}");
    }
}
