//! `credential.rotate.ops-runner` — the credential broker mints the ops
//! runner's credential (design f623e425 Q1, option A, decided by David
//! 2026-09-25; backlog 1e50e66b, the broker car after the door car of
//! backlog 6c9183de).
//!
//! WHAT IT ROTATES. The jobs API knows the ops runner on a host by a
//! credential the runner PRESENTS in `x-boss-runner-credential`, resolved
//! by the door (`boss_jobs::runner_credential`) against a directory of
//! slot files, `<host>.current|next|previous` — the mount of ONE Secret,
//! `boss/ops-runner-credential`, which only this handler writes. The value
//! has no issuer outside the estate: it is 32 random bytes, base64url (the
//! machine token's shape), minted here. So the issuer is this process, the
//! "revoke" is the old value leaving the Secret, and every judgement is
//! made by EFFECT through the door itself — `GET /api/jobs/runner-credential`
//! presenting a value answers which host and which slot the jobs API
//! resolves it to, never the value.
//!
//! ## Two firings, one declaration
//!
//! The rule row declares one host's credential: the Secret, the registry
//! id and the host whose slots it owns. Two rules hand the handler the
//! same args (pinned equal, CLAUDE.md §9a):
//!
//! - **Stage, on a `rotate-a-credential` packet's scope step.** Mint a
//!   value, write it into `<host>.next` with its provenance
//!   (`<host>.next.minted-for` = the packet) in one merge-patch, complete
//!   `issue` and `install` (the install says `delivery = "off-host"`, which
//!   readies the packet's `delivered` step), then verify by effect: the
//!   door resolves the staged value to this host. The host may hold
//!   `current` or — once its deposit has installed it, which it does as
//!   soon as the door resolves it, before any promotion — `next`. So the
//!   stage leaves `current` alone, and a value already in `next` (a
//!   rotation abandoned, or not yet promoted, when this one was scoped) is
//!   carried to `previous` in the same write rather than dropped: whatever
//!   the host presents keeps resolving (review 3930a3eb, F1). Only the
//!   newest displaced value is kept; a host that missed two stages holds
//!   one no slot names until its next installing pass. A held converge
//!   makes that window unbounded. The installed event names the dropped
//!   previous value by its last eight, so the retirement remains visible.
//!   Kubelet refreshes a Secret mount in about 60-90 s, so a
//!   verify that does not see the value yet answers "not yet" and the
//!   redelivery schedule carries the wait; a redelivery finds its own
//!   value staged and mints nothing.
//!
//! - **Promote, on the packet's `delivered` step** (`credential-delivery`),
//!   which the host's deposit (infra/forge/runner-credential-deposit.sh)
//!   completes only after the value it installed resolved to its host
//!   through the same door. The handler checks the host's last eight
//!   against the value staged for THIS packet, then writes `current` =
//!   that value and blanks `next` and `previous` in one merge-patch — the
//!   old value leaves every slot at once, and the host already holds the
//!   new one, so no reader is between values. It then confirms by effect:
//!   the new value resolves at slot `current`, which means the jobs API
//!   reads the promoted directory (kubelet swaps a Secret volume
//!   atomically), in which no slot holds the old value — and while this
//!   firing still holds the old value it presents it too and requires
//!   that nothing resolves it. Until both are true it answers "not yet".
//!
//! `previous` holds only a value a later stage displaced from `next`
//! (above). The promotion BLANKS it and confirms that value dead too: the
//! host has moved to the promoted value before the promotion (the
//! delivery record is that fact), so nothing is left for it to keep
//! valid — the review of 8e5de104's finding 4(b), "clear previous once
//! the host has moved", held structurally rather than by a second step.
//! Promotion requires the canonical resolved runner completer AND the
//! credential owner's original authenticated-delivery receipt, bound to
//! the actual packet, attempt, Secret UID and original installed command.
//! A copied actor label or printed suffix cannot substitute for that receipt.
//!
//! THE STAGE'S OWN IDENTITY (design 6e28ed42, David 2026-10-06). When the
//! deployment names a projected ServiceAccount token ([`STAGE_TOKEN_FILE_ENV`])
//! every phase command presents it, and the jobs API records the phase as
//! the workload it verified (`boss_jobs::credentials::broker_stage`) — not
//! as the rule label this handler types. No deployment names one yet: the
//! decision authorized no activation, so today the stage still goes out
//! under operator attribution, and this source claims no live proof.
//!
//! ## What it deliberately does not do
//!
//! Refuse anything on the request path (DR rule 62dac114): the door
//! resolves or passes, and no protocol declares the runner as a writer
//! until the declaration car. Deliver to a host: the forge's converge
//! reads the Secret and writes its own root-only file. Touch another
//! host's slots: a declaration names one host, and every key this writes
//! is `<that host>.…`.
//!
//! THE VALUE NEVER ENTERS A PACKET, AN EVENT OR AN ERROR: the host, the
//! slot, the Secret path, the value's length and last eight (the
//! identifier every brokered credential is named by), and observed
//! effects only.

use async_trait::async_trait;
use base64::Engine as _;
use boss_dispatcher::rules::expr::Value;
use boss_dispatcher::rules::handler::{Handler, HandlerError, InvocationContext, arg_string};
use boss_jobs::credentials::RotationPhase;
use boss_jobs::credentials::runner_delivery::InstalledCommand;
use boss_jobs::runner_credential::{HEADER, WHOAMI_PATH, slot_of};
use serde::{Deserialize, Serialize};
use serde_json::{Value as JsonValue, json};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use super::common::{StepEvent, dispatcher_reader_header};
use super::credential_issuer::{SecretData, SecretStore, WriteAt};
use super::credential_rotate_forgejo::last_eight;

/// The handler's registered name — the `handler = "…"` of both rules.
pub const HANDLER: &str = "credential.rotate.ops-runner";

/// The install step's `delivery`: the protocol's `delivered` step goes
/// ready on it, and the promotion waits for that step.
pub const DELIVERY_OFF_HOST: &str = "off-host";

/// Registered deposit client attribution. The resolver replaces the caller
/// identity with its canonical runner actor; promotion additionally requires
/// the original owner receipt, never this asserted client label.
pub const DEPOSIT_ACTOR: &str = "automation:runner-credential-deposit";

/// Names the file a separately projected ServiceAccount token sits in —
/// one audience, ten minutes (design 6e28ed42, David 2026-10-06). Set
/// NOWHERE today: no manifest projects that volume, because the decision
/// authorized no activation. Unset, the stage commands go out as before.
pub const STAGE_TOKEN_FILE_ENV: &str = "BOSS_BROKER_STAGE_TOKEN_FILE";

/// The step kinds of the two firings.
const SCOPE_KIND: &str = "credential-rotation";
const DELIVERY_KIND: &str = "credential-delivery";
const ISSUER: &str = "credential-broker (32 random bytes, base64url)";

/// A slot's Secret key: `<host>.<slot>` — the file the door reads.
pub fn slot_key(host: &str, slot: &str) -> String {
    format!("{host}.{slot}")
}

/// The provenance beside a slot: the rotation packet that put its value
/// there. The door names no slot by it (`slot_of` refuses a third dotted
/// part), so it rides in the same Secret without being a credential.
pub fn minted_for_key(host: &str, slot: &str) -> String {
    format!("{host}.{slot}.minted-for")
}

fn witness_key(host: &str, job_id: &str) -> String {
    format!("{host}.recovery.{job_id}.witness")
}
fn recovery_key(host: &str, job_id: &str, slot: &str) -> String {
    format!("{host}.recovery.{job_id}.{slot}")
}

/// Commands are intentions, not proof that their effects or owner commits
/// happened. The witness contains identifiers only; candidate/old values stay
/// in separate Secret keys excluded from the credential resolver.
#[derive(Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct RecoveryWitness {
    version: u32,
    job_id: String,
    credential_id: String,
    namespace: String,
    name: String,
    host: String,
    uid: String,
    attempt: String,
    last_eight: String,
    promoted: bool,
    commands: std::collections::BTreeMap<String, JsonValue>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct MintedCommand {
    issuer: String,
    value_length: usize,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct VerifiedCommand {
    method: String,
    observed: String,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RevokedCommand {
    delivered_last_eight: String,
    old_last_eight: JsonValue,
    carried_last_eight: JsonValue,
    confirmed_dead: String,
}

fn specific_command<T: serde::de::DeserializeOwned>(command: &JsonValue) -> Option<T> {
    let mut command = command.clone();
    let fields = command.as_object_mut()?;
    for field in ["job_id", "host", "observation_id"] {
        fields.remove(field);
    }
    serde_json::from_value(command).ok()
}

fn nullable_identifier(value: &JsonValue) -> bool {
    value.is_null()
        || value.as_str().is_some_and(|value| {
            value.len() == 8
                && value
                    .bytes()
                    .all(|c| c.is_ascii_alphanumeric() || b"-_".contains(&c))
        })
}

impl Held {
    fn from_secret(secret: &SecretData, host: &str) -> Self {
        let get = |key: String| {
            secret
                .data
                .get(&key)
                .cloned()
                .filter(|v| !v.trim().is_empty())
        };
        Self {
            current: get(slot_key(host, "current")),
            current_for: get(minted_for_key(host, "current")),
            next: get(slot_key(host, "next")),
            next_for: get(minted_for_key(host, "next")),
            previous: get(slot_key(host, "previous")),
        }
    }
}

/// 32 random bytes, base64url without padding: 43 characters, no
/// whitespace, sendable as a header value — the machine token's shape.
pub fn fresh_value() -> String {
    use rand::RngExt;
    let mut bytes = [0u8; 32];
    rand::rng().fill(&mut bytes[..]);
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes)
}

// ---------------------------------------------------------------------------
// Pure planning — the decisions under test
// ---------------------------------------------------------------------------

/// One host's slots as the Secret holds them. A blank key is no value —
/// the door reads a blank slot as no credential, and the promotion blanks
/// what it clears.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Held {
    pub current: Option<String>,
    pub current_for: Option<String>,
    pub next: Option<String>,
    pub next_for: Option<String>,
    pub previous: Option<String>,
}

impl Held {
    /// The staged value, when it is THIS packet's.
    pub fn staged_for(&self, job_id: &str) -> Option<&str> {
        (self.next_for.as_deref() == Some(job_id))
            .then_some(self.next.as_deref())
            .flatten()
    }

    /// The current value, when this packet promoted it.
    pub fn promoted_by(&self, job_id: &str) -> Option<&str> {
        (self.current_for.as_deref() == Some(job_id))
            .then_some(self.current.as_deref())
            .flatten()
    }
}

/// The scope step's naming of the old value, judged against the one value
/// a rotation replaces: the host's `current`. A runner credential has no
/// name or id anywhere, so `old_token` names nothing; a last eight must be
/// `current`'s. Either mismatch is refused BEFORE anything is minted — a
/// scoper who expects a value dead must never read a packet that closed
/// `rotated` over another one.
pub fn judge_scope_naming(
    old_token: Option<&str>,
    old_last8: Option<&str>,
    current: Option<&str>,
    host: &str,
) -> Result<(), String> {
    if let Some(name) = old_token {
        return Err(format!(
            "the scope step names old_token {name:?}, but a runner credential has no name or \
             id — the rotation replaces {host}'s current value. Name it by \
             old_token_last_eight, or name none"
        ));
    }
    match (old_last8, current) {
        (None, _) => Ok(()),
        (Some(l8), None) => Err(format!(
            "the scope step names a value ending in {l8}, but {host} holds no current value; \
             nothing is minted"
        )),
        (Some(l8), Some(v)) if last_eight(v) == l8 => Ok(()),
        (Some(l8), Some(v)) => Err(format!(
            "the scope step names a value ending in {l8}, but {host}'s current value ends in \
             {}; nothing is minted",
            last_eight(v)
        )),
    }
}

/// What a delivery firing does with the Secret as it stands.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Promotion {
    /// This packet's value is already `current`: a prior firing promoted
    /// it. Confirm and record only.
    Done { value: String },
    /// This packet's value is staged in `next` and is what the host
    /// delivered: promote it.
    Promote { value: String },
}

/// Judge the host's delivery record against the Secret. Every refusal is
/// permanent — the Secret and the record say the same on a redelivery —
/// and each leaves the old value valid.
pub fn plan_promotion(
    held: &Held,
    job_id: &str,
    delivered_l8: &str,
    host: &str,
) -> Result<Promotion, String> {
    if let Some(v) = held.promoted_by(job_id) {
        return Ok(Promotion::Done {
            value: v.to_string(),
        });
    }
    let Some(v) = held.staged_for(job_id) else {
        return Err(format!(
            "{host}.next does not hold a value staged for this packet ({}): a later rotation \
             replaced it, or none was installed. Nothing is promoted and {host}'s current \
             value stays valid",
            match held.next_for.as_deref() {
                Some(other) => format!("it names {other}"),
                None => "it names no packet".to_string(),
            }
        ));
    };
    if last_eight(v) != delivered_l8 {
        return Err(format!(
            "the host recorded delivery of a value ending in {delivered_l8}, but {host}.next \
             holds this packet's value ending in {}; nothing is promoted and {host}'s current \
             value stays valid",
            last_eight(v)
        ));
    }
    Ok(Promotion::Promote {
        value: v.to_string(),
    })
}

// ---------------------------------------------------------------------------
// The declaration — rule-row data
// ---------------------------------------------------------------------------

struct Declaration<'a> {
    secret_namespace: &'a str,
    secret_name: &'a str,
    credential_id: &'a str,
    host: &'a str,
}

impl<'a> Declaration<'a> {
    /// Every refusal here is PERMANENT: the rule row says the same thing
    /// on every redelivery.
    fn parse(args: &'a [(String, Value)]) -> Result<Self, HandlerError> {
        let host = arg_string(args, "host")?;
        // The host becomes the prefix of every key written; the door's own
        // grammar judges it, so a host the door would never read a slot
        // of cannot be minted for.
        let probe = slot_key(host, "current");
        if slot_of(&probe) != Some((host, "current")) {
            return Err(HandlerError::Permanent(format!(
                "host = {host:?} is not an estate host id (lowercase, digits, inner hyphens): the \
                 door would read no slot of it, so nothing is minted"
            )));
        }
        Ok(Self {
            secret_namespace: arg_string(args, "secret_namespace")?,
            secret_name: arg_string(args, "secret_name")?,
            credential_id: arg_string(args, "credential_id")?,
            host,
        })
    }

    fn key(&self, slot: &str) -> String {
        slot_key(self.host, slot)
    }

    fn secret(&self) -> String {
        format!("k8s Secret {}/{}", self.secret_namespace, self.secret_name)
    }
}

// ---------------------------------------------------------------------------
// The handler
// ---------------------------------------------------------------------------

/// How long one invocation waits for the door to see a Secret write
/// before answering "not yet". Bounded well inside the 30 s ack window;
/// the redelivery schedule (~98 s over eight deliveries) carries the rest
/// of kubelet's 60-90 s.
#[derive(Debug, Clone, Copy)]
pub struct ResolvePoll {
    pub attempts: u32,
    pub interval: Duration,
}

impl Default for ResolvePoll {
    fn default() -> Self {
        Self {
            attempts: 4,
            interval: Duration::from_secs(5),
        }
    }
}

/// What the door made of one presented value.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Seen {
    Resolved { host: String, slot: String },
    Unresolved,
}

impl Seen {
    fn describe(&self) -> String {
        match self {
            Seen::Resolved { host, slot } => format!("resolved to host {host}, slot {slot}"),
            Seen::Unresolved => "resolved: false".to_string(),
        }
    }
}

pub struct CredentialRotateOpsRunner {
    client: boss_core::machine_token::Client,
    jobs_base: String,
    secrets: Arc<dyn SecretStore>,
    poll: ResolvePoll,
    /// The projected broker-stage token's file (design 6e28ed42), when
    /// the deployment names one. `None` — every deployment today — sends
    /// the phases exactly as before.
    stage_token_file: Option<PathBuf>,
}

struct StepView {
    id: String,
    status: String,
    /// Who completed it, as the jobs API stamps it (`completed_by`).
    completed_by: Option<String>,
}

impl CredentialRotateOpsRunner {
    pub fn new(jobs_base: impl Into<String>, secrets: Arc<dyn SecretStore>) -> Arc<Self> {
        Self::with_poll(jobs_base, secrets, ResolvePoll::default())
    }

    pub fn with_poll(
        jobs_base: impl Into<String>,
        secrets: Arc<dyn SecretStore>,
        poll: ResolvePoll,
    ) -> Arc<Self> {
        let stage_token_file = std::env::var_os(STAGE_TOKEN_FILE_ENV)
            .filter(|path| !path.is_empty())
            .map(PathBuf::from);
        Self::with_stage_token(jobs_base, secrets, poll, stage_token_file)
    }

    pub fn with_stage_token(
        jobs_base: impl Into<String>,
        secrets: Arc<dyn SecretStore>,
        poll: ResolvePoll,
        stage_token_file: Option<PathBuf>,
    ) -> Arc<Self> {
        Arc::new(Self {
            client: super::common::api_client(),
            jobs_base: jobs_base.into(),
            secrets,
            poll,
            stage_token_file,
        })
    }

    fn jobs(&self) -> &str {
        self.jobs_base.trim_end_matches('/')
    }

    async fn observed_secret(&self, d: &Declaration<'_>) -> Result<SecretData, HandlerError> {
        let secret = self
            .secrets
            .read_secret(d.secret_namespace, d.secret_name)
            .await
            .map_err(HandlerError::Downstream)?
            .ok_or_else(|| HandlerError::Downstream("declared Secret is unavailable".into()))?;
        if secret.uid.is_empty() || secret.version.is_empty() {
            return Err(HandlerError::Downstream(
                "Secret has no usable UID/version precondition".into(),
            ));
        }
        Ok(secret)
    }

    fn witness(
        &self,
        d: &Declaration<'_>,
        secret: &SecretData,
        job_id: &str,
    ) -> Result<RecoveryWitness, HandlerError> {
        let raw = secret
            .data
            .get(&witness_key(d.host, job_id))
            .ok_or_else(|| {
                HandlerError::Downstream("staged effect has no original recovery witness".into())
            })?;
        let witness: RecoveryWitness = serde_json::from_str(raw)
            .map_err(|_| HandlerError::Downstream("recovery witness is malformed".into()))?;
        if witness.version != 1
            || witness.job_id != job_id
            || witness.credential_id != d.credential_id
            || witness.namespace != d.secret_namespace
            || witness.name != d.secret_name
            || witness.host != d.host
            || witness.uid != secret.uid
            || uuid::Uuid::parse_str(&witness.attempt).is_err()
            || witness.last_eight.len() != 8
            || witness.commands.len() != RotationPhase::ALL.len()
            || RotationPhase::ALL.iter().any(|phase| {
                witness.commands.get(phase.as_str()).is_none_or(|command| {
                    command["job_id"] != job_id
                        || command["host"] != d.host
                        || command["observation_id"]
                            != format!("{}:{}", witness.attempt, phase.as_str())
                })
            })
        {
            return Err(HandlerError::Downstream(
                "recovery witness is foreign or incomplete".into(),
            ));
        }
        let held = Held::from_secret(secret, d.host);
        let candidate = if witness.promoted {
            held.promoted_by(job_id)
        } else {
            held.staged_for(job_id)
        }
        .ok_or_else(|| {
            HandlerError::Downstream("witness has no exact packet-bound candidate".into())
        })?;
        let invalid = || {
            HandlerError::Downstream(
                "original phase command is malformed or disagrees with the effect witness".into(),
            )
        };
        let minted: MintedCommand =
            specific_command(&witness.commands["minted"]).ok_or_else(invalid)?;
        let installed: InstalledCommand =
            specific_command(&witness.commands["installed"]).ok_or_else(invalid)?;
        let verified: VerifiedCommand =
            specific_command(&witness.commands["verified"]).ok_or_else(invalid)?;
        let revoked: RevokedCommand =
            specific_command(&witness.commands["revoked"]).ok_or_else(invalid)?;
        let prior = |slot: &str| {
            secret
                .data
                .get(&recovery_key(d.host, job_id, slot))
                .filter(|value| !value.is_empty())
                .map(|value| last_eight(value))
        };
        if last_eight(candidate) != witness.last_eight
            || minted.issuer != ISSUER
            || minted.value_length != candidate.len()
            || installed.secret_namespace != d.secret_namespace
            || installed.secret_name != d.secret_name
            || installed.secret_key != d.key("next")
            || installed.secret_uid != secret.uid
            || installed.precondition_version.is_empty()
            || installed.value_length != candidate.len()
            || installed.last_eight != witness.last_eight
            || installed.delivery != DELIVERY_OFF_HOST
            || !(installed.replaced_staged_for.is_null()
                || installed
                    .replaced_staged_for
                    .as_str()
                    .is_some_and(|id| uuid::Uuid::parse_str(id).is_ok()))
            || !nullable_identifier(&installed.carried_to_previous)
            || !nullable_identifier(&installed.dropped_previous)
            || (!installed.carried_to_previous.is_null()
                && installed.carried_to_previous != json!(prior("carried")))
            || verified.method != "api"
            || verified.observed != format!("resolved to host {}", d.host)
            || revoked.delivered_last_eight != witness.last_eight
            || revoked.old_last_eight != json!(prior("old"))
            || revoked.carried_last_eight != json!(prior("carried"))
            || revoked.confirmed_dead
                != format!(
                    "GET {WHOAMI_PATH} confirms host {} current; original old and carried values resolve to nothing",
                    d.host
                )
        {
            return Err(invalid());
        }
        Ok(witness)
    }

    async fn write_observed(
        &self,
        d: &Declaration<'_>,
        observed: &SecretData,
        entries: &[(String, String)],
    ) -> Result<(), HandlerError> {
        let entries: Vec<_> = entries
            .iter()
            .map(|(key, value)| (key.as_str(), value.as_str()))
            .collect();
        match self
            .secrets
            .write_keys_if(d.secret_namespace, d.secret_name, &entries, observed)
            .await
            .map_err(HandlerError::Downstream)?
        {
            WriteAt::Written => Ok(()),
            WriteAt::Moved => Err(HandlerError::Downstream(
                "Secret UID/version moved; nothing written".into(),
            )),
        }
    }

    /// Present `value` to the door and read what it resolved to. The value
    /// rides in the header the door takes off the request before anything
    /// downstream sees it; the answer names a host and a slot, never it.
    async fn whoami(&self, value: &str) -> Result<Seen, HandlerError> {
        let url = format!("{}{WHOAMI_PATH}", self.jobs());
        let resp = self
            .client
            .get(&url)
            .header("x-boss-user", dispatcher_reader_header())
            .header("x-sim-origin", super::common::sim_origin_value())
            .header(HEADER, value)
            .send()
            .await
            .map_err(|e| HandlerError::Downstream(format!("GET {url}: {e}")))?;
        if !resp.status().is_success() {
            let status = resp.status();
            return Err(HandlerError::Downstream(format!(
                "GET {url} returned {status} — a jobs API without the credential door answers \
                 no resolution"
            )));
        }
        let body: JsonValue = resp
            .json()
            .await
            .map_err(|e| HandlerError::Downstream(format!("{url}: {e}")))?;
        match body.get("resolved").and_then(JsonValue::as_bool) {
            Some(true) => Ok(Seen::Resolved {
                host: body
                    .get("host")
                    .and_then(JsonValue::as_str)
                    .unwrap_or_default()
                    .to_string(),
                slot: body
                    .get("slot")
                    .and_then(JsonValue::as_str)
                    .unwrap_or_default()
                    .to_string(),
            }),
            Some(false) => Ok(Seen::Unresolved),
            None => Err(HandlerError::Downstream(format!(
                "GET {url} answered without `resolved`: not the credential door's whoami"
            ))),
        }
    }

    /// Ask the door until `done` holds of what it answers, within this
    /// invocation's bound; the last answer either way.
    async fn watch(
        &self,
        value: &str,
        done: impl Fn(&Seen) -> bool,
    ) -> Result<(bool, Seen), HandlerError> {
        let mut seen = Seen::Unresolved;
        for attempt in 0..self.poll.attempts.max(1) {
            if attempt > 0 && !self.poll.interval.is_zero() {
                tokio::time::sleep(self.poll.interval).await;
            }
            seen = self.whoami(value).await?;
            if done(&seen) {
                return Ok((true, seen));
            }
        }
        Ok((false, seen))
    }

    /// The projected broker-stage token, read afresh for every command —
    /// kubelet rotates the file well inside its ten minutes, and a value
    /// held in memory would be the stale one. `Ok(None)` when no file is
    /// named. A named file that cannot be presented is an ERROR, never a
    /// fall back to the typed label: the deployment said this workload
    /// proves itself. The error names the path and the fault, not a byte
    /// of the content.
    fn stage_token(&self) -> Result<Option<reqwest::header::HeaderValue>, HandlerError> {
        const MAX_BYTES: u64 = 8 * 1024;
        let Some(path) = &self.stage_token_file else {
            return Ok(None);
        };
        // The estate's own hosts only, by the rule the machine token is
        // stamped under (`machine_token::Hosts`): the client beside this
        // header withholds the estate token from any other host, and a
        // workload token must not travel further than that one does.
        if !boss_core::machine_token::hosts().allows_url(self.jobs()) {
            return Err(HandlerError::Permanent(
                "the jobs API this handler was given is not one of the estate's own hosts; \
                 the broker-stage token is not sent there and no runner credential stage is sent without it"
                    .into(),
            ));
        }
        let unavailable = |why: &str| {
            HandlerError::Downstream(format!(
                "the broker-stage token at {} {why}; no runner credential stage is sent without it",
                path.display()
            ))
        };
        let meta =
            std::fs::metadata(path).map_err(|e| unavailable(&format!("is unreadable ({e})")))?;
        if !meta.is_file() || meta.len() > MAX_BYTES {
            return Err(unavailable("is not a regular token file within bounds"));
        }
        let text = std::fs::read_to_string(path)
            .map_err(|e| unavailable(&format!("is unreadable ({e})")))?;
        let token = text.trim();
        if token.is_empty() {
            return Err(unavailable("is empty"));
        }
        let mut header = reqwest::header::HeaderValue::from_str(token)
            .map_err(|_| unavailable("is not one header-safe line"))?;
        header.set_sensitive(true);
        Ok(Some(header))
    }

    async fn record_phase(
        &self,
        rule_name: &str,
        credential_id: &str,
        phase: RotationPhase,
        evidence: JsonValue,
    ) -> Result<(), HandlerError> {
        let url = format!(
            "{}/api/credentials/{credential_id}/rotation/{}",
            self.jobs(),
            phase.as_str()
        );
        use boss_jobs::credentials::receipt::RotationOutcome;
        let actor = super::common::dispatcher_actor_header(rule_name);
        let user: boss_policy_client::User = serde_json::from_str(&actor)
            .map_err(|_| HandlerError::Permanent("dispatcher actor header is malformed".into()))?;
        // With a token the owner records the stage as the WORKLOAD the
        // token proves, whatever label rides beside it (design 6e28ed42);
        // the receipt must name that actor, or it is not this command's.
        let stage_token = self.stage_token()?;
        let expected_actor = match &stage_token {
            Some(_) => boss_jobs::credentials::broker_stage::ACTOR.to_string(),
            None => user
                .ambient_actor()
                .ok_or_else(|| {
                    HandlerError::Permanent("dispatcher phase command has no actor identity".into())
                })?
                .to_string(),
        };
        let mut request = self
            .client
            .post(&url)
            .header("x-boss-user", &actor)
            .header("x-sim-origin", super::common::sim_origin_value());
        if let Some(token) = stage_token {
            request = request.header(boss_jobs::credentials::broker_stage::HEADER, token);
        }
        let response = request
            .json(&evidence)
            .send()
            .await
            .map_err(|e| HandlerError::Downstream(format!("POST {url}: {e}")))?;
        if !response.status().is_success() {
            let status = response.status();
            let body = response.text().await.map_err(|e| {
                HandlerError::Downstream(format!("POST {url} error body unreadable: {e}"))
            })?;
            return Err(HandlerError::Downstream(format!(
                "POST {url} returned {status}: {body}"
            )));
        }
        let body: JsonValue = response
            .json()
            .await
            .map_err(|_| HandlerError::Downstream("phase owner response is unreadable".into()))?;
        let outcome: RotationOutcome =
            serde_json::from_value(body.get("observation").cloned().ok_or_else(|| {
                HandlerError::Downstream("phase owner response has no original receipt".into())
            })?)
            .map_err(|_| HandlerError::Downstream("phase owner receipt is malformed".into()))?;
        let receipt = match outcome {
            RotationOutcome::Recorded { receipt } | RotationOutcome::Replayed { receipt } => {
                receipt
            }
            RotationOutcome::LegacyRecorded => {
                return Err(HandlerError::Downstream(
                    "phase owner did not retain the original observation".into(),
                ));
            }
        };
        let mut expected = evidence.clone();
        let observation_id = boss_jobs::credentials::receipt::observation(&mut expected)
            .map_err(|_| HandlerError::Permanent("phase command observation is invalid".into()))?
            .ok_or_else(|| {
                HandlerError::Permanent("phase command has no durable observation identity".into())
            })?;
        expected["credential_id"] = json!(credential_id);
        let original: JsonValue = serde_json::from_str(&receipt.evidence_json).map_err(|_| {
            HandlerError::Downstream("phase owner original evidence is unreadable".into())
        })?;
        if receipt.version != 1
            || receipt.credential_id != credential_id
            || receipt.phase != phase.as_str()
            || receipt.observation_id != observation_id
            || receipt.actor != expected_actor
            || receipt.event_id.is_nil()
            || receipt.canonical != boss_core::job::canonical_json_bytes(&expected)
            || receipt.canonical != boss_core::job::canonical_json_bytes(&original)
        {
            return Err(HandlerError::Downstream(
                "phase owner receipt disagrees with the original exact command".into(),
            ));
        }
        Ok(())
    }

    /// The registry row must exist BEFORE anything is minted: every phase
    /// is recorded through the rotation door, which answers 404 for an
    /// undeclared id, and a value staged whose events cannot land is a
    /// value with no record.
    async fn require_registry_row(&self, credential_id: &str) -> Result<(), HandlerError> {
        let url = format!("{}/api/credentials/{credential_id}", self.jobs());
        let resp = self
            .client
            .get(&url)
            .header("x-boss-user", dispatcher_reader_header())
            .header("x-sim-origin", super::common::sim_origin_value())
            .send()
            .await
            .map_err(|e| HandlerError::Downstream(format!("GET {url}: {e}")))?;
        match resp.status() {
            s if s.is_success() => {
                // A successful read is not proof of the declared credential:
                // a different row/kind could otherwise receive runner mint
                // events after the first Secret write (1e50e66b).
                let row: boss_jobs::credentials::CredentialRow = resp.json().await.map_err(|_| {
                    HandlerError::Downstream(format!(
                        "GET {url} returned no complete credential registry row; nothing is minted"
                    ))
                })?;
                if row.id != credential_id
                    || row.kind != "ops-runner-credential"
                    || row.principal != boss_jobs::runner_credential::PRINCIPAL
                {
                    return Err(HandlerError::Permanent(format!(
                        "credential {credential_id} registry identity is not its declared runner kind and principal; nothing is minted"
                    )));
                }
                Ok(())
            }
            reqwest::StatusCode::NOT_FOUND => Err(HandlerError::Permanent(format!(
                "credential {credential_id} has no registry row (GET {url} answered 404): its \
                 rotation events would have nowhere to land, so nothing is minted. Declare the \
                 row (POST /api/credentials/batch, instance data) first"
            ))),
            s => Err(HandlerError::Downstream(format!("GET {url} returned {s}"))),
        }
    }

    async fn fetch_steps(&self, job_id: &str) -> Result<HashMap<String, StepView>, HandlerError> {
        let url = format!("{}/api/jobs/{job_id}", self.jobs());
        let resp = self
            .client
            .get(&url)
            .header("x-boss-user", dispatcher_reader_header())
            .header("x-sim-origin", super::common::sim_origin_value())
            .send()
            .await
            .map_err(|e| HandlerError::Downstream(format!("GET {url}: {e}")))?;
        if !resp.status().is_success() {
            let status = resp.status();
            return Err(HandlerError::Downstream(format!(
                "GET {url} returned {status}"
            )));
        }
        let body: JsonValue = resp
            .json()
            .await
            .map_err(|e| HandlerError::Downstream(format!("{url}: {e}")))?;
        Ok(body
            .get("steps")
            .and_then(JsonValue::as_array)
            .into_iter()
            .flatten()
            .filter_map(|s| {
                Some((
                    s.get("spec_slug")?.as_str()?.to_string(),
                    StepView {
                        id: s.get("id")?.as_str()?.to_string(),
                        status: s.get("status")?.as_str()?.to_string(),
                        completed_by: s
                            .get("completed_by")
                            .and_then(JsonValue::as_str)
                            .map(str::to_string),
                    },
                ))
            })
            .collect())
    }

    /// Complete one packet step through the step merge door, then its
    /// status. A completed step is left alone — the redelivery path; a
    /// slug the packet lacks is skipped.
    async fn complete_step(
        &self,
        rule_name: &str,
        job_id: &str,
        steps: &HashMap<String, StepView>,
        slug: &str,
        evidence: &[(&str, String)],
    ) -> Result<(), HandlerError> {
        let Some(step) = steps.get(slug) else {
            tracing::warn!(job_id, slug, "rotation packet has no such step; skipping");
            return Ok(());
        };
        if step.status == "completed" {
            return Ok(());
        }
        let fields = evidence
            .iter()
            .map(|(k, v)| ((*k).to_string(), json!(v)))
            .collect();
        super::common::complete_step(
            &self.client,
            self.jobs(),
            job_id,
            &step.id,
            fields,
            rule_name,
        )
        .await
    }

    /// The scope firing: mint into `next`, record issue and install, then
    /// verify through the door.
    async fn stage(
        &self,
        rule: &str,
        ev: &StepEvent<'_>,
        d: &Declaration<'_>,
    ) -> Result<(), HandlerError> {
        let steps = self.fetch_steps(ev.job_id).await?;
        let done = |slug: &str| steps.get(slug).is_some_and(|s| s.status == "completed");
        if ["issue", "install", "verify"].iter().all(|s| done(s)) {
            // Staged and verified, redelivered: the delivery firing does
            // the rest.
            return Ok(());
        }
        // Before anything is minted: a stage told to present a token it
        // cannot read would leave a value staged whose events cannot land.
        self.stage_token()?;
        let observed = self.observed_secret(d).await?;
        let held = Held::from_secret(&observed, d.host);
        let (value, witness) = match held.staged_for(ev.job_id) {
            Some(value) => {
                let witness = self.witness(d, &observed, ev.job_id)?;
                if witness.promoted || witness.last_eight != last_eight(value) {
                    return Err(HandlerError::Downstream(
                        "staged value disagrees with its original witness".into(),
                    ));
                }
                (value.to_owned(), witness)
            }
            None if done("install") => {
                return Err(HandlerError::Permanent(
                    "this packet's installed candidate is no longer staged; nothing minted".into(),
                ));
            }
            None => {
                if observed.data.contains_key(&witness_key(d.host, ev.job_id)) {
                    return Err(HandlerError::Downstream("original candidate was superseded; its witness remains, no new attempt is minted".into()));
                }
                judge_scope_naming(
                    meta_str(ev.metadata, "old_token"),
                    meta_str(ev.metadata, "old_token_last_eight"),
                    held.current.as_deref(),
                    d.host,
                )
                .map_err(HandlerError::Permanent)?;
                self.require_registry_row(d.credential_id).await?;
                let value = fresh_value();
                let attempt = uuid::Uuid::new_v4().to_string();
                let mut commands = std::collections::BTreeMap::new();
                commands.insert(
                    "minted".into(),
                    json!({"job_id":ev.job_id,"host":d.host,
                    "issuer":ISSUER,"value_length":value.len()}),
                );
                commands.insert("installed".into(),json!({"job_id":ev.job_id,"host":d.host,
                    "secret_namespace":d.secret_namespace,"secret_name":d.secret_name,"secret_key":d.key("next"),
                    "secret_uid":observed.uid,"precondition_version":observed.version,
                    "value_length":value.len(),"last_eight":last_eight(&value),"replaced_staged_for":held.next_for,
                    "carried_to_previous":held.next.as_deref().map(last_eight),
                    "dropped_previous":held.previous.as_deref().filter(|previous| held.next.as_deref().is_some_and(|next| next != *previous)).map(last_eight),
                    "delivery":DELIVERY_OFF_HOST}));
                commands.insert(
                    "verified".into(),
                    json!({"job_id":ev.job_id,"host":d.host,"method":"api",
                    "observed":format!("resolved to host {}",d.host)}),
                );
                commands.insert("revoked".into(),json!({"job_id":ev.job_id,"host":d.host,
                    "delivered_last_eight":last_eight(&value),"old_last_eight":held.current.as_deref().map(last_eight),
                    "carried_last_eight":held.next.as_deref().or(held.previous.as_deref()).map(last_eight),
                    "confirmed_dead":format!("GET {WHOAMI_PATH} confirms host {} current; original old and carried values resolve to nothing",d.host)}));
                for (phase, command) in &mut commands {
                    command["observation_id"] = json!(format!("{attempt}:{phase}"));
                }
                let witness = RecoveryWitness {
                    version: 1,
                    job_id: ev.job_id.into(),
                    credential_id: d.credential_id.into(),
                    namespace: d.secret_namespace.into(),
                    name: d.secret_name.into(),
                    host: d.host.into(),
                    uid: observed.uid.clone(),
                    attempt,
                    last_eight: last_eight(&value).into(),
                    promoted: false,
                    commands,
                };
                let raw = serde_json::to_string(&witness).map_err(|_| {
                    HandlerError::Downstream("cannot encode recovery witness".into())
                })?;
                let mut entries = vec![
                    (d.key("next"), value.clone()),
                    (minted_for_key(d.host, "next"), ev.job_id.into()),
                    (witness_key(d.host, ev.job_id), raw),
                    (
                        recovery_key(d.host, ev.job_id, "old"),
                        held.current.clone().unwrap_or_default(),
                    ),
                    (
                        recovery_key(d.host, ev.job_id, "carried"),
                        held.next
                            .clone()
                            .or_else(|| held.previous.clone())
                            .unwrap_or_default(),
                    ),
                ];
                if let Some(displaced) = held.next {
                    entries.push((d.key("previous"), displaced));
                }
                // The candidate and original commands commit together before a
                // completed Minted fact is recorded. A lost Secret ack is read back.
                self.write_observed(d, &observed, &entries).await?;
                let readback = self.observed_secret(d).await?;
                let actual = self.witness(d, &readback, ev.job_id)?;
                if Held::from_secret(&readback, d.host).staged_for(ev.job_id)
                    != Some(value.as_str())
                    || actual != witness
                {
                    return Err(HandlerError::Downstream(
                        "Secret write lacks exact candidate/witness readback".into(),
                    ));
                }
                (value, witness)
            }
        };
        for phase in [RotationPhase::Minted, RotationPhase::Installed] {
            self.record_phase(
                rule,
                d.credential_id,
                phase,
                witness.commands[phase.as_str()].clone(),
            )
            .await?;
        }

        self.complete_step(
            rule,
            ev.job_id,
            &steps,
            "issue",
            &[
                (
                    "issued",
                    format!(
                        "32 random bytes, base64url ({} characters, ending {}), minted for host \
                         {} by the credential broker (credential.minted on the log)",
                        value.len(),
                        last_eight(&value),
                        d.host
                    ),
                ),
                (
                    "issuer",
                    format!(
                        "credential-broker ({rule}); no external issuer — the jobs API's \
                         credential door is the only verifier"
                    ),
                ),
            ],
        )
        .await?;
        self.complete_step(
            rule,
            ev.job_id,
            &steps,
            "install",
            &[
                (
                    "installed",
                    format!(
                        "{} key {} holds this packet's value (…{}), {} names this packet; \
                         {}.current is untouched, so the runner's held value keeps resolving \
                         until the host has delivered this one",
                        d.secret(),
                        d.key("next"),
                        last_eight(&value),
                        minted_for_key(d.host, "next"),
                        d.host
                    ),
                ),
                (
                    "permissions",
                    format!("writer principal runner:ops for host {}", d.host),
                ),
                ("delivery", DELIVERY_OFF_HOST.to_string()),
            ],
        )
        .await?;

        let host = d.host;
        let (ok, seen) = self
            .watch(
                &value,
                |s| matches!(s, Seen::Resolved { host: h, .. } if h == host),
            )
            .await?;
        if !ok {
            return Err(match seen {
                Seen::Resolved { .. } => HandlerError::Permanent(format!(
                    "the staged value {} — not host {host}: the Secret's slots disagree with \
                     this declaration. Nothing is promoted; {host}'s current value stays valid",
                    seen.describe()
                )),
                Seen::Unresolved => HandlerError::Downstream(format!(
                    "not yet: GET {WHOAMI_PATH} presenting the value staged in {} answered {} — \
                     kubelet refreshes a Secret mount in about 60-90 s. Nothing is promoted; \
                     {host}'s current value stays valid",
                    d.key("next"),
                    seen.describe()
                )),
            });
        }
        self.record_phase(
            rule,
            d.credential_id,
            RotationPhase::Verified,
            witness.commands["verified"].clone(),
        )
        .await?;

        self.complete_step(
            rule,
            ev.job_id,
            &steps,
            "verify",
            &[
                (
                    "verified",
                    format!(
                        "GET {WHOAMI_PATH} presenting the staged value (…{}) {} — the jobs \
                         API's credential door reads the broker's Secret",
                        last_eight(&value),
                        seen.describe()
                    ),
                ),
                ("method", "api".to_string()),
            ],
        )
        .await
    }

    /// The delivery firing: promote the delivered value to `current`,
    /// clear the rest, confirm by effect.
    async fn promote(
        &self,
        rule: &str,
        ev: &StepEvent<'_>,
        d: &Declaration<'_>,
    ) -> Result<(), HandlerError> {
        let steps = self.fetch_steps(ev.job_id).await?;
        let done = |slug: &str| steps.get(slug).is_some_and(|s| s.status == "completed");
        if done("revoke") {
            return Ok(());
        }
        self.stage_token()?;
        let completer = steps
            .get("delivered")
            .and_then(|s| s.completed_by.as_deref());
        if completer != Some(boss_jobs::runner_credential::ACTOR) {
            return Err(HandlerError::Permanent(format!(
                "the delivered step was completed by {}, not by the resolved host runner: authenticated owner delivery is required, and \
                 the promotion kills the old one. Nothing is promoted and {}'s current value \
                 stays valid; abandon this packet and scope another",
                completer.unwrap_or("no recorded completer"),
                d.host
            )));
        }
        let delivered = meta_str(ev.metadata, "delivered_last_eight").ok_or_else(|| {
            HandlerError::Permanent(
                "the delivered step carries no delivered_last_eight; a delivery record must name \
                 the value the host installed"
                    .into(),
            )
        })?;
        let observed = self.observed_secret(d).await?;
        let held = Held::from_secret(&observed, d.host);
        let mut witness = self.witness(d, &observed, ev.job_id)?;
        let plan =
            plan_promotion(&held, ev.job_id, delivered, d.host).map_err(HandlerError::Permanent)?;
        if witness.last_eight != delivered
            || witness.promoted != matches!(plan, Promotion::Done { .. })
        {
            return Err(HandlerError::Downstream(
                "promotion slots disagree with original recovery witness".into(),
            ));
        }
        // A completed-by label is not proof. Read the original credential
        // owner observation and bind it to this actual Secret generation.
        let url = format!(
            "{}/api/credentials/{}/delivery/{}",
            self.jobs(),
            d.credential_id,
            witness.attempt
        );
        let response = self
            .client
            .get(&url)
            .header("x-boss-user", dispatcher_reader_header())
            .header("x-sim-origin", super::common::sim_origin_value())
            .send()
            .await
            .map_err(|e| HandlerError::Downstream(format!("GET {url}: {e}")))?;
        if !response.status().is_success() {
            return Err(HandlerError::Downstream(format!(
                "authenticated delivery owner receipt unavailable: GET {url} returned {}",
                response.status()
            )));
        }
        let body: JsonValue = response
            .json()
            .await
            .map_err(|_| HandlerError::Downstream("delivery owner response unreadable".into()))?;
        let receipt: boss_jobs::credentials::receipt::RotationReceipt =
            serde_json::from_value(body["receipt"].clone())
                .map_err(|_| HandlerError::Downstream("delivery owner receipt malformed".into()))?;
        let request = boss_jobs::credentials::runner_delivery::DeliveryRequest {
            job_id: uuid::Uuid::parse_str(ev.job_id).map_err(|_| {
                HandlerError::Permanent("delivery packet identity malformed".into())
            })?,
            attempt: uuid::Uuid::parse_str(&witness.attempt)
                .map_err(|_| HandlerError::Downstream("recovery attempt malformed".into()))?,
            secret_uid: witness.uid.clone(),
        };
        if !boss_jobs::credentials::runner_delivery::receipt_matches_delivery(
            &receipt,
            d.credential_id,
            d.host,
            &request,
            &witness.commands["installed"],
        ) {
            return Err(HandlerError::Downstream(
                "original delivery receipt disagrees with actual recovery generation".into(),
            ));
        }
        // The original private candidates remain in separate Secret keys,
        // never in this witness, receipt, packet, log or error.
        let original = |slot: &str| {
            observed
                .data
                .get(&recovery_key(d.host, ev.job_id, slot))
                .cloned()
                .filter(|value| !value.is_empty())
        };
        let old = original("old");
        let carried = original("carried");
        if witness.commands["revoked"]["old_last_eight"] != json!(old.as_deref().map(last_eight))
            || witness.commands["revoked"]["carried_last_eight"]
                != json!(carried.as_deref().map(last_eight))
        {
            return Err(HandlerError::Downstream(
                "recovery candidates disagree with original retirement history".into(),
            ));
        }
        if !done("issue") || !done("install") {
            return Err(HandlerError::Downstream("source issue/install lack their acknowledged owner phases; delivery cannot impersonate the stage actor".into()));
        }
        let host = d.host;

        // Verify, when the scope firing ran out of redeliveries before the
        // door saw the value: the host's deposit proved it through the
        // same door before it recorded delivery, so this converges.
        let value = match &plan {
            Promotion::Done { value } | Promotion::Promote { value } => value.clone(),
        };
        if !done("verify") {
            let (ok, seen) = self
                .watch(
                    &value,
                    |s| matches!(s, Seen::Resolved { host: h, .. } if h == host),
                )
                .await?;
            if !ok {
                return Err(HandlerError::Downstream(format!(
                    "not yet: the delivered value (…{}) {} through GET {WHOAMI_PATH}; nothing \
                     is promoted",
                    last_eight(&value),
                    seen.describe()
                )));
            }
            // A real independent delivery read is a distinct observation,
            // not a replay under another actor or a forged source header.
            let mut command = witness.commands["verified"].clone();
            command["observation_id"] = json!(format!("{}:verified-delivery", witness.attempt));
            self.record_phase(rule, d.credential_id, RotationPhase::Verified, command)
                .await?;

            self.complete_step(
                rule,
                ev.job_id,
                &steps,
                "verify",
                &[
                    (
                        "verified",
                        format!(
                            "GET {WHOAMI_PATH} presenting the value (…{}) {} (verified on the \
                             delivery firing)",
                            last_eight(&value),
                            seen.describe()
                        ),
                    ),
                    ("method", "api".to_string()),
                ],
            )
            .await?;
        }

        if let Promotion::Promote { value } = &plan {
            witness.promoted = true;
            let raw = serde_json::to_string(&witness)
                .map_err(|_| HandlerError::Downstream("cannot encode promotion witness".into()))?;
            self.write_observed(
                d,
                &observed,
                &[
                    (d.key("current"), value.clone()),
                    (minted_for_key(host, "current"), ev.job_id.into()),
                    (d.key("next"), String::new()),
                    (minted_for_key(host, "next"), String::new()),
                    (d.key("previous"), String::new()),
                    (witness_key(host, ev.job_id), raw),
                ],
            )
            .await?;
            let readback = self.observed_secret(d).await?;
            let actual = self.witness(d, &readback, ev.job_id)?;
            if !actual.promoted
                || Held::from_secret(&readback, host).promoted_by(ev.job_id) != Some(value.as_str())
                || actual != witness
            {
                return Err(HandlerError::Downstream(
                    "promotion lacks exact effect/witness readback".into(),
                ));
            }
        }

        // Confirm by effect. The new value at slot `current` means the jobs
        // API reads the promoted directory, which kubelet swaps whole; the
        // old value, while this firing holds it, must resolve to nothing.
        let (ok, seen) = self
            .watch(
                &value,
                |s| matches!(s, Seen::Resolved { host: h, slot } if h == host && slot == "current"),
            )
            .await?;
        if !ok {
            return Err(HandlerError::Downstream(format!(
                "not yet: {host}.current holds this packet's value in the Secret, and the door \
                 answers it {} — kubelet has not refreshed the mount. The old value still \
                 resolves until it does",
                seen.describe()
            )));
        }
        let mut dead = Vec::new();
        for (what, o) in [
            ("old value", old.as_deref()),
            ("carried value", carried.as_deref()),
        ]
        .into_iter()
        .filter_map(|(w, o)| o.map(|o| (w, o)))
        .filter(|(_, o)| *o != value)
        {
            let answer = self.whoami(o).await?;
            if answer != Seen::Unresolved {
                return Err(HandlerError::Downstream(format!(
                    "not yet: the {what} (…{}) still {} after the promotion",
                    last_eight(o),
                    answer.describe()
                )));
            }
            dead.push(format!(
                "GET {WHOAMI_PATH} presenting the {what} (…{}) answered resolved: false",
                last_eight(o)
            ));
        }
        let old_dead = (!dead.is_empty()).then(|| dead.join("; "));
        let revoked = match (&plan, old.as_deref()) {
            (Promotion::Promote { .. }, Some(o)) => format!(
                "{host}'s old value (…{}) left {} at the promotion: {} now holds this packet's \
                 value (…{}), and {}.next and {}.previous are blank",
                last_eight(o),
                d.secret(),
                d.key("current"),
                last_eight(&value),
                host,
                host
            ),
            (Promotion::Promote { .. }, None) => format!(
                "{host} held no current value before this rotation; {} now holds this packet's \
                 value (…{}), and {}.next and {}.previous are blank",
                d.key("current"),
                last_eight(&value),
                host,
                host
            ),
            (Promotion::Done { .. }, _) => format!(
                "an earlier firing promoted this packet's value (…{}) to {} and blanked {}.next \
                 and {}.previous; the old value left the Secret with it",
                last_eight(&value),
                d.key("current"),
                host,
                host
            ),
        };
        let confirmed = format!(
            "GET {WHOAMI_PATH} presenting the new value {}: the door reads the promoted slots, in \
             which no slot holds the old value{}",
            seen.describe(),
            old_dead
                .as_deref()
                .map(|s| format!("; {s}"))
                .unwrap_or_default()
        );
        self.record_phase(
            rule,
            d.credential_id,
            RotationPhase::Revoked,
            witness.commands["revoked"].clone(),
        )
        .await?;
        self.complete_step(
            rule,
            ev.job_id,
            &steps,
            "revoke",
            &[("revoked", revoked), ("confirmed_dead", confirmed)],
        )
        .await
    }
}

/// A non-empty trimmed string field of a step's metadata.
fn meta_str<'a>(m: &'a serde_json::Map<String, JsonValue>, key: &str) -> Option<&'a str> {
    m.get(key)
        .and_then(JsonValue::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
}

#[async_trait]
impl Handler for CredentialRotateOpsRunner {
    fn name(&self) -> &'static str {
        HANDLER
    }

    async fn invoke(
        &self,
        args: &[(String, Value)],
        ctx: &InvocationContext,
    ) -> Result<(), HandlerError> {
        let d = Declaration::parse(args)?;
        let ev = StepEvent::from_payload(&ctx.event_payload)?;
        if uuid::Uuid::parse_str(ev.job_id).is_err() {
            return Err(HandlerError::Permanent(
                "rotation packet identity is not a UUID; nothing read or minted".into(),
            ));
        }
        if ev.subject_id != d.credential_id {
            return Err(HandlerError::Permanent(format!(
                "{} fired for subject {:?}, but its declaration is credential {:?}: nothing is \
                 read or minted",
                ctx.rule_name, ev.subject_id, d.credential_id
            )));
        }
        match ev.kind {
            SCOPE_KIND => self.stage(&ctx.rule_name, &ev, &d).await,
            DELIVERY_KIND => self.promote(&ctx.rule_name, &ev, &d).await,
            other => Err(HandlerError::Permanent(format!(
                "{} fired on a `{other}` step; this handler stages on `{SCOPE_KIND}` and \
                 promotes on `{DELIVERY_KIND}`",
                ctx.rule_name
            ))),
        }
    }
}

#[cfg(test)]
mod tests;
