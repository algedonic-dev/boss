//! The gateway joins the log — auth events for the edge
//! (Q1+Q2 resolved 2026-08-11; folded into
//! docs/architecture-decisions.md §Policy & auth).
//!
//! Q1: the gateway stages events on the transactional outbox through
//! its own small pool (recipe 3 of transactional-audit-log.md — the
//! record-only shape, `PgOutboxRecorder`), because a session is a
//! cookie, not a row: there is no domain write for the event to join,
//! and what the pool buys is membership in the one pipeline — durable
//! staging, relay ordering, replay, the ref-check trigger — not
//! atomicity. The pool connects to the service database
//! ([`AUDIT_SINK_URL_VAR`]); the INSERT-only role Q1 first chose
//! (111-gateway-audit-events.sql) is retired, and why is on that
//! constant.
//!
//! Q2: three kinds, registered in `event_kinds` with source
//! `gateway`. `auth.login.denied` carries a closed reason and NO
//! subject reference — no employee matched, and asserting one would
//! both lie and trip the ref-check. IdP *transport* failures
//! (discovery down, token exchange) are not auth decisions and stay
//! plain warn lines in the handlers. A Class registry that cannot
//! confirm a verified employee's role IS one: the person proved who
//! they are and is refused a session because of it, so that 503 is
//! `reason: role_unconfirmed` (backlog 8f45e0b4). `auth.login.succeeded`
//! carries the method so the imminent passkey path lands as an enum
//! value, not a schema change, and a `downgrade: {from, to}` when the
//! session carries less than the employee row asked for.
//! `auth.session.guest` counts mints of the unauthenticated read-only
//! capability.
//!
//! The auth-admin doors joined later (backlog 17ae5248, 2026-09-25):
//! `auth.credential.written` (onboard: created or overwritten) and
//! `auth.reset.issued` (issue-reset), each naming the target email and
//! the acting session — never a password or a token.
//!
//! `auth.session.elevated` joined with the owner's passkey elevation
//! (backlog 3c92c5b8, 2026-09-27): the platform owner's session raised
//! to the operator tier by a verified assertion from an operator-tier
//! key — the one change of trust a live session can undergo, and the one
//! emission RECORDED before its act rather than staged beside it. With
//! it, `auth.passkey.enrolled`: a self-service enrolment, the act the
//! adversarial review found left no trace (car 0bde9b99, H1).
//!
//! Failure posture (the LogTransport principle): emitting never
//! blocks and never fails a login. The handler hands the event to a
//! bounded channel; a background task stages it. Channel full,
//! channel gone, no pool configured, or the INSERT failing all
//! degrade to the structured `tracing::warn!` that was the record
//! before this module existed — never to silence.
//!
//! Timestamps are wall-clock, minted at staging time — sim time is
//! retired from the record (David, 2026-08-22, packet a7a4cae5), and
//! an auth decision is real-world activity in any clock mode. The
//! handler never awaits anything to stamp.

use std::sync::Arc;

use boss_core::event::Event;
use boss_core::port::EventRecorder;
use serde_json::json;

/// How a login was attempted. `as_str` values are payload vocabulary
/// — append variants, never rename them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AuthMethod {
    Password,
    Oidc,
    /// The hardware-key WebAuthn ceremony at the gateway's own
    /// verifier (docs/design/break-glass-is-a-key-you-hold.md).
    BreakGlass,
}

impl AuthMethod {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Password => "password",
            Self::Oidc => "oidc",
            Self::BreakGlass => "break-glass",
        }
    }
}

/// Why a login was denied. Closed set (Q2): an authentication
/// *decision* about a person — transport failures don't belong here.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeniedReason {
    /// The credential itself failed verification (local 401).
    BadCredentials,
    /// The credential (or IdP assertion) verified, but no employee
    /// record matches — the fail-closed path.
    NoEmployeeRecord,
    /// The IdP itself refused the user (`error=access_denied`).
    IdpDenied,
    /// The credential verified and an employee record matched, but the
    /// Class registry could not say whether the row's tenant role is a
    /// live `role` Class, so no session was minted (the 503). The
    /// registry's silence is a transport failure; refusing the person
    /// because of it is a decision about them, and until backlog
    /// 8f45e0b4 (2026-09-27) it left only a warn line.
    RoleUnconfirmed,
}

impl DeniedReason {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::BadCredentials => "bad_credentials",
            Self::NoEmployeeRecord => "no_employee_record",
            Self::IdpDenied => "idp_denied",
            Self::RoleUnconfirmed => "role_unconfirmed",
        }
    }
}

/// A session minted with less than its employee row asked for: the row
/// held `from`, and the session carries `to` (the visitor). Rides the
/// `auth.login.succeeded` event as `downgrade: {from, to}` — until
/// backlog 8f45e0b4 (2026-09-27) a downgrade was a warn line only, so the
/// log said an employee signed in and never that they signed in unable
/// to write.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Downgrade<'a> {
    pub from: &'a str,
    pub to: &'a str,
}

/// Who performed an auth-administration act, read off the caller's
/// verified session — never off the request body, which is not
/// evidence of anything.
#[derive(Debug, Clone, Copy)]
pub struct AdminActor<'a> {
    /// The session's username (the admin's email).
    pub email: &'a str,
    pub employee_id: Option<&'a str>,
    pub role: Option<&'a str>,
}

impl AdminActor<'_> {
    /// `actor`, plus `actor_employee_id` / `actor_role` when the
    /// session carries them — omitted rather than asserted as null.
    fn stamp(self, payload: &mut serde_json::Value) {
        payload["actor"] = json!(self.email);
        if let Some(id) = self.employee_id {
            payload["actor_employee_id"] = json!(id);
        }
        if let Some(role) = self.role {
            payload["actor_role"] = json!(role);
        }
    }
}

/// What an elevation event names — see [`AuthAudit::session_elevated`].
#[derive(Debug, Clone, Copy)]
pub struct Elevation<'a> {
    pub email: &'a str,
    pub employee_id: &'a str,
    pub elevated_at: u64,
    pub expires_at: u64,
    pub credential_label: &'a str,
    pub credential_registered_at: &'a str,
}

/// Depth of the hand-off channel. Logins are a handful per user per
/// day against a relay that drains thousands of rows a second; if
/// this ever fills, the DB is down and the warn backstop is the
/// honest record anyway.
const QUEUE_DEPTH: usize = 256;

/// A staged emission: kind + payload. The drain task adds the
/// clock-routed timestamp and the `gateway` source when it builds
/// the [`Event`].
type Staged = (&'static str, serde_json::Value);

/// The gateway's auth-event emitter. Cheap to clone; handlers call
/// the `login_*`/`guest_*` methods inline and never wait on the
/// database or the clock.
#[derive(Clone)]
pub struct AuthAudit {
    tx: Option<tokio::sync::mpsc::Sender<Staged>>,
    /// The same recorder the drain task owns, for the one emission that
    /// must be RECORDED before the act it describes may happen — a
    /// session elevation (review of car 0bde9b99, L2: no silent grant).
    /// Every other emission stays fire-and-forget.
    recorder: Option<Arc<dyn EventRecorder>>,
}

impl AuthAudit {
    /// No staging path configured. Every emit degrades to the
    /// structured warn line — exactly the record this deployment had
    /// before the module existed.
    pub fn disabled() -> Self {
        Self {
            tx: None,
            recorder: None,
        }
    }

    /// Spawn the drain task over a recorder. The task owns it and
    /// runs until the last `AuthAudit` clone drops.
    pub fn spawn(recorder: Arc<dyn EventRecorder>) -> Self {
        let (tx, mut rx) = tokio::sync::mpsc::channel::<Staged>(QUEUE_DEPTH);
        let direct = recorder.clone();
        tokio::spawn(async move {
            while let Some((kind, payload)) = rx.recv().await {
                let event = Event::new("gateway", kind, payload, boss_clock_client::wall_now());
                if let Err(e) = recorder.record(&event).await {
                    warn_unrecorded(
                        event.kind.as_str(),
                        &event.payload,
                        &format!("outbox insert failed: {e}"),
                    );
                }
            }
        });
        Self {
            tx: Some(tx),
            recorder: Some(direct),
        }
    }

    /// An authentication decision went against the caller.
    /// `email_claimed` is the identity the caller *claimed*, not an
    /// employee reference; `None` when the IdP refused before any
    /// identity reached us.
    pub fn login_denied(
        &self,
        email_claimed: Option<&str>,
        method: AuthMethod,
        reason: DeniedReason,
        idp: Option<&str>,
    ) {
        let mut payload = json!({
            "method": method.as_str(),
            "reason": reason.as_str(),
        });
        if let Some(e) = email_claimed {
            // The claim is caller-controlled and unauthenticated: keep a
            // bounded prefix and say it was cut (see EMAIL_CLAIMED_MAX).
            if e.chars().count() > EMAIL_CLAIMED_MAX {
                let cut: String = e.chars().take(EMAIL_CLAIMED_MAX).collect();
                payload["email_claimed"] = json!(cut);
                payload["email_claimed_truncated"] = json!(true);
            } else {
                payload["email_claimed"] = json!(e);
            }
        }
        if let Some(i) = idp {
            payload["idp"] = json!(i);
        }
        self.emit("auth.login.denied", payload);
    }

    /// A session was minted for an authenticated employee. `downgrade`
    /// is `Some` when the session carries less than the row's role; it
    /// is omitted from the payload, not asserted as null, otherwise.
    pub fn login_succeeded(
        &self,
        email: &str,
        employee_id: Option<&str>,
        method: AuthMethod,
        downgrade: Option<Downgrade<'_>>,
    ) {
        self.emit(
            "auth.login.succeeded",
            succeeded_payload(email, employee_id, method, downgrade),
        );
    }

    /// An emergency session was minted by a verified break-glass
    /// assertion. The same `auth.login.succeeded` as every other method,
    /// plus WHICH key asserted: its `credential_id` (the WebAuthn public
    /// handle) and the `label` of the committed record holding it
    /// (`primary`/`backup`), both public by construction. Until backlog
    /// 9bbfb244 (2026-09-29) the event carried only `{email, method}`, so
    /// the record could not show that both keys open the door. `label`
    /// is asserted as null when no committed record holds the id.
    pub fn break_glass_login_succeeded(
        &self,
        email: &str,
        credential_id: &str,
        label: Option<&str>,
    ) {
        let mut payload = succeeded_payload(email, None, AuthMethod::BreakGlass, None);
        payload["credential_id"] = json!(credential_id);
        payload["label"] = json!(label);
        self.emit("auth.login.succeeded", payload);
    }

    /// The unauthenticated read-only guest capability was exercised.
    pub fn guest_session(&self, email: &str) {
        self.emit("auth.session.guest", json!({ "email": email }));
    }

    /// A break-glass hardware credential passed the enrollment
    /// ceremony. Enrolling an emergency key is an auth-administration
    /// act; it gets its own kind rather than riding `login.succeeded`
    /// because no session is minted by it.
    pub fn break_glass_enrolled(&self, label: &str, credential_id: &str, aaguid: &str) {
        self.emit(
            "auth.break-glass.enrolled",
            json!({
                "label": label,
                "credential_id": credential_id,
                "aaguid": aaguid,
            }),
        );
    }

    /// A local credential was written through the auth-admin door
    /// (`POST /api/auth/onboard`). `created` is false when the write
    /// replaced an existing credential — the upsert that used to leave
    /// no trace either way (backlog 17ae5248). Never the password.
    pub fn credential_written(&self, email: &str, created: bool, actor: AdminActor<'_>) {
        let mut payload = json!({
            "email": email,
            "outcome": if created { "created" } else { "overwritten" },
        });
        actor.stamp(&mut payload);
        self.emit("auth.credential.written", payload);
    }

    /// An administrator issued a password reset for `email`
    /// (`POST /api/auth/issue-reset`). `mailed` is whether the link
    /// left the building; a token issued but not sent is still a token
    /// issued. Never the token (backlog 17ae5248).
    pub fn reset_issued(&self, email: &str, mailed: bool, actor: AdminActor<'_>) {
        let mut payload = json!({ "email": email, "mailed": mailed });
        actor.stamp(&mut payload);
        self.emit("auth.reset.issued", payload);
    }

    /// A passkey was enrolled through the self-service ceremony
    /// (`register_finish`). Enrolling a key is an auth act and left no
    /// trace until the review of car 0bde9b99 (H1); `access_tier` says
    /// what the key may do — a self-service key is `user`, which never
    /// elevates a session. Never a credential id or any key material.
    pub fn passkey_enrolled(&self, email: &str, employee_id: &str, label: &str, access_tier: &str) {
        self.emit(
            "auth.passkey.enrolled",
            json!({
                "email": email,
                "employee_id": employee_id,
                "label": label,
                "access_tier": access_tier,
            }),
        );
    }

    /// A session was elevated to the operator tier by a verified
    /// assertion from the platform owner's operator-tier passkey (backlog
    /// 3c92c5b8). The one trust change a live session can undergo, so it
    /// is RECORDED before the grant, not staged beside it: `Err` when no
    /// recorder is configured or the outbox insert fails, and the caller
    /// refuses the elevation (review of car 0bde9b99, L2 — no silent
    /// grant). Names who, when the assertion was verified, when the
    /// elevation ends (the session's own expiry — never extended), and
    /// which key: its label and registration time, neither secret. Both
    /// instants are RFC 3339 UTC. Never a credential id or any assertion
    /// material.
    pub async fn session_elevated(&self, elevation: Elevation<'_>) -> Result<(), String> {
        let instant = |secs: u64| {
            i64::try_from(secs)
                .ok()
                .and_then(|s| chrono::DateTime::from_timestamp(s, 0))
                .map(|t| t.to_rfc3339_opts(chrono::SecondsFormat::Secs, true))
        };
        let payload = json!({
            "email": elevation.email,
            "employee_id": elevation.employee_id,
            "method": "passkey",
            "access_tier": "operator",
            "elevated_at": instant(elevation.elevated_at),
            "expires_at": instant(elevation.expires_at),
            "credential_label": elevation.credential_label,
            "credential_registered_at": elevation.credential_registered_at,
        });
        let kind = "auth.session.elevated";
        let Some(recorder) = &self.recorder else {
            warn_unrecorded(
                kind,
                &payload,
                "no audit staging configured — elevation refused",
            );
            return Err("no audit staging configured".to_string());
        };
        let event = Event::new("gateway", kind, payload, boss_clock_client::wall_now());
        recorder.record(&event).await.map_err(|e| {
            warn_unrecorded(
                kind,
                &event.payload,
                &format!("outbox insert failed — elevation refused: {e}"),
            );
            format!("outbox insert failed: {e}")
        })
    }

    fn emit(&self, kind: &'static str, payload: serde_json::Value) {
        match &self.tx {
            None => warn_unrecorded(kind, &payload, "no audit staging configured"),
            Some(tx) => {
                if let Err(e) = tx.try_send((kind, payload)) {
                    let ((kind, payload), why) = match e {
                        tokio::sync::mpsc::error::TrySendError::Full(v) => (v, "audit queue full"),
                        tokio::sync::mpsc::error::TrySendError::Closed(v) => {
                            (v, "audit drain task gone")
                        }
                    };
                    warn_unrecorded(kind, &payload, why);
                }
            }
        }
    }
}

/// The `auth.login.succeeded` payload every method shares: `email` and
/// `method`, plus `employee_id` and `downgrade` only when they are
/// `Some` — omitted, not asserted as null.
fn succeeded_payload(
    email: &str,
    employee_id: Option<&str>,
    method: AuthMethod,
    downgrade: Option<Downgrade<'_>>,
) -> serde_json::Value {
    let mut payload = json!({
        "email": email,
        "method": method.as_str(),
    });
    if let Some(id) = employee_id {
        payload["employee_id"] = json!(id);
    }
    if let Some(d) = downgrade {
        payload["downgrade"] = json!({ "from": d.from, "to": d.to });
    }
    payload
}

/// The most of a claimed email a denied-login event keeps, in
/// characters: RFC 5321's 254-character path limit, so no real address
/// is ever cut. The claim is whatever an unauthenticated caller sent,
/// and an unbounded one staged an outbox row larger than NATS publishes
/// — a row the relay cannot deliver and every later event waits behind
/// (adversarial review of the d49b4355 car, 2026-09-28).
pub const EMAIL_CLAIMED_MAX: usize = 254;

/// The variable the auth-event sink's database URL is read from: the
/// service database every binary in the container reads, which the
/// cluster manifest sets from the `database-url` key of `boss-secrets`.
///
/// It had its own variable until 2026-09-28, meant to carry an
/// INSERT-only role (111-gateway-audit-events.sql). Two things were
/// true of that (backlog d49b4355, 7ec7113b): only the retired
/// bare-metal drop-in ever set it, so in the cluster every auth event
/// was a warn line and `/api/events/tail?source=gateway` answered
/// nothing; and the role's password was its own name, published in the
/// tree. The least privilege it promised was never real in the
/// container either — the launcher starts the gateway with the whole
/// container environment, `BOSS_POSTGRES_URL` included — so the
/// variable collapsed onto the one the environment already carries
/// (CLAUDE.md §9a). That URL is the Postgres SUPERUSER `boss`, so the
/// gateway's pool is two superuser connections until the gateway gets
/// an environment of its own (docs/architecture-decisions.md, §Policy
/// & auth). The role is left unable to log in by
/// 20260928012747-the-gateway-audit-role-opens-no-session.sql.
pub const AUDIT_SINK_URL_VAR: &str = "BOSS_POSTGRES_URL";

/// The auth-event sink's URL, read through `lookup` (the process
/// environment in `main`); `None` — no sink, every event a warn line —
/// when it is unset or blank.
pub fn audit_sink_url(lookup: impl Fn(&str) -> Option<String>) -> Option<String> {
    lookup(AUDIT_SINK_URL_VAR).filter(|url| !url.trim().is_empty())
}

/// The backstop record. Structured and greppable so a deployment
/// with no pool — or a pool that is down — still never silently
/// pretends nothing happened.
fn warn_unrecorded(kind: &str, payload: &serde_json::Value, why: &str) {
    tracing::warn!(
        kind = %kind,
        payload = %payload,
        why,
        "auth event NOT staged to the outbox — this warn line is the record"
    );
}

/// In-memory recorder + drain helper, shared by the handler tests in
/// `local_auth` and `oidc` — the port's test adapter, per the
/// no-mocks rule.
#[cfg(test)]
pub(crate) mod testing {
    use super::*;
    use std::sync::Mutex;

    #[derive(Default)]
    pub(crate) struct Captured(pub(crate) Mutex<Vec<Event>>);

    #[async_trait::async_trait]
    impl EventRecorder for Captured {
        async fn record(&self, event: &Event) -> Result<(), String> {
            self.0.lock().unwrap().push(event.clone());
            Ok(())
        }
    }

    pub(crate) async fn drain(cap: &Captured, want: usize) -> Vec<Event> {
        for _ in 0..200 {
            if cap.0.lock().unwrap().len() >= want {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(2)).await;
        }
        cap.0.lock().unwrap().clone()
    }
}

#[cfg(test)]
mod tests {
    use super::testing::{Captured, drain};
    use super::*;

    #[tokio::test]
    async fn denied_carries_reason_and_no_subject_reference() {
        let cap = Arc::new(Captured::default());
        let audit = AuthAudit::spawn(cap.clone());
        audit.login_denied(
            Some("who@example.com"),
            AuthMethod::Oidc,
            DeniedReason::NoEmployeeRecord,
            Some("https://idm.example"),
        );
        let events = drain(&cap, 1).await;
        assert_eq!(events.len(), 1);
        let e = &events[0];
        assert_eq!(e.kind, "auth.login.denied");
        assert_eq!(e.source, "gateway");
        assert_eq!(e.payload["reason"], "no_employee_record");
        assert_eq!(e.payload["method"], "oidc");
        assert_eq!(e.payload["email_claimed"], "who@example.com");
        assert_eq!(e.payload["idp"], "https://idm.example");
        // The whole point of the denied event: no employee matched,
        // so the payload must not assert one (ref-check honesty).
        assert!(e.payload.get("employee_id").is_none());
    }

    /// Adversarial review of the d49b4355 car (2026-09-28): the claimed
    /// email is whatever an UNAUTHENTICATED caller sent, and once the
    /// sink is live a megabyte of it becomes an outbox row larger than
    /// NATS's 1 MiB max_payload — which the relay cannot publish, and
    /// every later event queues behind it. The record keeps a bounded
    /// prefix and says it was cut.
    #[tokio::test]
    async fn a_claimed_email_is_capped_and_says_so() {
        let cap = Arc::new(Captured::default());
        let audit = AuthAudit::spawn(cap.clone());
        let huge = format!("{}@example.com", "é".repeat(1_500_000));
        audit.login_denied(
            Some(&huge),
            AuthMethod::Password,
            DeniedReason::BadCredentials,
            None,
        );
        audit.login_denied(
            Some("who@example.com"),
            AuthMethod::Password,
            DeniedReason::BadCredentials,
            None,
        );
        let events = drain(&cap, 2).await;
        let cut = events[0].payload["email_claimed"]
            .as_str()
            .expect("a capped email is still a string");
        assert_eq!(cut.chars().count(), EMAIL_CLAIMED_MAX);
        assert!(
            huge.starts_with(cut),
            "the cap keeps a prefix, not a rewrite"
        );
        assert_eq!(events[0].payload["email_claimed_truncated"], true);
        assert_eq!(events[1].payload["email_claimed"], "who@example.com");
        assert!(
            events[1].payload.get("email_claimed_truncated").is_none(),
            "an ordinary email is not marked"
        );
    }

    #[tokio::test]
    async fn idp_refusal_needs_no_claimed_email() {
        let cap = Arc::new(Captured::default());
        let audit = AuthAudit::spawn(cap.clone());
        audit.login_denied(None, AuthMethod::Oidc, DeniedReason::IdpDenied, None);
        let events = drain(&cap, 1).await;
        assert_eq!(events[0].payload["reason"], "idp_denied");
        assert!(events[0].payload.get("email_claimed").is_none());
    }

    #[tokio::test]
    async fn succeeded_names_the_method_for_the_passkey_future() {
        let cap = Arc::new(Captured::default());
        let audit = AuthAudit::spawn(cap.clone());
        audit.login_succeeded("op@example.com", Some("emp-1"), AuthMethod::Password, None);
        let events = drain(&cap, 1).await;
        let e = &events[0];
        assert_eq!(e.kind, "auth.login.succeeded");
        assert_eq!(e.payload["method"], "password");
        assert_eq!(e.payload["employee_id"], "emp-1");
    }

    /// Backlog 9bbfb244: an emergency sign-in names the key that asserted
    /// — its credential id and its label, both public — so the record
    /// alone shows that each enrolled key opens the door (DR 62dac114
    /// item 1 needed David's word on 2026-09-29, because two events read
    /// only `{email, method}`). A label no committed record holds is
    /// asserted as null, not omitted: the key verified and the record
    /// could not name it, which is itself the finding. Key set whole.
    #[tokio::test]
    async fn a_break_glass_login_names_the_key_that_asserted() {
        let cap = Arc::new(Captured::default());
        let audit = AuthAudit::spawn(cap.clone());
        audit.break_glass_login_succeeded("break-glass-operator", "cred-b", Some("backup"));
        audit.break_glass_login_succeeded("break-glass-operator", "cred-x", None);
        let events = drain(&cap, 2).await;
        assert_eq!(events.len(), 2);
        let e = &events[0];
        assert_eq!(e.kind, "auth.login.succeeded");
        assert_eq!(e.source, "gateway");
        assert_eq!(e.payload["email"], "break-glass-operator");
        assert_eq!(e.payload["method"], "break-glass");
        assert_eq!(e.payload["credential_id"], "cred-b");
        assert_eq!(e.payload["label"], "backup");
        let mut keys: Vec<&str> = e
            .payload
            .as_object()
            .expect("object payload")
            .keys()
            .map(String::as_str)
            .collect();
        keys.sort_unstable();
        assert_eq!(keys, ["credential_id", "email", "label", "method"]);
        assert_eq!(events[1].payload["credential_id"], "cred-x");
        assert_eq!(
            events[1].payload.get("label"),
            Some(&serde_json::Value::Null)
        );
    }

    /// Backlog 8f45e0b4 item (3): a downgraded mint says what the row
    /// held and what the session carries, and a login the registry
    /// could not confirm is a denial with its own closed reason.
    #[tokio::test]
    async fn a_downgrade_and_an_unconfirmed_role_are_on_the_record() {
        let cap = Arc::new(Captured::default());
        let audit = AuthAudit::spawn(cap.clone());
        audit.login_succeeded(
            "op@example.com",
            Some("emp-1"),
            AuthMethod::Oidc,
            Some(Downgrade {
                from: "break-glass",
                to: "visitor",
            }),
        );
        audit.login_denied(
            Some("op@example.com"),
            AuthMethod::Password,
            DeniedReason::RoleUnconfirmed,
            None,
        );
        let events = drain(&cap, 2).await;
        assert_eq!(events.len(), 2);
        assert_eq!(events[0].kind, "auth.login.succeeded");
        assert_eq!(
            events[0].payload["downgrade"],
            json!({ "from": "break-glass", "to": "visitor" })
        );
        assert_eq!(events[1].kind, "auth.login.denied");
        assert_eq!(events[1].payload["reason"], "role_unconfirmed");
        assert!(events[1].payload.get("employee_id").is_none());
    }

    /// Review of car 0bde9b99, H1: a self-service enrolment is on the
    /// record — who, which label, and the tier it was stored at — and
    /// carries no credential id or key material. Key set asserted whole.
    #[tokio::test]
    async fn an_enrolment_names_the_employee_label_and_tier_only() {
        let cap = Arc::new(Captured::default());
        let audit = AuthAudit::spawn(cap.clone());
        audit.passkey_enrolled("op@example.com", "emp-1", "yubikey", "user");
        let events = drain(&cap, 1).await;
        let e = &events[0];
        assert_eq!(e.kind, "auth.passkey.enrolled");
        assert_eq!(e.source, "gateway");
        assert_eq!(e.payload["access_tier"], "user");
        let mut keys: Vec<&str> = e
            .payload
            .as_object()
            .expect("object payload")
            .keys()
            .map(String::as_str)
            .collect();
        keys.sort_unstable();
        assert_eq!(keys, ["access_tier", "email", "employee_id", "label"]);
    }

    /// Review L2: an elevation is RECORDED or refused — with no staging
    /// configured it answers `Err`, so the caller cannot grant silently.
    #[tokio::test]
    async fn an_elevation_with_no_staging_is_an_error_not_a_warn_line() {
        let elevation = Elevation {
            email: "op@example.com",
            employee_id: "emp-1",
            elevated_at: 1,
            expires_at: 2,
            credential_label: "yubikey",
            credential_registered_at: "2026-09-01T12:00:00Z",
        };
        assert!(
            AuthAudit::disabled()
                .session_elevated(elevation)
                .await
                .is_err()
        );
        let cap = Arc::new(Captured::default());
        AuthAudit::spawn(cap.clone())
            .session_elevated(elevation)
            .await
            .expect("recorded");
        assert_eq!(cap.0.lock().unwrap().len(), 1, "recorded before returning");
    }

    #[tokio::test]
    async fn guest_mint_is_counted_under_its_own_kind() {
        let cap = Arc::new(Captured::default());
        let audit = AuthAudit::spawn(cap.clone());
        audit.guest_session("guest@algedonic.dev");
        let events = drain(&cap, 1).await;
        assert_eq!(events[0].kind, "auth.session.guest");
        assert_eq!(events[0].payload["email"], "guest@algedonic.dev");
    }

    /// Backlog 17ae5248: a credential written through the auth-admin
    /// door names who wrote it, to which email, and whether it was
    /// created or overwritten — and carries nothing that is itself a
    /// credential. The key set is asserted whole so a later field
    /// cannot ride in unread.
    #[tokio::test]
    async fn a_credential_write_names_actor_target_and_outcome_only() {
        let cap = Arc::new(Captured::default());
        let audit = AuthAudit::spawn(cap.clone());
        let actor = AdminActor {
            email: "admin@example.com",
            employee_id: Some("emp-admin"),
            role: Some("platform-admin"),
        };
        audit.credential_written("new@example.com", true, actor);
        audit.credential_written("new@example.com", false, actor);
        let events = drain(&cap, 2).await;
        assert_eq!(events.len(), 2);
        assert_eq!(events[0].kind, "auth.credential.written");
        assert_eq!(events[0].source, "gateway");
        assert_eq!(events[0].payload["outcome"], "created");
        assert_eq!(events[1].payload["outcome"], "overwritten");
        let e = &events[0];
        assert_eq!(e.payload["email"], "new@example.com");
        assert_eq!(e.payload["actor"], "admin@example.com");
        assert_eq!(e.payload["actor_employee_id"], "emp-admin");
        assert_eq!(e.payload["actor_role"], "platform-admin");
        let mut keys: Vec<&str> = e
            .payload
            .as_object()
            .expect("object payload")
            .keys()
            .map(String::as_str)
            .collect();
        keys.sort_unstable();
        assert_eq!(
            keys,
            [
                "actor",
                "actor_employee_id",
                "actor_role",
                "email",
                "outcome"
            ]
        );
    }

    /// Backlog 17ae5248: an admin-issued reset names who issued it and
    /// for which email, never the token; `mailed` says whether the
    /// link left the building.
    #[tokio::test]
    async fn a_reset_issue_names_actor_and_target_only() {
        let cap = Arc::new(Captured::default());
        let audit = AuthAudit::spawn(cap.clone());
        audit.reset_issued(
            "user@example.com",
            true,
            AdminActor {
                email: "glass@example.com",
                employee_id: None,
                role: Some("break-glass"),
            },
        );
        let events = drain(&cap, 1).await;
        let e = &events[0];
        assert_eq!(e.kind, "auth.reset.issued");
        assert_eq!(e.payload["email"], "user@example.com");
        assert_eq!(e.payload["actor"], "glass@example.com");
        assert_eq!(e.payload["actor_role"], "break-glass");
        assert_eq!(e.payload["mailed"], true);
        assert!(
            e.payload.get("actor_employee_id").is_none(),
            "an absent employee id is omitted, not asserted as null"
        );
        let mut keys: Vec<&str> = e
            .payload
            .as_object()
            .expect("object payload")
            .keys()
            .map(String::as_str)
            .collect();
        keys.sort_unstable();
        assert_eq!(keys, ["actor", "actor_role", "email", "mailed"]);
    }

    #[tokio::test]
    async fn disabled_mode_degrades_to_the_warn_line_without_panic() {
        // No channel, no recorder: the call must be a no-op beyond
        // the warn backstop — a deployment without the pool keeps
        // exactly its pre-module behavior.
        let audit = AuthAudit::disabled();
        audit.login_denied(
            Some("who@example.com"),
            AuthMethod::Password,
            DeniedReason::BadCredentials,
            None,
        );
        audit.login_succeeded("op@example.com", None, AuthMethod::Password, None);
        audit.guest_session("guest@algedonic.dev");
    }

    #[tokio::test]
    async fn recorder_failure_never_reaches_the_caller() {
        struct Failing;
        #[async_trait::async_trait]
        impl EventRecorder for Failing {
            async fn record(&self, _: &Event) -> Result<(), String> {
                Err("db down".into())
            }
        }
        let audit = AuthAudit::spawn(Arc::new(Failing));
        // Emit must not error or panic; the drain task warns.
        audit.login_succeeded("op@example.com", None, AuthMethod::Password, None);
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }

    /// An environment holding exactly the given pairs.
    fn env<'a>(pairs: &'a [(&'a str, &'a str)]) -> impl Fn(&str) -> Option<String> + 'a {
        move |k| {
            pairs
                .iter()
                .find(|(name, _)| *name == k)
                .map(|(_, v)| v.to_string())
        }
    }

    /// The sink is the service database every binary in the container
    /// already reads — the cluster sets it from the `database-url` key of
    /// `boss-secrets` (backlog d49b4355: the sink's own variable was set
    /// only by a bare-metal drop-in, so the cluster staged nothing).
    #[test]
    fn the_sink_is_the_service_database() {
        let url = "postgres://svc@postgres/boss";
        assert_eq!(
            audit_sink_url(env(&[("BOSS_POSTGRES_URL", url)])).as_deref(),
            Some(url)
        );
    }

    /// The drop-in's variable carried a role whose password was its own
    /// name (backlog 7ec7113b); setting it configures nothing now, so a
    /// copy of that URL left on some host cannot bring the role back.
    #[test]
    fn the_retired_drop_in_variable_configures_nothing() {
        let retired = concat!("BOSS_GATEWAY_", "AUDIT_DB_URL");
        assert_eq!(
            audit_sink_url(env(&[(retired, "postgres://r@127.0.0.1/boss")])),
            None
        );
    }

    #[test]
    fn a_blank_url_is_no_sink() {
        assert_eq!(audit_sink_url(env(&[("BOSS_POSTGRES_URL", "  ")])), None);
        assert_eq!(audit_sink_url(env(&[])), None);
    }
}
