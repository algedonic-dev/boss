//! `credential.rotate.github-app-installation` — the credential broker's
//! third issuer (design 76155676, David 2026-09-27; backlog 81eb6d4d).
//!
//! The root is a GitHub App installed on the `algedonic-dev`
//! organisation: its App id, installation id and private key, placed
//! ONCE by David as root material in Secret
//! `boss/boss-credential-broker-root` (keys `github-app.id`,
//! `github-app.installation-id`, `github-app.private-key.pem`) and handed
//! to the dispatcher as env, the posture of the forge and Cloudflare
//! roots. Everything a consumer holds is MINTED from it: a one-hour
//! installation token, narrowed to the repositories and permissions the
//! consumer's rule row declares, PATCHed into ONE named Secret. No
//! personal access token is minted for BOSS again.
//!
//! ## One App, many installations
//!
//! The App id and private key are the single root; an INSTALLATION is per
//! organisation — algedonic-dev today, a hosting customer's org (or one
//! that brings its own repository) later. The root's
//! `github-app.installation-id` is the default; a rule row naming
//! `installation_id` mints for that installation instead, so a second
//! organisation is rule rows, a Secret name in the broker's Role and a
//! registry row — no code and no new root key.
//!
//! ## Two firings, one declaration
//!
//! The rule row is the credential's consumer declaration — the
//! installation, the Secret, the narrowing, the repository that proves
//! it — and two rules hand the
//! handler the same args (pinned equal, CLAUDE.md §9a):
//!
//! - **Refresh (`phase = "refresh"`), on a clock.** An installation
//!   token dies an hour after its mint and GitHub offers no longer one,
//!   so the value in the Secret must be replaced before it expires or
//!   every consumer reads a dead credential. The clock rule fires every
//!   fifteen minutes; the handler reads the expiry it recorded beside the
//!   token (`<secret_key>.expires-at`) and re-mints only when fewer than
//!   `refresh_within_minutes` remain — a redelivered tick finds a fresh
//!   token and does nothing. A refresh mints, verifies, installs, and
//!   records ONE `credential.installed` event (which stamps the registry
//!   row's `rotated_at`); it revokes nothing, because the value it
//!   replaced is still what an off-host consumer reads until its next
//!   pass, and it dies on its own within the refresh window. This is the
//!   choice of a cadence over re-opening a packet per hour: a packet is
//!   the record of a decision someone made, and "an hour passed" is not
//!   one — the cadence is the routine, the packet is the exception.
//!
//! - **Rotation, on a `rotate-a-credential` packet's scope step** — the
//!   exposure path. Mint, verify by effect, revoke the value the Secret
//!   holds, install, and record each phase as the packet's own steps and
//!   a `credential.*` event.
//!
//! ## A per-act token is keyed on its request
//!
//! The org-admin token the GitHub verbs act with is minted per act, by
//! the refresh path, on the ops-request that needs it (design 76c46869).
//! Its rules name that request as `request_id`, and the refresh reads
//! and writes [`request_key`] — `<secret_key>-<request id>`, with its
//! expiry and provenance (`minted-for` = the request) beside it — rather
//! than the one `secret_key` (backlog 4ce4ec55). One key per owner
//! stranded the second of two approved writes: approval 2 found approval
//! 1's token fresh and minted nothing, write 1 revoked it, and write 2
//! had nothing to act with, its single-use approval spent. Keyed per
//! request, a sibling's fresh token never stands in for this one, and a
//! write's revoke (github-act.sh, by the runner's `OPS_REQUEST_ID`)
//! reaches its own key alone. A `request_id` that is not a lowercase
//! packet uuid, or one on a firing that is not a refresh, is refused
//! before anything is read.
//!
//! ## Why the revoke precedes the install here
//!
//! The forge handler revokes last, after the install, by the token's
//! numeric id. An installation token has no id to revoke by: GitHub ends
//! one only when it is PRESENTED (`DELETE /installation/token`,
//! authenticated by the token itself), and the broker holds the old value
//! only until it overwrites it. Revoke after the write, and a firing that
//! dies between the two loses the one way to end the old token early. So
//! the order is mint → verify the new one → revoke and confirm the old
//! one dead → write. The new token is proven before anything destructive
//! runs, which is the protocol's rule; a consumer reading the Secret in
//! the instant between the revoke and the write reads a dead value once.
//! A redelivery after a death before the write finds the old value
//! already dead (GitHub answers 401) and carries on. And just before the
//! revoke, the held value's `expires-at` is set to now, so that death
//! never leaves a dead token that reads as live: the refresh re-mints on
//! its next tick and the forge render removes the host's file.
//!
//! ## Nothing is written that was not verified
//!
//! A fresh token that cannot read `verify_repo` is never written: the
//! Secret keeps the value it held, nothing is revoked, and the useless
//! fresh token is ended on the spot. An exchange that answers anything
//! but `201 Created` mints nothing and writes nothing.
//!
//! ## Idempotence
//!
//! The Secret carries its own provenance beside the value, written in the
//! same merge-patch: `<secret_key>.minted-for` is the rotation packet's
//! id, or `refresh`. A packet firing that finds its own id there, or its
//! `install` step recorded, mints nothing and records only what is left.
//!
//! THE VALUE NEVER ENTERS A PACKET, AN EVENT OR AN ERROR: identifiers
//! (installation id, Secret path, a value's length and last eight) and
//! observed effects only.

use async_trait::async_trait;
use boss_dispatcher::rules::expr::Value;
use boss_dispatcher::rules::handler::{Handler, HandlerError, InvocationContext, arg, arg_string};
use boss_jobs::credentials::RotationPhase;
use serde_json::{Value as JsonValue, json};
use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;

use super::common::{StepEvent, dispatcher_reader_header};
use super::credential_issuer::{
    GitHubAppIssuer, InstallationScope, SecretStore, github_id, github_repo, github_repo_name,
};
use super::credential_rotate_forgejo::last_eight;

/// The handler's registered name — the `handler = "…"` of both rules.
pub const HANDLER: &str = "credential.rotate.github-app-installation";

/// The one value of the `phase` arg: the clock rule's re-mint.
pub const REFRESH_PHASE: &str = "refresh";

/// What `<secret_key>.minted-for` says when a refresh, not a packet,
/// put the value there.
pub const MINTED_BY_REFRESH: &str = "refresh";

/// The install step's `delivery`. The protocol's `delivered` step goes
/// ready only on `off-host`, and the revoke here never waits for a host:
/// it ran before the install (see the module doc).
pub const DELIVERY_NOT_AWAITED: &str = "not-awaited";

/// The Secret key holding the token's expiry, RFC 3339 — beside the
/// value, so the refresh and any consumer can tell a live value from a
/// dead one without asking GitHub.
pub fn expires_key(secret_key: &str) -> String {
    format!("{secret_key}.expires-at")
}

/// The Secret key naming what put the value there: a rotation packet's
/// id, or [`MINTED_BY_REFRESH`].
pub fn minted_for_key(secret_key: &str) -> String {
    format!("{secret_key}.minted-for")
}

// ---------------------------------------------------------------------------
// Pure planning — the decisions under test
// ---------------------------------------------------------------------------

/// What a refresh firing does with the Secret as it stands.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RefreshPlan {
    /// The installed token outlives the refresh window: nothing to do.
    Fresh { minutes_left: i64 },
    /// Re-mint, and why — the reason rides on the event.
    Mint { why: String },
}

pub fn plan_refresh(
    installed: Option<&str>,
    expires_at: Option<&str>,
    now: chrono::DateTime<chrono::Utc>,
    within_minutes: i64,
) -> RefreshPlan {
    if installed.is_none_or(|t| t.trim().is_empty()) {
        return RefreshPlan::Mint {
            why: "the Secret holds no token".into(),
        };
    }
    let Some(expires) = expires_at
        .and_then(|s| chrono::DateTime::parse_from_rfc3339(s.trim()).ok())
        .map(|t| t.with_timezone(&chrono::Utc))
    else {
        return RefreshPlan::Mint {
            why: "the Secret records no readable expiry beside its token".into(),
        };
    };
    let minutes_left = (expires - now).num_minutes();
    if minutes_left < within_minutes {
        RefreshPlan::Mint {
            why: if minutes_left <= 0 {
                format!("the installed token expired at {}", expires.to_rfc3339())
            } else {
                format!(
                    "the installed token expires in {minutes_left} minutes, inside the \
                     {within_minutes}-minute refresh window"
                )
            },
        }
    } else {
        RefreshPlan::Fresh { minutes_left }
    }
}

/// The rule's `permissions` arg: `name:level` pairs, comma-separated —
/// `contents:write,metadata:read`. A level is `read`, `write` or `admin`;
/// a name is GitHub's snake_case. Anything else is an authoring fault.
pub fn parse_permissions(raw: &str) -> Result<BTreeMap<String, String>, String> {
    let mut out = BTreeMap::new();
    for pair in raw.split(',').map(str::trim).filter(|p| !p.is_empty()) {
        let Some((name, level)) = pair.split_once(':') else {
            return Err(format!("permission {pair:?} is not name:level"));
        };
        let (name, level) = (name.trim(), level.trim());
        if name.is_empty() || !name.chars().all(|c| c.is_ascii_lowercase() || c == '_') {
            return Err(format!(
                "permission name {name:?} is not GitHub's snake_case"
            ));
        }
        if !matches!(level, "read" | "write" | "admin") {
            return Err(format!(
                "permission {name}: level {level:?} is not read, write or admin"
            ));
        }
        if out.insert(name.to_string(), level.to_string()).is_some() {
            return Err(format!("permission {name} is declared twice"));
        }
    }
    Ok(out)
}

/// The rule's `repositories` arg: repository NAMES, comma-separated.
pub fn parse_repositories(raw: &str) -> Result<Vec<String>, String> {
    raw.split(',')
        .map(str::trim)
        .filter(|r| !r.is_empty())
        .map(|r| github_repo_name(r).map(str::to_string))
        .collect()
}

/// The scope step's naming of an old token, judged against the one token
/// this handler can revoke: the value the Secret holds. An installation
/// token has no name or id at GitHub, so `old_token` names nothing; a
/// last eight must be the Secret's value's, because a token the broker
/// no longer holds cannot be presented to GitHub to end it. Either
/// mismatch is refused BEFORE anything is minted — a scoper who expects a
/// token dead must never read a packet that closed `rotated` over it.
pub fn judge_scope_naming(
    old_token: Option<&str>,
    old_last8: Option<&str>,
    installed: Option<&str>,
) -> Result<(), String> {
    if let Some(name) = old_token {
        return Err(format!(
            "the scope step names old_token {name:?}, but a GitHub installation token has no \
             name or id to revoke by — GitHub ends one only when it is presented, so the \
             broker revokes the value the Secret holds. Name it by old_token_last_eight, or \
             name none"
        ));
    }
    match (old_last8, installed) {
        (None, _) => Ok(()),
        (Some(l8), None) => Err(format!(
            "the scope step names a token ending in {l8}, but the Secret holds no token; the \
             broker can revoke only a value it holds, and nothing is minted"
        )),
        (Some(l8), Some(v)) if last_eight(v) == l8 => Ok(()),
        (Some(l8), Some(v)) => Err(format!(
            "the scope step names a token ending in {l8}, but the Secret holds one ending in \
             {}; a token replaced earlier is no longer held, cannot be presented to GitHub to \
             end it, and expires on its own within an hour of its mint. Nothing is minted",
            last_eight(v)
        )),
    }
}

// ---------------------------------------------------------------------------
// The declaration — rule-row data
// ---------------------------------------------------------------------------

/// A non-empty trimmed string field of the scope step's metadata.
fn meta_str<'a>(m: &'a serde_json::Map<String, JsonValue>, key: &str) -> Option<&'a str> {
    m.get(key)
        .and_then(JsonValue::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
}

fn optional_arg<'a>(args: &'a [(String, Value)], name: &str) -> Option<&'a str> {
    match arg(args, name) {
        Some(Value::String(s)) => Some(s.trim()).filter(|s| !s.is_empty()),
        _ => None,
    }
}

struct Declaration<'a> {
    secret_namespace: &'a str,
    secret_name: &'a str,
    credential_id: &'a str,
    verify_repo: &'a str,
    scope: InstallationScope,
    phase: Option<&'a str>,
    refresh_within_minutes: Option<i64>,
    /// The ops-request a per-act firing serves, when the rule names one.
    request_id: Option<&'a str>,
    /// The key the token is read from and written to: the rule's
    /// `secret_key`, or [`request_key`] of it when a request is named.
    slot_key: String,
}

/// The Secret key a per-act token lives under: `<secret_key>-<request
/// id>` (backlog 4ce4ec55). One key per owner stranded the second of two
/// approved GitHub writes — approval 2 found approval 1's token fresh and
/// minted nothing, write 1 revoked it, write 2 had nothing to act with —
/// so each request's token is its own, and github-act.sh renders and
/// revokes the key of the request the ops runner hands it.
pub fn request_key(secret_key: &str, request_id: &str) -> String {
    format!("{secret_key}-{request_id}")
}

/// A packet id as the jobs API writes one: a lowercase, hyphenated uuid.
/// It becomes part of a Secret key, and the forge's render refuses any
/// other shape, so this is the only one a rule may bind.
pub fn is_packet_id(s: &str) -> bool {
    s.len() == 36
        && s.char_indices().all(|(i, c)| match i {
            8 | 13 | 18 | 23 => c == '-',
            _ => c.is_ascii_digit() || ('a'..='f').contains(&c),
        })
}

impl Declaration<'_> {
    fn key(&self) -> &str {
        &self.slot_key
    }
}

impl<'a> Declaration<'a> {
    /// Every refusal here is PERMANENT: the rule row says the same thing
    /// on every redelivery, so retrying it is noise.
    fn parse(args: &'a [(String, Value)]) -> Result<Self, HandlerError> {
        let verify_repo =
            github_repo(arg_string(args, "verify_repo")?).map_err(HandlerError::Permanent)?;
        let scope = InstallationScope {
            installation: optional_arg(args, "installation_id")
                .map(|raw| github_id("rule arg installation_id", raw))
                .transpose()
                .map_err(HandlerError::Permanent)?,
            repositories: optional_arg(args, "repositories")
                .map(parse_repositories)
                .transpose()
                .map_err(HandlerError::Permanent)?
                .unwrap_or_default(),
            permissions: optional_arg(args, "permissions")
                .map(parse_permissions)
                .transpose()
                .map_err(HandlerError::Permanent)?
                .unwrap_or_default(),
        };
        let refresh_within_minutes = optional_arg(args, "refresh_within_minutes")
            .map(|raw| {
                raw.parse::<i64>()
                    .ok()
                    .filter(|m| (1..60).contains(m))
                    .ok_or_else(|| {
                        HandlerError::Permanent(format!(
                            "refresh_within_minutes = {raw:?} is not a whole number of minutes \
                             between 1 and 59 (an installation token lives 60)"
                        ))
                    })
            })
            .transpose()?;
        let phase = optional_arg(args, "phase");
        let request_id = optional_arg(args, "request_id");
        if let Some(r) = request_id {
            if !is_packet_id(r) {
                return Err(HandlerError::Permanent(format!(
                    "request_id = {r:?} is not an ops-request id (a lowercase, hyphenated \
                     uuid): it becomes part of the Secret key the forge's render reads, so \
                     nothing is read or minted"
                )));
            }
            if phase != Some(REFRESH_PHASE) {
                return Err(HandlerError::Permanent(format!(
                    "request_id = {r:?} names a request on a firing that is not a refresh: a \
                     per-act token is minted by the refresh path, and a rotation packet keys by \
                     its own declaration"
                )));
            }
        }
        let secret_key = arg_string(args, "secret_key")?;
        Ok(Self {
            secret_namespace: arg_string(args, "secret_namespace")?,
            secret_name: arg_string(args, "secret_name")?,
            credential_id: arg_string(args, "credential_id")?,
            verify_repo,
            scope,
            phase,
            refresh_within_minutes,
            request_id,
            slot_key: request_id
                .map_or_else(|| secret_key.to_string(), |r| request_key(secret_key, r)),
        })
    }
}

// ---------------------------------------------------------------------------
// The handler
// ---------------------------------------------------------------------------

pub struct CredentialRotateGitHubApp {
    client: boss_core::machine_token::Client,
    jobs_base: String,
    issuer: Arc<dyn GitHubAppIssuer>,
    secrets: Arc<dyn SecretStore>,
}

struct StepView {
    id: String,
    status: String,
}

/// What the Secret holds, read once per firing.
struct Installed {
    token: Option<String>,
    expires_at: Option<String>,
    minted_for: Option<String>,
}

/// The evidence the four steps record, from whichever firing did the work.
struct Evidence {
    issued: String,
    installed: String,
    verified: String,
    revoked: String,
    confirmed_dead: Option<String>,
}

impl CredentialRotateGitHubApp {
    pub fn new(
        jobs_base: impl Into<String>,
        issuer: Arc<dyn GitHubAppIssuer>,
        secrets: Arc<dyn SecretStore>,
    ) -> Arc<Self> {
        Arc::new(Self {
            client: super::common::api_client(),
            jobs_base: jobs_base.into(),
            issuer,
            secrets,
        })
    }

    fn jobs(&self) -> &str {
        self.jobs_base.trim_end_matches('/')
    }

    /// The installation this declaration mints for: its own, or the
    /// root's default.
    fn installation_of(&self, d: &Declaration<'_>) -> String {
        d.scope
            .installation
            .map_or_else(|| self.issuer.installation(), |i| i.to_string())
    }

    async fn read_installed(&self, d: &Declaration<'_>) -> Result<Installed, HandlerError> {
        let read = |key: String| async move {
            self.secrets
                .read_key(d.secret_namespace, d.secret_name, &key)
                .await
                .map_err(HandlerError::Downstream)
        };
        Ok(Installed {
            token: read(d.key().to_string()).await?,
            expires_at: read(expires_key(d.key())).await?,
            minted_for: read(minted_for_key(d.key())).await?,
        })
    }

    /// The token, its expiry and its provenance as ONE write.
    async fn install(
        &self,
        d: &Declaration<'_>,
        token: &str,
        expires_at: &str,
        minted_for: &str,
    ) -> Result<(), HandlerError> {
        let (ek, mk) = (expires_key(d.key()), minted_for_key(d.key()));
        self.secrets
            .write_keys(
                d.secret_namespace,
                d.secret_name,
                &[
                    (d.key(), token),
                    (ek.as_str(), expires_at),
                    (mk.as_str(), minted_for),
                ],
            )
            .await
            .map_err(HandlerError::Downstream)
    }

    /// Verify a fresh token by effect. On failure it is ended on the spot
    /// — it was never written anywhere, so nothing reads it — and the
    /// refusal says what was and was not touched.
    async fn verify_or_discard(
        &self,
        d: &Declaration<'_>,
        token: &str,
    ) -> Result<(), HandlerError> {
        let readable = self
            .issuer
            .repo_readable_with(token, d.verify_repo)
            .await
            .map_err(HandlerError::Downstream)?;
        if readable {
            return Ok(());
        }
        let discarded = match self.issuer.revoke_installation_token(token).await {
            Ok(true) => "the fresh token was revoked on the spot".to_string(),
            Ok(false) => "GitHub already refuses the fresh token".to_string(),
            Err(e) => {
                format!("revoking the fresh token failed too ({e}); it expires within the hour")
            }
        };
        Err(HandlerError::Downstream(format!(
            "verify-by-effect failed: a fresh installation token cannot read {}; it was NOT \
             written — Secret {}/{} still holds its previous value, and nothing was revoked; \
             {discarded}",
            d.verify_repo, d.secret_namespace, d.secret_name
        )))
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

    /// The credential's registry row must exist BEFORE anything is
    /// minted: every phase is recorded through the rotation door, which
    /// answers 404 for an undeclared id, and a token minted (or, on a
    /// refresh, installed) whose event cannot land is a value with no
    /// record — a later tick finds it fresh and never records it. So an
    /// undeclared row refuses up front, naming the row to declare.
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
                    },
                ))
            })
            .collect())
    }

    /// Complete one packet step through the step merge door, then its
    /// status (`common::complete_step`). A completed step is left alone
    /// — that is the redelivery path; a slug the packet lacks is skipped.
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

    /// The clock door: re-mint before the installed token expires.
    async fn refresh(
        &self,
        ctx: &InvocationContext,
        d: &Declaration<'_>,
    ) -> Result<(), HandlerError> {
        let within = d.refresh_within_minutes.ok_or_else(|| {
            HandlerError::Permanent(
                "a refresh firing names no refresh_within_minutes: without it there is no \
                 window to judge the installed token's expiry against"
                    .into(),
            )
        })?;
        let now = boss_clock_client::wall_now();
        let held = self.read_installed(d).await?;
        let why = match plan_refresh(
            held.token.as_deref(),
            held.expires_at.as_deref(),
            now,
            within,
        ) {
            RefreshPlan::Fresh { minutes_left } => {
                tracing::debug!(
                    credential_id = d.credential_id,
                    minutes_left,
                    "installation token outlives the refresh window; nothing to do"
                );
                return Ok(());
            }
            RefreshPlan::Mint { why } => why,
        };
        self.require_registry_row(d.credential_id).await?;
        let minted = self
            .issuer
            .mint_installation_token(&d.scope)
            .await
            .map_err(HandlerError::Downstream)?;
        self.verify_or_discard(d, &minted.token).await?;
        let expires = minted.expires_at.to_rfc3339();
        // Provenance: the request a per-act token was minted for, else the
        // clock's refresh.
        self.install(
            d,
            &minted.token,
            &expires,
            d.request_id.unwrap_or(MINTED_BY_REFRESH),
        )
        .await?;
        self.record_phase(
            &ctx.rule_name,
            d.credential_id,
            RotationPhase::Installed,
            json!({
                "refresh": true,
                "trigger": ctx.triggering_event_id,
                "why": why,
                "installation": self.installation_of(d),
                "expires_at": expires,
                "repositories": minted.repositories,
                "permissions": minted.permissions,
                "verify_repo": d.verify_repo,
                "verified": "the token read the repository before it was written",
                "secret_namespace": d.secret_namespace,
                "secret_name": d.secret_name,
                "secret_key": d.key(),
                "request_id": d.request_id,
                "value_length": minted.token.len(),
                "replaced": if held.token.is_some() {
                    "left to expire on its own: an off-host consumer may still read it until its next pass"
                } else {
                    "nothing"
                },
            }),
        )
        .await
    }

    /// The packet door: mint → verify → revoke the held value → install.
    async fn rotate(
        &self,
        ctx: &InvocationContext,
        d: &Declaration<'_>,
    ) -> Result<(), HandlerError> {
        if ctx.event_payload.get("job_id").is_none() {
            return Err(HandlerError::Permanent(format!(
                "{} fired without a packet and without phase = \"{REFRESH_PHASE}\"; a clock \
                 firing must say it is a refresh",
                ctx.rule_name
            )));
        }
        let ev = StepEvent::from_payload(&ctx.event_payload)?;
        let rule = ctx.rule_name.as_str();
        let steps = self.fetch_steps(ev.job_id).await?;
        let step_done = |slug: &str| steps.get(slug).is_some_and(|s| s.status == "completed");
        if ["issue", "install", "verify", "revoke"]
            .iter()
            .all(|s| step_done(s))
        {
            // A finished rotation, redelivered.
            return Ok(());
        }
        let held = self.read_installed(d).await?;
        let secret = format!(
            "k8s Secret {}/{} key {}",
            d.secret_namespace,
            d.secret_name,
            d.key()
        );
        let installation = self.installation_of(d);

        let evidence = if step_done("install") || held.minted_for.as_deref() == Some(ev.job_id) {
            // The Secret holds this packet's token, or its install is on
            // record: a prior firing did the work, and the write came
            // only after the verify and the revoke (module doc), each of
            // which it recorded on the log first. Record what is left.
            if !step_done("install") {
                self.record_phase(
                    rule,
                    d.credential_id,
                    RotationPhase::Installed,
                    json!({
                        "job_id": ev.job_id,
                        "secret_namespace": d.secret_namespace,
                        "secret_name": d.secret_name,
                        "secret_key": d.key(),
                        "value_length": held.token.as_deref().map_or(0, str::len),
                        "expires_at": held.expires_at,
                        "converged": true,
                    }),
                )
                .await?;
            }
            Evidence {
                issued: format!(
                    "GitHub App installation token minted for this packet via POST \
                     /app/installations/{installation}/access_tokens by an earlier firing \
                     (credential.minted on the log; {}.minted-for names this packet)",
                    d.key()
                ),
                installed: format!(
                    "{secret} holds this packet's token ({} bytes, expires {}; {} names this \
                     packet); the revoke does not wait for delivery — it ran before the write",
                    held.token.as_deref().map_or(0, str::len),
                    held.expires_at.as_deref().unwrap_or("unrecorded"),
                    minted_for_key(d.key())
                ),
                verified: format!(
                    "GET /repos/{} answered 200 with the new token before it was written \
                     (the broker writes only a verified token; credential.verified on the log)",
                    d.verify_repo
                ),
                revoked: "the value this packet replaced was revoked and confirmed dead before \
                          the write, or the Secret held none (credential.revoked on the log \
                          when there was one)"
                    .to_string(),
                confirmed_dead: None,
            }
        } else {
            judge_scope_naming(
                meta_str(ev.metadata, "old_token"),
                meta_str(ev.metadata, "old_token_last_eight"),
                held.token.as_deref(),
            )
            .map_err(HandlerError::Permanent)?;
            self.require_registry_row(d.credential_id).await?;

            let minted = self
                .issuer
                .mint_installation_token(&d.scope)
                .await
                .map_err(HandlerError::Downstream)?;
            let expires = minted.expires_at.to_rfc3339();
            let permissions = InstallationScope {
                permissions: minted.permissions.clone(),
                ..InstallationScope::default()
            }
            .permissions_text();
            let repositories = if minted.repositories.is_empty() {
                "every repository the installation holds".to_string()
            } else {
                minted.repositories.join(",")
            };
            self.record_phase(
                rule,
                d.credential_id,
                RotationPhase::Minted,
                json!({
                    "job_id": ev.job_id,
                    "installation": installation,
                    "expires_at": expires,
                    "repositories": minted.repositories,
                    "permissions": minted.permissions,
                }),
            )
            .await?;

            self.verify_or_discard(d, &minted.token).await?;
            self.record_phase(
                rule,
                d.credential_id,
                RotationPhase::Verified,
                json!({
                    "job_id": ev.job_id,
                    "verify_repo": d.verify_repo,
                    "method": "api",
                }),
            )
            .await?;

            // Revoke the held value BEFORE it is overwritten — the only
            // moment the broker can still present it to GitHub.
            let (revoked, confirmed_dead) = match held.token.as_deref() {
                None => (
                    "the Secret held no previous token; nothing to revoke".to_string(),
                    None,
                ),
                Some(old) => {
                    // Mark the held value not-live FIRST (adversarial
                    // review of 58b00a77): a death between the DELETE
                    // below and the install would otherwise leave a dead
                    // token under its old, live expiry — the refresh
                    // would read it as fresh for up to twenty minutes and
                    // the forge render would hand it to the push. With
                    // the expiry at now, every reader sees "not live":
                    // the next refresh mints, the render drops the file.
                    self.secrets
                        .write_key(
                            d.secret_namespace,
                            d.secret_name,
                            &expires_key(d.key()),
                            &boss_clock_client::wall_now().to_rfc3339(),
                        )
                        .await
                        .map_err(HandlerError::Downstream)?;
                    let ended_now = self
                        .issuer
                        .revoke_installation_token(old)
                        .await
                        .map_err(HandlerError::Downstream)?;
                    let dead = self
                        .issuer
                        .installation_token_is_dead(old)
                        .await
                        .map_err(HandlerError::Downstream)?;
                    if !dead {
                        return Err(HandlerError::Downstream(format!(
                            "the held token ending in {} still authenticates after DELETE \
                             /installation/token; the fresh token is NOT written over it, and \
                             expires unused within the hour",
                            last_eight(old)
                        )));
                    }
                    let confirmed = format!(
                        "GET /installation/repositories with the token ending in {} answered 401",
                        last_eight(old)
                    );
                    self.record_phase(
                        rule,
                        d.credential_id,
                        RotationPhase::Revoked,
                        json!({
                            "job_id": ev.job_id,
                            "old_token_last_eight": last_eight(old),
                            "deleted_now": ended_now,
                            "confirmed_dead": confirmed,
                        }),
                    )
                    .await?;
                    (
                        if ended_now {
                            format!(
                                "installation token ending in {} (the value the Secret held) \
                                 revoked via DELETE /installation/token",
                                last_eight(old)
                            )
                        } else {
                            format!(
                                "installation token ending in {} (the value the Secret held) \
                                 was already dead — expired or revoked before this firing",
                                last_eight(old)
                            )
                        },
                        Some(confirmed),
                    )
                }
            };

            self.install(d, &minted.token, &expires, ev.job_id).await?;
            self.record_phase(
                rule,
                d.credential_id,
                RotationPhase::Installed,
                json!({
                    "job_id": ev.job_id,
                    "secret_namespace": d.secret_namespace,
                    "secret_name": d.secret_name,
                    "secret_key": d.key(),
                    "value_length": minted.token.len(),
                    "expires_at": expires,
                }),
            )
            .await?;
            Evidence {
                issued: format!(
                    "GitHub App installation token minted via POST \
                     /app/installations/{installation}/access_tokens, expires {expires}; \
                     repositories {repositories}; permissions {permissions}"
                ),
                installed: format!(
                    "{secret} updated ({} bytes) with {} and {}; the revoke does not wait for \
                     delivery — it ran before the write, the one moment the old value could \
                     still be presented to GitHub",
                    minted.token.len(),
                    expires_key(d.key()),
                    minted_for_key(d.key())
                ),
                verified: format!(
                    "GET /repos/{} answered 200 with the new token before it was written",
                    d.verify_repo
                ),
                revoked,
                confirmed_dead,
            }
        };

        self.complete_step(
            rule,
            ev.job_id,
            &steps,
            "issue",
            &[
                ("issued", evidence.issued),
                (
                    "issuer",
                    format!(
                        "credential-broker ({rule}), root: the GitHub App in \
                         boss-credential-broker-root (github-app.id, github-app.installation-id, \
                         github-app.private-key.pem)"
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
                ("installed", evidence.installed),
                ("permissions", d.scope.permissions_text()),
                ("delivery", DELIVERY_NOT_AWAITED.to_string()),
            ],
        )
        .await?;
        self.complete_step(
            rule,
            ev.job_id,
            &steps,
            "verify",
            &[
                ("verified", evidence.verified),
                ("method", "api".to_string()),
            ],
        )
        .await?;
        let mut revoke = vec![("revoked", evidence.revoked)];
        if let Some(c) = evidence.confirmed_dead {
            revoke.push(("confirmed_dead", c));
        }
        self.complete_step(rule, ev.job_id, &steps, "revoke", &revoke)
            .await
    }
}

#[async_trait]
impl Handler for CredentialRotateGitHubApp {
    fn name(&self) -> &'static str {
        HANDLER
    }

    async fn invoke(
        &self,
        args: &[(String, Value)],
        ctx: &InvocationContext,
    ) -> Result<(), HandlerError> {
        let d = Declaration::parse(args)?;
        match d.phase {
            None => self.rotate(ctx, &d).await,
            Some(REFRESH_PHASE) => self.refresh(ctx, &d).await,
            Some(other) => Err(HandlerError::Permanent(format!(
                "phase = {other:?} is not one this handler runs: the clock rule passes \
                 \"{REFRESH_PHASE}\", a rotation packet's scope step passes none"
            ))),
        }
    }
}

#[cfg(test)]
mod tests;
