//! Registered diagnostics advance through durable intent, launch and receipt ports.
//! The conductor adapter owns effects; request data never supplies a command.
use anyhow::{Context, Result, bail};
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use uuid::Uuid;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Request {
    pub car: Uuid,
    pub candidate: String,
    pub baseline: String,
    pub receipt_sha256: String,
}

impl Request {
    fn validate(&self) -> Result<()> {
        for (name, value, size) in [
            ("candidate", &self.candidate, 40),
            ("baseline", &self.baseline, 40),
            ("receipt", &self.receipt_sha256, 64),
        ] {
            if value.len() != size
                || !value
                    .bytes()
                    .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
            {
                bail!("diagnostic {name} must be an exact lowercase hexadecimal identity");
            }
        }
        if self.car.is_nil() {
            bail!("diagnostic car identity is absent");
        }
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub(super) struct Inputs {
    pub request: Request,
    pub admitted_original: Value,
    pub pinned_protocol: Value,
    pub current_car: Value,
    pub car_binding: Value,
    pub policy: Value,
    pub edit_level: Value,
    pub required_checks: Vec<String>,
    pub expected_executor_identity: String,
    pub observed_cluster_identity: String,
}

impl Inputs {
    fn same_authority_binding(&self, other: &Self) -> bool {
        // Publication changes an old row's status, not the protocol
        // the existing packet was admitted under. Keep both full originals.
        let body = |value: &Value| {
            let mut value = value.clone();
            if let Some(object) = value.as_object_mut() {
                object.remove("status");
            }
            value
        };
        self.request == other.request
            && self.admitted_original == other.admitted_original
            && body(&self.pinned_protocol) == body(&other.pinned_protocol)
            && self.car_binding == other.car_binding
            && self.policy == other.policy
            && self.edit_level == other.edit_level
            && self.required_checks == other.required_checks
            && self.expected_executor_identity == other.expected_executor_identity
            && self.observed_cluster_identity == other.observed_cluster_identity
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub(super) struct Intent {
    pub attempt: Uuid,
    pub inputs: Inputs,
}

impl Intent {
    pub fn job_name(&self) -> String {
        format!("consist-{}", self.attempt.simple())
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub(super) struct Launch {
    pub job_uid: String,
    pub assembled: String,
    pub manifest: Value,
    pub original_output: Value,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub(super) struct Observation {
    pub job_uid: String,
    pub pod_uid: String,
    pub original_output: Value,
    pub original_receipt: String,
    pub init_exit: Option<i32>,
    pub worker_exit: Option<i32>,
    pub observer_refusal: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub(super) enum Outcome {
    RequiredChecksPassed,
    CandidateFailed,
    Incomplete,
    ExecutionFailed,
    ObserverUnknown,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub(super) struct Receipt {
    pub intent: Intent,
    pub launch: Launch,
    pub observation: Observation,
    pub outcome: Outcome,
}

// Operation v2 records an observed preparation refusal without pretending
// compute existed. The original executed receipt stays compatible with v1.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct PreCreateRefusal {
    pub schema: u32,
    pub intent: Intent,
    pub invocation: Uuid,
    pub native_launch: NativeLaunchAbsent,
    pub command: Value,
    pub originals: Vec<Value>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub(super) enum NativeLaunchAbsent {
    Absent,
}

impl std::fmt::Display for PreCreateRefusal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "observed preparation refusal before native creation")
    }
}
impl std::error::Error for PreCreateRefusal {}

pub(super) fn allows_pre_create_refusal(intent: &Intent) -> bool {
    intent.inputs.pinned_protocol["steps"]
        .as_array()
        .is_some_and(|steps| {
            steps.iter().any(|s| {
                s["title"] == "execute"
                    && s["metadata_defaults"]["diagnostic_operation"]["version"] == 2
            })
        })
}

pub(super) fn validate_pre_create_refusal(value: &PreCreateRefusal) -> Result<()> {
    use base64::Engine;
    if value.schema != 1 || value.invocation.is_nil() || !allows_pre_create_refusal(&value.intent) {
        bail!("pre-create refusal has no versioned invocation contract");
    }
    let start = serde_json::json!({"phase":"preparing","invocation":value.invocation,"attempt":value.intent.attempt});
    if value
        .originals
        .iter()
        .filter(|r| r["observation"] == start)
        .count()
        != 1
        || value
            .originals
            .iter()
            .filter(|r| r["observation"]["phase"] == "preparing")
            .count()
            != 1
        || value
            .originals
            .iter()
            .any(|r| r["observation"]["phase"] == "entering-create")
        || value.originals.iter().any(|r| {
            let observation = &r["observation"];
            // ConsistControl journals args before spawning kubectl. A missing
            // acknowledgement cannot erase that write-before-create evidence.
            ["argv", "args"].iter().any(|field| {
                observation[field]
                    .as_array()
                    .is_some_and(|argv| argv.first().is_some_and(|value| value == "create"))
            })
        })
        || value.originals.last().map(|r| &r["observation"]) != Some(&value.command)
        || value.command["argv"] != serde_json::json!(["fetch", "origin"])
        || value.command["version"] != 1
        || !value.command["input_b64"].is_null()
        || value.command["exit"].as_i64().is_none_or(|exit| exit == 0)
    {
        bail!("pre-create refusal has no complete observed first-fetch failure");
    }
    for field in ["stdout_b64", "stderr_b64"] {
        base64::engine::general_purpose::STANDARD.decode(
            value.command[field]
                .as_str()
                .context("pre-create refusal command stream absent")?,
        )?;
    }
    for (sequence, row) in value.originals.iter().enumerate() {
        if row["sequence"].as_u64() != Some(sequence as u64) {
            bail!("pre-create refusal original sequence differs");
        }
    }
    Ok(())
}

pub(super) enum FirstIntent {
    Recorded(Intent),
    Replayed(Intent),
}

pub(super) enum Advance {
    Assessed,
    Pending,
    Unknown(String),
    Complete(Box<Receipt>),
    Refused(Box<PreCreateRefusal>),
}

/// A native adapter must retain original command observations BEFORE parsing,
/// and an ambiguous create must return an error without retrying the effect.
#[async_trait]
pub(super) trait Door: Send + Sync {
    async fn inputs(&self) -> Result<Inputs>;
    async fn claim(&self) -> Result<()>;
    async fn first_intent(&self, proposed: &Intent) -> Result<FirstIntent>;
    async fn launch_record(&self) -> Result<Option<Launch>>;
    async fn receipt_record(&self) -> Result<Option<Receipt>>;
    async fn launch(&self, intent: &Intent) -> Result<Launch>;
    async fn keep_launch(&self, launch: &Launch) -> Result<()>;
    async fn observe(&self, intent: &Intent, launch: &Launch) -> Result<Option<Observation>>;
    async fn keep_receipt(&self, receipt: &Receipt) -> Result<()>;
    async fn finish(&self, receipt: &Receipt) -> Result<()>;
    async fn pre_create_refusal_record(&self) -> Result<Option<PreCreateRefusal>> {
        Ok(None)
    }
    async fn keep_pre_create_refusal(&self, _refusal: &PreCreateRefusal) -> Result<()> {
        bail!("pre-create refusal retention is unavailable")
    }
    async fn finish_pre_create_refusal(&self, _refusal: &PreCreateRefusal) -> Result<()> {
        bail!("pre-create refusal completion is unavailable")
    }
}

pub(super) async fn advance(door: &dyn Door, nonce: Uuid) -> Result<Advance> {
    let inputs = door.inputs().await?;
    inputs.request.validate()?;
    if inputs.expected_executor_identity.is_empty()
        || inputs.observed_cluster_identity != inputs.expected_executor_identity
    {
        bail!("diagnostic executor is not the observed conductor ServiceAccount");
    }
    if inputs.required_checks.is_empty()
        || inputs.edit_level.is_null()
        || inputs.policy.is_null()
        || inputs.car_binding.is_null()
    {
        bail!("diagnostic operation, policy or edit-level evidence is absent");
    }
    door.claim().await?;
    let proposed = Intent {
        attempt: nonce,
        inputs,
    };
    let (intent, fresh) = match door.first_intent(&proposed).await? {
        FirstIntent::Recorded(intent) if intent == proposed => (intent, true),
        FirstIntent::Recorded(_) => {
            bail!("recorded diagnostic intent differs from the proposed original")
        }
        FirstIntent::Replayed(intent) => (intent, false),
    };
    if intent.attempt.is_nil() || !intent.inputs.same_authority_binding(&proposed.inputs) {
        bail!("retained diagnostic intent differs from current authoritative inputs");
    }
    if let Some(receipt) = door.receipt_record().await? {
        if receipt.intent != intent {
            bail!("retained diagnostic receipt belongs to another intent");
        }
        validate_receipt(&receipt)?;
        door.finish(&receipt).await?;
        return Ok(Advance::Complete(Box::new(receipt)));
    }
    if let Some(refusal) = door.pre_create_refusal_record().await? {
        if refusal.intent != intent {
            bail!("retained pre-create refusal belongs to another intent");
        }
        validate_pre_create_refusal(&refusal)?;
        if door.launch_record().await?.is_some() {
            bail!("pre-create refusal conflicts with acknowledged launch");
        }
        door.finish_pre_create_refusal(&refusal).await?;
        return Ok(Advance::Refused(Box::new(refusal)));
    }
    let launch =
        match door.launch_record().await? {
            Some(launch) => launch,
            None if fresh => {
                let launch = match door.launch(&intent).await {
                    Ok(launch) => launch,
                    Err(error) => {
                        let Some(refusal) = error.downcast_ref::<PreCreateRefusal>() else {
                            return Err(error);
                        };
                        if refusal.intent != intent {
                            bail!("pre-create refusal differs from admitted intent");
                        }
                        validate_pre_create_refusal(refusal)?;
                        door.keep_pre_create_refusal(refusal).await?;
                        if door.pre_create_refusal_record().await?.as_ref() != Some(refusal) {
                            bail!("pre-create refusal original readback differs; no completion");
                        }
                        door.finish_pre_create_refusal(refusal).await?;
                        return Ok(Advance::Refused(Box::new(refusal.clone())));
                    }
                };
                door.keep_launch(&launch).await?;
                launch
            }
            None => return Ok(Advance::Unknown(
                "intent exists without an acknowledged immutable launch; never recreate compute"
                    .into(),
            )),
        };
    if launch.job_uid.is_empty() || launch.assembled.len() != 40 {
        bail!("diagnostic launch acknowledgement has no native identity or assembled head");
    }
    let Some(observation) = door.observe(&intent, &launch).await? else {
        return Ok(Advance::Pending);
    };
    let outcome = judge(&intent, &launch, &observation)?;
    let receipt = Receipt {
        intent,
        launch,
        observation,
        outcome,
    };
    // If retention fails, completion is forbidden. Replay uses these originals.
    door.keep_receipt(&receipt).await?;
    door.finish(&receipt).await?;
    Ok(Advance::Complete(Box::new(receipt)))
}

fn validate_receipt(receipt: &Receipt) -> Result<()> {
    if judge(&receipt.intent, &receipt.launch, &receipt.observation)? != receipt.outcome {
        bail!("retained outcome disagrees with its original full receipt");
    }
    Ok(())
}

fn judge(intent: &Intent, launch: &Launch, observation: &Observation) -> Result<Outcome> {
    if observation.job_uid != launch.job_uid {
        bail!("diagnostic receipt native incarnation differs from its acknowledged launch");
    }
    if observation.init_exit.is_some_and(|exit| exit != 0)
        || observation.worker_exit.is_some_and(|exit| exit != 0)
    {
        return Ok(Outcome::ExecutionFailed);
    }
    if observation.observer_refusal.is_some() {
        return Ok(Outcome::ObserverUnknown);
    }
    if observation.init_exit != Some(0)
        || observation.worker_exit != Some(0)
        || observation.pod_uid.is_empty()
    {
        bail!("diagnostic receipt has no completed native init/worker and pod identity");
    }
    let raw: Value = serde_json::from_str(&observation.original_receipt)
        .context("diagnostic worker output is not one complete original receipt")?;
    if raw["version"] != 1
        || raw["identity"] != intent.job_name()
        || raw["head"] != launch.assembled
        || raw["baseline"] != intent.inputs.request.baseline
        || raw["exclusions"]["exit"] != 0
    {
        bail!("diagnostic full receipt does not bind the request's identity, tree and baseline");
    }
    let rows = raw["runs"]
        .as_array()
        .context("diagnostic receipt has no complete check roster")?;
    let mut names = std::collections::BTreeSet::new();
    let mut failed = false;
    for row in rows {
        let name = row["name"]
            .as_str()
            .context("diagnostic check identity absent")?;
        if name.is_empty()
            || !names.insert(name)
            || !row["stdout"].is_string()
            || !row["stderr"].is_string()
        {
            bail!("diagnostic check identity/output missing or duplicated");
        }
        match row["exit"].as_i64() {
            Some(0 | 126 | 127) => {}
            Some(_) => failed = true,
            None if row["unavailable"].is_string() => {}
            None => bail!("diagnostic check has neither an exit nor unavailable evidence"),
        }
    }
    if failed {
        return Ok(Outcome::CandidateFailed);
    }
    let required_pass = intent.inputs.required_checks.iter().all(|required| {
        rows.iter()
            .any(|row| row["name"] == *required && row["exit"] == 0)
    });
    Ok(if required_pass {
        Outcome::RequiredChecksPassed
    } else {
        Outcome::Incomplete
    })
}
