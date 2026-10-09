//! Broker-owned preparation of an exact-purpose SSH transport key (1e50e66b).
//!
//! This is the source dependency for the remote runner deposit. Preparing
//! a pair is not enrollment: no receiver grant, rotation completion, or
//! old-key revocation is inferred here. A later delivery handler must prove
//! the separately authorized receiver before advancing those phases.
use super::credential_issuer::{SecretStore, WriteAt};
use async_trait::async_trait;

/// Only the fields consumed at this boundary. Required fields have no
/// defaults: an omitted native field is not a completed authorization.
#[derive(serde::Deserialize)]
pub struct PreparationPacket {
    id: String,
    kind: String,
    status: String,
    partition: String,
    // boss-core's own `Subject`, the type the jobs API serializes
    // (`{subject_kind, id}`), not a private mirror of it. This read
    // carried one — `{kind, id}` — from the runner key's handler (train
    // 927) until delta review 76249509 (B1) drove the handler against
    // the real router: every real packet answered "malformed consumed
    // field" at this, the handler's FIRST read, a Permanent error, so
    // after the human scope nothing was minted, for ever, for either
    // key. The stub jobs server in `tests/broker_transport_key.rs`
    // spelled the subject this struct's way, so its 25 tests were green;
    // `tests/deposit_key_enrollment_e2e.rs` is the test that reads what
    // the router serves (CLAUDE.md §9a: a fact that lives twice).
    subject: boss_core::job::Subject,
    steps: Vec<PreparationStep>,
}

#[derive(serde::Deserialize)]
pub struct PreparationStep {
    pub id: String,
    spec_slug: String,
    kind: String,
    pub status: String,
    metadata: serde_json::Map<String, serde_json::Value>,
    // Other steps need not have a completer. Scope requires one below.
    completed_by: Option<String>,
    assurance_required: Option<String>,
    #[serde(default)]
    sign_offs_required: Vec<String>,
}

impl PreparationPacket {
    pub fn step(&self, slug: &str) -> Result<&PreparationStep, String> {
        let mut matches = self.steps.iter().filter(|step| step.spec_slug == slug);
        let step = matches
            .next()
            .ok_or_else(|| format!("preparation packet lacks {slug}"))?;
        if matches.next().is_some() {
            return Err(format!("preparation packet repeats {slug}"));
        }
        Ok(step)
    }
}

/// Re-read native scope before the first Secret read or issuer call.
/// The API enforces human_only at completion; the recorded completer is
/// checked as well. This is not a claim that the runner authentication
/// prerequisites in the parent packet have been completed.
pub fn authorize_preparation(
    value: &serde_json::Value,
    packet_id: &str,
    scope_id: &str,
    protocol_kind: &str,
    credential_id: &str,
) -> Result<PreparationPacket, String> {
    let packet: PreparationPacket = serde_json::from_value(value.clone())
        .map_err(|_| "native preparation packet has a malformed consumed field")?;
    if packet.partition != "real"
        || packet.id != packet_id
        || packet.kind != protocol_kind
        || packet.status != "open"
        || packet.subject.kind != "custom"
        || packet.subject.id != credential_id
    {
        return Err("native packet does not match the declared preparation identity".into());
    }
    let scope = packet.step("scope")?;
    if scope.id != scope_id
        || scope.kind != "credential-rotation"
        || scope.status != "completed"
        || scope.metadata.get("human_only") != Some(&serde_json::Value::Bool(true))
        || scope
            .metadata
            .get("credential")
            .and_then(serde_json::Value::as_str)
            != Some(credential_id)
        || !scope
            .completed_by
            .as_deref()
            .is_some_and(|actor| actor.starts_with("emp-") && actor.len() > 4)
    {
        return Err("transport preparation requires its native completed human scope".into());
    }
    for slug in ["issue", "install", "enroll"] {
        packet.step(slug)?;
    }
    let mut ids = std::collections::BTreeSet::new();
    for step in &packet.steps {
        if step.id.is_empty()
            || !ids.insert(&step.id)
            || !matches!(
                step.status.as_str(),
                "pending" | "ready" | "active" | "completed" | "skipped"
            )
        {
            return Err(
                "native preparation steps have duplicate identities or invalid state".into(),
            );
        }
    }
    for slug in ["issue", "install"] {
        let step = packet.step(slug)?;
        if step.kind != "task"
            || !step.sign_offs_required.is_empty()
            || step.assurance_required.is_some()
            || step.metadata.get("human_only") == Some(&serde_json::Value::Bool(true))
        {
            return Err(
                "preparation machine step has an unexpected human completion contract".into(),
            );
        }
    }
    let enroll = packet.step("enroll")?;
    if enroll.kind != "sign-off"
        || enroll.metadata.get("human_only") != Some(&serde_json::Value::Bool(true))
        || enroll.assurance_required.as_deref() != Some("presence")
        || enroll.sign_offs_required != ["platform-admin"]
    {
        return Err("receiver enrollment must remain a distinct human sign-off".into());
    }
    Ok(packet)
}

/// What the first transport is for; the purpose of a rule row that
/// names none (the runner's row, 1e50e66b).
pub const RUNNER_PURPOSE: &str = "ops-runner credential deposit";

/// The command a rule row forces on its key: one line with no quote,
/// backslash or control character, so that wrapped as
/// `command="<it>",restrict` it cannot close its own quoting or carry a
/// second option or key onto the step a passkey signs.
pub fn is_forced_command(command: &str) -> bool {
    !command.trim().is_empty()
        && !command
            .chars()
            .any(|c| c == '"' || c == '\\' || c.is_control())
}

/// The authorized_keys line for `command` and `public_key`: the forced
/// command, `restrict` (no pty, forwarding, agent or user rc), the key.
pub fn authorized_keys_line(command: &str, public_key: &str) -> String {
    format!("command=\"{command}\",restrict {public_key}")
}

/// The registered broker executor prepares a public enrollment proposal.
/// Receiver installation is a separate human step; this handler never
/// completes it or records verified/revoked effects.
pub struct CredentialPrepareSshDeposit {
    jobs: String,
    client: boss_core::machine_token::Client,
    store: std::sync::Arc<dyn SecretStore>,
    issuer: std::sync::Arc<dyn KeyIssuer>,
}

impl CredentialPrepareSshDeposit {
    pub fn new(
        jobs: &str,
        store: std::sync::Arc<dyn SecretStore>,
        issuer: std::sync::Arc<dyn KeyIssuer>,
    ) -> std::sync::Arc<Self> {
        std::sync::Arc::new(Self {
            jobs: jobs.trim_end_matches('/').into(),
            client: super::common::api_client(),
            store,
            issuer,
        })
    }

    async fn read(
        &self,
        path: &str,
    ) -> Result<serde_json::Value, boss_dispatcher::rules::handler::HandlerError> {
        use boss_dispatcher::rules::handler::HandlerError;
        let response = self
            .client
            .get(format!("{}{path}", self.jobs))
            .header("x-boss-user", super::common::dispatcher_reader_header())
            .header("x-sim-origin", super::common::sim_origin_value())
            .send()
            .await
            .map_err(|error| {
                HandlerError::Downstream(format!("preparation read failed: {error}"))
            })?;
        if !response.status().is_success() {
            return Err(HandlerError::Downstream(format!(
                "preparation GET {path} returned {}",
                response.status()
            )));
        }
        response
            .json()
            .await
            .map_err(|error| HandlerError::Downstream(format!("preparation GET {path}: {error}")))
    }
}

#[async_trait]
impl boss_dispatcher::rules::handler::Handler for CredentialPrepareSshDeposit {
    fn name(&self) -> &'static str {
        "credential.prepare.ssh-deposit"
    }

    async fn invoke(
        &self,
        args: &[(String, boss_dispatcher::rules::expr::Value)],
        context: &boss_dispatcher::rules::handler::InvocationContext,
    ) -> Result<(), boss_dispatcher::rules::handler::HandlerError> {
        use boss_dispatcher::rules::handler::{HandlerError, arg_string};
        use serde_json::json;
        let namespace = arg_string(args, "secret_namespace")?;
        let secret = arg_string(args, "secret_name")?;
        let credential = arg_string(args, "credential_id")?;
        let protocol = arg_string(args, "protocol_kind")?;
        let receiver = arg_string(args, "receiver_host")?;
        // WHAT THE KEY IS FOR, AND THE ONE LINE THAT ENROLLS IT, are the
        // rule row's to declare (backlog 88379df3; design-doc bdc60b65,
        // gcp-push). Until a second transport existed both were the
        // runner's, spelled here: a key prepared to carry the estate
        // machine token would have put `ops-runner credential deposit`
        // under the passkey that signs its enrollment. A row that names
        // neither keeps the runner's purpose and carries no line.
        let purpose = match boss_dispatcher::rules::handler::arg(args, "purpose") {
            None => RUNNER_PURPOSE,
            Some(_) => arg_string(args, "purpose")?,
        };
        let forced = match boss_dispatcher::rules::handler::arg(args, "forced_command") {
            None => None,
            Some(_) => Some(arg_string(args, "forced_command")?),
        };
        if purpose.is_empty() || purpose.chars().any(char::is_control) {
            return Err(HandlerError::Permanent(
                "preparation rule declares an empty or multi-line purpose".into(),
            ));
        }
        if forced.is_some_and(|line| !is_forced_command(line)) {
            return Err(HandlerError::Permanent(
                "preparation rule's forced_command is empty or carries a quote, a backslash or a line break; nothing minted".into(),
            ));
        }
        let event = super::common::StepEvent::from_payload(&context.event_payload)?;
        if event.kind != "credential-rotation"
            || event.subject_kind != "custom"
            || event.subject_id != credential
        {
            return Err(HandlerError::Permanent(
                "preparation event does not match the declared credential scope".into(),
            ));
        }
        uuid::Uuid::parse_str(event.job_id)
            .map_err(|_| HandlerError::Permanent("preparation event lacks a packet UUID".into()))?;
        // Native authorization is read before either credential storage or
        // key generation. Registry presence is not authority to mint.
        let body = self.read(&format!("/api/jobs/{}", event.job_id)).await?;
        let packet =
            authorize_preparation(&body, event.job_id, event.step_id, protocol, credential)
                .map_err(HandlerError::Permanent)?;
        let row = self.read(&format!("/api/credentials/{credential}")).await?;
        if row.get("id").and_then(serde_json::Value::as_str) != Some(credential)
            || row.get("kind").and_then(serde_json::Value::as_str) != Some("ssh-transport-key")
            || row
                .get("storage_location")
                .and_then(serde_json::Value::as_str)
                != Some(format!("Secret {namespace}/{secret}").as_str())
            || receiver.is_empty()
        {
            return Err(HandlerError::Permanent(
                "preparation registry row does not match the declared transport storage".into(),
            ));
        }
        // Once a public proposal or preparation receipt exists, storage
        // loss is a failed replay, never permission to generate a new key.
        let mut recorded = false;
        let mut recorded_public: Option<&str> = None;
        for slug in ["issue", "install", "enroll"] {
            let step = packet.step(slug).map_err(HandlerError::Permanent)?;
            let public = step.metadata.get("public_key");
            // Under a forced command the enrollment step holds the LINE
            // and no bare key (below). A line there is still a recorded
            // proposal — storage lost after it is a failed replay — and
            // it is judged whole further down, against the line this
            // handler joins from the pair the Secret holds.
            if slug == "enroll"
                && forced.is_some()
                && public.is_none()
                && step.metadata.contains_key("authorized_keys_line")
            {
                recorded = true;
                continue;
            }
            if step.status == "completed" || public.is_some() {
                recorded = true;
                let public = public
                    .and_then(serde_json::Value::as_str)
                    .filter(|value| !value.is_empty())
                    .ok_or_else(|| {
                        HandlerError::Permanent(
                            "recorded preparation lacks its public identity; nothing minted".into(),
                        )
                    })?;
                if recorded_public.is_some_and(|prior| prior != public) {
                    return Err(HandlerError::Permanent(
                        "recorded preparation public identities disagree; nothing minted".into(),
                    ));
                }
                recorded_public = Some(public);
            }
        }
        let prepared = prepare_checked(
            self.store.as_ref(),
            self.issuer.as_ref(),
            namespace,
            secret,
            event.job_id,
            recorded,
            recorded_public,
        )
        .await
        .map_err(HandlerError::Downstream)?;
        let PreparedKey::AwaitingEnrollment { public_key } = prepared else {
            return Err(HandlerError::Downstream(
                "transport Secret changed during preparation; re-read before retry".into(),
            ));
        };
        let enroll = packet.step("enroll").map_err(HandlerError::Permanent)?;
        // The whole authorized_keys line, joined here from the rule's
        // forced command and the public half this handler prepared, so
        // the passkey signs the line itself and nobody assembles it.
        //
        // AND WHERE THERE IS A LINE, THE STEP CARRIES NO BARE KEY (backlog
        // dea2236f). This wrote `public_key` beside the line until the
        // first live enrolment (packet 96a0f7bb, 2026-10-07), where the
        // key alone was the one copied into authorized_keys: sshd took
        // it as a login, and for ten to fifteen minutes a key minted for
        // one receiver command opened the deposit account's shell. The
        // line ends with the key, so nothing a signer needs is lost; the
        // public half to COMPARE it with is on the completed `install`
        // receipt, which nobody can rewrite. A row that forces nothing
        // has no line to place, and keeps the key as its proposal.
        let proposal = match forced {
            Some(forced) => json!({
                "receiver_host": receiver,
                "purpose": purpose,
                "authorized_keys_line": authorized_keys_line(forced, &public_key),
            }),
            None => {
                json!({"public_key": public_key, "receiver_host": receiver, "purpose": purpose})
            }
        };
        let proposal_fields = proposal.as_object().ok_or_else(|| {
            HandlerError::Permanent("enrollment proposal was not an object".into())
        })?;
        let same_proposal = proposal_fields
            .iter()
            .all(|(key, value)| enroll.metadata.get(key) == Some(value));
        if !same_proposal {
            if enroll.status == "completed" {
                return Err(HandlerError::Permanent(
                    "completed receiver enrollment does not match the prepared public proposal"
                        .into(),
                ));
            }
            super::common::write_json(
                &self.client,
                reqwest::Method::PATCH,
                &format!(
                    "{}/api/jobs/{}/steps/{}/metadata",
                    self.jobs, event.job_id, enroll.id
                ),
                &proposal,
                &context.rule_name,
            )
            .await?;
        }
        for (slug, field, evidence) in [
            (
                "issue",
                "issued",
                "Broker prepared an SSH pair; public key awaits receiver enrollment",
            ),
            (
                "install",
                "installed",
                "Pair and packet provenance read back from declared Secret; receiver installation unproved",
            ),
        ] {
            let step = packet.step(slug).map_err(HandlerError::Permanent)?;
            if step.status == "completed" {
                continue;
            }
            let fields = json!({field: evidence, "public_key": public_key, "receiver_host": receiver, "purpose": purpose, "enrollment": "pending"});
            let fields = fields.as_object().cloned().ok_or_else(|| {
                HandlerError::Permanent("preparation evidence was not an object".into())
            })?;
            // Preparation records only the native step's completion
            // event. The rotation phase endpoint has no packet/phase
            // idempotence and is deliberately not called here.
            if !fields
                .iter()
                .all(|(key, value)| step.metadata.get(key) == Some(value))
            {
                super::common::write_json(
                    &self.client,
                    reqwest::Method::PATCH,
                    &format!(
                        "{}/api/jobs/{}/steps/{}/metadata",
                        self.jobs, event.job_id, step.id
                    ),
                    &serde_json::Value::Object(fields),
                    &context.rule_name,
                )
                .await?;
            }
            super::common::write_json(
                &self.client,
                reqwest::Method::PUT,
                &format!("{}/api/jobs/{}/steps/{}", self.jobs, event.job_id, step.id),
                &json!({"status": "completed"}),
                &context.rule_name,
            )
            .await?;
        }
        Ok(())
    }
}

/// Private bytes go only to the Secret store, never to a preparation receipt.
pub struct IssuedKey {
    private_key: String,
    public_key: String,
}

impl IssuedKey {
    pub fn new(private_key: String, public_key: String) -> Self {
        Self {
            private_key,
            public_key,
        }
    }
}

impl std::fmt::Debug for IssuedKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("IssuedKey")
            .field("public_key", &self.public_key)
            .finish_non_exhaustive()
    }
}

#[async_trait]
pub trait KeyIssuer: Send + Sync {
    /// Errors describe the failure without private key bytes.
    async fn mint(&self) -> Result<IssuedKey, String>;
    /// Pure validation of the pair, including its mathematical key material.
    /// No storage, minting, or private bytes in errors. Every issuer must
    /// define this boundary; presence alone cannot authorize replay.
    fn validate_pair(&self, private_key: &str, public_key: &str) -> Result<(), String>;
}

/// No process or private staging file: cancellation leaves only an owned
/// computation in memory, never a persistent private half (1e50e66b).
pub struct RsaSshIssuer;

pub const RSA_BITS: usize = 3072;

fn ssh_string(bytes: &[u8], wire: &mut Vec<u8>) -> Result<(), String> {
    let length = u32::try_from(bytes.len()).map_err(|_| "SSH field exceeds its wire bound")?;
    wire.extend_from_slice(&length.to_be_bytes());
    wire.extend_from_slice(bytes);
    Ok(())
}

fn ssh_mpint(bytes: &[u8], wire: &mut Vec<u8>) -> Result<(), String> {
    // SSH mpint is signed. Positive RSA integers with their high bit set
    // need a leading zero; a missing one changes the mathematical key.
    if bytes.first().is_some_and(|byte| byte & 0x80 != 0) {
        let positive = std::iter::once(0)
            .chain(bytes.iter().copied())
            .collect::<Vec<_>>();
        ssh_string(&positive, wire)
    } else {
        ssh_string(bytes, wire)
    }
}

fn rsa_ssh_public(private: &rsa::RsaPrivateKey) -> Result<String, String> {
    use base64::Engine;
    use rsa::traits::PublicKeyParts;
    let mut wire = Vec::new();
    ssh_string(b"ssh-rsa", &mut wire)?;
    ssh_mpint(&private.e().to_bytes_be(), &mut wire)?;
    ssh_mpint(&private.n().to_bytes_be(), &mut wire)?;
    Ok(format!(
        "ssh-rsa {}",
        base64::engine::general_purpose::STANDARD.encode(wire)
    ))
}

#[async_trait]
impl KeyIssuer for RsaSshIssuer {
    fn validate_pair(&self, private_key: &str, public_key: &str) -> Result<(), String> {
        use rsa::pkcs1::DecodeRsaPrivateKey;
        let private = rsa::RsaPrivateKey::from_pkcs1_pem(private_key)
            .map_err(|_| "stored RSA private key is malformed")?;
        private
            .validate()
            .map_err(|_| "stored RSA private key is mathematically invalid")?;
        if rsa_ssh_public(&private)? != public_key {
            return Err("stored private key does not match the exact public identity".into());
        }
        Ok(())
    }

    async fn mint(&self) -> Result<IssuedKey, String> {
        tokio::task::spawn_blocking(|| {
            use rsa::pkcs1::EncodeRsaPrivateKey;
            let private = rsa::RsaPrivateKey::new(&mut rsa::rand_core::OsRng, RSA_BITS)
                .map_err(|_| "RSA transport key generation failed")?;
            let public = rsa_ssh_public(&private)?;
            let pem = private
                .to_pkcs1_pem(rsa::pkcs1::LineEnding::LF)
                .map_err(|_| "RSA transport key encoding failed")?;
            // The encoder adds a terminal LF; the existing Secret adapter
            // trims read values. Store the canonical representation here so
            // exact readback still detects changed pair bytes or provenance.
            // Internal line breaks remain untouched, as does the shared reader.
            Ok(IssuedKey::new(
                pem.trim_end_matches('\n').to_owned(),
                public,
            ))
        })
        .await
        .map_err(|_| "RSA transport key worker did not return")?
    }
}

#[derive(Debug, PartialEq, Eq)]
pub enum PreparedKey {
    AwaitingEnrollment {
        public_key: String,
    },
    /// Another firing won the conditional write; re-read before any action.
    Moved,
}

/// Prepare once, under the declared Secret's version. No unconditional write
/// can safely initialize a key: two firings could publish different halves
/// or replace the pair a person just authorized.
pub async fn prepare(
    store: &dyn SecretStore,
    issuer: &dyn KeyIssuer,
    namespace: &str,
    secret: &str,
    packet: &str,
) -> Result<PreparedKey, String> {
    prepare_checked(store, issuer, namespace, secret, packet, false, None).await
}

async fn prepare_checked(
    store: &dyn SecretStore,
    issuer: &dyn KeyIssuer,
    namespace: &str,
    secret: &str,
    packet: &str,
    recorded: bool,
    expected_public: Option<&str>,
) -> Result<PreparedKey, String> {
    uuid::Uuid::parse_str(packet).map_err(|_| "transport preparation needs a packet UUID")?;
    if namespace.is_empty() || secret.is_empty() {
        return Err("transport preparation needs a declared namespace and Secret".into());
    }
    let held = store
        .read_secret(namespace, secret)
        .await
        .map_err(|_| "could not read the declared transport Secret")?
        .ok_or("the declared transport Secret is absent; preparation does not create it")?;
    let private = held.data.get("private_key").filter(|v| !v.is_empty());
    let public = held.data.get("public_key").filter(|v| !v.is_empty());
    let owner = held.data.get("minted_for").filter(|v| !v.is_empty());
    match (private, public, owner) {
        (Some(private), Some(public), Some(owner)) if owner == packet => {
            if expected_public.is_some_and(|expected| expected != public) {
                return Err("stored pair differs from recorded public preparation; nothing minted or replaced".into());
            }
            issuer
                .validate_pair(private, public)
                .map_err(|_| "stored transport pair is invalid; nothing minted or replaced")?;
            return Ok(PreparedKey::AwaitingEnrollment {
                public_key: public.clone(),
            });
        }
        (None, None, None) if !recorded => {}
        (None, None, None) => {
            return Err("recorded preparation pair is absent; nothing minted or replaced".into());
        }
        _ => {
            return Err(
                "transport Secret already holds a pair or incomplete provenance; nothing replaced"
                    .into(),
            );
        }
    }
    let issued = issuer
        .mint()
        .await
        .map_err(|why| format!("transport key issuer failed; nothing installed: {why}"))?;
    if issued.private_key.is_empty() || issued.public_key.is_empty() {
        return Err("transport key issuer returned an incomplete pair; nothing installed".into());
    }
    issuer
        .validate_pair(&issued.private_key, &issued.public_key)
        .map_err(|_| "transport key issuer returned an invalid pair; nothing installed")?;
    match store
        .write_keys_at(
            namespace,
            secret,
            &[
                ("private_key", &issued.private_key),
                ("public_key", &issued.public_key),
                ("minted_for", packet),
            ],
            &held.version,
        )
        .await
        .map_err(|_| "could not conditionally install the transport pair")?
    {
        WriteAt::Written => {
            let observed = store
                .read_secret(namespace, secret)
                .await
                .map_err(|_| "could not read back the prepared transport pair")?
                .ok_or("prepared transport Secret disappeared before readback")?;
            if observed.data.get("private_key") != Some(&issued.private_key)
                || observed.data.get("public_key") != Some(&issued.public_key)
                || observed.data.get("minted_for").map(String::as_str) != Some(packet)
            {
                return Err(
                    "transport pair or provenance disagreed on readback; enrollment not ready"
                        .into(),
                );
            }
            Ok(PreparedKey::AwaitingEnrollment {
                public_key: issued.public_key,
            })
        }
        WriteAt::Moved => Ok(PreparedKey::Moved),
    }
}
