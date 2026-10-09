//! The login door: `X-Boss-User.id` resolves through the alias table
//! before anything downstream reads it.
//!
//! Where a human's login is resolved at the gateway (oidc.rs →
//! `bootstrap_email` → `session.employee_id`), an agent's is resolved
//! HERE, because the pod's doors (`boss-api`, the `boss` CLI) speak to
//! the jobs API directly with the address in the header — there is no
//! gateway session in that path to hold the resolved id. So the jobs
//! API's own edge is the one place every agent write passes.
//!
//! Layer order is the contract: this layer sits OUTSIDE
//! `request_context_middleware`, so the ambient actor is set from the
//! REWRITTEN header, and the `CurrentUser` extractor every handler uses
//! reads the same rewritten header. Nothing downstream has to know an
//! alias existed — which is exactly the property oidc.rs gives humans.
//!
//! [`decide`] is the rule, pure and pinned below; [`resolve_login`] is
//! the axum shell around it.
//!
//! THE ROW'S ROLE (backlog 4e51bf23, review 42cd8068 N1). Resolving the
//! id was half of what oidc.rs gives a person: the gateway also takes
//! the ROLE from the person's record. This door kept whatever role the
//! caller sent (`User { id: actor, ..user }`), and the pod's doors send
//! `platform-admin` at operator tier for every named caller — so,
//! measured 2026-10-08, `agent-codex` (row role `null`) and
//! `agent-claude` (row role `engineering-agent`) both wrote as
//! platform-admin, and a row's role decided nothing. [`judge`] is the
//! other half: a registered agent's role and tier are its row's.
//!
//! It is behind a word of its own, [`ROW_ROLE_MODE_FILE`], read as the
//! machine gate's is: absent or `off` and `report` change nothing;
//! `enforce` judges. The word exists because of what the measurement
//! found beside the defect: `engineering-agent` holds none of the 210
//! policy rules, so the day this judges by the row with the rows as
//! they are, the session that coordinates the estate cannot read a
//! packet. The rows are the operator's to change, and they change
//! FIRST; then the word. Its way back is the word, over the LAN kube
//! road, which needs neither a train nor the jobs API.
//!
//! WHAT THIS DOES NOT CLOSE. The id is still asserted: a caller that
//! signs `emp-david` or `automation:dispatcher` is not an agent here
//! and passes with the role it sent, until the role-of-record resolver
//! enforces behind a credential-bound identity (design abf9eeae row H,
//! and review ed9181b4's preconditions on this item). This door makes
//! an HONEST agent's row true; it does not stop a dishonest one.

use std::sync::Arc;

use axum::body::Body;
use axum::extract::{Request, State};
use axum::http::header::{CONTENT_LENGTH, CONTENT_TYPE};
use axum::http::{HeaderValue, Method, StatusCode};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use boss_core::actor::REGISTERED_AGENT_PREFIX;
use boss_core::machine_gate::{Mode, ModeSwitch};
use boss_core::publisher::DomainPublisher;
use boss_policy_client::{AccessTier, User};

use super::port::AgentsRegistry;
use super::types::AgentRow;

/// The event kind that counts the window: one per WRITE admitted under
/// an address-shaped login no alias maps. Declared in
/// `20260915212644-an-agent-login-resolves-to-its-registered-identity.sql`.
pub const UNRESOLVED_LOGIN: &str = "actor.login.unresolved";

/// The header the extractor and the request-context middleware read.
const HEADER: &str = "x-boss-user";

/// What the door does with one request's identity.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Resolution {
    /// The id is an alias: sign the request as the actor it maps to.
    Resolved(String),
    /// An address-shaped login no alias maps, on a write: admit it (the
    /// window is open) and count it.
    Unresolved,
    /// Not a login at all (an employee id, an automation slug, the
    /// anonymous request), a read, or an id already canonical: pass
    /// through untouched, count nothing.
    PassThrough,
}

/// Is this id shaped like a login rather than an identity? An `@` is
/// the whole test — the same test `isHumanActor` on the client uses to
/// call an address a session rather than a person — because every
/// identity the vocabulary already describes (`emp-*`, `automation:*`,
/// `<mode>:<model>`, `agent-*`) is address-free.
fn is_login_shaped(id: &str) -> bool {
    id.contains('@')
}

/// A write is a call whose actor is recorded; reads attribute nothing,
/// which is why boss-cli's identity.rs lets an unnamed read through
/// and refuses an unnamed write. HEAD/OPTIONS ride with GET.
fn is_write(method: &Method) -> bool {
    !matches!(*method, Method::GET | Method::HEAD | Method::OPTIONS)
}

/// The rule. `alias` is what the registry answered for `id`.
pub fn decide(id: &str, alias: Option<String>, method: &Method) -> Resolution {
    match alias {
        Some(actor) if actor != id => Resolution::Resolved(actor),
        Some(_) => Resolution::PassThrough,
        None if is_login_shaped(id) && is_write(method) => Resolution::Unresolved,
        None => Resolution::PassThrough,
    }
}

/// The door's two dependencies: who answers the alias question, and
/// where the count is written. Mount it with
/// `axum::middleware::from_fn_with_state(Arc::new(door), resolve_login)`,
/// applied AFTER (so it runs BEFORE) `request_context_middleware`.
pub struct LoginDoor {
    registry: Arc<dyn AgentsRegistry>,
    publisher: DomainPublisher,
    rows: Arc<dyn Fn() -> Mode + Send + Sync>,
}

impl LoginDoor {
    pub fn new(registry: Arc<dyn AgentsRegistry>, publisher: DomainPublisher) -> Self {
        Self {
            registry,
            publisher,
            rows: Arc::new(|| Mode::Off),
        }
    }

    /// Judge a registered agent by its row while `mode` answers
    /// [`Mode::Enforce`]. Asked on every request, so a mounted
    /// [`boss_core::machine_gate::ModeSwitch`] moves it with no restart.
    /// A door built without this never judges — every test and tool
    /// that mounts the door for its alias resolution keeps exactly that.
    pub fn judging_rows_when(mut self, mode: impl Fn() -> Mode + Send + Sync + 'static) -> Self {
        self.rows = Arc::new(mode);
        self
    }

    /// Judge by whatever `word` reads NOW. The switch is asked on every
    /// request and nothing is taken from it when the door is built, so
    /// an absent file is `off` for as long as it is absent and a word
    /// written later moves the door with no restart.
    pub fn judging_rows_by_word(self, word: Arc<ModeSwitch>) -> Self {
        self.judging_rows_when(move || word.mode())
    }

    /// The binary's whole wiring (review cdff423f N6, mutant M1b): mount
    /// the word at [`ROW_ROLE_MODE_FILE`] (or where
    /// [`ROW_ROLE_MODE_FILE_ENV`] names) and judge by it. It lives here
    /// because a closure written in `boss_jobs_api.rs` is a line no test
    /// runs — one that answered `enforce` whatever the file said passed
    /// every test the car had. The binary now makes this one call, with
    /// nothing to get wrong in it, and its text is held to that
    /// (`the_binary_wires_the_door_to_the_mounted_word_and_to_nothing_else`).
    /// THIS function is run by one test, in a process of its own because
    /// it reads the environment: `the_login_door_mounts_the_agent_role_
    /// word_the_pod_mounts.rs` (delta review 012ccafa D2 — until it,
    /// nothing ran these lines, and a body that always enforced, or one
    /// that read `policy-check`'s file, passed everything). Call it
    /// inside the runtime, at boot, as [`ModeSwitch::mount`] asks.
    pub fn judging_rows_by_the_mounted_word(self) -> Self {
        self.judging_rows_by_word(ModeSwitch::mount(
            "agent role",
            ROW_ROLE_MODE_FILE_ENV,
            ROW_ROLE_MODE_FILE,
        ))
    }
}

/// Where the word lives: one more key of the `boss-machine-gate`
/// ConfigMap, which the jobs pod mounts whole. No key, no file, `off`.
/// Held to the manifest's mount path and key, and to being no sibling
/// word's file, by boss-testing's `the_agent_role_word_is_not_enforce_
/// until_the_rows_are_read_back.rs`.
pub const ROW_ROLE_MODE_FILE: &str = "/etc/boss/machine-gate/agent-role";
pub const ROW_ROLE_MODE_FILE_ENV: &str = "BOSS_AGENT_ROLE_MODE_FILE";

/// Is `id` in the agents registry's own namespace? The table's CHECK
/// admits `agent-<slug>` and nothing else, so an id spelled this way is
/// either a row or a claim to be one. A person (`emp-*`) and an
/// automation (`automation:*`, `rule:*`) are neither, and are not this
/// door's question.
fn is_agent_shaped(id: &str) -> bool {
    id.starts_with(REGISTERED_AGENT_PREFIX)
}

/// What a registered agent is judged as: the row's role at the tier
/// the row's role carries.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Judged {
    /// The registered id (`agent-claude`).
    pub actor: String,
    /// The id the request arrived signed with — the alias, or the id.
    pub signed_as: String,
    /// The row's id, or `None` when the registry holds no such row.
    pub registry_row: Option<String>,
    /// The row's role as it is stored; `None` is the row's `null`.
    pub row_role: Option<String>,
    pub role: String,
    pub tier: AccessTier,
    pub asserted_role: String,
    pub asserted_tier: AccessTier,
}

/// The rule. `row` is the registry's row for `actor`, if it holds one.
///
/// - A row with a role writes as that role. A row with `null`, and an
///   `agent-*` id with no row at all, write as
///   [`boss_core::roles::GUEST_ROLE`] — the role core already gives a
///   caller that carries none, which every policy rule but one denies.
///   No new spelling for "nothing".
/// - THE TIER. The `agents` table has no tier column, so the row cannot
///   be asked. Operator tier is "the operator who owns the deployment"
///   (`trust::is_trusted`), and core names one role for that:
///   [`boss_core::roles::PLATFORM_ADMIN_ROLE`]. A row holding it is
///   judged at operator tier; every other row at user tier, whatever
///   was asserted. An agent that should hold operator tier under a
///   narrower role needs the column — a decision, and not this car's.
pub fn judge(asserted: &User, actor: &str, row: Option<&AgentRow>) -> Judged {
    let row_role = row.and_then(|r| r.role.clone());
    let role = row_role
        .clone()
        .unwrap_or_else(|| boss_core::roles::GUEST_ROLE.to_string());
    let tier = if role == boss_core::roles::PLATFORM_ADMIN_ROLE {
        AccessTier::Operator
    } else {
        AccessTier::User
    };
    Judged {
        actor: actor.to_string(),
        signed_as: asserted.id.clone(),
        registry_row: row.map(|r| r.id.clone()),
        row_role,
        role,
        tier,
        asserted_role: asserted.role.clone(),
        asserted_tier: asserted.access_tier,
    }
}

impl Judged {
    /// Did the caller send something the row does not give?
    pub fn overrode(&self) -> bool {
        self.asserted_role != self.role || self.asserted_tier != self.tier
    }

    /// One sentence a refused caller can act on: who, which row, what
    /// the row gives, what was asserted, and where the role is changed.
    pub fn why(&self) -> String {
        let holds = match (&self.registry_row, &self.row_role) {
            (Some(row), Some(role)) => {
                format!("agents registry row `{row}` holds role `{role}`")
            }
            (Some(row), None) => format!(
                "agents registry row `{row}` holds no role (null), which is judged as `{}`",
                self.role
            ),
            (None, _) => format!(
                "the agents registry holds no row for `{}`, which is judged as `{}`",
                self.actor, self.role
            ),
        };
        let sent = if self.asserted_role == self.role {
            format!("the request asserted `{}` as well", self.asserted_role)
        } else {
            format!(
                "the request asserted `{}`, which is ignored: a registered agent writes with \
                 its row's role, never the one it sends",
                self.asserted_role
            )
        };
        format!(
            "`{}` (signed as `{}`) is judged as `{}` at {} tier: {holds}; {sent}. To change \
             what this agent may do, change the row's role (the operator's decision, GET \
             /api/agents) — or grant that role the rule (backlog 4e51bf23).",
            self.actor,
            self.signed_as,
            self.role,
            tier_word(self.tier),
        )
    }

    fn note(&self) -> serde_json::Value {
        serde_json::json!({
            "actor": self.actor,
            "signed_as": self.signed_as,
            "registry_row": self.registry_row,
            "row_role": self.row_role,
            "judged_as_role": self.role,
            "judged_at_tier": tier_word(self.tier),
            "asserted_role": self.asserted_role,
            "asserted_tier": tier_word(self.asserted_tier),
            "why": self.why(),
        })
    }
}

fn tier_word(tier: AccessTier) -> &'static str {
    match tier {
        AccessTier::User => "user",
        AccessTier::Operator => "operator",
        AccessTier::Auditor => "auditor",
    }
}

/// The largest refusal body this door will read back to annotate. A
/// refusal is a small JSON object; anything longer passes as it is.
const MAX_ANNOTATED_BODY: usize = 64 * 1024;

/// Put `judged` on a 403's JSON body as `role_of_record`, so the
/// refusal — whichever check downstream made it — names who was
/// refused, which row decided, what it gives and what was asserted. A
/// body that is not a JSON object passes untouched: the note is an
/// addition to a refusal, never a reason to lose one.
async fn annotate_refusal(resp: Response, judged: &Judged) -> Response {
    if resp.status() != StatusCode::FORBIDDEN {
        return resp;
    }
    tracing::warn!(
        actor = %judged.actor,
        signed_as = %judged.signed_as,
        row_role = ?judged.row_role,
        judged_as = %judged.role,
        asserted_role = %judged.asserted_role,
        "a registered agent was refused under its row's role"
    );
    let is_json = resp
        .headers()
        .get(CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| v.starts_with("application/json"));
    if !is_json {
        return resp;
    }
    let (mut parts, body) = resp.into_parts();
    let Ok(bytes) = axum::body::to_bytes(body, MAX_ANNOTATED_BODY).await else {
        // A refusal longer than the bound (none is today): nothing of
        // it is left to pass on, so the 403 stands with what is known.
        return (
            StatusCode::FORBIDDEN,
            axum::Json(serde_json::json!({
                "error": "forbidden",
                "role_of_record": judged.note(),
            })),
        )
            .into_response();
    };
    let annotated = match serde_json::from_slice::<serde_json::Value>(&bytes) {
        Ok(serde_json::Value::Object(mut map)) => {
            map.insert("role_of_record".to_string(), judged.note());
            serde_json::to_vec(&serde_json::Value::Object(map)).ok()
        }
        _ => None,
    };
    match annotated {
        Some(out) => {
            parts.headers.remove(CONTENT_LENGTH);
            Response::from_parts(parts, Body::from(out))
        }
        None => Response::from_parts(parts, Body::from(bytes)),
    }
}

/// What the 503 says when a registry read failed.
const REGISTRY_DARK: &str = "the agents registry could not answer, so this agent's role is unknown";
/// What it says when the registry answered and the judged identity
/// could not be put on the request (unreachable in practice).
const JUDGEMENT_UNWRITTEN: &str = "the agents registry answered, and the identity it judged could not be written to the request, which is refused rather than passed with the role it asserted";

/// The refusal when the registry cannot say what an agent's row holds
/// and the word is `enforce`: 503, naming the registry. Passing the
/// asserted role instead would make a registry blip the way around the
/// row; and the registry is the jobs API's own database, so a request
/// refused here had little chance of being served behind it.
///
/// THE BODY IS A FIXED SENTENCE (review cdff423f N4). It used to carry
/// the storage error verbatim as `registry_error`, and the id that
/// reaches this arm is asserted: any LAN caller that signed `agent-x`
/// while the registry was dark read the database driver's text. `error`
/// is `&'static str` so that no caller can hand it one; the detail is
/// logged at ERROR by the arm that refuses, where the operator reads it.
fn registry_unavailable(id: &str, error: &'static str) -> Response {
    (
        StatusCode::SERVICE_UNAVAILABLE,
        axum::Json(serde_json::json!({
            "error": error,
            "actor": id,
            "hint": format!(
                "a registered agent writes with its row's role (backlog 4e51bf23) and the row \
                 could not be read; retry. The word that turns this judgement off is \
                 {ROW_ROLE_MODE_FILE} (`report`)."
            ),
        })),
    )
        .into_response()
}

fn with_user(req: &mut Request, user: &User) -> bool {
    match serde_json::to_string(user)
        .ok()
        .and_then(|s| HeaderValue::from_str(&s).ok())
    {
        Some(value) => {
            req.headers_mut().insert(HEADER, value);
            true
        }
        None => false,
    }
}

/// The shell: read the header, ask the registry, apply [`decide`].
///
/// A registry that cannot answer (`Err`) is NOT a refusal during the
/// window: the request passes unresolved and the failure is logged at
/// ERROR. Refusing here would let a database blip take every write
/// down with it — the boot-guard lesson of 2026-09-07 — and the window
/// exists precisely so nothing is refused yet. When the next car closes
/// the window, this arm becomes a 503 that names the registry.
pub async fn resolve_login(
    State(door): State<Arc<LoginDoor>>,
    mut req: Request,
    next: Next,
) -> Response {
    let Some(user) = req
        .headers()
        .get(HEADER)
        .and_then(|v| v.to_str().ok())
        .and_then(|s| serde_json::from_str::<User>(s).ok())
    else {
        // No header, or one the extractor will 400 on its own: not
        // this door's question.
        return next.run(req).await;
    };
    if user.id == "anonymous" {
        return next.run(req).await;
    }
    let mode = (door.rows)();
    let alias = match door.registry.resolve_login(&user.id).await {
        Ok(alias) => alias,
        Err(e)
            if mode == Mode::Enforce
                && (is_login_shaped(&user.id) || is_agent_shaped(&user.id)) =>
        {
            tracing::error!(login = %user.id, error = %e, "agents registry could not answer; an agent's role is unknown and the request is refused 503");
            return registry_unavailable(&user.id, REGISTRY_DARK);
        }
        Err(e) => {
            tracing::error!(
                login = %user.id,
                error = %e,
                "agents registry could not answer; the request passes UNRESOLVED (migration window open)"
            );
            None
        }
    };
    let resolution = decide(&user.id, alias, req.method());
    // THE ROW'S ROLE. Asked of a registered agent only: an id an alias
    // resolved, or one spelled in the registry's own namespace.
    let actor = match &resolution {
        Resolution::Resolved(actor) => actor.clone(),
        _ => user.id.clone(),
    };
    if mode != Mode::Off && is_agent_shaped(&actor) {
        let rows = door.registry.list().await;
        if mode == Mode::Enforce {
            let row = match rows {
                Ok(rows) => rows.into_iter().find(|r| r.id == actor),
                Err(e) => {
                    tracing::error!(actor = %actor, error = %e, "agents registry could not list its rows; an agent's role is unknown and the request is refused 503");
                    return registry_unavailable(&actor, REGISTRY_DARK);
                }
            };
            let judged = judge(&user, &actor, row.as_ref());
            let as_judged = User {
                id: actor,
                role: judged.role.clone(),
                access_tier: judged.tier,
                ..user
            };
            if !with_user(&mut req, &as_judged) {
                // Unreachable in practice (a User that deserialized
                // re-serializes). Passing the request as it arrived
                // would pass the asserted role, so it is refused.
                tracing::error!(actor = %as_judged.id, "the judged identity could not be written to the request; refused 503 rather than passed as asserted");
                return registry_unavailable(&as_judged.id, JUDGEMENT_UNWRITTEN);
            }
            let resp = next.run(req).await;
            return annotate_refusal(resp, &judged).await;
        }
        // `report`: say what `enforce` would change, and change nothing.
        match rows {
            Ok(rows) => {
                let row = rows.into_iter().find(|r| r.id == actor);
                let judged = judge(&user, &actor, row.as_ref());
                if judged.overrode() {
                    tracing::info!(
                        actor = %judged.actor,
                        asserted_role = %judged.asserted_role,
                        row_role = ?judged.row_role,
                        would_judge_as = %judged.role,
                        "agent-role report: under enforce this request would be judged by its row"
                    );
                }
            }
            Err(e) => {
                tracing::error!(actor = %actor, error = %e, "agents registry could not list its rows; nothing is judged in report");
            }
        }
    }
    match resolution {
        Resolution::Resolved(actor) => {
            let resolved = User { id: actor, ..user };
            if !with_user(&mut req, &resolved) {
                // A User that deserialized re-serializes; this arm is
                // unreachable in practice, and if it is reached the
                // request passes as it arrived rather than being
                // dropped on the floor.
                tracing::error!(login = %resolved.id, "could not re-serialize the resolved user; passing the request unresolved");
            }
        }
        Resolution::Unresolved => {
            let method = req.method().to_string();
            let path = req.uri().path().to_string();
            tracing::warn!(
                login = %user.id,
                method = %method,
                path = %path,
                "write signed with an address no actor alias maps — admitted (migration window open) and counted as {UNRESOLVED_LOGIN}"
            );
            // The door observed it: the event's actor is the service,
            // and the unresolved login is the SUBJECT in the payload.
            // Stamping the event with the login itself would put the
            // address into `_actor`, which is the defect being counted.
            let stamp = door
                .publisher
                .stamp_with_actor(door.publisher.default_actor())
                .await;
            let event = stamp.event(
                UNRESOLVED_LOGIN,
                serde_json::json!({
                    "login": user.id,
                    "method": method,
                    "path": path,
                }),
            );
            door.publisher.publish(event).await;
        }
        Resolution::PassThrough => {}
    }
    next.run(req).await
}

#[cfg(test)]
mod tests {
    use super::*;

    fn some(s: &str) -> Option<String> {
        Some(s.to_string())
    }

    /// The 503's sentences are read by whoever was refused: one space
    /// between words. A joined line continuation left six in one of
    /// them (delta review 012ccafa, D3).
    #[test]
    fn a_refusal_sentence_holds_no_run_of_spaces() {
        for sentence in [REGISTRY_DARK, JUDGEMENT_UNWRITTEN] {
            assert!(!sentence.contains("  "), "{sentence:?}");
            assert_eq!(sentence, sentence.trim(), "{sentence:?}");
        }
    }

    /// The alias resolves on every method: reads must see the same
    /// identity writes stamp, or "my steps" would answer for the wrong
    /// actor.
    #[test]
    fn an_alias_resolves_on_reads_and_writes_alike() {
        for m in [
            Method::GET,
            Method::POST,
            Method::PUT,
            Method::PATCH,
            Method::DELETE,
        ] {
            assert_eq!(
                decide("claude@algedonic.dev", some("agent-claude"), &m),
                Resolution::Resolved("agent-claude".into()),
                "{m}"
            );
        }
    }

    /// An unmatched address counts on a write and on nothing else.
    #[test]
    fn an_unmatched_address_counts_only_on_a_write() {
        for m in [Method::POST, Method::PUT, Method::PATCH, Method::DELETE] {
            assert_eq!(
                decide("nobody@example.test", None, &m),
                Resolution::Unresolved,
                "{m}"
            );
        }
        for m in [Method::GET, Method::HEAD, Method::OPTIONS] {
            assert_eq!(
                decide("nobody@example.test", None, &m),
                Resolution::PassThrough,
                "{m}"
            );
        }
    }

    /// Identities the vocabulary already describes are not logins and
    /// never count, even on writes and even unregistered.
    #[test]
    fn identities_that_are_not_logins_pass_through() {
        for id in [
            "emp-david",
            "automation:train-conductor",
            "rule:bill-approve",
            "claude:opus-5[1m]",
            "agent-claude",
            "operator:unidentified",
            "brewery-sim",
        ] {
            assert_eq!(
                decide(id, None, &Method::PUT),
                Resolution::PassThrough,
                "{id}"
            );
        }
    }

    fn asserting(id: &str, role: &str, tier: AccessTier) -> User {
        User {
            id: id.into(),
            role: role.into(),
            access_tier: tier,
            territory_account_ids: Vec::new(),
            direct_report_ids: Vec::new(),
            department: None,
        }
    }

    fn row(id: &str, role: Option<&str>) -> AgentRow {
        AgentRow {
            id: id.into(),
            display_name: id.into(),
            default_model: "opus-5".into(),
            role: role.map(str::to_string),
            department: None,
            hourly_budget_usd_micros: None,
            max_concurrent_runs: None,
            aliases: Vec::new(),
        }
    }

    /// The row decides both halves, whatever was sent: operator tier
    /// goes with the platform-admin row and with no other.
    #[test]
    fn the_row_gives_the_role_and_the_tier() {
        let sent = asserting(
            "claude@algedonic.dev",
            "platform-admin",
            AccessTier::Operator,
        );
        let admin = judge(
            &sent,
            "agent-claude",
            Some(&row("agent-claude", Some("platform-admin"))),
        );
        assert_eq!(
            (admin.role.as_str(), admin.tier),
            ("platform-admin", AccessTier::Operator)
        );
        assert!(!admin.overrode());

        let narrow = judge(
            &sent,
            "agent-claude",
            Some(&row("agent-claude", Some("engineering-agent"))),
        );
        assert_eq!(
            (narrow.role.as_str(), narrow.tier),
            ("engineering-agent", AccessTier::User)
        );
        assert!(narrow.overrode());

        for none in [Some(row("agent-claude", None)), None] {
            let j = judge(&sent, "agent-claude", none.as_ref());
            assert_eq!(
                (j.role.as_str(), j.tier),
                (boss_core::roles::GUEST_ROLE, AccessTier::User)
            );
            assert!(j.overrode());
        }
    }

    /// The row can RAISE as well as lower: the header is not read for
    /// the role at all, so a client that sends none still writes as
    /// its row — which is what lets the client doors stop sending one.
    #[test]
    fn a_row_holding_the_role_gives_it_to_a_caller_that_asserted_less() {
        let sent = asserting("agent-claude", "guest", AccessTier::User);
        let j = judge(
            &sent,
            "agent-claude",
            Some(&row("agent-claude", Some("platform-admin"))),
        );
        assert_eq!(
            (j.role.as_str(), j.tier),
            ("platform-admin", AccessTier::Operator)
        );
    }

    /// An asserted operator tier under the row's own role is still not
    /// the row's to give.
    #[test]
    fn an_asserted_tier_above_the_rows_is_dropped() {
        let sent = asserting("agent-x", "engineering-agent", AccessTier::Operator);
        let j = judge(
            &sent,
            "agent-x",
            Some(&row("agent-x", Some("engineering-agent"))),
        );
        assert_eq!(j.tier, AccessTier::User);
        assert!(j.overrode(), "the role matched and the tier did not");
    }

    /// The sentence names all four: who, which row, what it gives, and
    /// what was asserted.
    #[test]
    fn the_refusal_names_who_which_row_what_it_gives_and_what_was_sent() {
        let sent = asserting(
            "codex@algedonic.dev",
            "platform-admin",
            AccessTier::Operator,
        );
        let why = judge(&sent, "agent-codex", Some(&row("agent-codex", None))).why();
        for part in [
            "`agent-codex`",
            "signed as `codex@algedonic.dev`",
            "row `agent-codex` holds no role (null)",
            "judged as `guest`",
            "asserted `platform-admin`, which is ignored",
        ] {
            assert!(why.contains(part), "missing {part:?} in: {why}");
        }
        let ghost = judge(&sent, "agent-ghost", None).why();
        assert!(
            ghost.contains("the agents registry holds no row for `agent-ghost`"),
            "{ghost}"
        );
    }

    /// Only the registry's own namespace is this door's question.
    #[test]
    fn only_an_agent_spelled_id_is_judged() {
        assert!(is_agent_shaped("agent-claude"));
        for id in [
            "emp-david",
            "automation:dispatcher",
            "rule:x",
            "claude:opus-5[1m]",
            "operator:unidentified",
            "guest@example.test",
        ] {
            assert!(!is_agent_shaped(id), "{id}");
        }
    }

    /// A registry answering with the id itself changes nothing.
    #[test]
    fn a_self_alias_is_a_pass_through() {
        assert_eq!(
            decide("agent-claude", some("agent-claude"), &Method::PUT),
            Resolution::PassThrough
        );
    }
}
