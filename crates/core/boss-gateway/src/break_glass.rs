//! Break-glass — the emergency door is a hardware key you hold.
//!
//! Design: docs/design/break-glass-is-a-key-you-hold.md, Q1-Q6
//! resolved on packet e9703d8f (2026-09-03). The gateway is the
//! break-glass *verifier*: a minimal WebAuthn relying party whose
//! whole credential store is public material in an in-tree ConfigMap
//! (`infra/cluster/manifests/boss-break-glass-credentials.yaml`).
//! A verified assertion mints a `boss_session` carrying the NARROW
//! `break-glass` role (Q4) — deploy rollback, merge approval, auth
//! administration; never platform-admin.
//!
//! What each resolved question shaped here:
//!
//! - **Q1 — any attested roaming authenticator.** Registration asks
//!   the browser for `attestation: "direct"` and the finish handler
//!   refuses any credential whose verified attestation is not a
//!   certificate-chain form (`Basic` / `AttCa` / `AnonCa`). No AAGUID
//!   allowlist. Honesty about the limit: webauthn-rs verifies the
//!   attestation *statement* (signature over authData + clientData,
//!   per format), but without a curated FIDO-MDS root store the chain
//!   is not pinned to known vendor roots — a sophisticated attacker
//!   could self-sign a plausible chain. What the check DOES exclude
//!   is every real synced-passkey provider (iCloud Keychain, Google
//!   Password Manager, browser-software keys), which attest `none`,
//!   and any credential flagged backup-eligible (the BE flag every
//!   synced passkey sets). Device-bound is enforced as far as the
//!   crate allows; the residual gap is documented, not papered over.
//! - **Q2 — both keys enrolled upfront.** Two records, labels
//!   `primary` and `backup`, either sufficient to assert.
//! - **Q3 — browser ceremony only.** The self-contained ceremony page
//!   at `/break-glass` is served from a constant in this binary — no
//!   SPA bundle, no upstream service, nothing that can be down when
//!   the gateway itself is up. Terminal cases are covered by the PIV
//!   and sk-SSH applets on the same physical key, outside this module.
//! - **Q5 — credential records in an in-tree ConfigMap.** Public keys
//!   are public. Enrollment does NOT write the ConfigMap: an
//!   imperative patch to a converge-managed object is a change with
//!   an expiry (the converge loop reapplies the in-tree manifest and
//!   would silently erase enrolled keys). Instead the finish handler
//!   EMITS the complete record — response body and log line — and a
//!   car commits it to the manifest. Under a presence authorisation the
//!   record is also written WHOLE onto the packet's `enrol` step
//!   ([`record_spend`]), and that step is the durable copy the commit
//!   car reads: the body lives in one browser tab and the log line in a
//!   pod the next converge replaces (backlog 4a173252 — on 2026-09-28
//!   both records were copied by hand from `kubectl logs`, because the
//!   step kept every field but `public_key` and `sign_count`). Every
//!   field is public, which is what makes a jobs-API step a fit home
//!   for it. An enrolment under a break-glass session has no packet,
//!   so its record still lives only in the body and the log. The
//!   kubelet propagates the ConfigMap update into the mount without a
//!   restart, and this module re-reads the directory on every
//!   ceremony. The tree stays
//!   the source of truth, which is the entire point of Q5.
//! - **Q6 — one train of soak.** `credentials.toml` and
//!   `POST /api/auth/login` are untouched; this RP lands alongside
//!   them. The PVC retirement is the follow-up car after prod proof.
//!
//! Enrollment gating (design 03451237, David 2026-09-22, all four
//! questions accepted as proposed): ONLY an already-authenticated
//! break-glass session (rotation under the emergency session), or
//! PRESENCE — a `break-glass-enrolment` packet whose `authorise` step
//! an employee on this gateway's named list (`BOSS_BREAK_GLASS_
//! AUTHORISERS`, never a role) approved with a passkey over its current
//! shape, naming the label and THIS door's relying party, touched from
//! that same employee's session, and not yet spent. The enrolment
//! writes the credential it made onto the packet's `enrol` step before
//! the record is handed out, so each authorisation enrols one key and a
//! replay is refused naming the credential it was spent on.
//!
//! The bootstrap token and its zero-credentials window RETIRED with
//! that (Q4). The window existed only to bound a shared secret, and it
//! is what turned the repair of backlog 1c4c100a — two keys enrolled
//! under `playground.algedonic.dev` while the door answers as
//! `boss.algedonic.dev` — into "empty the store, then re-enrol", with
//! no emergency door at all in between. Independence is a property of
//! the ASSERT path, which touches no other service; enrolment is an
//! administrative act done while things work, so it may read the jobs
//! API. A software passkey may AUTHORISE an enrolment and may never BE
//! the emergency credential: Q1's refusal below is unchanged.
//!
//! Each record names the relying party it was enrolled under
//! (`rp_id`), because a WebAuthn credential is bound to it by the
//! AUTHENTICATOR and nothing here can widen that. A record bound to
//! another party is never offered at the assertion — the door says
//! which keys and which party instead — and the in-tree pin
//! `the_break_glass_records_answer_the_door_they_serve` holds the
//! committed records to the manifest's `BOSS_PUBLIC_URL`, so the next
//! cutover is refused at its gate rather than found in an emergency.
//!
//! Sign counters, honestly: the durable counter is the `sign_count`
//! committed in the ConfigMap; this process keeps a monotonic
//! in-memory high-water mark on top of it, and webauthn-rs rejects
//! any assertion whose counter does not advance past the value the
//! credential carried at challenge time. Within a process lifetime
//! clone detection is therefore real; across a gateway restart the
//! floor falls back to the committed value, so a clone used only
//! between restarts could evade it until the operator refreshes
//! `sign_count` in the manifest (each successful assertion logs the
//! new value for exactly that purpose).
//!
//! Pending ceremony state lives in-process (single-use, five-minute
//! TTL). Deliberate: the break-glass verifier must not depend on any
//! other service being alive — the design's whole table of levers is
//! about which verifiers survive which outages — and the gateway
//! deploys single-replica (`strategy: Recreate`), so there is no
//! second instance for a ceremony to land on.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use async_trait::async_trait;
use axum::Json;
use axum::extract::State;
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::response::{Html, IntoResponse, Response};
use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use boss_core::job::{Assurance, Step, StepStatus};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::sync::Arc;
use webauthn_rs::prelude::{
    AttestationMetadata, AuthenticatorAttachment, PublicKeyCredential, RegisterPublicKeyCredential,
    RequestChallengeResponse, SecurityKey, SecurityKeyAuthentication, SecurityKeyRegistration, Url,
    Uuid, Webauthn, WebauthnBuilder,
};
use webauthn_rs_core::proto::AttestationConveyancePreference;

use crate::session::{self, Session};

/// The break-glass session's actor name. A constant, like
/// [`crate::local_auth::GUEST_EMAIL`]: nothing the caller sends
/// decides who a break-glass session is, and an actor in the audit
/// log should be a name you can look up — the credential manifest
/// documents who holds the keys.
pub const BREAK_GLASS_ACTOR: &str = "break-glass-operator";

/// One hour, not the normal session's 24: an emergency session is for
/// the emergency, and renewing it costs one touch of the key.
pub const BREAK_GLASS_TTL_SECONDS: u64 = 60 * 60;

/// How long a begun ceremony may take before its challenge expires.
/// Generous because a PIN + touch on a safe-stored backup key is not
/// a two-second interaction; still bounded because a pending
/// challenge is server state.
const CEREMONY_TTL: Duration = Duration::from_secs(300);

// --------------------------------------------------------------------
// The credential record — public material only (Q5).
// --------------------------------------------------------------------

/// Which physical key a record belongs to (Q2: both enrolled
/// upfront — primary carried, backup in a safe).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum CredentialLabel {
    Primary,
    Backup,
}

impl CredentialLabel {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Primary => "primary",
            Self::Backup => "backup",
        }
    }
}

/// One enrolled hardware credential. Every field is public by
/// construction — a leaked record is a leaked public key.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct BreakGlassCredential {
    /// base64url (no padding) of the raw credential id.
    pub credential_id: String,
    /// base64url (no padding) of the serialized webauthn-rs
    /// [`SecurityKey`] — public key, verified attestation, flags.
    /// Same encoding the presence-passkey rows use.
    pub public_key: String,
    /// The committed sign-counter floor. Refresh it from the value
    /// each successful assertion logs; see the module docs for what
    /// the floor does and does not guarantee across restarts.
    pub sign_count: u32,
    /// The authenticator model's AAGUID as reported by its verified
    /// attestation. All-zero for FIDO-U2F-era keys, whose attestation
    /// format predates AAGUID conveyance. Recorded for the operator's
    /// inventory — Q1 decided against gating on it.
    pub aaguid: String,
    pub enrolled_at: DateTime<Utc>,
    pub label: CredentialLabel,
    /// The relying-party id the credential was registered under — the
    /// host of `BOSS_PUBLIC_URL` at enrolment. The authenticator will
    /// assert for no other, so this is what a door compares itself to
    /// (backlog 1c4c100a: two keys enrolled under
    /// `playground.algedonic.dev` stopped opening a door that answers as
    /// `boss.algedonic.dev`, and nothing noticed). `None` only on a
    /// record committed before the field existed, which reads as
    /// UNRECORDED — never as "this door".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rp_id: Option<String>,
}

/// Build the record a finished enrolment emits — pure, so the fields a
/// committed record carries are pinned without an authenticator.
pub fn new_record(
    sk_value: &Value,
    aaguid: String,
    label: CredentialLabel,
    door: &Door,
    enrolled_at: DateTime<Utc>,
) -> BreakGlassCredential {
    BreakGlassCredential {
        credential_id: sk_value["cred"]["cred_id"]
            .as_str()
            .unwrap_or_default()
            .to_string(),
        public_key: URL_SAFE_NO_PAD.encode(serde_json::to_vec(sk_value).unwrap_or_default()),
        sign_count: sk_value["cred"]["counter"].as_u64().unwrap_or(0) as u32,
        aaguid,
        enrolled_at,
        label,
        rp_id: Some(door.rp_id.clone()),
    }
}

/// Which door a record can open, read off the record alone.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Binding {
    /// Enrolled under this door's relying-party id.
    ThisDoor,
    /// Enrolled under another — the authenticator will refuse to assert
    /// here, whatever this process does.
    Elsewhere(String),
    /// Committed before records named their relying party. Offered,
    /// because it may be good, and reported as unknown, because nothing
    /// says it is.
    Unrecorded,
}

pub fn binding(record: &BreakGlassCredential, door_rp_id: &str) -> Binding {
    match record.rp_id.as_deref() {
        None => Binding::Unrecorded,
        Some(rp) if rp == door_rp_id => Binding::ThisDoor,
        Some(rp) => Binding::Elsewhere(rp.to_string()),
    }
}

/// The records bound to another relying party, as `label → rp_id`
/// words, for the refusal and the boot log. Empty when none are.
fn foreign_records(records: &[BreakGlassCredential], door_rp_id: &str) -> Vec<String> {
    records
        .iter()
        .filter_map(|r| match binding(r, door_rp_id) {
            Binding::Elsewhere(rp) => Some(format!("{} → {rp}", r.label.as_str())),
            _ => None,
        })
        .collect()
}

/// Load every credential record from `dir` (the mounted ConfigMap:
/// one `<label>.json` file per key). A missing directory is an empty
/// store — that is the bootstrap state, not an error.
///
/// A malformed record is skipped LOUDLY (error log naming the file)
/// rather than failing the whole load: refusing to run the ceremony
/// because the backup record has a typo, while the primary record is
/// intact, would brick the one door this module exists to keep open.
pub fn load_store(dir: &Path) -> anyhow::Result<Vec<BreakGlassCredential>> {
    if !dir.exists() {
        return Ok(Vec::new());
    }
    let mut out = Vec::new();
    let mut entries: Vec<PathBuf> = std::fs::read_dir(dir)?
        .filter_map(|e| e.ok().map(|e| e.path()))
        .collect();
    entries.sort();
    for path in entries {
        let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
            continue;
        };
        // ConfigMap mounts carry `..data` symlink machinery; skip
        // anything hidden and anything that is not a .json record.
        if name.starts_with('.') || !name.ends_with(".json") || !path.is_file() {
            continue;
        }
        let parsed = std::fs::read_to_string(&path)
            .map_err(anyhow::Error::from)
            .and_then(|raw| {
                serde_json::from_str::<BreakGlassCredential>(&raw).map_err(anyhow::Error::from)
            });
        match parsed {
            Ok(rec) => out.push(rec),
            Err(e) => {
                tracing::error!(
                    file = %path.display(),
                    error = %e,
                    "break-glass credential record unreadable — SKIPPED; the \
                     emergency door is narrower than the manifest intends"
                );
            }
        }
    }
    Ok(out)
}

// --------------------------------------------------------------------
// Pure ceremony rules — the testable core.
// --------------------------------------------------------------------

/// The protocol an enrolment authorisation runs under, and its two
/// steps by `spec_slug` (`infra/platform/workflows/break-glass-
/// enrolment.toml`, pinned by `platform_bundle_break_glass_enrolment`).
pub const AUTHORISATION_KIND: &str = "break-glass-enrolment";
pub const AUTHORISE_STEP: &str = "authorise";
pub const ENROL_STEP: &str = "enrol";

/// The env var naming who may authorise an enrolment: employee ids,
/// comma-separated (Q2 — a named list, in-tree in the manifest, changed
/// by a car someone reviews; never a role, which is registry data
/// whoever can write a policy row could widen). Unset or empty is
/// NOBODY.
pub const AUTHORISERS_ENV: &str = "BOSS_BREAK_GLASS_AUTHORISERS";

/// [`AUTHORISERS_ENV`]'s value as a list: comma-separated, trimmed,
/// blanks dropped. An empty value is an empty list — nobody.
pub fn parse_authorisers(raw: &str) -> Vec<String> {
    raw.split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(String::from)
        .collect()
}

/// The identity this door presents to an authenticator: the relying-
/// party id and the origin, both from `BOSS_PUBLIC_URL`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Door {
    pub rp_id: String,
    /// `scheme://host[:port]`, no trailing slash — the form a browser
    /// writes into clientDataJSON.
    pub origin: String,
}

impl Door {
    pub fn of(origin: &Url) -> anyhow::Result<Self> {
        Ok(Self {
            rp_id: origin
                .host_str()
                .ok_or_else(|| anyhow::anyhow!("BOSS_PUBLIC_URL has no host"))?
                .to_string(),
            origin: origin.origin().ascii_serialization(),
        })
    }
}

/// Whether a session enrols by being the emergency session itself —
/// rotation under a break-glass assertion. No other ROLE enrols, not
/// even platform-admin: an employee enrols only through a presence-
/// signed packet ([`judge_authorisation`]).
pub fn session_may_enroll(session_role: Option<&str>) -> bool {
    session_role == Some(boss_core::roles::BREAK_GLASS_ROLE)
}

/// A presence authorisation, judged sufficient for ONE enrolment.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Authorised {
    pub job_id: Uuid,
    /// The step the spend is written to.
    pub enrol_step_id: Uuid,
    /// The key the signature named — the enrolment's label is this, not
    /// whatever the page sends.
    pub label: CredentialLabel,
    /// The employee whose passkey signed it.
    pub authorised_by: String,
    /// The `enrol` step's metadata as judged. The spend is written back
    /// as this plus the credential, in ONE completing PUT, and that PUT
    /// refuses a body omitting a stored key — so a step written between
    /// the judgement and the spend refuses the spend (review F2).
    pub enrol_metadata: Value,
}

/// Who authorised an enrollment.
#[derive(Debug, PartialEq, Eq)]
pub enum EnrollAuthz {
    /// An already-authenticated break-glass session.
    BreakGlassSession,
    /// A presence-signed `break-glass-enrolment` packet.
    Presence(Authorised),
}

/// JUDGE ONE AUTHORISATION PACKET, as the jobs API returned it. Pure,
/// so every refusal is pinned by a test; the handlers read the packet
/// and call this again at finish, under the enrolment lock, so what is
/// spent is what was judged.
///
/// Authorises only when all of these hold, and otherwise refuses
/// naming the one that did not:
/// - the packet runs [`AUTHORISATION_KIND`];
/// - its [`AUTHORISE_STEP`] is completed with `decision = approved`;
/// - that step carries a LIVE presence stamp — never voided, over its
///   current shape, read with the one rule (`Step::live_stamps`, design
///   87329a13) — by an employee on `authorisers` (Q1, Q2);
/// - the enrolment is made from that same employee's session: the
///   packet id is on the board, so without this anyone who saw an
///   approved packet could spend it on a key of their own;
/// - the step's `label` is primary or backup, and its `rp_id` and
///   `origin` are this door's — a signature for another deployment
///   enrols nothing here (the incident, 1c4c100a);
/// - its [`ENROL_STEP`] is neither completed nor carrying a
///   `credential_id` (Q3, single-use by record).
pub fn judge_authorisation(
    job: &Value,
    door: &Door,
    authorisers: &[String],
    caller_employee: Option<&str>,
) -> Result<Authorised, String> {
    let job_id = job["id"].as_str().unwrap_or_default();
    let kind = job["kind"].as_str().unwrap_or_default();
    if kind != AUTHORISATION_KIND {
        return Err(format!(
            "packet {job_id} is a `{kind}`, not a `{AUTHORISATION_KIND}` — only that \
             protocol authorises a break-glass enrolment"
        ));
    }
    // Review F1: only a packet still in flight authorises. A cancelled
    // one keeps its ready `enrol` step, which would otherwise spend.
    let status = job["status"].as_str().unwrap_or("unknown");
    if status != "open" {
        return Err(format!(
            "packet {job_id} is {status}, and only an open packet authorises an enrolment"
        ));
    }
    let job_uuid = Uuid::parse_str(job_id).map_err(|_| "the packet carries no id".to_string())?;
    let step = |slug: &str| -> Result<Step, String> {
        let raw = job["steps"]
            .as_array()
            .and_then(|s| s.iter().find(|s| s["spec_slug"] == slug))
            .ok_or_else(|| format!("packet {job_id} has no `{slug}` step"))?;
        serde_json::from_value::<Step>(raw.clone())
            .map_err(|_| format!("packet {job_id}'s `{slug}` step is unreadable"))
    };
    let authorise = step(AUTHORISE_STEP)?;
    if authorise.status != StepStatus::Completed {
        return Err(format!(
            "the authorisation is not approved yet: packet {job_id}'s `{AUTHORISE_STEP}` \
             step is {:?}",
            authorise.status
        )
        .to_lowercase());
    }
    let decision = authorise.metadata["decision"].as_str().unwrap_or("none");
    if decision != "approved" {
        return Err(format!(
            "the authorisation was not approved: packet {job_id}'s decision is `{decision}`"
        ));
    }
    if authorisers.is_empty() {
        return Err(format!(
            "no authoriser is configured on this gateway ({AUTHORISERS_ENV} is empty), so \
             nobody may authorise a break-glass enrolment"
        ));
    }
    let signers: Vec<&str> = authorise
        .live_stamps()
        .filter(|st| st.assurance == Assurance::Presence)
        .map(|st| st.authority_id.as_str())
        .collect();
    if signers.is_empty() {
        return Err(format!(
            "packet {job_id}'s `{AUTHORISE_STEP}` step carries no live presence stamp — a \
             session sign-off, or a passkey signature over content that has since changed, \
             authorises nothing"
        ));
    }
    let Some(signer) = signers
        .iter()
        .copied()
        .find(|s| authorisers.iter().any(|a| a == s))
    else {
        return Err(format!(
            "packet {job_id} was signed by {}, none of whom is on this gateway's authoriser \
             list ({})",
            signers.join(", "),
            authorisers.join(", ")
        ));
    };
    match caller_employee {
        Some(caller) if caller == signer => {}
        Some(caller) => {
            return Err(format!(
                "packet {job_id} was signed by {signer}; its key is enrolled from \
                 {signer}'s own session, not {caller}'s"
            ));
        }
        None => {
            return Err(format!(
                "packet {job_id} was signed by {signer}; its key is enrolled from \
                 {signer}'s own session, and this request carries none"
            ));
        }
    }
    let label: CredentialLabel = serde_json::from_value(authorise.metadata["label"].clone())
        .map_err(|_| {
            format!(
                "packet {job_id} names label {}, and a break-glass key is `primary` or \
                 `backup`",
                authorise.metadata["label"]
            )
        })?;
    let named = |key: &str| authorise.metadata[key].as_str().unwrap_or("").to_string();
    let (rp_id, origin) = (named("rp_id"), named("origin"));
    if rp_id != door.rp_id {
        return Err(format!(
            "the authorisation is for relying party `{rp_id}`; this door answers as `{}` — a \
             key enrolled for one cannot open the other",
            door.rp_id
        ));
    }
    if origin != door.origin {
        return Err(format!(
            "the authorisation is for origin `{origin}`; this door is `{}`",
            door.origin
        ));
    }
    let enrol = step(ENROL_STEP)?;
    if let Some(spent_on) = enrol.metadata["credential_id"].as_str() {
        return Err(format!(
            "this authorisation was already spent on credential {spent_on} at {} — file a \
             new `{AUTHORISATION_KIND}` packet for another enrolment",
            enrol.metadata["enrolled_at"]
                .as_str()
                .unwrap_or("an unrecorded time")
        ));
    }
    if enrol.status == StepStatus::Completed || enrol.status == StepStatus::Skipped {
        return Err(format!(
            "packet {job_id}'s `{ENROL_STEP}` step is already closed — file a new \
             `{AUTHORISATION_KIND}` packet"
        ));
    }
    Ok(Authorised {
        job_id: job_uuid,
        enrol_step_id: *enrol.id.inner().as_uuid(),
        label,
        authorised_by: signer.to_string(),
        enrol_metadata: enrol.metadata.clone(),
    })
}

/// The jobs API as the enrolment ceremony uses it: read an
/// authorisation packet, and write the spend onto its `enrol` step. A
/// port so the handlers are tested against memory, and the HTTP adapter
/// is [`JobsApiAuthorisations`].
#[async_trait]
pub trait EnrolmentAuthorisations: Send + Sync {
    /// The packet, read whole (full steps, stamps included).
    async fn packet(&self, job_id: Uuid) -> Result<Value, String>;
    /// Complete the `enrol` step carrying `metadata` — the step's whole
    /// metadata, spend included — in ONE write. The completion is the
    /// single-use record: a completed step refuses every later metadata
    /// write, so nothing can null the credential back out and re-arm the
    /// packet (review F2). Any refusal is an `Err`, and fatal.
    async fn complete_enrol(
        &self,
        job_id: Uuid,
        enrol_step_id: Uuid,
        metadata: &Value,
    ) -> Result<(), String>;
}

/// Write the spend for `record` onto the authorisation's `enrol` step.
/// Called BEFORE the record is handed out: a record whose spend the
/// jobs API refused would leave the authorisation reusable, so the
/// enrolment fails instead (the credential made on the key is then an
/// orphan nothing commits). The spend rides on every key the step held
/// when it was judged, because the step PUT refuses a body that omits
/// one — which also refuses a spend onto a step written since.
///
/// The spend is the WHOLE record, serialized exactly as the manifest
/// holds it (backlog 4a173252): the step is the durable copy the commit
/// car reads, because the 201 body lives in one browser tab and the log
/// line in a pod the next converge replaces. It wrote five keys until
/// 2026-09-28, leaving out `public_key` and `sign_count` — the two a
/// committed record verifies with — so both records enrolled that day
/// were copied by hand from `kubectl logs`. Every field is public (Q5);
/// `the_enrol_step_carries_every_field_of_the_record` names them.
pub async fn record_spend(
    port: &dyn EnrolmentAuthorisations,
    authorised: &Authorised,
    record: &BreakGlassCredential,
) -> Result<(), String> {
    let mut metadata = authorised
        .enrol_metadata
        .as_object()
        .cloned()
        .unwrap_or_default();
    let whole = serde_json::to_value(record)
        .map_err(|e| format!("the enrolled record did not serialize ({e})"))?;
    if let Value::Object(fields) = whole {
        metadata.extend(fields);
    }
    port.complete_enrol(
        authorised.job_id,
        authorised.enrol_step_id,
        &Value::Object(metadata),
    )
    .await
}

/// How long the gateway waits on the jobs API during an enrolment. An
/// enrolment is administrative and may fail; it must never hang a
/// ceremony page, nor hold the enrolment lock indefinitely (review F4).
pub const JOBS_TIMEOUT: Duration = Duration::from_secs(10);

/// The HTTP adapter: the gateway reads and writes as its own service
/// identity ([`crate::passkey::sign_as_gateway`]) — the packet's
/// authority is the stamp on it, not who fetched it. Because that read
/// is privileged, `BreakGlassState::authorise` refuses anyone who is not
/// an authoriser BEFORE it is made (review F3).
pub struct JobsApiAuthorisations {
    pub http: crate::machine_client::MachineClient,
    pub jobs_base: String,
}

#[async_trait]
impl EnrolmentAuthorisations for JobsApiAuthorisations {
    async fn packet(&self, job_id: Uuid) -> Result<Value, String> {
        let url = format!("{}/api/jobs/{job_id}", self.jobs_base);
        // Fixed text on failure: reqwest's errors name the internal URL.
        let resp = crate::passkey::sign_as_gateway(self.http.get(url))
            .send()
            .await
            .map_err(|_| "jobs unreachable — the authorisation cannot be read".to_string())?;
        match resp.status() {
            s if s.is_success() => resp
                .json()
                .await
                .map_err(|_| format!("packet {job_id} is malformed")),
            reqwest::StatusCode::NOT_FOUND => Err(format!("no packet {job_id}")),
            s => Err(format!("reading packet {job_id} answered {s}")),
        }
    }

    async fn complete_enrol(
        &self,
        job_id: Uuid,
        enrol_step_id: Uuid,
        metadata: &Value,
    ) -> Result<(), String> {
        let step = format!("{}/api/jobs/{job_id}/steps/{enrol_step_id}", self.jobs_base);
        // ONE write: the status and the whole metadata together (review
        // F2). It was a merge then a close, and a refused close only
        // logged — leaving a credential on a still-open step that any
        // step writer could null out to re-arm the packet.
        let resp = crate::passkey::sign_as_gateway(self.http.put(step))
            .json(&json!({ "status": "completed", "metadata": metadata }))
            .send()
            .await
            .map_err(|_| "jobs unreachable — the spend was not recorded".to_string())?;
        if resp.status().is_success() {
            Ok(())
        } else {
            Err(format!(
                "completing packet {job_id}'s `{ENROL_STEP}` step answered {}",
                resp.status()
            ))
        }
    }
}

/// Q1's enforcement, applied to the serialized [`SecurityKey`] the
/// finish handler produced (the same serde seam passkey.rs uses).
/// Fail-closed: anything unrecognized is refused.
///
/// Accepted: a verified certificate-chain attestation (`Basic`,
/// `AttCa`, `AnonCa`) on a credential that is NOT backup-eligible.
/// Refused: `none` (every synced/software passkey), `Self_`
/// (surrogate self-signature — proves possession of the credential
/// key, not of vendor hardware), `Uncertain`, `ECDAA`, and any
/// backup-eligible credential regardless of its attestation.
pub fn require_attested_hardware(sk: &Value) -> Result<(), String> {
    let cred = &sk["cred"];
    if !cred.is_object() {
        return Err("credential serialization unrecognized — refusing".into());
    }
    if cred["backup_eligible"] != Value::Bool(false) {
        return Err(
            "credential is backup-eligible: the private key can leave the \
             hardware, so this is a synced passkey, not a device-bound key"
                .into(),
        );
    }
    let data = &cred["attestation"]["data"];
    let chain_form = data
        .as_object()
        .and_then(|o| (o.len() == 1).then(|| o.keys().next().cloned()).flatten())
        .filter(|k| matches!(k.as_str(), "Basic" | "AttCa" | "AnonCa"));
    match chain_form {
        Some(_) => Ok(()),
        None => Err(format!(
            "attestation required: this authenticator provided {} — a synced or \
             software passkey cannot hold the break-glass key",
            match data {
                Value::String(s) => format!("'{s}'"),
                other => other.to_string(),
            }
        )),
    }
}

/// The counter floor a credential carries into an authentication:
/// the committed manifest value, the counter inside the serialized
/// credential, and this process's high-water mark — whichever is
/// highest. webauthn-rs then refuses any assertion that does not
/// advance past it.
pub fn effective_counter(record: u32, serialized: u32, seen_this_process: Option<u32>) -> u32 {
    record.max(serialized).max(seen_this_process.unwrap_or(0))
}

/// The WebAuthn clone-detection rule, stated once: when either side
/// has a nonzero counter, a reported counter that fails to advance
/// signals a possible cloned key. (Counter-less authenticators
/// report zero forever and carry no signal.) webauthn-rs enforces
/// this inside `finish_securitykey_authentication`; this function is
/// the module's statement of the contract, pinned by tests.
pub fn counter_regressed(stored: u32, reported: u32) -> bool {
    (stored > 0 || reported > 0) && reported <= stored
}

/// Mint the narrow break-glass session (Q4). No employee id — the
/// authority is the role, and resolving an employee would make the
/// emergency door depend on boss-people being up, which is exactly
/// the dependency the design forbids the verifier to have.
pub fn mint_session(session_key: &[u8]) -> (String, Session) {
    let mut sess = Session::new(BREAK_GLASS_ACTOR, BREAK_GLASS_TTL_SECONDS);
    sess.role = Some(boss_core::roles::BREAK_GLASS_ROLE.to_string());
    let cookie_value = sess.encode(session_key);
    let set_cookie = session::set_cookie(
        session::COOKIE_NAME,
        &cookie_value,
        BREAK_GLASS_TTL_SECONDS,
        "/",
    );
    (set_cookie, sess)
}

// --------------------------------------------------------------------
// Runtime state.
// --------------------------------------------------------------------

struct Pending<T> {
    state: T,
    expires: Instant,
}

/// A begun registration, and the authorisation packet it was begun
/// under (`None` under a break-glass session): the finish must name the
/// same one, so a second approved packet cannot be swapped in between.
struct BegunRegistration {
    state: SecurityKeyRegistration,
    authorisation: Option<Uuid>,
}

pub struct BreakGlassState {
    pub session_key: Vec<u8>,
    pub webauthn: Webauthn,
    /// The relying party and origin this door presents — what each
    /// record's `rp_id` and each authorisation's intent are held to.
    pub door: Door,
    /// The mounted ConfigMap directory. Read on every ceremony so a
    /// committed record propagates without a restart.
    pub dir: PathBuf,
    /// Employee ids whose passkey may authorise an enrolment (Q2).
    pub authorisers: Vec<String>,
    /// Where authorisation packets are read and spent.
    pub authorisations: Arc<dyn EnrolmentAuthorisations>,
    pub audit: crate::audit::AuthAudit,
    /// Held across a finish's re-judge, spend and emit, so two finishes
    /// racing one authorisation cannot both pass the single-use check.
    /// In-process is enough: the gateway deploys single-replica
    /// (`strategy: Recreate`), the same fact the pending maps rely on.
    enrol_lock: tokio::sync::Mutex<()>,
    pending_reg: Mutex<HashMap<String, Pending<BegunRegistration>>>,
    pending_auth: Mutex<HashMap<String, Pending<SecurityKeyAuthentication>>>,
    /// Per-credential sign-counter high-water marks for this process
    /// lifetime, keyed by base64url credential id.
    counters: Mutex<HashMap<String, u32>>,
}

/// The break-glass relying party, built identically for production
/// (`from_env`) and tests so the two cannot drift (CLAUDE.md §9a).
///
/// Presence-only (`danger_set_user_presence_only_security_keys`) BY
/// DESIGN: the door is "a touch on an attested, device-bound key"
/// (docs/design/break-glass-is-a-key-you-hold.md), never a PIN. It
/// also drops webauthn-rs's default SecurityKey `credProtect:
/// UserVerificationRequired` extension, which the browser rejects as
/// incongruent with the flow's `userVerification: preferred` BEFORE
/// the touch (a `NotSupportedError` at options validation) — the bug
/// that blocked the first real enrollment. Device-binding is
/// unaffected: attestation stays `direct` (raised in `enroll_begin`)
/// and the finish handler still refuses any backup-eligible / synced
/// credential. Only the PIN prompt goes.
fn break_glass_webauthn(rp_id: &str, origin: &Url) -> anyhow::Result<Webauthn> {
    Ok(WebauthnBuilder::new(rp_id, origin)?
        .rp_name("BOSS break-glass")
        .danger_set_user_presence_only_security_keys(true)
        .build()?)
}

/// What the break-glass door is built from, read out of the environment
/// by [`BootConfig::from_env`] — split from [`BreakGlassState::boot`] so
/// the boot is tested without mutating process env (review F3).
#[derive(Debug, Clone)]
pub struct BootConfig {
    /// `BOSS_PUBLIC_URL`: the door's relying party and origin.
    pub public_url: String,
    /// `BOSS_BREAK_GLASS_DIR`: the mounted credential store.
    pub dir: PathBuf,
    /// [`AUTHORISERS_ENV`], parsed.
    pub authorisers: Vec<String>,
    /// `BOSS_JOBS_UPSTREAM`: where authorisations and the boot alarm go.
    pub jobs_base: String,
}

impl BootConfig {
    pub fn from_env() -> Self {
        Self {
            public_url: std::env::var("BOSS_PUBLIC_URL")
                .unwrap_or_else(|_| "http://localhost:8000".into()),
            dir: std::env::var("BOSS_BREAK_GLASS_DIR")
                .unwrap_or_else(|_| "/etc/boss/break-glass".into())
                .into(),
            authorisers: parse_authorisers(&std::env::var(AUTHORISERS_ENV).unwrap_or_default()),
            jobs_base: std::env::var("BOSS_JOBS_UPSTREAM")
                .unwrap_or_else(|_| boss_ports::url("jobs")),
        }
    }
}

impl BreakGlassState {
    /// rp_id / origin derive from BOSS_PUBLIC_URL, exactly like the
    /// presence-passkey ceremony — one host, one RP identity. The boot
    /// alarm files through the jobs API; its task is not waited on.
    pub fn from_env(session_key: Vec<u8>, audit: crate::audit::AuthAudit) -> anyhow::Result<Self> {
        let config = BootConfig::from_env();
        let alarms = Arc::new(crate::break_glass_alarm::JobsApiAlarms::new(
            config.jobs_base.clone(),
        )?);
        // The handle is dropped: the task runs on, the boot does not wait.
        let (state, _alarm) = Self::boot(config, session_key, audit, alarms)?;
        Ok(state)
    }

    /// Build the door from `config` and raise the boot alarm through
    /// `alarms`. The alarm's task handle is returned so a test can await
    /// what it came to; nothing it answers can fail this call.
    pub fn boot(
        config: BootConfig,
        session_key: Vec<u8>,
        audit: crate::audit::AuthAudit,
        alarms: Arc<dyn crate::break_glass_alarm::AlarmFiling>,
    ) -> anyhow::Result<(
        Self,
        Option<tokio::task::JoinHandle<crate::break_glass_alarm::Outcome>>,
    )> {
        let origin = Url::parse(&config.public_url)?;
        let door = Door::of(&origin)?;
        let webauthn = break_glass_webauthn(&door.rp_id, &origin)?;
        let state = Self {
            session_key,
            webauthn,
            door,
            dir: config.dir,
            authorisers: config.authorisers,
            authorisations: Arc::new(JobsApiAuthorisations {
                http: crate::machine_client::MachineClient::build(
                    reqwest::Client::builder().timeout(JOBS_TIMEOUT),
                )?,
                jobs_base: config.jobs_base,
            }),
            audit,
            enrol_lock: tokio::sync::Mutex::new(()),
            pending_reg: Mutex::new(HashMap::new()),
            pending_auth: Mutex::new(HashMap::new()),
            counters: Mutex::new(HashMap::new()),
        };
        // Said at boot as well as at the door, and FILED, not only
        // logged (5eb583c2): a store with no key bound to this door — an
        // empty one, or keys enrolled for another relying party (1c4c100a)
        // — is a total failure of the emergency path, and the one moment
        // it is certain to be read otherwise is the emergency. The filing
        // is detached and best-effort; nothing it answers stops the boot.
        let alarm = crate::break_glass_alarm::alarm_if_unbound(&state.dir, &state.door, alarms);
        Ok((state, alarm))
    }

    fn session(&self, headers: &HeaderMap) -> Option<Session> {
        let cookie_header = headers.get(header::COOKIE)?.to_str().ok()?;
        let raw = session::find_cookie(cookie_header, session::COOKIE_NAME)?;
        Session::decode(raw, &self.session_key).ok()
    }

    /// Who authorises this enrollment request, or the refusal (status,
    /// words). A break-glass session enrols as itself; anything else
    /// must name a presence-signed packet, which is read and judged.
    async fn authorise(
        &self,
        headers: &HeaderMap,
        authorisation: Option<&str>,
    ) -> Result<EnrollAuthz, ErrResp> {
        let sess = self.session(headers);
        if session_may_enroll(sess.as_ref().and_then(|s| s.role.as_deref())) {
            return Ok(EnrollAuthz::BreakGlassSession);
        }
        let Some(raw) = authorisation else {
            return Err((
                StatusCode::FORBIDDEN,
                format!(
                    "enrollment refused: name a `{AUTHORISATION_KIND}` packet whose \
                     `{AUTHORISE_STEP}` step an authoriser approved with a passkey, or \
                     enrol under a break-glass session"
                ),
            ));
        };
        // Review F3: only an authoriser's own session gets as far as a
        // packet read. The read is made as the gateway's platform-admin
        // identity, and a refusal after it names the packet's kind,
        // decision and signer — none of which an anonymous caller, or an
        // employee off the list, has any business learning.
        let caller = sess.as_ref().and_then(|s| s.employee_id.as_deref());
        if !caller.is_some_and(|c| self.authorisers.iter().any(|a| a == c)) {
            return Err((
                StatusCode::FORBIDDEN,
                "enrollment refused: a presence-authorised enrolment is made from the signed-in \
                 session of an authoriser on this gateway's list"
                    .to_string(),
            ));
        }
        let job_id = Uuid::parse_str(raw.trim()).map_err(|_| {
            (
                StatusCode::BAD_REQUEST,
                "the authorisation must be a packet id".to_string(),
            )
        })?;
        let job = self
            .authorisations
            .packet(job_id)
            .await
            .map_err(|e| (StatusCode::BAD_GATEWAY, e))?;
        judge_authorisation(
            &job,
            &self.door,
            &self.authorisers,
            sess.as_ref().and_then(|s| s.employee_id.as_deref()),
        )
        .map(EnrollAuthz::Presence)
        .map_err(|why| (StatusCode::FORBIDDEN, format!("enrollment refused: {why}")))
    }

    fn store(&self) -> Result<Vec<BreakGlassCredential>, ErrResp> {
        load_store(&self.dir).map_err(|e| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("break-glass credential store unreadable: {e}"),
            )
        })
    }

    /// The store's records as live [`SecurityKey`]s, counters bumped
    /// to the effective floor. A record whose public_key does not
    /// decode is skipped loudly, same contract as [`load_store`].
    fn security_keys(&self, records: &[BreakGlassCredential]) -> Vec<SecurityKey> {
        let counters = lock_recover(&self.counters);
        records
            .iter()
            .filter_map(|rec| {
                let decoded = URL_SAFE_NO_PAD
                    .decode(&rec.public_key)
                    .ok()
                    .and_then(|b| serde_json::from_slice::<Value>(&b).ok());
                let Some(mut v) = decoded else {
                    tracing::error!(
                        label = rec.label.as_str(),
                        "break-glass public_key undecodable — record SKIPPED"
                    );
                    return None;
                };
                let inner = v["cred"]["counter"].as_u64().unwrap_or(0) as u32;
                let floor = effective_counter(
                    rec.sign_count,
                    inner,
                    counters.get(&rec.credential_id).copied(),
                );
                v["cred"]["counter"] = json!(floor);
                match serde_json::from_value::<SecurityKey>(v) {
                    Ok(sk) => Some(sk),
                    Err(e) => {
                        tracing::error!(
                            label = rec.label.as_str(),
                            error = %e,
                            "break-glass public_key not a stored SecurityKey — record SKIPPED"
                        );
                        None
                    }
                }
            })
            .collect()
    }
}

fn lock_recover<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|p| p.into_inner())
}

/// Small error value for helper Results — converted to a Response at
/// the handler boundary (clippy::result_large_err; same idiom as the
/// presence-passkey module: these are cold refusal paths).
type ErrResp = (StatusCode, String);

fn take_pending<T>(map: &Mutex<HashMap<String, Pending<T>>>, id: &str) -> Result<T, ErrResp> {
    let mut map = lock_recover(map);
    let now = Instant::now();
    map.retain(|_, p| p.expires > now);
    match map.remove(id) {
        Some(p) => Ok(p.state),
        None => Err((
            StatusCode::GONE,
            "challenge unknown, spent or expired — begin again".into(),
        )),
    }
}

fn put_pending<T>(map: &Mutex<HashMap<String, Pending<T>>>, id: String, state: T) {
    let mut map = lock_recover(map);
    let now = Instant::now();
    map.retain(|_, p| p.expires > now);
    map.insert(
        id,
        Pending {
            state,
            expires: now + CEREMONY_TTL,
        },
    );
}

// --------------------------------------------------------------------
// Enrollment ceremony.
// --------------------------------------------------------------------

#[derive(Deserialize)]
pub struct EnrollBeginBody {
    /// The `break-glass-enrolment` packet authorising this enrolment.
    /// Absent under a break-glass session. (A `token` field from a page
    /// served before the token retired is ignored, and opens nothing.)
    #[serde(default)]
    pub authorisation: Option<String>,
}

/// `POST /api/auth/break-glass/enroll/begin`
pub async fn enroll_begin(
    State(state): State<Arc<BreakGlassState>>,
    headers: HeaderMap,
    Json(body): Json<EnrollBeginBody>,
) -> Response {
    let records = match state.store() {
        Ok(v) => v,
        Err(r) => return r.into_response(),
    };
    let authz = match state
        .authorise(&headers, body.authorisation.as_deref())
        .await
    {
        Ok(a) => a,
        Err(r) => return r.into_response(),
    };
    let (authorisation, label) = match &authz {
        EnrollAuthz::Presence(a) => (Some(a.job_id), Some(a.label.as_str())),
        EnrollAuthz::BreakGlassSession => (None, None),
    };

    let exclude = if records.is_empty() {
        None
    } else {
        Some(
            records
                .iter()
                .filter_map(|r| URL_SAFE_NO_PAD.decode(&r.credential_id).ok())
                .map(webauthn_rs::prelude::CredentialID::from)
                .collect(),
        )
    };
    // A stable user handle: break-glass has exactly one "user" — the
    // deployment's emergency operator identity.
    let user_uuid = Uuid::new_v5(&Uuid::NAMESPACE_OID, BREAK_GLASS_ACTOR.as_bytes());
    let (mut ccr, reg_state) = match state.webauthn.start_securitykey_registration(
        user_uuid,
        BREAK_GLASS_ACTOR,
        "BOSS break-glass",
        exclude,
        // Q1: no CA allowlist. The chain-form requirement is enforced
        // at finish by `require_attested_hardware`.
        None,
        Some(AuthenticatorAttachment::CrossPlatform),
    ) {
        Ok(v) => v,
        Err(e) => {
            return (StatusCode::INTERNAL_SERVER_ERROR, format!("webauthn: {e}")).into_response();
        }
    };
    // The security-key flow only requests `direct` conveyance when a
    // CA list is supplied; we require the statement without the
    // allowlist, so raise the ask ourselves. The registration state
    // does not pin the preference — verification is unaffected.
    ccr.public_key.attestation = Some(AttestationConveyancePreference::Direct);

    let challenge_id = Uuid::new_v4().to_string();
    put_pending(
        &state.pending_reg,
        challenge_id.clone(),
        BegunRegistration {
            state: reg_state,
            authorisation,
        },
    );
    Json(json!({ "challenge_id": challenge_id, "options": ccr, "label": label })).into_response()
}

#[derive(Deserialize)]
pub struct EnrollFinishBody {
    pub challenge_id: String,
    /// The packet the challenge was begun under; see [`EnrollBeginBody`].
    #[serde(default)]
    pub authorisation: Option<String>,
    /// Under a break-glass session, the label to enrol. Under an
    /// authorisation the packet's label is the enrolment's, and a
    /// different one here is refused rather than silently replaced.
    pub label: CredentialLabel,
    pub credential: RegisterPublicKeyCredential,
}

/// The manifest a finished enrollment tells the operator to commit
/// into — named in code so the response can never drift from the
/// repo layout silently.
pub const CREDENTIALS_MANIFEST: &str = "infra/cluster/manifests/boss-break-glass-credentials.yaml";

/// `POST /api/auth/break-glass/enroll/finish` — verifies the
/// attestation and EMITS the credential record for the operator to
/// commit (see the module docs for why enrollment never writes the
/// ConfigMap itself).
pub async fn enroll_finish(
    State(state): State<Arc<BreakGlassState>>,
    headers: HeaderMap,
    Json(body): Json<EnrollFinishBody>,
) -> Response {
    // One presence finish at a time from the re-judge to the emitted
    // record: the single-use check below reads the packet, and the spend
    // writes it. A break-glass session spends no packet, so it never
    // waits here — a hung jobs API must not hold the emergency session's
    // own rotation (review F4).
    let is_session = session_may_enroll(
        state
            .session(&headers)
            .as_ref()
            .and_then(|s| s.role.as_deref()),
    );
    let _one_at_a_time = if is_session {
        None
    } else {
        Some(state.enrol_lock.lock().await)
    };
    let authz = match state
        .authorise(&headers, body.authorisation.as_deref())
        .await
    {
        Ok(a) => a,
        Err(r) => return r.into_response(),
    };
    let begun = match take_pending(&state.pending_reg, &body.challenge_id) {
        Ok(v) => v,
        Err(r) => return r.into_response(),
    };
    let finishing_under = match &authz {
        EnrollAuthz::Presence(a) => Some(a.job_id),
        EnrollAuthz::BreakGlassSession => None,
    };
    if begun.authorisation != finishing_under {
        let named = |a: Option<Uuid>| {
            a.map(|id| format!("packet {id}"))
                .unwrap_or_else(|| "a break-glass session".into())
        };
        return (
            StatusCode::FORBIDDEN,
            format!(
                "this challenge was begun under {} and cannot be finished under {} — begin \
                 again",
                named(begun.authorisation),
                named(finishing_under)
            ),
        )
            .into_response();
    }
    let label = match &authz {
        EnrollAuthz::Presence(a) if a.label != body.label => {
            return (
                StatusCode::FORBIDDEN,
                format!(
                    "packet {} authorises the {} key, not the {} key",
                    a.job_id,
                    a.label.as_str(),
                    body.label.as_str()
                ),
            )
                .into_response();
        }
        EnrollAuthz::Presence(a) => a.label,
        EnrollAuthz::BreakGlassSession => body.label,
    };
    let sk = match state
        .webauthn
        .finish_securitykey_registration(&body.credential, &begun.state)
    {
        Ok(v) => v,
        Err(e) => {
            return (
                StatusCode::BAD_REQUEST,
                format!("registration rejected: {e}"),
            )
                .into_response();
        }
    };
    let sk_value = match serde_json::to_value(&sk) {
        Ok(v) => v,
        Err(e) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("credential serialization failed: {e}"),
            )
                .into_response();
        }
    };
    if let Err(reason) = require_attested_hardware(&sk_value) {
        return (StatusCode::BAD_REQUEST, reason).into_response();
    }

    let aaguid = match &sk.attestation().metadata {
        AttestationMetadata::Packed { aaguid } | AttestationMetadata::Tpm { aaguid, .. } => {
            aaguid.to_string()
        }
        // FIDO-U2F attestation predates AAGUID conveyance; the
        // record keeps the all-zero id rather than inventing one.
        _ => Uuid::nil().to_string(),
    };
    let record = new_record(
        &sk_value,
        aaguid,
        label,
        &state.door,
        // Wall time via the sanctioned stamp source: enrolling an
        // emergency key is real-world activity in any clock mode,
        // same decision the auth-audit drain records under.
        boss_clock_client::wall_now(),
    );
    // Q3: the spend lands BEFORE the record leaves this process, so an
    // authorisation the jobs API did not record as spent enrols nothing.
    let authorised_by = match &authz {
        EnrollAuthz::Presence(a) => {
            if let Err(why) = record_spend(state.authorisations.as_ref(), a, &record).await {
                return (
                    StatusCode::BAD_GATEWAY,
                    format!(
                        "the key registered, but the spend could not be recorded on packet {} \
                         ({why}), so no record is issued — begin again",
                        a.job_id
                    ),
                )
                    .into_response();
            }
            Some(a.authorised_by.clone())
        }
        EnrollAuthz::BreakGlassSession => None,
    };
    state
        .audit
        .break_glass_enrolled(record.label.as_str(), &record.credential_id, &record.aaguid);
    // The record is public material; logging it whole is a second copy
    // if the browser tab is lost. Under a presence authorisation the
    // durable copy is the packet's `enrol` step, written above; the log
    // dies with the pod (backlog 4a173252).
    tracing::info!(
        label = record.label.as_str(),
        credential_id = %record.credential_id,
        authorisation = ?finishing_under,
        authorised_by = authorised_by.as_deref().unwrap_or(BREAK_GLASS_ACTOR),
        record = %serde_json::to_string(&record).unwrap_or_default(),
        "break-glass credential enrolled — commit this record to {CREDENTIALS_MANIFEST}"
    );
    (
        StatusCode::CREATED,
        Json(json!({
            "credential": record,
            "commit_to": CREDENTIALS_MANIFEST,
            "config_map_key": format!("{}.json", record.label.as_str()),
            "authorisation": finishing_under,
            "authorised_by": authorised_by,
        })),
    )
        .into_response()
}

// --------------------------------------------------------------------
// Assertion ceremony — the emergency door itself.
// --------------------------------------------------------------------

/// `POST /api/auth/break-glass/assert/begin`
pub async fn assert_begin(State(state): State<Arc<BreakGlassState>>) -> Response {
    let (rcr, auth_state) = match state.begin_assertion() {
        Ok(v) => v,
        Err(r) => return r.into_response(),
    };
    let challenge_id = Uuid::new_v4().to_string();
    put_pending(&state.pending_auth, challenge_id.clone(), auth_state);
    Json(json!({ "challenge_id": challenge_id, "options": rcr })).into_response()
}

impl BreakGlassState {
    /// BEGIN ONE BREAK-GLASS ASSERTION over the committed records bound to
    /// this door: the options for the browser and the state its finish
    /// needs. The emergency sign-in keeps the state in its own pending map;
    /// the passkey promotion's vouch (`crate::promotion`, design 2cb6256f
    /// Q1) keeps it with its ceremony — one begin, so the two can never
    /// offer different keys.
    pub(crate) fn begin_assertion(
        &self,
    ) -> Result<(RequestChallengeResponse, SecurityKeyAuthentication), ErrResp> {
        let records = self.store()?;
        if records.is_empty() {
            return Err((
                StatusCode::CONFLICT,
                "no break-glass credential enrolled — see the enrollment ceremony in \
                 the credential manifest"
                    .into(),
            ));
        }
        // A key bound to another relying party cannot assert here — the
        // authenticator refuses, whatever this process offers — so it is
        // never offered, and when it is all there is, the door says which
        // keys, which party, and which this door is (1c4c100a). Offering
        // them produced a browser error naming none of that.
        let foreign = foreign_records(&records, &self.door.rp_id);
        let records: Vec<BreakGlassCredential> = records
            .into_iter()
            .filter(|r| !matches!(binding(r, &self.door.rp_id), Binding::Elsewhere(_)))
            .collect();
        if records.is_empty() {
            return Err((
                StatusCode::CONFLICT,
                format!(
                    "every enrolled break-glass key is bound to another relying party ({}) and \
                     this door answers as {} — no key can open it. Re-enrol through a \
                     {AUTHORISATION_KIND} packet.",
                    foreign.join(", "),
                    self.door.rp_id
                ),
            ));
        }
        let keys = self.security_keys(&records);
        if keys.is_empty() {
            return Err((
                StatusCode::INTERNAL_SERVER_ERROR,
                "no enrolled credential record is readable — the store is present but \
                 every record failed to decode"
                    .into(),
            ));
        }
        self.webauthn
            .start_securitykey_authentication(&keys)
            .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, format!("webauthn: {e}")))
    }

    /// FINISH ONE BREAK-GLASS ASSERTION begun by [`Self::begin_assertion`]:
    /// verify it (origin, relying party, presence, signature, counter —
    /// all inside webauthn-rs), advance this process's counter floor, and
    /// answer the credential id (base64url) that asserted. `Err` is
    /// webauthn-rs's own text. Records nothing: each caller puts its own
    /// act on the record. The emergency sign-in's behaviour is exactly
    /// what it was before this was split out of it.
    pub(crate) fn finish_assertion(
        &self,
        credential: &PublicKeyCredential,
        auth_state: &SecurityKeyAuthentication,
    ) -> Result<String, String> {
        let result = self
            .webauthn
            .finish_securitykey_authentication(credential, auth_state)
            .map_err(|e| format!("assertion rejected: {e}"))?;
        // Advance this process's counter floor and tell the operator the
        // durable one is behind (module docs: the committed sign_count is
        // the restart-surviving floor).
        let cred_id = URL_SAFE_NO_PAD.encode(result.cred_id().as_slice());
        {
            let mut counters = lock_recover(&self.counters);
            let entry = counters.entry(cred_id.clone()).or_insert(0);
            *entry = (*entry).max(result.counter());
        }
        tracing::info!(
            credential_id = %cred_id,
            sign_count = result.counter(),
            "break-glass assertion verified — refresh sign_count in \
             {CREDENTIALS_MANIFEST} to carry this floor across restarts"
        );
        Ok(cred_id)
    }

    /// The label of the committed record holding `credential_id`, read
    /// from the store as it is now — `None` when no record holds it.
    pub(crate) fn label_of(&self, credential_id: &str) -> Option<CredentialLabel> {
        self.store()
            .ok()?
            .into_iter()
            .find(|r| r.credential_id == credential_id)
            .map(|r| r.label)
    }

    /// Put a verified emergency sign-in on the record, naming the key
    /// that asserted: `credential_id` as [`Self::finish_assertion`]
    /// answered it, and its label resolved through [`Self::label_of`] —
    /// the pair the promotion vouch already uses. Backlog 9bbfb244: the
    /// event said only `{email, method}`, so DR 62dac114's "both keys
    /// open the door" rested on the operator's word, not the log.
    pub(crate) fn record_sign_in(&self, credential_id: &str) {
        let label = self.label_of(credential_id);
        self.audit.break_glass_login_succeeded(
            BREAK_GLASS_ACTOR,
            credential_id,
            label.map(CredentialLabel::as_str),
        );
    }
}

#[derive(Deserialize)]
pub struct AssertFinishBody {
    pub challenge_id: String,
    pub credential: PublicKeyCredential,
}

/// `POST /api/auth/break-glass/assert/finish` — a verified assertion
/// mints the narrow break-glass session. A failed one refuses
/// loudly, like every refusal in this system.
pub async fn assert_finish(
    State(state): State<Arc<BreakGlassState>>,
    Json(body): Json<AssertFinishBody>,
) -> Response {
    let auth_state = match take_pending(&state.pending_auth, &body.challenge_id) {
        Ok(v) => v,
        Err(r) => return r.into_response(),
    };
    // Held rather than tested inline so the credential id it answers
    // reaches the record (backlog 9bbfb244); the refusal below is
    // unchanged and returns, so past it `verified` is always Ok.
    let verified = state.finish_assertion(&body.credential, &auth_state);
    if let Err(why) = verified {
        state.audit.login_denied(
            Some(BREAK_GLASS_ACTOR),
            crate::audit::AuthMethod::BreakGlass,
            crate::audit::DeniedReason::BadCredentials,
            None,
        );
        return (StatusCode::UNAUTHORIZED, why).into_response();
    }
    if let Ok(cred_id) = &verified {
        state.record_sign_in(cred_id);
    }

    let (set_cookie, sess) = mint_session(&state.session_key);
    let mut headers = HeaderMap::new();
    if let Ok(v) = HeaderValue::from_str(&set_cookie) {
        headers.insert(header::SET_COOKIE, v);
    }
    (
        StatusCode::OK,
        headers,
        Json(json!({
            "actor": BREAK_GLASS_ACTOR,
            "role": sess.role,
            "expires_in": BREAK_GLASS_TTL_SECONDS,
        })),
    )
        .into_response()
}

// --------------------------------------------------------------------
// The ceremony page — self-contained, served from this binary.
// --------------------------------------------------------------------

/// `GET /break-glass`. Unlinked from the SPA on purpose — the
/// understated posture the login page documents is preserved: this
/// is a typed URL, not a button. It is served from a constant in the
/// gateway binary so it exists exactly when the verifier does.
pub async fn ceremony_page() -> Response {
    Html(CEREMONY_HTML).into_response()
}

const CEREMONY_HTML: &str = r#"<!doctype html>
<html lang="en">
<head>
<meta charset="utf-8">
<meta name="robots" content="noindex">
<title>BOSS break-glass</title>
<style>
  body { font: 15px/1.5 system-ui, sans-serif; max-width: 34rem;
         margin: 4rem auto; padding: 0 1rem; color: #222; }
  h1 { font-size: 1.2rem; }
  button { font: inherit; padding: .5rem 1rem; cursor: pointer; }
  input, select { font: inherit; padding: .35rem; width: 100%;
                  box-sizing: border-box; margin: .2rem 0 .8rem; }
  pre { background: #f4f4f4; padding: .8rem; overflow-x: auto;
        white-space: pre-wrap; word-break: break-all; }
  .err { color: #a00; }
  .ok { color: #060; }
  details { margin-top: 2.5rem; }
</style>
</head>
<body>
<h1>Break-glass</h1>
<p>The emergency door. Touch your enrolled security key to open an
emergency session carrying the narrow break-glass role.</p>
<button id="assert">Touch key &amp; sign in</button>
<p id="assert-out"></p>

<details>
<summary>Enrollment (a new key, or rotation)</summary>
<p>Runs under a <code>break-glass-enrolment</code> packet whose
<code>authorise</code> step you approved with your passkey, from your own
signed-in session — or under an existing break-glass session. The
packet names the key's label and this door's relying party. The
finished record must be committed to
<code>infra/cluster/manifests/boss-break-glass-credentials.yaml</code>
— nothing is stored until it lands there.</p>
<label>Authorisation packet id (blank when using a break-glass session)</label>
<input id="authorisation" autocomplete="off">
<label>Label (under a packet, the packet's label is used)</label>
<select id="label"><option>primary</option><option>backup</option></select>
<button id="enroll">Enroll this key</button>
<p id="enroll-out"></p>
<pre id="record" hidden></pre>
</details>

<script>
"use strict";
const b64uToBuf = (s) => Uint8Array.from(
  atob(s.replace(/-/g, "+").replace(/_/g, "/")), c => c.charCodeAt(0));
const bufToB64u = (b) => btoa(String.fromCharCode(...new Uint8Array(b)))
  .replace(/\+/g, "-").replace(/\//g, "_").replace(/=+$/, "");
const post = async (url, body) => {
  const r = await fetch(url, { method: "POST",
    headers: { "content-type": "application/json" },
    body: JSON.stringify(body ?? {}) });
  const text = await r.text();
  if (!r.ok) throw new Error(r.status + ": " + text);
  return JSON.parse(text);
};
const say = (id, msg, cls) => {
  const el = document.getElementById(id);
  el.textContent = msg; el.className = cls || "";
};

document.getElementById("assert").addEventListener("click", async () => {
  try {
    say("assert-out", "requesting challenge…");
    const begin = await post("/api/auth/break-glass/assert/begin");
    const pk = begin.options.publicKey;
    pk.challenge = b64uToBuf(pk.challenge);
    (pk.allowCredentials || []).forEach(c => { c.id = b64uToBuf(c.id); });
    say("assert-out", "touch your key…");
    const cred = await navigator.credentials.get({ publicKey: pk });
    const out = await post("/api/auth/break-glass/assert/finish", {
      challenge_id: begin.challenge_id,
      credential: {
        id: cred.id, rawId: bufToB64u(cred.rawId), type: cred.type,
        extensions: cred.getClientExtensionResults(),
        response: {
          authenticatorData: bufToB64u(cred.response.authenticatorData),
          clientDataJSON: bufToB64u(cred.response.clientDataJSON),
          signature: bufToB64u(cred.response.signature),
          userHandle: cred.response.userHandle
            ? bufToB64u(cred.response.userHandle) : null,
        },
      },
    });
    say("assert-out", "session open (" + out.role + ", " +
        out.expires_in + "s) — redirecting…", "ok");
    setTimeout(() => { window.location.href = "/"; }, 800);
  } catch (e) { say("assert-out", String(e), "err"); }
});

document.getElementById("enroll").addEventListener("click", async () => {
  try {
    const authorisation = document.getElementById("authorisation").value.trim() || null;
    const label = document.getElementById("label").value;
    say("enroll-out", "requesting challenge…");
    const begin = await post("/api/auth/break-glass/enroll/begin", { authorisation });
    const pk = begin.options.publicKey;
    pk.challenge = b64uToBuf(pk.challenge);
    pk.user.id = b64uToBuf(pk.user.id);
    (pk.excludeCredentials || []).forEach(c => { c.id = b64uToBuf(c.id); });
    say("enroll-out", "touch your key…");
    const cred = await navigator.credentials.create({ publicKey: pk });
    const out = await post("/api/auth/break-glass/enroll/finish", {
      challenge_id: begin.challenge_id, authorisation, label: begin.label || label,
      credential: {
        id: cred.id, rawId: bufToB64u(cred.rawId), type: cred.type,
        extensions: cred.getClientExtensionResults(),
        response: {
          attestationObject: bufToB64u(cred.response.attestationObject),
          clientDataJSON: bufToB64u(cred.response.clientDataJSON),
        },
      },
    });
    say("enroll-out", "enrolled. Commit the record below as ConfigMap key '" +
        out.config_map_key + "' in " + out.commit_to, "ok");
    const rec = document.getElementById("record");
    rec.hidden = false;
    rec.textContent = JSON.stringify(out.credential, null, 2);
  } catch (e) { say("enroll-out", String(e), "err"); }
});
</script>
</body>
</html>
"#;

// --------------------------------------------------------------------
// Tests.
// --------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::to_bytes;
    use tempfile::TempDir;

    const KEY: &[u8] = b"break-glass-test-session-key-32b";

    /// A real serialized credential, taken verbatim from
    /// webauthn-rs-core 0.5.5's own test suite (MPL-2.0) — a Yubico
    /// security key with a verified `Basic` attestation chain. Public
    /// material only. Used to build synthetic-but-deserializable
    /// [`SecurityKey`] values without an authenticator in the loop.
    const YUBIKEY_CRED: &str = r#"{"cred_id":"uZcVDBVS68E_MtAgeQpElJxldF_6cY9sSvbWqx_qRh8wiu42lyRBRmh5yFeD_r9k130dMbFHBHI9RTFgdJQIzQ","cred":{"type_":"ES256","key":{"EC_EC2":{"curve":"SECP256R1","x":[194,126,127,109,252,23,131,21,252,6,223,99,44,254,140,27,230,17,94,5,133,28,104,41,144,69,171,149,161,26,200,243],"y":[143,123,183,156,24,178,21,248,117,159,162,69,171,52,188,252,26,59,6,47,103,92,19,58,117,103,249,0,219,8,95,196]}}},"counter":2,"user_verified":false,"backup_eligible":false,"backup_state":false,"registration_policy":"preferred","extensions":{"cred_protect":"NotRequested","hmac_create_secret":"NotRequested"},"attestation":{"data":{"Basic":["MIICvTCCAaWgAwIBAgIEK_F8eDANBgkqhkiG9w0BAQsFADAuMSwwKgYDVQQDEyNZdWJpY28gVTJGIFJvb3QgQ0EgU2VyaWFsIDQ1NzIwMDYzMTAgFw0xNDA4MDEwMDAwMDBaGA8yMDUwMDkwNDAwMDAwMFowbjELMAkGA1UEBhMCU0UxEjAQBgNVBAoMCVl1YmljbyBBQjEiMCAGA1UECwwZQXV0aGVudGljYXRvciBBdHRlc3RhdGlvbjEnMCUGA1UEAwweWXViaWNvIFUyRiBFRSBTZXJpYWwgNzM3MjQ2MzI4MFkwEwYHKoZIzj0CAQYIKoZIzj0DAQcDQgAEdMLHhCPIcS6bSPJZWGb8cECuTN8H13fVha8Ek5nt-pI8vrSflxb59Vp4bDQlH8jzXj3oW1ZwUDjHC6EnGWB5i6NsMGowIgYJKwYBBAGCxAoCBBUxLjMuNi4xLjQuMS40MTQ4Mi4xLjcwEwYLKwYBBAGC5RwCAQEEBAMCAiQwIQYLKwYBBAGC5RwBAQQEEgQQxe9V_62aS5-1gK3rr-Am0DAMBgNVHRMBAf8EAjAAMA0GCSqGSIb3DQEBCwUAA4IBAQCLbpN2nXhNbunZANJxAn_Cd-S4JuZsObnUiLnLLS0FPWa01TY8F7oJ8bE-aFa4kTe6NQQfi8-yiZrQ8N-JL4f7gNdQPSrH-r3iFd4SvroDe1jaJO4J9LeiFjmRdcVa-5cqNF4G1fPCofvw9W4lKnObuPakr0x_icdVq1MXhYdUtQk6Zr5mBnc4FhN9qi7DXqLHD5G7ZFUmGwfIcD2-0m1f1mwQS8yRD5-_aDCf3vutwddoi3crtivzyromwbKklR4qHunJ75LGZLZA8pJ_mXnUQ6TTsgRqPvPXgQPbSyGMf2z_DIPbQqCD_Bmc4dj9o6LozheBdDtcZCAjSPTAd_ui"]},"metadata":"None"},"attestation_format":"Packed"}"#;

    fn yubikey_sk_value() -> Value {
        json!({ "cred": serde_json::from_str::<Value>(YUBIKEY_CRED).unwrap() })
    }

    fn record_from(sk: &Value, label: CredentialLabel, sign_count: u32) -> BreakGlassCredential {
        BreakGlassCredential {
            credential_id: sk["cred"]["cred_id"].as_str().unwrap().to_string(),
            public_key: URL_SAFE_NO_PAD.encode(serde_json::to_vec(sk).unwrap()),
            sign_count,
            aaguid: Uuid::nil().to_string(),
            enrolled_at: Utc::now(),
            label,
            rp_id: Some(DOOR_RP.to_string()),
        }
    }

    fn write_record(dir: &Path, name: &str, rec: &BreakGlassCredential) {
        std::fs::write(dir.join(name), serde_json::to_vec_pretty(rec).unwrap()).unwrap();
    }

    /// The relying party every test door answers as.
    const DOOR_RP: &str = "boss.test";
    const DOOR_ORIGIN: &str = "https://boss.test";
    /// The one employee on the test door's authoriser list.
    const AUTHORISER: &str = "emp-authoriser";
    const PACKET: &str = "7d7c2a51-2f0e-4a51-9a0e-3c1b6b0d9a11";
    const OTHER_PACKET: &str = "1e3f5a7c-9b2d-4f6e-8a0c-2d4f6b8a0c1e";
    const AUTHORISE_ID: &str = "0f1e2d3c-4b5a-4968-8776-655443322110";
    const ENROL_ID: &str = "a1b2c3d4-e5f6-4a7b-8c9d-0e1f2a3b4c5d";

    fn state_with(dir: &Path) -> Arc<BreakGlassState> {
        state_with_port(
            dir,
            Arc::new(MemoryAuthorisations::default()),
            &[AUTHORISER],
        )
    }

    fn state_with_port(
        dir: &Path,
        authorisations: Arc<dyn EnrolmentAuthorisations>,
        authorisers: &[&str],
    ) -> Arc<BreakGlassState> {
        state_heard_by(
            dir,
            authorisations,
            authorisers,
            crate::audit::AuthAudit::disabled(),
        )
    }

    /// A test door whose auth events land in `audit` — the in-memory
    /// recorder of `crate::audit::testing`, per the no-mocks rule.
    fn state_heard_by(
        dir: &Path,
        authorisations: Arc<dyn EnrolmentAuthorisations>,
        authorisers: &[&str],
        audit: crate::audit::AuthAudit,
    ) -> Arc<BreakGlassState> {
        let origin = Url::parse(DOOR_ORIGIN).unwrap();
        let webauthn = break_glass_webauthn(DOOR_RP, &origin).unwrap();
        Arc::new(BreakGlassState {
            session_key: KEY.to_vec(),
            webauthn,
            door: Door::of(&origin).unwrap(),
            dir: dir.to_path_buf(),
            authorisers: authorisers.iter().map(|a| a.to_string()).collect(),
            authorisations,
            audit,
            enrol_lock: tokio::sync::Mutex::new(()),
            pending_reg: Mutex::new(HashMap::new()),
            pending_auth: Mutex::new(HashMap::new()),
            counters: Mutex::new(HashMap::new()),
        })
    }

    fn break_glass_headers() -> HeaderMap {
        let (_, sess) = mint_session(KEY);
        cookie_headers(&sess)
    }

    fn cookie_headers(sess: &Session) -> HeaderMap {
        let mut headers = HeaderMap::new();
        headers.insert(
            header::COOKIE,
            HeaderValue::from_str(&format!("{}={}", session::COOKIE_NAME, sess.encode(KEY)))
                .unwrap(),
        );
        headers
    }

    /// A signed-in employee's session, the kind an authoriser holds.
    fn employee_headers(employee_id: &str) -> HeaderMap {
        let mut sess = Session::new(employee_id, 600);
        sess.employee_id = Some(employee_id.to_string());
        sess.role = Some("platform-admin".to_string());
        cookie_headers(&sess)
    }

    // ---- the authorisation packet, as the jobs API returns it --------

    /// A `break-glass-enrolment` packet whose `authorise` step an
    /// authoriser approved with a passkey over its current shape, and
    /// whose `enrol` step is waiting — the one state that authorises.
    fn packet(label: &str) -> Value {
        let mut p = json!({
            "id": PACKET,
            "kind": AUTHORISATION_KIND,
            "status": "open",
            "steps": [
                {
                    "id": AUTHORISE_ID,
                    "job_id": PACKET,
                    "spec_slug": AUTHORISE_STEP,
                    "kind": "sign-off",
                    "title": format!("Authorise enrolling the {label} break-glass key"),
                    "status": "completed",
                    "metadata": {
                        "label": label,
                        "rp_id": DOOR_RP,
                        "origin": DOOR_ORIGIN,
                        "decision": "approved",
                        "decided_at": "2026-09-28T10:00:00Z",
                    },
                    "sign_offs": [],
                },
                {
                    "id": ENROL_ID,
                    "job_id": PACKET,
                    "spec_slug": ENROL_STEP,
                    "kind": "task",
                    "title": "Enrol the key",
                    "status": "ready",
                    "metadata": { "procedure": "Completed by the gateway." },
                },
            ],
        });
        stamp(&mut p, AUTHORISER, "presence");
        p
    }

    fn authorise_of(p: &mut Value) -> &mut Value {
        &mut p["steps"][0]
    }

    /// Replace the authorise step's stamps with one by `who`, of
    /// `assurance`, over the step's CURRENT shape — so each test below
    /// changes exactly one thing and the stamp still binds the rest.
    fn stamp(p: &mut Value, who: &str, assurance: &str) {
        let step = authorise_of(p);
        let hash =
            boss_core::job::step_shape_hash(step["title"].as_str().unwrap(), &step["metadata"]);
        step["sign_offs"] = json!([{
            "authority_id": who,
            "role": "platform-admin",
            "stamped_at": "2026-09-28T10:00:01Z",
            "shape_hash": hash,
            "assurance": assurance,
            "presence_nonce": "n0nce",
        }]);
    }

    fn door() -> Door {
        Door::of(&Url::parse(DOOR_ORIGIN).unwrap()).unwrap()
    }

    fn judged(p: &Value, authorisers: &[&str]) -> Result<Authorised, String> {
        let list: Vec<String> = authorisers.iter().map(|a| a.to_string()).collect();
        judge_authorisation(p, &door(), &list, Some(AUTHORISER))
    }

    /// The jobs API, held in memory: packets by id, how many times one
    /// was read, and every `enrol` completion the gateway wrote.
    #[derive(Default)]
    struct MemoryAuthorisations {
        packets: Mutex<HashMap<String, Value>>,
        reads: Mutex<usize>,
        spends: Mutex<Vec<(String, String, Value)>>,
        refuse_spend: bool,
    }

    impl MemoryAuthorisations {
        fn holding(packets: Vec<Value>) -> Self {
            let m = Self::default();
            for p in packets {
                m.packets
                    .lock()
                    .unwrap()
                    .insert(p["id"].as_str().unwrap().to_string(), p);
            }
            m
        }
    }

    #[async_trait::async_trait]
    impl EnrolmentAuthorisations for MemoryAuthorisations {
        async fn packet(&self, job_id: Uuid) -> Result<Value, String> {
            *self.reads.lock().unwrap() += 1;
            self.packets
                .lock()
                .unwrap()
                .get(&job_id.to_string())
                .cloned()
                .ok_or_else(|| format!("no packet {job_id}"))
        }

        async fn complete_enrol(
            &self,
            job_id: Uuid,
            enrol_step_id: Uuid,
            metadata: &Value,
        ) -> Result<(), String> {
            if self.refuse_spend {
                return Err("the jobs API refused the spend".into());
            }
            self.spends.lock().unwrap().push((
                job_id.to_string(),
                enrol_step_id.to_string(),
                metadata.clone(),
            ));
            Ok(())
        }
    }

    async fn body_string(resp: Response) -> String {
        let bytes = to_bytes(resp.into_body(), 256 * 1024).await.unwrap();
        String::from_utf8_lossy(&bytes).into_owned()
    }

    /// Regression (the first real enrollment, 2026-09-07): webauthn-rs's
    /// default SecurityKey flow sets credProtect=UserVerificationRequired,
    /// which the browser rejects as incongruent with the flow's
    /// userVerification=preferred BEFORE the touch. The presence-only
    /// builder must leave credProtect absent.
    #[test]
    fn enrollment_options_carry_no_cred_protect() {
        let origin = Url::parse("https://boss.test").unwrap();
        let webauthn = break_glass_webauthn("boss.test", &origin).unwrap();
        let user = Uuid::new_v5(&Uuid::NAMESPACE_OID, BREAK_GLASS_ACTOR.as_bytes());
        let (ccr, _rs) = webauthn
            .start_securitykey_registration(
                user,
                BREAK_GLASS_ACTOR,
                "BOSS break-glass",
                None,
                None,
                Some(AuthenticatorAttachment::CrossPlatform),
            )
            .unwrap();
        let cred_protect = ccr
            .public_key
            .extensions
            .as_ref()
            .and_then(|e| e.cred_protect.as_ref());
        assert!(
            cred_protect.is_none(),
            "presence-only break-glass must not request credProtect (browsers reject it pre-touch); got {cred_protect:?}"
        );
    }

    // ---- store ------------------------------------------------------

    #[test]
    fn a_missing_directory_is_an_empty_store_not_an_error() {
        let td = TempDir::new().unwrap();
        let store = load_store(&td.path().join("nope")).unwrap();
        assert!(store.is_empty());
    }

    #[test]
    fn records_round_trip_through_the_store() {
        let td = TempDir::new().unwrap();
        let rec = record_from(&yubikey_sk_value(), CredentialLabel::Primary, 7);
        write_record(td.path(), "primary.json", &rec);
        let store = load_store(td.path()).unwrap();
        assert_eq!(store.len(), 1);
        assert_eq!(store[0].credential_id, rec.credential_id);
        assert_eq!(store[0].sign_count, 7);
        assert_eq!(store[0].label, CredentialLabel::Primary);
    }

    /// A ConfigMap mount carries `..data` machinery and an operator
    /// may leave a stray note; neither is a credential.
    #[test]
    fn non_record_files_are_ignored() {
        let td = TempDir::new().unwrap();
        std::fs::write(td.path().join(".hidden.json"), "junk").unwrap();
        std::fs::write(td.path().join("README.txt"), "notes").unwrap();
        std::fs::create_dir(td.path().join("..data")).unwrap();
        assert!(load_store(td.path()).unwrap().is_empty());
    }

    /// One broken record must not brick the door the intact record
    /// still opens.
    #[test]
    fn a_malformed_record_is_skipped_not_fatal() {
        let td = TempDir::new().unwrap();
        std::fs::write(td.path().join("backup.json"), "{not json").unwrap();
        let rec = record_from(&yubikey_sk_value(), CredentialLabel::Primary, 0);
        write_record(td.path(), "primary.json", &rec);
        let store = load_store(td.path()).unwrap();
        assert_eq!(store.len(), 1, "the intact record survives");
        assert_eq!(store[0].label, CredentialLabel::Primary);
    }

    #[test]
    fn a_label_outside_primary_backup_is_refused_at_parse() {
        let mut rec = serde_json::to_value(record_from(
            &yubikey_sk_value(),
            CredentialLabel::Primary,
            0,
        ))
        .unwrap();
        rec["label"] = json!("skeleton");
        assert!(serde_json::from_value::<BreakGlassCredential>(rec).is_err());
    }

    // ---- enrollment gate --------------------------------------------

    #[test]
    fn a_break_glass_session_may_always_enroll() {
        // Rotation under the emergency session: no packet, records
        // already enrolled — the session is the authorization.
        assert!(session_may_enroll(Some("break-glass")));
    }

    #[test]
    fn no_other_session_role_may_enroll_by_its_role() {
        for role in ["platform-admin", "audit-readonly", "ceo", ""] {
            assert!(
                !session_may_enroll(Some(role)),
                "role {role:?} must not enroll a break-glass key by its role — not \
                 even platform-admin: a role is registry data (03451237 q2)"
            );
        }
        assert!(!session_may_enroll(None));
    }

    // ---- presence authorises an enrolment (design 03451237) ----------

    #[test]
    fn a_presence_approved_packet_for_this_door_authorises_its_label() {
        let a = judged(&packet("primary"), &[AUTHORISER]).expect("authorised");
        assert_eq!(a.label, CredentialLabel::Primary);
        assert_eq!(a.authorised_by, AUTHORISER);
        assert_eq!(a.job_id.to_string(), PACKET);
        assert_eq!(a.enrol_step_id.to_string(), ENROL_ID);
    }

    /// Q1: presence is the only assurance that authorises. A session
    /// sign-off is "someone signed in clicked approve".
    #[test]
    fn a_session_stamp_authorises_nothing() {
        let mut p = packet("primary");
        stamp(&mut p, AUTHORISER, "session");
        let why = judged(&p, &[AUTHORISER]).unwrap_err();
        assert!(why.contains("presence"), "{why}");
    }

    /// The stamp binds the shape it signed: content edited after the
    /// signature (here the label) leaves no live stamp — a signature
    /// for the backup key cannot enrol the primary.
    #[test]
    fn a_stamp_over_different_content_authorises_nothing() {
        let mut p = packet("backup");
        authorise_of(&mut p)["metadata"]["label"] = json!("primary");
        let why = judged(&p, &[AUTHORISER]).unwrap_err();
        assert!(why.contains("presence"), "{why}");
    }

    /// A voided stamp stays on the step as provenance and never
    /// counts again (design 87329a13) — the one rule, not a copy.
    #[test]
    fn a_voided_stamp_authorises_nothing() {
        let mut p = packet("primary");
        authorise_of(&mut p)["sign_offs"][0]["voided_at"] = json!("2026-09-28T10:05:00Z");
        assert!(judged(&p, &[AUTHORISER]).is_err());
    }

    /// Q2: who may authorise is a NAMED LIST on the gateway, not a
    /// role. A presence stamp by anyone off the list is refused, and
    /// the refusal names both the signer and the list.
    #[test]
    fn a_stamp_by_someone_off_the_list_authorises_nothing() {
        let mut p = packet("primary");
        stamp(&mut p, "emp-someone-else", "presence");
        let why = judge_authorisation(
            &p,
            &door(),
            &[AUTHORISER.to_string()],
            Some("emp-someone-else"),
        )
        .unwrap_err();
        assert!(why.contains("emp-someone-else"), "{why}");
        assert!(why.contains(AUTHORISER), "{why}");
    }

    #[test]
    fn the_authoriser_list_is_named_ids_and_empty_is_nobody() {
        assert_eq!(
            parse_authorisers(" emp-david , emp-second,,"),
            vec!["emp-david".to_string(), "emp-second".to_string()]
        );
        assert!(parse_authorisers("").is_empty());
        assert!(parse_authorisers(" , ").is_empty());
    }

    /// An empty list is nobody, and says so — never "anyone".
    #[test]
    fn an_empty_authoriser_list_authorises_nobody() {
        let why = judged(&packet("primary"), &[]).unwrap_err();
        assert!(why.contains("BOSS_BREAK_GLASS_AUTHORISERS"), "{why}");
    }

    /// The key is touched from the signer's own session. The packet id
    /// is no secret — it is on the board — so without this anyone who
    /// saw an approved packet could spend it on a key of their own.
    #[test]
    fn the_enrolment_is_made_from_the_signers_own_session() {
        let list = [AUTHORISER.to_string()];
        let why = judge_authorisation(&packet("primary"), &door(), &list, Some("emp-bystander"))
            .unwrap_err();
        assert!(why.contains(AUTHORISER), "{why}");
        let why = judge_authorisation(&packet("primary"), &door(), &list, None).unwrap_err();
        assert!(why.contains("session"), "{why}");
    }

    #[test]
    fn a_rejected_decision_authorises_nothing() {
        let mut p = packet("primary");
        authorise_of(&mut p)["metadata"]["decision"] = json!("rejected");
        stamp(&mut p, AUTHORISER, "presence");
        let why = judged(&p, &[AUTHORISER]).unwrap_err();
        assert!(why.contains("rejected"), "{why}");
    }

    #[test]
    fn an_authorise_step_not_yet_completed_authorises_nothing() {
        let mut p = packet("primary");
        authorise_of(&mut p)["status"] = json!("active");
        let why = judged(&p, &[AUTHORISER]).unwrap_err();
        assert!(why.contains("not approved yet"), "{why}");
    }

    /// THE INCIDENT THIS PACKET WAS FILED FOR (1c4c100a): a credential
    /// is bound to its relying party. An authorisation for another
    /// door's id enrols nothing here, and says which two ids differ.
    #[test]
    fn an_authorisation_for_another_relying_party_authorises_nothing() {
        let mut p = packet("primary");
        authorise_of(&mut p)["metadata"]["rp_id"] = json!("playground.test");
        stamp(&mut p, AUTHORISER, "presence");
        let why = judged(&p, &[AUTHORISER]).unwrap_err();
        assert!(why.contains("playground.test"), "{why}");
        assert!(why.contains(DOOR_RP), "{why}");

        let mut p = packet("primary");
        authorise_of(&mut p)["metadata"]["origin"] = json!("https://playground.test");
        stamp(&mut p, AUTHORISER, "presence");
        let why = judged(&p, &[AUTHORISER]).unwrap_err();
        assert!(why.contains("https://playground.test"), "{why}");
    }

    #[test]
    fn a_packet_that_is_not_open_authorises_nothing() {
        // Review F1: a cancelled packet keeps its ready `enrol` step, so
        // without this it still authorised.
        for status in ["cancelled", "closed", "draft"] {
            let mut p = packet("primary");
            p["status"] = json!(status);
            let why = judged(&p, &[AUTHORISER]).unwrap_err();
            assert!(why.contains(status), "{status}: {why}");
        }
    }

    #[test]
    fn a_packet_of_another_protocol_authorises_nothing() {
        let mut p = packet("primary");
        p["kind"] = json!("approval");
        let why = judged(&p, &[AUTHORISER]).unwrap_err();
        assert!(why.contains(AUTHORISATION_KIND), "{why}");
    }

    #[test]
    fn a_label_outside_primary_backup_authorises_nothing() {
        let mut p = packet("primary");
        authorise_of(&mut p)["metadata"]["label"] = json!("skeleton");
        stamp(&mut p, AUTHORISER, "presence");
        assert!(judged(&p, &[AUTHORISER]).is_err());
    }

    /// Q3: single-use by record. A packet whose `enrol` step already
    /// names a credential is refused, and the refusal names it.
    #[test]
    fn a_spent_authorisation_is_refused_naming_the_credential() {
        let mut p = packet("primary");
        p["steps"][1]["metadata"] = json!({
            "credential_id": "Zm9yZ2Vk",
            "enrolled_at": "2026-09-28T10:07:00Z",
        });
        let why = judged(&p, &[AUTHORISER]).unwrap_err();
        assert!(why.contains("Zm9yZ2Vk"), "{why}");

        let mut p = packet("primary");
        p["steps"][1]["status"] = json!("completed");
        assert!(judged(&p, &[AUTHORISER]).is_err());
    }

    // ---- the relying party a record was enrolled under (1c4c100a) ----

    /// The durable half of the incident: a record says which relying
    /// party it was enrolled for, so a door that answers as another
    /// can be noticed without anyone touching a key.
    #[test]
    fn a_record_carries_the_relying_party_it_was_enrolled_under() {
        let rec = new_record(
            &yubikey_sk_value(),
            Uuid::nil().to_string(),
            CredentialLabel::Backup,
            &door(),
            Utc::now(),
        );
        assert_eq!(rec.rp_id.as_deref(), Some(DOOR_RP));
        assert_eq!(rec.label, CredentialLabel::Backup);
        let json = serde_json::to_value(&rec).unwrap();
        assert_eq!(
            json["rp_id"],
            json!(DOOR_RP),
            "the committed record carries it"
        );
    }

    /// Records committed before the field existed still load — they
    /// read as UNRECORDED, never as "this door".
    #[test]
    fn a_record_from_before_the_field_reads_as_unrecorded() {
        let mut v = serde_json::to_value(record_from(
            &yubikey_sk_value(),
            CredentialLabel::Primary,
            0,
        ))
        .unwrap();
        v.as_object_mut().unwrap().remove("rp_id");
        let rec: BreakGlassCredential = serde_json::from_value(v).unwrap();
        assert_eq!(rec.rp_id, None);
        assert_eq!(binding(&rec, DOOR_RP), Binding::Unrecorded);
    }

    #[test]
    fn binding_names_the_relying_party_a_record_answers() {
        let mut rec = record_from(&yubikey_sk_value(), CredentialLabel::Primary, 0);
        assert_eq!(binding(&rec, DOOR_RP), Binding::ThisDoor);
        rec.rp_id = Some("playground.test".into());
        assert_eq!(
            binding(&rec, DOOR_RP),
            Binding::Elsewhere("playground.test".into())
        );
    }

    // ---- attestation policy (Q1) ------------------------------------

    #[test]
    fn a_basic_attestation_chain_is_accepted() {
        assert!(require_attested_hardware(&yubikey_sk_value()).is_ok());
    }

    #[test]
    fn none_attestation_is_rejected() {
        let mut sk = yubikey_sk_value();
        sk["cred"]["attestation"]["data"] = json!("None");
        let err = require_attested_hardware(&sk).unwrap_err();
        assert!(err.contains("attestation required"), "{err}");
    }

    /// Surrogate self-attestation proves possession of the credential
    /// key, not of vendor hardware — a software authenticator can
    /// produce it freely.
    #[test]
    fn self_and_uncertain_attestation_are_rejected() {
        for variant in ["Self_", "Uncertain", "ECDAA"] {
            let mut sk = yubikey_sk_value();
            sk["cred"]["attestation"]["data"] = json!(variant);
            assert!(
                require_attested_hardware(&sk).is_err(),
                "{variant} must be refused"
            );
        }
    }

    /// The BE flag is what every synced passkey sets: the private key
    /// can leave the device. Attestation form does not matter then.
    #[test]
    fn a_backup_eligible_credential_is_rejected_even_with_a_chain() {
        let mut sk = yubikey_sk_value();
        sk["cred"]["backup_eligible"] = json!(true);
        let err = require_attested_hardware(&sk).unwrap_err();
        assert!(err.contains("backup-eligible"), "{err}");
    }

    /// Fail closed: a shape this check does not recognize is a
    /// refusal, not a shrug.
    #[test]
    fn unrecognized_shapes_are_refused() {
        assert!(require_attested_hardware(&json!({})).is_err());
        let mut sk = yubikey_sk_value();
        sk["cred"]["attestation"]["data"] = json!({ "Novel": [] });
        assert!(require_attested_hardware(&sk).is_err());
        // A missing backup_eligible is not "false".
        let mut sk = yubikey_sk_value();
        sk["cred"]
            .as_object_mut()
            .unwrap()
            .remove("backup_eligible");
        assert!(require_attested_hardware(&sk).is_err());
    }

    // ---- counters ---------------------------------------------------

    #[test]
    fn effective_counter_is_the_highest_floor_available() {
        assert_eq!(effective_counter(5, 2, None), 5, "manifest wins");
        assert_eq!(effective_counter(1, 9, None), 9, "serialized wins");
        assert_eq!(effective_counter(3, 2, Some(11)), 11, "process memory wins");
        assert_eq!(effective_counter(0, 0, None), 0, "counter-less key");
    }

    #[test]
    fn counter_regression_is_the_clone_signal() {
        assert!(counter_regressed(5, 5), "no advance = possible clone");
        assert!(counter_regressed(5, 3), "backwards = possible clone");
        assert!(!counter_regressed(5, 6), "advancing is healthy");
        assert!(
            !counter_regressed(0, 0),
            "a counter-less authenticator carries no signal"
        );
        assert!(!counter_regressed(0, 1), "first count on a fresh record");
    }

    /// The floor actually reaches the credential handed to webauthn:
    /// the SecurityKey built for authentication carries the merged
    /// counter, which is what makes the crate's regression check
    /// enforce OUR floor rather than a stale one.
    #[test]
    fn security_keys_carry_the_merged_counter_floor() {
        let td = TempDir::new().unwrap();
        // Serialized counter inside public_key is 2; manifest says 40.
        let rec = record_from(&yubikey_sk_value(), CredentialLabel::Primary, 40);
        write_record(td.path(), "primary.json", &rec);
        let state = state_with(td.path());
        // And the process has seen 90 since.
        lock_recover(&state.counters).insert(rec.credential_id.clone(), 90);
        let keys = state.security_keys(&load_store(td.path()).unwrap());
        assert_eq!(keys.len(), 1);
        let v = serde_json::to_value(&keys[0]).unwrap();
        assert_eq!(v["cred"]["counter"], json!(90));
    }

    // ---- session mint -----------------------------------------------

    #[test]
    fn the_minted_session_is_narrow_and_expiring() {
        let (set_cookie, sess) = mint_session(KEY);
        assert_eq!(sess.role.as_deref(), Some("break-glass"));
        assert_eq!(sess.username, BREAK_GLASS_ACTOR);
        assert!(
            sess.employee_id.is_none(),
            "the emergency session must not depend on (or invent) an employee row"
        );
        assert_eq!(sess.access_tier(), "user", "no tier elevation rides along");
        assert!(set_cookie.contains("HttpOnly"));
        assert!(set_cookie.contains(&format!("Max-Age={BREAK_GLASS_TTL_SECONDS}")));
        // And it decodes as a valid session under the same key.
        let cookie_value = set_cookie
            .split(';')
            .next()
            .unwrap()
            .split_once('=')
            .unwrap()
            .1;
        let decoded = Session::decode(cookie_value, KEY).unwrap();
        assert_eq!(decoded.role.as_deref(), Some("break-glass"));
    }

    // ---- handlers: the refusal and happy-begin paths ----------------

    fn begin_body(v: Value) -> Json<EnrollBeginBody> {
        Json(serde_json::from_value(v).unwrap())
    }

    /// Q4: the bootstrap token retired. An empty store and a token in
    /// the body — the old window's exact shape — open nothing, and the
    /// refusal names the door that replaced it.
    #[tokio::test]
    async fn the_retired_bootstrap_token_opens_nothing() {
        let td = TempDir::new().unwrap();
        let state = state_with(td.path());
        let resp = enroll_begin(
            State(state),
            HeaderMap::new(),
            begin_body(json!({ "token": "t0k3n" })),
        )
        .await;
        assert_eq!(resp.status(), StatusCode::FORBIDDEN);
        let why = body_string(resp).await;
        assert!(why.contains(AUTHORISATION_KIND), "{why}");
    }

    #[tokio::test]
    async fn a_presence_authorised_begin_asks_for_direct_attestation() {
        let td = TempDir::new().unwrap();
        let port = Arc::new(MemoryAuthorisations::holding(vec![packet("primary")]));
        let state = state_with_port(td.path(), port, &[AUTHORISER]);
        let resp = enroll_begin(
            State(state),
            employee_headers(AUTHORISER),
            begin_body(json!({ "authorisation": PACKET })),
        )
        .await;
        assert_eq!(resp.status(), StatusCode::OK);
        let v: Value = serde_json::from_str(&body_string(resp).await).unwrap();
        assert!(v["challenge_id"].is_string());
        assert_eq!(v["label"], json!("primary"), "the label is the packet's");
        assert_eq!(
            v["options"]["publicKey"]["attestation"],
            json!("direct"),
            "Q1: the ceremony must ask the authenticator to convey its statement"
        );
    }

    /// The repair the old window forced — empty the store first — is
    /// gone: a presence-authorised enrolment proceeds with keys already
    /// enrolled, so the door is never without a record.
    #[tokio::test]
    async fn a_presence_authorised_begin_needs_no_empty_store() {
        let td = TempDir::new().unwrap();
        let rec = record_from(&yubikey_sk_value(), CredentialLabel::Primary, 0);
        write_record(td.path(), "primary.json", &rec);
        let port = Arc::new(MemoryAuthorisations::holding(vec![packet("primary")]));
        let state = state_with_port(td.path(), port, &[AUTHORISER]);
        let resp = enroll_begin(
            State(state),
            employee_headers(AUTHORISER),
            begin_body(json!({ "authorisation": PACKET })),
        )
        .await;
        assert_eq!(resp.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn a_spent_authorisation_is_refused_at_begin() {
        let td = TempDir::new().unwrap();
        let mut p = packet("backup");
        p["steps"][1]["metadata"] = json!({ "credential_id": "c3BlbnQ" });
        let port = Arc::new(MemoryAuthorisations::holding(vec![p]));
        let state = state_with_port(td.path(), port, &[AUTHORISER]);
        let resp = enroll_begin(
            State(state),
            employee_headers(AUTHORISER),
            begin_body(json!({ "authorisation": PACKET })),
        )
        .await;
        assert_eq!(resp.status(), StatusCode::FORBIDDEN);
        assert!(body_string(resp).await.contains("c3BlbnQ"));
    }

    #[tokio::test]
    async fn an_unreadable_authorisation_is_refused_by_name() {
        let td = TempDir::new().unwrap();
        let state = state_with(td.path());
        let resp = enroll_begin(
            State(state),
            employee_headers(AUTHORISER),
            begin_body(json!({ "authorisation": "not-a-packet-id" })),
        )
        .await;
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    }

    /// A challenge begun under one authorisation is finished under that
    /// one: a second approved packet cannot be swapped in between.
    #[tokio::test]
    async fn a_challenge_is_finished_under_the_authorisation_it_began_with() {
        let td = TempDir::new().unwrap();
        let mut other = packet("backup");
        other["id"] = json!(OTHER_PACKET);
        let port = Arc::new(MemoryAuthorisations::holding(vec![
            packet("primary"),
            other,
        ]));
        let state = state_with_port(td.path(), port, &[AUTHORISER]);
        let begun = enroll_begin(
            State(state.clone()),
            employee_headers(AUTHORISER),
            begin_body(json!({ "authorisation": PACKET })),
        )
        .await;
        let v: Value = serde_json::from_str(&body_string(begun).await).unwrap();
        let finish: EnrollFinishBody = serde_json::from_value(json!({
            "challenge_id": v["challenge_id"],
            "authorisation": OTHER_PACKET,
            "label": "backup",
            "credential": {
                "id": "x", "rawId": "eA", "type": "public-key",
                "response": { "attestationObject": "eA", "clientDataJSON": "eA" },
            },
        }))
        .unwrap();
        let resp = enroll_finish(State(state), employee_headers(AUTHORISER), Json(finish)).await;
        assert_eq!(resp.status(), StatusCode::FORBIDDEN);
        assert!(body_string(resp).await.contains(PACKET));
    }

    /// Q3: the spend is written BEFORE the record is handed out, and a
    /// spend the jobs API refuses hands out nothing — a record with no
    /// spend on the packet would make the authorisation reusable.
    #[tokio::test]
    async fn the_spend_is_recorded_before_the_record_is_emitted() {
        let a = judged(&packet("primary"), &[AUTHORISER]).unwrap();
        let rec = new_record(
            &yubikey_sk_value(),
            Uuid::nil().to_string(),
            a.label,
            &door(),
            Utc::now(),
        );
        let port = MemoryAuthorisations::default();
        record_spend(&port, &a, &rec).await.expect("spent");
        let spends = port.spends.lock().unwrap().clone();
        assert_eq!(spends.len(), 1);
        let (job, step, spend) = &spends[0];
        assert_eq!(job, PACKET);
        assert_eq!(step, ENROL_ID);
        assert_eq!(spend["credential_id"], json!(rec.credential_id));
        assert_eq!(spend["label"], json!("primary"));
        assert_eq!(spend["rp_id"], json!(DOOR_RP));
        assert!(spend["enrolled_at"].is_string());
        // Review F2: the completion carries EVERY key the step already
        // held, so the one PUT the adapter makes is accepted whole — the
        // step's PUT refuses a body that omits a stored key.
        assert_eq!(
            spend["procedure"],
            json!("Completed by the gateway."),
            "the stored keys ride with the spend"
        );

        let refusing = MemoryAuthorisations {
            refuse_spend: true,
            ..Default::default()
        };
        assert!(record_spend(&refusing, &a, &rec).await.is_err());
    }

    /// Backlog 4a173252: the enrol step is the durable copy of the
    /// record, so EVERY field of it lands there, byte for byte as the
    /// manifest holds it. On 2026-09-28 the step kept five keys and the
    /// two a committed record verifies with (`public_key`, `sign_count`)
    /// lived only in the 201 body and one log line in a pod the next
    /// converge replaced — both records were copied by hand from
    /// `kubectl logs` before the roll.
    ///
    /// The field list is spelled out, not derived, on purpose (trust
    /// boundary, credentials): a field added to the record reaches the
    /// jobs API only after someone edits this list and says it is
    /// public, as Q5 says every present field is.
    #[tokio::test]
    async fn the_enrol_step_carries_every_field_of_the_record() {
        let a = judged(&packet("primary"), &[AUTHORISER]).unwrap();
        let rec = new_record(
            &yubikey_sk_value(),
            Uuid::nil().to_string(),
            a.label,
            &door(),
            Utc::now(),
        );
        let port = MemoryAuthorisations::default();
        record_spend(&port, &a, &rec).await.expect("spent");
        let (_, _, spend) = port.spends.lock().unwrap()[0].clone();

        let whole = serde_json::to_value(&rec).unwrap();
        let fields: Vec<&str> = whole
            .as_object()
            .unwrap()
            .keys()
            .map(String::as_str)
            .collect();
        let mut expected = vec![
            "aaguid",
            "credential_id",
            "enrolled_at",
            "label",
            "public_key",
            "rp_id",
            "sign_count",
        ];
        expected.sort_unstable();
        let mut got = fields.clone();
        got.sort_unstable();
        assert_eq!(got, expected, "the record's fields, each one public (Q5)");
        for field in fields {
            assert_eq!(
                spend[field], whole[field],
                "`{field}` lands on the enrol step exactly as the record carries it"
            );
        }
        // And the step's copy IS a record: the commit car reads it back
        // off the packet rather than out of a pod log.
        let back: BreakGlassCredential = serde_json::from_value(spend.clone()).unwrap();
        assert_eq!(back.public_key, rec.public_key);
        assert_eq!(back.sign_count, rec.sign_count);
    }

    /// A stub jobs API: every request's method, path and body, and the
    /// status to answer the step write with.
    async fn stub_jobs(
        step_status: StatusCode,
    ) -> (String, Arc<Mutex<Vec<(String, String, Value)>>>) {
        let seen: Arc<Mutex<Vec<(String, String, Value)>>> = Arc::default();
        let log = seen.clone();
        let app = axum::Router::new().fallback(
            move |method: axum::http::Method, uri: axum::http::Uri, body: axum::body::Bytes| {
                let log = log.clone();
                async move {
                    let v = serde_json::from_slice(&body).unwrap_or(Value::Null);
                    log.lock()
                        .unwrap()
                        .push((method.to_string(), uri.path().to_string(), v));
                    step_status
                }
            },
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        (format!("http://{addr}"), seen)
    }

    /// Review F2: the spend is ONE write — the step PUT carrying
    /// `status: completed` and the whole metadata — so the terminal
    /// freeze is the compare-and-set. Two writes (a merge, then a close
    /// that only logged when refused) left a credential on a still-open
    /// step that any step writer could null back out and re-arm.
    #[tokio::test]
    async fn the_http_spend_is_one_completing_put() {
        let (base, seen) = stub_jobs(StatusCode::OK).await;
        let adapter = JobsApiAuthorisations {
            http: crate::machine_client::MachineClient::build(reqwest::Client::builder()).unwrap(),
            jobs_base: base,
        };
        let job = Uuid::parse_str(PACKET).unwrap();
        let step = Uuid::parse_str(ENROL_ID).unwrap();
        let metadata = json!({ "credential_id": "Y3JlZA", "procedure": "kept" });
        adapter
            .complete_enrol(job, step, &metadata)
            .await
            .expect("written");
        let seen = seen.lock().unwrap().clone();
        assert_eq!(seen.len(), 1, "exactly one write: {seen:?}");
        let (method, path, body) = &seen[0];
        assert_eq!(method, "PUT");
        assert_eq!(path, &format!("/api/jobs/{PACKET}/steps/{ENROL_ID}"));
        assert_eq!(body["status"], json!("completed"));
        assert_eq!(body["metadata"], metadata);
    }

    /// And a refused completion is FATAL: no record is issued on a spend
    /// the record does not hold.
    #[tokio::test]
    async fn a_refused_completion_is_fatal() {
        let (base, _seen) = stub_jobs(StatusCode::CONFLICT).await;
        let adapter = JobsApiAuthorisations {
            http: crate::machine_client::MachineClient::build(reqwest::Client::builder()).unwrap(),
            jobs_base: base,
        };
        let refused = adapter
            .complete_enrol(
                Uuid::parse_str(PACKET).unwrap(),
                Uuid::parse_str(ENROL_ID).unwrap(),
                &json!({}),
            )
            .await;
        assert!(refused.is_err());
    }

    /// Review F3: a caller who is not an authoriser learns nothing about
    /// a packet — it is refused before the gateway reads one, so the
    /// refusal cannot carry the packet's kind, decision or signer.
    #[tokio::test]
    async fn a_caller_off_the_list_reads_no_packet() {
        let td = TempDir::new().unwrap();
        let port = Arc::new(MemoryAuthorisations::holding(vec![packet("primary")]));
        let state = state_with_port(td.path(), port.clone(), &[AUTHORISER]);
        for headers in [HeaderMap::new(), employee_headers("emp-bystander")] {
            let resp = enroll_begin(
                State(state.clone()),
                headers,
                begin_body(json!({ "authorisation": PACKET })),
            )
            .await;
            assert_eq!(resp.status(), StatusCode::FORBIDDEN);
            let why = body_string(resp).await;
            assert!(!why.contains(AUTHORISER), "no signer leaks: {why}");
        }
        assert_eq!(*port.reads.lock().unwrap(), 0, "no packet was read");
    }

    /// Review F4: a break-glass session's rotation never waits behind a
    /// presence enrolment's lock — the lock guards the packet spend, and
    /// a hung jobs API must not hold the emergency session's own door.
    #[tokio::test]
    async fn a_break_glass_rotation_is_not_held_behind_the_enrolment_lock() {
        let td = TempDir::new().unwrap();
        let state = state_with(td.path());
        let _held = state.enrol_lock.lock().await;
        let finish: EnrollFinishBody = serde_json::from_value(json!({
            "challenge_id": "never-begun",
            "label": "backup",
            "credential": {
                "id": "x", "rawId": "eA", "type": "public-key",
                "response": { "attestationObject": "eA", "clientDataJSON": "eA" },
            },
        }))
        .unwrap();
        let resp = tokio::time::timeout(
            Duration::from_secs(5),
            enroll_finish(State(state.clone()), break_glass_headers(), Json(finish)),
        )
        .await
        .expect("the session's finish did not wait on the lock");
        assert_eq!(resp.status(), StatusCode::GONE);
    }

    /// After the first record lands a break-glass session still enrolls
    /// (the backup / rotation path) — and the enrolled key is excluded
    /// from re-enrollment.
    #[tokio::test]
    async fn a_break_glass_session_enrolls_with_its_keys_excluded() {
        let td = TempDir::new().unwrap();
        let rec = record_from(&yubikey_sk_value(), CredentialLabel::Primary, 0);
        write_record(td.path(), "primary.json", &rec);
        let state = state_with(td.path());

        let denied = enroll_begin(
            State(state.clone()),
            HeaderMap::new(),
            begin_body(json!({})),
        )
        .await;
        assert_eq!(denied.status(), StatusCode::FORBIDDEN);

        let allowed =
            enroll_begin(State(state), break_glass_headers(), begin_body(json!({}))).await;
        assert_eq!(allowed.status(), StatusCode::OK);
        let v: Value = serde_json::from_str(&body_string(allowed).await).unwrap();
        let excluded = v["options"]["publicKey"]["excludeCredentials"]
            .as_array()
            .expect("enrolled credentials are excluded");
        assert_eq!(excluded.len(), 1);
        assert_eq!(excluded[0]["id"], json!(rec.credential_id));
    }

    #[tokio::test]
    async fn assert_begin_with_no_credentials_is_a_conflict() {
        let td = TempDir::new().unwrap();
        let state = state_with(td.path());
        let resp = assert_begin(State(state)).await;
        assert_eq!(resp.status(), StatusCode::CONFLICT);
    }

    /// THE SILENT FAILURE, SPOKEN (1c4c100a): when every enrolled key
    /// is bound to another relying party, the door says so — which keys,
    /// which party, which this door is — instead of offering keys the
    /// authenticator will refuse with a browser error that names none.
    #[tokio::test]
    async fn keys_bound_to_another_relying_party_are_named_not_offered() {
        let td = TempDir::new().unwrap();
        let mut primary = record_from(&yubikey_sk_value(), CredentialLabel::Primary, 4);
        primary.rp_id = Some("playground.test".into());
        write_record(td.path(), "primary.json", &primary);
        let state = state_with(td.path());
        let resp = assert_begin(State(state)).await;
        assert_eq!(resp.status(), StatusCode::CONFLICT);
        let why = body_string(resp).await;
        assert!(why.contains("primary"), "{why}");
        assert!(why.contains("playground.test"), "{why}");
        assert!(why.contains(DOOR_RP), "{why}");
    }

    #[tokio::test]
    async fn only_keys_bound_to_this_door_are_offered() {
        let td = TempDir::new().unwrap();
        let mut stale = record_from(&yubikey_sk_value(), CredentialLabel::Primary, 4);
        stale.rp_id = Some("playground.test".into());
        write_record(td.path(), "primary.json", &stale);
        let mut sk2 = yubikey_sk_value();
        sk2["cred"]["cred_id"] = json!(URL_SAFE_NO_PAD.encode([9u8; 32]));
        let fresh = record_from(&sk2, CredentialLabel::Backup, 0);
        write_record(td.path(), "backup.json", &fresh);

        let state = state_with(td.path());
        let resp = assert_begin(State(state)).await;
        assert_eq!(resp.status(), StatusCode::OK);
        let v: Value = serde_json::from_str(&body_string(resp).await).unwrap();
        let ids: Vec<&str> = v["options"]["publicKey"]["allowCredentials"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|c| c["id"].as_str())
            .collect();
        assert_eq!(ids, vec![fresh.credential_id.as_str()]);
    }

    #[tokio::test]
    async fn assert_begin_offers_every_enrolled_key() {
        let td = TempDir::new().unwrap();
        // Q2: two records, either sufficient — both must be offered.
        let primary = record_from(&yubikey_sk_value(), CredentialLabel::Primary, 3);
        write_record(td.path(), "primary.json", &primary);
        let mut sk2 = yubikey_sk_value();
        sk2["cred"]["cred_id"] = json!(URL_SAFE_NO_PAD.encode([9u8; 32]));
        let backup = record_from(&sk2, CredentialLabel::Backup, 0);
        write_record(td.path(), "backup.json", &backup);

        let state = state_with(td.path());
        let resp = assert_begin(State(state)).await;
        assert_eq!(resp.status(), StatusCode::OK);
        let v: Value = serde_json::from_str(&body_string(resp).await).unwrap();
        let allow = v["options"]["publicKey"]["allowCredentials"]
            .as_array()
            .expect("allowCredentials present");
        let ids: Vec<&str> = allow.iter().filter_map(|c| c["id"].as_str()).collect();
        assert_eq!(ids.len(), 2);
        assert!(ids.contains(&primary.credential_id.as_str()));
        assert!(ids.contains(&backup.credential_id.as_str()));
    }

    /// A challenge is single-use and unknown ids answer GONE — the
    /// replay surface of the ceremony state.
    #[tokio::test]
    async fn a_spent_or_unknown_challenge_is_gone() {
        let td = TempDir::new().unwrap();
        let state = state_with(td.path());
        let resp = assert_finish(
            State(state),
            Json(AssertFinishBody {
                challenge_id: "never-minted".into(),
                credential: serde_json::from_value(json!({
                    "id": "x", "rawId": "eA",
                    "response": {
                        "authenticatorData": "eA", "clientDataJSON": "eA",
                        "signature": "eA", "userHandle": null,
                    },
                    "type": "public-key",
                }))
                .unwrap(),
            }),
        )
        .await;
        assert_eq!(resp.status(), StatusCode::GONE);
    }

    /// Backlog 9bbfb244: a verified sign-in names, on its
    /// `auth.login.succeeded`, the credential that asserted and the
    /// label of the committed record holding it — resolved the way the
    /// promotion vouch resolves it (`label_of`), so a readout of the log
    /// can show the backup key opened the door as well as the primary.
    /// An id no committed record holds still names itself, label null.
    #[tokio::test]
    async fn a_sign_in_names_the_key_that_asserted() {
        let td = TempDir::new().unwrap();
        let primary = record_from(&yubikey_sk_value(), CredentialLabel::Primary, 3);
        write_record(td.path(), "primary.json", &primary);
        let mut sk2 = yubikey_sk_value();
        sk2["cred"]["cred_id"] = json!(URL_SAFE_NO_PAD.encode([9u8; 32]));
        let backup = record_from(&sk2, CredentialLabel::Backup, 0);
        write_record(td.path(), "backup.json", &backup);
        let cap = Arc::new(crate::audit::testing::Captured::default());
        let state = state_heard_by(
            td.path(),
            Arc::new(MemoryAuthorisations::default()),
            &[AUTHORISER],
            crate::audit::AuthAudit::spawn(cap.clone()),
        );

        state.record_sign_in(&backup.credential_id);
        state.record_sign_in("an-id-no-record-holds");

        let events = crate::audit::testing::drain(&cap, 2).await;
        assert_eq!(events.len(), 2);
        let e = &events[0];
        assert_eq!(e.kind, "auth.login.succeeded");
        assert_eq!(e.payload["email"], BREAK_GLASS_ACTOR);
        assert_eq!(e.payload["method"], "break-glass");
        assert_eq!(e.payload["credential_id"], backup.credential_id.as_str());
        assert_eq!(e.payload["label"], "backup");
        assert_eq!(events[1].payload["credential_id"], "an-id-no-record-holds");
        assert_eq!(events[1].payload.get("label"), Some(&Value::Null));
    }

    /// Backlog 9bbfb244, the other half: an assertion that does not
    /// verify records exactly what it recorded before — one
    /// `auth.login.denied`, bad_credentials — and nothing that names a
    /// key: the id on a refused assertion is the caller's claim, not a
    /// key that opened the door. No session, no success event.
    #[tokio::test]
    async fn a_refused_assertion_records_only_its_denial() {
        let td = TempDir::new().unwrap();
        let rec = record_from(&yubikey_sk_value(), CredentialLabel::Primary, 0);
        write_record(td.path(), "primary.json", &rec);
        let cap = Arc::new(crate::audit::testing::Captured::default());
        let state = state_heard_by(
            td.path(),
            Arc::new(MemoryAuthorisations::default()),
            &[AUTHORISER],
            crate::audit::AuthAudit::spawn(cap.clone()),
        );
        let begun = assert_begin(State(state.clone())).await;
        assert_eq!(begun.status(), StatusCode::OK);
        let v: Value = serde_json::from_str(&body_string(begun).await).unwrap();
        let challenge_id = v["challenge_id"].as_str().unwrap().to_string();

        let resp = assert_finish(
            State(state),
            Json(AssertFinishBody {
                challenge_id,
                credential: serde_json::from_value(json!({
                    "id": rec.credential_id, "rawId": rec.credential_id,
                    "response": {
                        "authenticatorData": "eA", "clientDataJSON": "eA",
                        "signature": "eA", "userHandle": null,
                    },
                    "type": "public-key",
                }))
                .unwrap(),
            }),
        )
        .await;
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
        assert!(resp.headers().get(header::SET_COOKIE).is_none());

        // Wait for a second event that must never come.
        let events = crate::audit::testing::drain(&cap, 2).await;
        assert_eq!(events.len(), 1, "{events:?}");
        let e = &events[0];
        assert_eq!(e.kind, "auth.login.denied");
        assert_eq!(e.payload["method"], "break-glass");
        assert_eq!(e.payload["reason"], "bad_credentials");
        assert!(e.payload.get("credential_id").is_none());
        assert!(e.payload.get("label").is_none());
    }

    /// The store is re-read per ceremony, so a record committed while
    /// the gateway runs opens the door without a restart — the kubelet
    /// half of the Q5 write path.
    #[tokio::test]
    async fn a_record_committed_after_boot_is_seen_without_restart() {
        let td = TempDir::new().unwrap();
        let state = state_with(td.path());
        assert_eq!(
            assert_begin(State(state.clone())).await.status(),
            StatusCode::CONFLICT
        );
        let rec = record_from(&yubikey_sk_value(), CredentialLabel::Primary, 0);
        write_record(td.path(), "primary.json", &rec);
        assert_eq!(assert_begin(State(state)).await.status(), StatusCode::OK);
    }

    fn boot_config(dir: &Path) -> BootConfig {
        BootConfig {
            public_url: DOOR_ORIGIN.to_string(),
            dir: dir.to_path_buf(),
            authorisers: vec![AUTHORISER.to_string()],
            // Never dialled: the boot alarm goes through the port handed in.
            jobs_base: "http://127.0.0.1:9".to_string(),
        }
    }

    /// Review F3 (5eb583c2): the BOOT raises the alarm — the door built
    /// from its config files through the port it was handed, under this
    /// door's key. Deleting the hook from `boot` fails here.
    #[tokio::test]
    async fn the_boot_files_the_alarm_for_a_door_with_no_bound_key() {
        use crate::break_glass_alarm::{Outcome, tests::MemoryAlarms};
        let td = TempDir::new().unwrap();
        let mut foreign = record_from(&yubikey_sk_value(), CredentialLabel::Primary, 4);
        foreign.rp_id = Some("playground.test".into());
        write_record(td.path(), "primary.json", &foreign);
        let port = Arc::new(MemoryAlarms::default());
        let (state, alarm) = BreakGlassState::boot(
            boot_config(td.path()),
            KEY.to_vec(),
            crate::audit::AuthAudit::disabled(),
            port.clone(),
        )
        .expect("the door boots");
        assert_eq!(state.door.rp_id, DOOR_RP);
        let out = alarm.expect("an unbound door raises").await.unwrap();
        assert_eq!(out, Outcome::Filed);
        let filed = port.filed.lock().unwrap();
        assert_eq!(filed.len(), 1);
        assert_eq!(
            filed[0]["metadata"]["estate_finding"],
            json!(format!("break_glass_unbound:{DOOR_RP}"))
        );
    }

    /// A door with a key bound to it boots without touching the jobs API.
    #[tokio::test]
    async fn the_boot_of_a_bound_door_raises_nothing() {
        use crate::break_glass_alarm::tests::MemoryAlarms;
        let td = TempDir::new().unwrap();
        let rec = record_from(&yubikey_sk_value(), CredentialLabel::Primary, 0);
        write_record(td.path(), "primary.json", &rec);
        let port = Arc::new(MemoryAlarms::default());
        let (_state, alarm) = BreakGlassState::boot(
            boot_config(td.path()),
            KEY.to_vec(),
            crate::audit::AuthAudit::disabled(),
            port.clone(),
        )
        .expect("the door boots");
        assert!(alarm.is_none());
        assert_eq!(*port.reads.lock().unwrap(), 0);
        assert!(port.filed.lock().unwrap().is_empty());
    }

    /// An arm that needs the patient is not an arm: a jobs API that
    /// refuses the filing still leaves a booted, serving door.
    #[tokio::test]
    async fn a_refused_boot_alarm_leaves_the_door_serving() {
        use crate::break_glass_alarm::{Outcome, tests::MemoryAlarms};
        let td = TempDir::new().unwrap();
        let port = Arc::new(MemoryAlarms {
            file_fails: true,
            ..Default::default()
        });
        let (state, alarm) = BreakGlassState::boot(
            boot_config(td.path()),
            KEY.to_vec(),
            crate::audit::AuthAudit::disabled(),
            port,
        )
        .expect("a failed filing never fails the boot");
        let out = alarm.expect("attempted").await.unwrap();
        assert!(matches!(out, Outcome::NotFiled(_)), "{out:?}");
        // The door answers: an empty store is a 409 naming the gap.
        let resp = assert_begin(State(Arc::new(state))).await;
        assert_eq!(resp.status(), StatusCode::CONFLICT);
    }

    /// The ceremony page is self-contained: no external script, no
    /// SPA asset, nothing that can be down when the gateway is up.
    #[tokio::test]
    async fn the_ceremony_page_is_self_contained() {
        let resp = ceremony_page().await;
        assert_eq!(resp.status(), StatusCode::OK);
        let html = body_string(resp).await;
        assert!(html.contains("/api/auth/break-glass/assert/begin"));
        assert!(html.contains("/api/auth/break-glass/enroll/begin"));
        assert!(
            html.contains("id=\"authorisation\"") && !html.contains("token"),
            "the page asks for the authorisation packet; the bootstrap token retired"
        );
        assert!(
            !html.contains("src=\"http") && !html.contains("href=\"http"),
            "the page must not reference any external asset"
        );
    }
}
