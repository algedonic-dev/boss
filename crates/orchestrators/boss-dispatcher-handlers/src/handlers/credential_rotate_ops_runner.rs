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
//!   newest displaced value is kept; a host that missed two stages inside
//!   one converge interval holds one no slot names until its next pass.
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
//! The delivery record is believed only from the deposit's own actor
//! ([`DEPOSIT_ACTOR`]): a `delivered` step completed by hand promotes
//! nothing (review 3930a3eb, N2).
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
use boss_jobs::runner_credential::{HEADER, WHOAMI_PATH, slot_of};
use serde_json::{Value as JsonValue, json};
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use super::common::{StepEvent, dispatcher_reader_header};
use super::credential_issuer::SecretStore;
use super::credential_rotate_forgejo::last_eight;

/// The handler's registered name — the `handler = "…"` of both rules.
pub const HANDLER: &str = "credential.rotate.ops-runner";

/// The install step's `delivery`: the protocol's `delivered` step goes
/// ready on it, and the promotion waits for that step.
pub const DELIVERY_OFF_HOST: &str = "off-host";

/// The one completer whose `delivered` record the promotion believes:
/// the host's deposit (infra/forge/runner-credential-deposit.sh signs as
/// it; pinned equal by the handler's tests). The promotion kills the old
/// value, and the last eight a delivery carries is printed on the install
/// step for anyone to copy, so a hand-completed `delivered` would kill a
/// value the host still presents (review 3930a3eb, N2). The actor rides
/// `x-boss-user`, which a machine-token holder can type (backlog
/// 2710c8fc), so this refuses the honest mistake, not the forger.
pub const DEPOSIT_ACTOR: &str = "automation:credential-deposit";

/// The step kinds of the two firings.
const SCOPE_KIND: &str = "credential-rotation";
const DELIVERY_KIND: &str = "credential-delivery";

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
        Arc::new(Self {
            client: super::common::api_client(),
            jobs_base: jobs_base.into(),
            secrets,
            poll,
        })
    }

    fn jobs(&self) -> &str {
        self.jobs_base.trim_end_matches('/')
    }

    async fn read(&self, d: &Declaration<'_>, key: String) -> Result<Option<String>, HandlerError> {
        Ok(self
            .secrets
            .read_key(d.secret_namespace, d.secret_name, &key)
            .await
            .map_err(HandlerError::Downstream)?
            .filter(|v| !v.trim().is_empty()))
    }

    async fn read_held(&self, d: &Declaration<'_>) -> Result<Held, HandlerError> {
        Ok(Held {
            current: self.read(d, d.key("current")).await?,
            current_for: self.read(d, minted_for_key(d.host, "current")).await?,
            next: self.read(d, d.key("next")).await?,
            next_for: self.read(d, minted_for_key(d.host, "next")).await?,
            previous: self.read(d, d.key("previous")).await?,
        })
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
        super::common::post_json(&self.client, &url, &evidence, rule_name).await
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
            s if s.is_success() => Ok(()),
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
        let held = self.read_held(d).await?;
        let value = match held.staged_for(ev.job_id) {
            // A prior firing minted and wrote this packet's value; it
            // recorded `minted` and `installed` on the log before its
            // steps. Converge what is left.
            Some(v) => v.to_string(),
            None if done("install") => {
                return Err(HandlerError::Permanent(format!(
                    "this packet's install is on record, but {}.next no longer holds its value \
                     (it names {}): a later rotation replaced it. Nothing is minted; abandon \
                     this packet",
                    d.host,
                    held.next_for.as_deref().unwrap_or("no packet")
                )));
            }
            None => {
                judge_scope_naming(
                    meta_str(ev.metadata, "old_token"),
                    meta_str(ev.metadata, "old_token_last_eight"),
                    held.current.as_deref(),
                    d.host,
                )
                .map_err(HandlerError::Permanent)?;
                self.require_registry_row(d.credential_id).await?;
                let value = fresh_value();
                self.record_phase(
                    rule,
                    d.credential_id,
                    RotationPhase::Minted,
                    json!({
                        "job_id": ev.job_id,
                        "host": d.host,
                        "issuer": "credential-broker (32 random bytes, base64url)",
                        "value_length": value.len(),
                    }),
                )
                .await?;
                let (next, next_for) = (d.key("next"), minted_for_key(d.host, "next"));
                let previous = d.key("previous");
                // A value already staged may already be INSTALLED on the
                // host — the deposit installs `next` as soon as the door
                // resolves it, before any promotion — so it is carried to
                // `previous` in this same write, never dropped: the door
                // resolves `previous`, and the promotion blanks it (review
                // 3930a3eb, F1).
                let mut entries = vec![
                    (next.as_str(), value.as_str()),
                    (next_for.as_str(), ev.job_id),
                ];
                if let Some(displaced) = held.next.as_deref() {
                    entries.push((previous.as_str(), displaced));
                }
                self.secrets
                    .write_keys(d.secret_namespace, d.secret_name, &entries)
                    .await
                    .map_err(HandlerError::Downstream)?;
                self.record_phase(
                    rule,
                    d.credential_id,
                    RotationPhase::Installed,
                    json!({
                        "job_id": ev.job_id,
                        "host": d.host,
                        "secret_namespace": d.secret_namespace,
                        "secret_name": d.secret_name,
                        "secret_key": next,
                        "value_length": value.len(),
                        "last_eight": last_eight(&value),
                        "replaced_staged_for": held.next_for,
                        "carried_to_previous": held.next.as_deref().map(last_eight),
                        "delivery": DELIVERY_OFF_HOST,
                    }),
                )
                .await?;
                value
            }
        };

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
            json!({
                "job_id": ev.job_id,
                "host": d.host,
                "method": "api",
                "observed": seen.describe(),
            }),
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
        let completer = steps
            .get("delivered")
            .and_then(|s| s.completed_by.as_deref());
        if completer != Some(DEPOSIT_ACTOR) {
            return Err(HandlerError::Permanent(format!(
                "the delivered step was completed by {}, not by the host's deposit \
                 ({DEPOSIT_ACTOR}): only the deposit proves the host holds the new value, and \
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
        let held = self.read_held(d).await?;
        let plan =
            plan_promotion(&held, ev.job_id, delivered, d.host).map_err(HandlerError::Permanent)?;
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
            self.record_phase(
                rule,
                d.credential_id,
                RotationPhase::Verified,
                json!({"job_id": ev.job_id, "host": host, "method": "api", "observed": seen.describe()}),
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

        // The old value: held only by the firing that promotes. A
        // redelivery after the write judges by the slot alone.
        let old = match &plan {
            Promotion::Promote { .. } => held.current.clone(),
            Promotion::Done { .. } => None,
        };
        // A value an earlier stage carried to `previous` (F1) dies here too.
        let carried = match &plan {
            Promotion::Promote { .. } => held.previous.clone(),
            Promotion::Done { .. } => None,
        };
        if let Promotion::Promote { value } = &plan {
            let keys = [
                d.key("current"),
                minted_for_key(host, "current"),
                d.key("next"),
                minted_for_key(host, "next"),
                d.key("previous"),
            ];
            self.secrets
                .write_keys(
                    d.secret_namespace,
                    d.secret_name,
                    &[
                        (keys[0].as_str(), value.as_str()),
                        (keys[1].as_str(), ev.job_id),
                        (keys[2].as_str(), ""),
                        (keys[3].as_str(), ""),
                        (keys[4].as_str(), ""),
                    ],
                )
                .await
                .map_err(HandlerError::Downstream)?;
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
            json!({
                "job_id": ev.job_id,
                "host": host,
                "delivered_last_eight": delivered,
                "old_last_eight": old.as_deref().map(last_eight),
                "carried_last_eight": carried.as_deref().map(last_eight),
                "confirmed_dead": confirmed,
            }),
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
