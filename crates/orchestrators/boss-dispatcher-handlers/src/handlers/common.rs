//! Common helpers shared across step-completion handlers.
//!
//! All step-completion handlers follow the same shape: read the
//! triggering `step.done.<kind>` event payload, extract step
//! metadata + subject + day, build an HTTP body, POST it. These
//! helpers cut the boilerplate to ~5 lines per handler.

use boss_dispatcher::rules::handler::HandlerError;
use boss_jobs::channels::{InputChannel, RECORDED_KEY};
use serde_json::Value;

/// Step-event payload fields the handlers commonly read.
///
/// The `step.done.<kind>` event published by jobs-api carries this
/// shape inside its `payload` envelope. The dispatcher unwraps the
/// envelope; handlers see this inner shape as `ctx.event_payload`.
#[derive(Debug)]
pub struct StepEvent<'a> {
    pub job_id: &'a str,
    pub step_id: &'a str,
    pub kind: &'a str,
    pub subject_kind: &'a str,
    pub subject_id: &'a str,
    pub completed_on: Option<chrono::NaiveDate>,
    /// Who the step is assigned to, if anyone. A named assignee is a
    /// STRONGER routing signal than a role — a role says someone like
    /// you should do this, an assignee says you specifically.
    pub assignee_id: Option<&'a str>,
    /// The parent job's owner, when the event names one — `step.done`
    /// carries it (`job_owner_id`) so a wait-is-over signal reaches the
    /// person waiting on the packet when the step names no role
    /// (backlog 58f0b536). `""` and absent both read as `None`.
    pub job_owner_id: Option<&'a str>,
    pub metadata: &'a serde_json::Map<String, Value>,
}

impl<'a> StepEvent<'a> {
    /// Extract the canonical fields from a step.done payload.
    /// Returns a tightly-typed view that handlers consume; errors
    /// surface as HandlerError::Downstream with a clear shape-mismatch
    /// message for the operator.
    pub fn from_payload(payload: &'a Value) -> Result<Self, HandlerError> {
        let obj = payload
            .as_object()
            .ok_or_else(|| HandlerError::Downstream("step.done payload is not an object".into()))?;

        let job_id = obj
            .get("job_id")
            .and_then(|v| v.as_str())
            .ok_or_else(|| HandlerError::Downstream("step.done payload missing job_id".into()))?;
        let step_id = obj
            .get("step_id")
            .and_then(|v| v.as_str())
            .ok_or_else(|| HandlerError::Downstream("step.done payload missing step_id".into()))?;
        let kind = obj
            .get("kind")
            .and_then(|v| v.as_str())
            .ok_or_else(|| HandlerError::Downstream("step.done payload missing kind".into()))?;
        let subject_kind = obj
            .get("subject_kind")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        let subject_id = obj.get("subject_id").and_then(|v| v.as_str()).unwrap_or("");
        let assignee_id = obj
            .get("assignee_id")
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty());
        let job_owner_id = obj
            .get("job_owner_id")
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty());
        let completed_on = obj
            .get("completed_on")
            .and_then(|v| v.as_str())
            .and_then(|s| chrono::NaiveDate::parse_from_str(s, "%Y-%m-%d").ok());
        let metadata = obj
            .get("metadata")
            .and_then(|v| v.as_object())
            .ok_or_else(|| {
                HandlerError::Downstream("step.done payload missing metadata object".into())
            })?;

        Ok(StepEvent {
            job_id,
            step_id,
            kind,
            subject_kind,
            subject_id,
            completed_on,
            assignee_id,
            job_owner_id,
            metadata,
        })
    }

    /// Convenience: pull a string field from step metadata, with a
    /// fallback closure for the common subject-derived defaults.
    pub fn meta_string_or<F: FnOnce(&Self) -> String>(&self, key: &str, fallback: F) -> String {
        self.metadata
            .get(key)
            .and_then(|v| v.as_str())
            .map(|s| s.to_string())
            .unwrap_or_else(|| fallback(self))
    }
}

/// Parse a `YYYY-MM-DD` string out of an optional JSON value, e.g. a
/// step-metadata field. Returns `None` when the value is absent, not a
/// string, or not a valid date — leaving the fallback to the caller.
pub(crate) fn parse_date(v: Option<&Value>) -> Option<chrono::NaiveDate> {
    v.and_then(|v| v.as_str())
        .and_then(|s| chrono::NaiveDate::parse_from_str(s, "%Y-%m-%d").ok())
}

/// The absorption fact's `source_id` for one driver:
/// `overhead-absorbed@{step_id}:{credit_account}`. Mirrors the id the
/// inventory absorption endpoint mints (`overhead_absorbed_handler`,
/// boss-inventory `http/items.rs`) — these two `format!`s are the
/// write/reconstruct halves of one contract; change them together.
/// The absorb side (`inventory.overhead.absorb`) posts one fact per
/// driver do; the drain side (`products.produce` drain-actual-wip)
/// reconstructs the same ids from its `overhead_accounts` rule arg —
/// both sets are rules data, and the brewery seed test asserts they
/// agree.
pub(crate) fn overhead_source_id(step_id: &str, credit_account: &str) -> String {
    format!("overhead-absorbed@{step_id}:{credit_account}")
}

/// The `x-sim-origin` value for a downstream call.
///
/// Reads the task-local the dispatch loop set from the TRIGGERING
/// event, so sim-ness is inherited rather than guessed. Downstream
/// services parse `"true"`/`"1"` as simulated and anything else as
/// real, so sending `"false"` explicitly is equivalent to omitting the
/// header — and saying it out loud is better than relying on absence,
/// because absence used to mean "ask the clock", which marked every
/// real user action on this deployment as simulated.
pub fn sim_origin_value() -> &'static str {
    if boss_core::sim_origin::is_in_sim_chain() {
        "true"
    } else {
        "false"
    }
}

/// The dispatcher's identity for a READ.
///
/// Writes stamp the specific rule (`dispatcher_actor_header`) because
/// the rule is provenance on the event that results. A read produces
/// no event, so the honest actor is the dispatcher itself — and a
/// read still has to present SOMEBODY, or it breaks the day the
/// service it calls grows a policy gate. That is not hypothetical:
/// one unstamped ledger read halted the whole WIP→FG→COGS chain when
/// `/api/ledger/*` became gated.
pub fn dispatcher_reader_header() -> String {
    serde_json::json!({
        "id": "automation:dispatcher",
        "role": "platform-admin",
        "access_tier": "operator",
        "territory_account_ids": [],
        "direct_report_ids": [],
        "department": "platform",
    })
    .to_string()
}

/// THE FLOOR under every handler that fans out over a roster read at
/// fire time (backlog 86ebf7fc): a roster that answers ZERO rows is
/// refused, never reported as a quiet pass.
///
/// WHY IT IS A REFUSAL AND NOT A WARNING. A wrong, dark or
/// policy-narrowed read answers `total: 0` instead of erroring — the
/// most expensive recurring shape in this codebase (CLAUDE.md §Doors,
/// "a wrong target answers instead of erroring"). The layer below can
/// be made honest — `GET /api/departments` refuses rather than flatten
/// a registry error to an empty list (80a77466) — and that is only
/// half: the consumer still has to refuse the EMPTY it is honestly
/// handed, because the rosters this applies to are ones whose zero
/// means the system is broken, not that the week was quiet. A warning
/// is not a refusal: it fires into a log nobody is reading at 00:00 on
/// a Monday, and the cadence passes with no packet anyone acts on.
///
/// WHAT REFUSING BUYS. `Permanent` terminates the firing at once
/// rather than burning the redelivery budget on a roster that will
/// answer empty identically every time, and the dead-letter is counted
/// on the dispatcher's liveness surface, where the estate chain reads
/// unrecorded dead-letters and files a finding (8834804a) — a packet
/// someone acts on, which is exactly what the quiet pass never made.
///
/// `why_zero_is_broken` is the caller's sentence, because the floor is
/// generic and the reason never is.
pub(crate) fn empty_roster_refusal(
    handler: &str,
    roster: &str,
    why_zero_is_broken: &str,
) -> HandlerError {
    HandlerError::Permanent(format!(
        "{handler}: the {roster} roster is empty — {why_zero_is_broken}. Firing successfully \
         over zero rows is absence treated as an answer, and it hides the fault for a whole \
         cadence at a time, so this refuses instead: a fan-out over an empty roster is a check \
         that is not running"
    ))
}

/// The client every handler's production constructor uses, for the
/// estate's own services and nothing else: it stamps the machine token
/// (7fcd78fa phase 1; design 6805c764) on every request it builds. One
/// definition, so a new handler cannot forget the token by writing
/// `Client::new()` out of habit -- the gate's 401 names the header if
/// one does.
///
/// STAMPED PER REQUEST (design 6805c764 car 2, the handlers slice;
/// review S1 of car 2 slice 1). Until this slice it read the token ONCE
/// into `default_headers`, and the dispatcher builds every handler, and
/// so every client, once at boot: after the broker rotated `current`,
/// all sixty-odd handlers went on sending the boot-time value until a
/// restart, and car 3's revoke of `previous` would have refused them
/// all at an enforcing gate. `machine_token::Client` reads the process's
/// one watched source on each request instead.
///
/// NEVER A THIRD PARTY'S CLIENT. The type is `machine_token::Client`,
/// not `reqwest::Client`, in every handler field, `with_client` and
/// shared helper below, so this client cannot be handed to anything
/// that speaks to Stripe, a webhook URL, the forge, Cloudflare or
/// GitHub — those take a `reqwest::Client` of their own
/// (`webhook_notify`, `stripe_*`, `credential_issuer`), and a mix-up is
/// a compile error rather than a leak. It follows no redirect, so a 3xx
/// from anything it reached comes back to the handler as a response
/// instead of carrying the token to the host a `Location` names.
pub fn api_client() -> boss_core::machine_token::Client {
    boss_core::machine_token::Client::build(reqwest::Client::builder())
        .expect("reqwest client always builds")
}

/// The owner a handler files a packet with: the platform owner as the
/// port answers it, or NOBODY with the refusal in the journal under the
/// rule's name (backlog 3c23662d — every alarm handler wrote
/// `"emp-david"` until 2026-09-18). The filing goes ahead either way:
/// the jobs API resolves the kind's `owner_role` from the same registry
/// or refuses by name, and a handler that fell silent for want of an
/// owner would be the failure mode the alarms exist to end.
pub async fn owner_for_filing(
    port: &dyn boss_core::platform_owner::PlatformOwner,
    rule_name: &str,
) -> String {
    boss_core::platform_owner::owner_for_filing(port, |e| {
        tracing::warn!(
            rule = rule_name,
            "{e}; filing with no owner named — the jobs API resolves the kind's owner_role, or refuses"
        )
    })
    .await
}

/// Re-exported, not defined here: the rules runner in core writes as
/// this same actor when it lands a dead-letter on a packet
/// (`boss_dispatcher::rules::dead_letter`), and an identity that exists
/// twice drifts — a policy refusal that reads as missing data. One
/// definition, in `boss_dispatcher::rules::actor` (CLAUDE.md §9a); every
/// call site here is unchanged.
pub use boss_dispatcher::rules::actor::dispatcher_actor_header;

/// POST a JSON body to a downstream service, stamping the dispatcher's
/// rule-as-actor `x-boss-user` header, and map a non-2xx response into a
/// `HandlerError::Downstream`.
///
/// This is the shared epilogue every step-completion handler ends with:
/// build the POST, attach `content-type: application/json` +
/// `x-boss-user: dispatcher_actor_header(rule_name)`, send, and turn a
/// transport failure or non-success status into a `Downstream` error with
/// the URL/status/body baked into the message. Handlers whose epilogue
/// differs (a PUT, a response-body read, a lenient no-fail webhook, or an
/// omitted header) keep their inline call.
pub(crate) async fn post_json(
    client: &boss_core::machine_token::Client,
    url: &str,
    body: &Value,
    rule_name: &str,
) -> Result<(), HandlerError> {
    let resp = client
        .post(url)
        .header("content-type", "application/json")
        .header("x-boss-user", dispatcher_actor_header(rule_name))
        .header("x-sim-origin", sim_origin_value())
        .json(body)
        .send()
        .await
        .map_err(|e| HandlerError::Downstream(format!("POST {url}: {e}")))?;
    if !resp.status().is_success() {
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        // House contract: 422 = deterministic request-data error (a
        // seed typo, a malformed body) — identical on every redelivery,
        // so the runner Terms immediately instead of burning the NAK
        // budget. Convergent conflicts (409 insufficient-stock, 404
        // not-yet-projected) stay retryable.
        if status == reqwest::StatusCode::UNPROCESSABLE_ENTITY {
            return Err(HandlerError::Permanent(format!(
                "POST {url} returned {status}: {body}"
            )));
        }
        return Err(HandlerError::Downstream(format!(
            "POST {url} returned {status}: {body}"
        )));
    }
    Ok(())
}

/// [`post_json`] for a create keyed on a DETERMINISTIC id: `Ok(true)`
/// when the POST created the row, `Ok(false)` when the service answered
/// 409 because the id is already held — which the caller must then
/// judge (a redelivery of its own create, or a real collision). Every
/// other answer is [`post_json`]'s. Added for `shipping.create` (backlog
/// be459ab9): the shipment store refuses a held id since 2026-10-01
/// instead of upserting over it.
pub(crate) async fn post_json_or_held(
    client: &boss_core::machine_token::Client,
    url: &str,
    body: &Value,
    rule_name: &str,
) -> Result<bool, HandlerError> {
    let resp = client
        .post(url)
        .header("content-type", "application/json")
        .header("x-boss-user", dispatcher_actor_header(rule_name))
        .header("x-sim-origin", sim_origin_value())
        .json(body)
        .send()
        .await
        .map_err(|e| HandlerError::Downstream(format!("POST {url}: {e}")))?;
    let status = resp.status();
    if status.is_success() {
        return Ok(true);
    }
    if status == reqwest::StatusCode::CONFLICT {
        return Ok(false);
    }
    let text = resp.text().await.unwrap_or_default();
    if status == reqwest::StatusCode::UNPROCESSABLE_ENTITY {
        return Err(HandlerError::Permanent(format!(
            "POST {url} returned {status}: {text}"
        )));
    }
    Err(HandlerError::Downstream(format!(
        "POST {url} returned {status}: {text}"
    )))
}

/// POST a packet and read the id the jobs API minted for it — the one
/// thing [`post_json`] does not return, and the thing a judging
/// handler's note on the judged packet names. Same 422 contract.
///
/// Lived in `ops_judge` until `maintenance.chore.file_reds` needed the
/// same POST-and-read (ac3270c7) — one definition rather than a second
/// copy (CLAUDE.md §9a).
pub(crate) async fn post_json_minted_id(
    client: &boss_core::machine_token::Client,
    url: &str,
    body: &Value,
    rule_name: &str,
) -> Result<String, HandlerError> {
    let resp = client
        .post(url)
        .header("content-type", "application/json")
        .header("x-boss-user", dispatcher_actor_header(rule_name))
        .header("x-sim-origin", sim_origin_value())
        .json(body)
        .send()
        .await
        .map_err(|e| HandlerError::Downstream(format!("POST {url}: {e}")))?;
    let status = resp.status();
    if !status.is_success() {
        let text = resp.text().await.unwrap_or_default();
        return Err(if status == reqwest::StatusCode::UNPROCESSABLE_ENTITY {
            HandlerError::Permanent(format!("POST {url} returned {status}: {text}"))
        } else {
            HandlerError::Downstream(format!("POST {url} returned {status}: {text}"))
        });
    }
    let created: Value = resp
        .json()
        .await
        .map_err(|e| HandlerError::Downstream(format!("POST {url} answer not JSON: {e}")))?;
    created
        .get("id")
        .or_else(|| created.get("data").and_then(|d| d.get("id")))
        .and_then(|v| v.as_str())
        .map(str::to_string)
        .ok_or_else(|| HandlerError::Downstream(format!("POST {url} answered no id: {created}")))
}

/// The `direct` inbox message that carries a machine-filed alarm to the
/// person it is owned by (backlog 35dabc3d). Measured 2026-11-17: the
/// unheld-review watch (fd20fb6c, 07:01Z) and the silent-session watch
/// (a79f9097, 08:01Z) fired inside a seven-hour stall, urgent, owned by
/// the platform owner and ASSIGNED TO NOBODY — and `messages.notify`
/// pages nobody for a role-only ready step, so their only reader was the
/// coordinator session that had stopped. A `direct` message is the kind
/// the inbox's needs-you filter is built on, sent by the dispatcher,
/// which owes nothing to any agent session.
///
/// The id is `<scope>:<key>:<recipient>`, and a watch that re-sends on
/// every tick while its alarm stays open is collapsed by the messages
/// store's `ON CONFLICT (id) DO NOTHING`: a send that failed after the
/// filing is repaired next tick, and a successful one is never doubled.
/// The key is the EPISODE. A watch that holds one open alarm per episode
/// (closing it re-arms the next) passes the alarm id, through
/// [`tell_owner`]; the factory-idle watch, whose one open alarm carries
/// several episodes (`factory_idle_since`, appended by `alarm_item`),
/// passes its episode stamp, so each new episode tells the owner once
/// (backlog f53b2df3). The shape is car f94d6d92's factory-idle message.
pub(crate) fn owner_message(
    scope: &str,
    key: &str,
    alarm_id: &str,
    alarm: &Value,
    owner: &str,
) -> Value {
    let field = |pointer: &str| alarm.pointer(pointer).and_then(Value::as_str).unwrap_or("");
    serde_json::json!({
        "id": format!("{scope}:{key}:{owner}"),
        "sender_id": "automation:dispatcher",
        "recipient_id": owner,
        "subject": field("/title"),
        "body": format!(
            "{} Opening this message goes straight to the alarm.",
            field("/metadata/detail")
        ),
        "kind": "direct",
        "entity_ref": {
            "entity_type": "job",
            "entity_id": alarm_id,
            "entity_path": format!("/jobs/{alarm_id}"),
        },
    })
}

/// Send [`owner_message`] through the messages service. An alarm with
/// no owner (the port refused at filing) or no id reaches the backlog
/// only, and the journal says so under the rule's name — there is no
/// one to tell, and failing the tick would also stop the filing.
pub(crate) async fn tell_owner(
    client: &boss_core::machine_token::Client,
    messages_base: &str,
    scope: &str,
    alarm_id: &str,
    alarm: &Value,
    owner: &str,
    rule_name: &str,
) -> Result<(), HandlerError> {
    tell_owner_keyed(
        client,
        messages_base,
        scope,
        alarm_id,
        alarm_id,
        alarm,
        owner,
        rule_name,
    )
    .await
}

/// [`tell_owner`] under the collapsing `key` [`owner_message`]
/// names. An empty key is refused like an empty alarm id: it would
/// collapse every episode's message into one.
#[allow(clippy::too_many_arguments)]
pub(crate) async fn tell_owner_keyed(
    client: &boss_core::machine_token::Client,
    messages_base: &str,
    scope: &str,
    key: &str,
    alarm_id: &str,
    alarm: &Value,
    owner: &str,
    rule_name: &str,
) -> Result<(), HandlerError> {
    if owner.trim().is_empty() || alarm_id.trim().is_empty() || key.trim().is_empty() {
        tracing::warn!(
            rule = rule_name,
            alarm_id,
            scope,
            "no owner or alarm id to tell — the alarm reaches the backlog only"
        );
        return Ok(());
    }
    post_json(
        client,
        &format!("{}/api/messages/send", messages_base.trim_end_matches('/')),
        &owner_message(scope, key, alarm_id, alarm, owner),
        rule_name,
    )
    .await
}

/// GET a JSON document from a downstream service, stamping the same
/// rule-as-actor `x-boss-user` header as [`post_json`], mapping
/// transport failures and non-2xx responses into
/// `HandlerError::Downstream`.
pub(crate) async fn get_json(
    client: &boss_core::machine_token::Client,
    url: &str,
    rule_name: &str,
) -> Result<Value, HandlerError> {
    let resp = client
        .get(url)
        .header("x-boss-user", dispatcher_actor_header(rule_name))
        .header("x-sim-origin", sim_origin_value())
        .send()
        .await
        .map_err(|e| HandlerError::Downstream(format!("GET {url}: {e}")))?;
    if !resp.status().is_success() {
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        // Same 422 contract as post_json.
        if status == reqwest::StatusCode::UNPROCESSABLE_ENTITY {
            return Err(HandlerError::Permanent(format!(
                "GET {url} returned {status}: {body}"
            )));
        }
        return Err(HandlerError::Downstream(format!(
            "GET {url} returned {status}: {body}"
        )));
    }
    resp.json()
        .await
        .map_err(|e| HandlerError::Downstream(format!("GET {url} not JSON: {e}")))
}

/// Stamp the INPUT LANE onto a machine filer's packet metadata
/// (backlog b2d9b432).
///
/// Every backlog-item this crate files has a perfectly known origin —
/// an estate alarm is telemetry, a red chore line is a pipeline
/// failure, a failed ops verb is a pipeline failure — better known than
/// any human filer knows theirs. None of them recorded it, so from
/// `FIELD_LIVE_AT` (2026-09-21T00:00:00Z) every one would have read as
/// "the filer did not say", and the filed-without-a-lane count would
/// have measured the machines instead of the operators. A count that
/// mixes "somebody forgot" with "the machines do not participate" is
/// worse than no count.
///
/// The lane is a REQUIRED ARGUMENT, not an option with a default: a
/// filer that cannot say which lane it is has no business filing, and a
/// default would quietly re-create the silence this fixes. The key and
/// the labels come from `boss_jobs::channels`, the one definition the
/// CLI door reads too, so this cannot drift from what `boss job file
/// --channel` accepts.
pub(crate) fn with_lane(metadata: Value, lane: InputChannel) -> Value {
    let mut metadata = match metadata {
        Value::Object(m) => m,
        // A filer that handed us something that is not an object had no
        // metadata to keep; the lane is still worth recording.
        _ => serde_json::Map::new(),
    };
    metadata.insert(RECORDED_KEY.into(), Value::String(lane.label().into()));
    Value::Object(metadata)
}

/// The lane's recorded label, for a filer whose metadata is an inline
/// `json!` object rather than a value to wrap. Same one definition as
/// [`with_lane`]; see it for why the lane is never defaulted.
pub(crate) fn lane_label(lane: InputChannel) -> &'static str {
    lane.label()
}

/// The rows of a listing, or a refusal naming the reading — the ONE
/// place this handler crate decides what a missing `data` array means.
///
/// WHY THIS EXISTS (backlog 833e2d0a). The same chain —
/// `.get("data") … .unwrap_or_default()` — turns an ERROR SHAPE into an
/// empty list, so a dark, narrowed or restarted far side reads as
/// "nothing to do" and the pass reports healthy. It has been cured in
/// three handlers ONE AT A TIME, each found by accident by someone
/// working on something else: `retro.open`'s departments (80a77466),
/// `sensor.poll`'s sensors (6c4c432a), and its owed readings
/// (0767c830). A grep sweep is not the cure and that is measured: in
/// `sensor_poll.rs` alone the chain appears four times, of which one
/// was the bug, one is safe by accident of its own truncation check
/// and one is a different shape. So the JUDGEMENT moves here instead,
/// and a reading that takes it cannot drift from the other readings.
///
/// `.filter(is_array)` rather than `as_array().to_vec()` is the
/// load-bearing detail: `"data": null` and `"data": {}` refuse too,
/// rather than being read as zero rows.
///
/// An EMPTY array is honest and stays honest — a registry may
/// legitimately hold nothing, so this puts no floor on the count. The
/// refusal is a `String` so the caller decides its class; at every
/// current call site that is `HandlerError::Downstream`, because a
/// listing with no `data` is a bad answer from the far side rather
/// than a deterministic fault in the request, and a redelivery can
/// succeed once the service or the policy scope is right.
pub(crate) fn rows_or_refuse<T: serde::de::DeserializeOwned>(
    listing: &Value,
    what: &str,
) -> Result<Vec<T>, String> {
    let rows = listing
        .get("data")
        .filter(|d| d.is_array())
        .ok_or_else(|| format!("{what} answered no `data` array"))?;
    serde_json::from_value(rows.clone()).map_err(|e| format!("{what}: rows not in shape: {e}"))
}

/// The one row a single-row read answered, or a refusal naming the
/// reading and what the body carried instead — the single-row
/// neighbour of [`rows_or_refuse`], and the one place this crate
/// decides what such a body means.
///
/// WHY THIS EXISTS (backlog f2eac973). Seven reads spelled
/// `.get("data").cloned().unwrap_or(job)`: unwrap an envelope, else take
/// the body. Measured against the servers, no envelope is ever sent —
/// `GET /api/jobs/{id}` answers a flattened `JobDetail` and `GET
/// /api/credentials/{id}` a bare `CredentialRow` — so the hedge never
/// fired and its fallback was the only live path: ANY 200 body was the
/// row. `maintenance.sweep.inspect` and `dns.observe` then failed a kind
/// check on it and returned `Ok(())`, a ready step skipped without a
/// word. The helper therefore does not unwrap an envelope either (that
/// would be the same guess, kept); it reads the row bare and requires
/// the one field every such row carries, a non-empty string `id`. An
/// envelope, an error body, `null` or a list all refuse, and the
/// refusal lists the body's top-level keys so the next reader does not
/// have to re-fetch it to learn what came back.
///
/// The refusal is a `String` for the same reason as `rows_or_refuse`:
/// the caller picks the class, and at every current site that is
/// `HandlerError::Downstream` — a bad answer from the far side, which a
/// redelivery can outlive.
pub(crate) fn row_or_refuse(body: Value, what: &str) -> Result<Value, String> {
    if body
        .get("id")
        .and_then(Value::as_str)
        .is_some_and(|id| !id.is_empty())
    {
        return Ok(body);
    }
    let carried = match &body {
        Value::Object(m) => format!(
            "keys [{}]",
            m.keys().map(String::as_str).collect::<Vec<_>>().join(", ")
        ),
        Value::Array(_) => "a list".to_string(),
        Value::Null => "null".to_string(),
        _ => "a scalar".to_string(),
    };
    Err(format!(
        "{what} answered no row (no string `id`; the body carried {carried})"
    ))
}

/// Every open Job of `kind`, steps inline, paged on the list's `total`
/// so a packet sorted past one page is still found — a capped page is
/// a false negative that grows with the board's age.
///
/// Lived in `jobs_run_car_probes` until `jobs.complete_step_matching`
/// needed the same walk (c34583cb) — one definition rather than a
/// second copy (CLAUDE.md §9a).
pub(crate) async fn open_jobs_of_kind(
    client: &boss_core::machine_token::Client,
    jobs_base: &str,
    kind: &str,
    rule_name: &str,
) -> Result<Vec<Value>, HandlerError> {
    open_jobs(client, jobs_base, Some(kind), rule_name).await
}

/// The same walk with the kind OPTIONAL: `None` is the whole open
/// board. A handler asking a question about claimed STEPS rather than
/// about packets of one kind needs it — `jobs.reclaim_abandoned_step`
/// looks for steps held by a dead executor run, and those sit on
/// packets of every kind an agent block appears on (a3397b01).
pub(crate) async fn open_jobs(
    client: &boss_core::machine_token::Client,
    jobs_base: &str,
    kind: Option<&str>,
    rule_name: &str,
) -> Result<Vec<Value>, HandlerError> {
    let kind_q = match kind {
        Some(k) => format!("kind={k}&"),
        None => String::new(),
    };
    jobs_where(
        client,
        jobs_base,
        &format!("{kind_q}status=open"),
        rule_name,
    )
    .await
}

/// The same paged walk over any `/api/jobs` filter — `filter` is the
/// query string without `limit`/`offset`. The open board is one filter
/// of it; the owed-proof obligation reads CLOSED cars by the key they
/// carry (`kind=ship-a-change&metadata_has=proof_owed`, b9005734), which
/// no open-only walk can reach.
///
/// Every row comes back with its steps WHOLE (`full=true`). The list
/// is going slim by default, a step's `metadata` only when asked
/// (backlog 9b473d4a), and the walk's callers read it off the listed
/// rows — the claimed step's `agent_model`, the matched field, the
/// triage disposition a retraction names, a decision's content — so
/// one flag here keeps the "steps inline" this crate's doc promises,
/// for every caller.
pub(crate) async fn jobs_where(
    client: &boss_core::machine_token::Client,
    jobs_base: &str,
    filter: &str,
    rule_name: &str,
) -> Result<Vec<Value>, HandlerError> {
    walk_jobs(client, jobs_base, filter, true, rule_name).await
}

/// The same paged walk with every listed step SLIM — no step
/// `metadata` or `fields` — for a caller that reads only the packet's
/// own keys and its steps' status, assignee and stamps. The open
/// backlog was 672 packets on 2026-10-18, and 200 of them weighed
/// 3.6 MB whole (9b473d4a); `jobs.urgent_item_watch` reads all of them
/// hourly to find the few urgent ones, and reads a step's metadata only
/// off the one packet it is about to write (backlog 9788e22d).
pub(crate) async fn jobs_where_slim(
    client: &boss_core::machine_token::Client,
    jobs_base: &str,
    filter: &str,
    rule_name: &str,
) -> Result<Vec<Value>, HandlerError> {
    walk_jobs(client, jobs_base, filter, false, rule_name).await
}

async fn walk_jobs(
    client: &boss_core::machine_token::Client,
    jobs_base: &str,
    filter: &str,
    full: bool,
    rule_name: &str,
) -> Result<Vec<Value>, HandlerError> {
    const PAGE: usize = 500;
    let mut rows: Vec<Value> = Vec::new();
    loop {
        let body = get_json(
            client,
            &format!(
                "{}/api/jobs?{filter}&full={full}&limit={PAGE}&offset={}",
                jobs_base.trim_end_matches('/'),
                rows.len()
            ),
            rule_name,
        )
        .await?;
        let total = body.get("total").and_then(Value::as_u64).unwrap_or(0) as usize;
        // A page with no `data` array is no answer, and reading it as
        // zero rows broke the loop below on `got == 0` and returned an
        // EMPTY BOARD to every rule that walks it (backlog 833e2d0a).
        let page: Vec<Value> =
            rows_or_refuse(&body, "GET /api/jobs").map_err(HandlerError::Downstream)?;
        let got = page.len();
        rows.extend(page);
        if got == 0 || rows.len() >= total {
            break;
        }
    }
    Ok(rows)
}

/// PUT or PATCH a body, mapping non-2xx the same way [`post_json`]
/// does. Completing a step is a PUT and merging job metadata is a
/// PATCH on the metadata door; the shared POST helper covers neither.
///
/// Lived in `jobs_auto_park` until `cadence.silence.sweep` needed the
/// same two verbs — one definition rather than a second copy
/// (CLAUDE.md 9a: collapse it if you can).
pub(crate) async fn write_json(
    client: &boss_core::machine_token::Client,
    method: reqwest::Method,
    url: &str,
    body: &Value,
    rule_name: &str,
) -> Result<(), HandlerError> {
    send_json(client, method, url, body, rule_name, None)
        .await
        .map(|_| ())
}

/// What a judged write came to: it landed, or the step no longer read as
/// judged and NOTHING was written (409 `step_changed`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Judged {
    Written,
    Moved,
}

/// [`write_json`] carrying the keys the write JUDGED, as it read them, in
/// `boss_jobs::step_metadata_write::EXPECT_HEADER` (backlog cd1f68c6):
/// the step merge door lands it only on a row that still reads so. The
/// step race is answered as [`Judged::Moved`] for the caller to say in
/// its own words; every other answer maps as [`write_json`]'s.
pub(crate) async fn write_json_expecting(
    client: &boss_core::machine_token::Client,
    method: reqwest::Method,
    url: &str,
    body: &Value,
    expect: &serde_json::Map<String, Value>,
    rule_name: &str,
) -> Result<Judged, HandlerError> {
    send_json(client, method, url, body, rule_name, Some(expect)).await
}

async fn send_json(
    client: &boss_core::machine_token::Client,
    method: reqwest::Method,
    url: &str,
    body: &Value,
    rule_name: &str,
    expect: Option<&serde_json::Map<String, Value>>,
) -> Result<Judged, HandlerError> {
    let verb = method.to_string();
    let request = client
        .request(method, url)
        .header("content-type", "application/json")
        .header("x-boss-user", dispatcher_actor_header(rule_name))
        .header("x-sim-origin", sim_origin_value());
    let request = match expect {
        Some(expect) => request.header(
            boss_jobs::step_metadata_write::EXPECT_HEADER,
            boss_jobs::step_metadata_write::expectation_header(expect),
        ),
        None => request,
    };
    let resp = request
        .json(body)
        .send()
        .await
        .map_err(|e| HandlerError::Downstream(format!("{verb} {url}: {e}")))?;
    if !resp.status().is_success() {
        let status = resp.status();
        let text = resp.text().await.unwrap_or_default();
        // Only a judged write reads the race as an answer: an unjudged
        // one keeps the retryable refusal it always was.
        if expect.is_some()
            && status == reqwest::StatusCode::CONFLICT
            && serde_json::from_str::<Value>(&text)
                .is_ok_and(|b| boss_jobs::step_metadata_write::is_step_race(&b))
        {
            return Ok(Judged::Moved);
        }
        return Err(if status == reqwest::StatusCode::UNPROCESSABLE_ENTITY {
            HandlerError::Permanent(format!("{verb} {url} returned {status}: {text}"))
        } else {
            HandlerError::Downstream(format!("{verb} {url} returned {status}: {text}"))
        });
    }
    Ok(Judged::Written)
}

/// The writes that complete step `sid` on packet `jid` carrying
/// `fields`, in order: the fields through the step's MERGE door, then
/// the status alone through the step PUT. Paths only — the caller
/// prefixes its base. Empty `fields` is the flip alone. Pure, so the
/// shape is pinned without a socket.
///
/// WHY TWO WRITES AND NOT THE ONE PUT EVERY HANDLER HERE USED TO SEND
/// (backlog e39a9d2a, stage 2 of design 93d2bddb). A handler read the
/// step, merged its own keys into what it read, and PUT `{status,
/// metadata}` back. The step PUT replaces metadata wholesale, so that
/// read-merge-write is correct only while nothing writes the step
/// between the handler's read and its PUT: stage 1 refuses such a PUT
/// 409 when it omits a key someone added meanwhile, and David's decided
/// end state refuses ANY metadata body on the PUT — the tighten is one
/// block in `update_step`, waiting on writers like these. The merge
/// door lands the keys against the row as it stands, so it can neither
/// race nor shed a key it does not name, and a handler no longer needs
/// the step's metadata at all to write its own. It goes FIRST because
/// required-at-done fields are judged when the step flips; if the flip
/// is then refused the step stays open carrying the fields, and a
/// redelivery merges the same keys again — the outcome a refused PUT
/// left. The same shape as `boss_jobs::car::StepWrite` (the auto-park
/// path) and boss-cli's `train::step_completion_writes`.
pub(crate) fn step_completion_writes(
    jid: &str,
    sid: &str,
    fields: serde_json::Map<String, Value>,
) -> Vec<(reqwest::Method, String, Value)> {
    let path = format!("/api/jobs/{jid}/steps/{sid}");
    let merge = (!fields.is_empty()).then(|| {
        (
            reqwest::Method::PATCH,
            format!("{path}/metadata"),
            Value::Object(fields),
        )
    });
    merge
        .into_iter()
        .chain(std::iter::once((
            reqwest::Method::PUT,
            path,
            serde_json::json!({ "status": "completed" }),
        )))
        .collect()
}

/// Complete step `sid` on packet `jid` with `fields`: the
/// [`step_completion_writes`], each through [`write_json`] against
/// `base`, stopping at the first refusal.
pub(crate) async fn complete_step(
    client: &boss_core::machine_token::Client,
    base: &str,
    jid: &str,
    sid: &str,
    fields: serde_json::Map<String, Value>,
    rule_name: &str,
) -> Result<(), HandlerError> {
    for (method, path, body) in step_completion_writes(jid, sid, fields) {
        write_json(client, method, &format!("{base}{path}"), &body, rule_name).await?;
    }
    Ok(())
}

/// [`complete_step`] for a step the handler READ (`step`, whose id is
/// `sid`): the status PUT names the resource claim the step held as
/// read, so completing a claimed step frees its slot (backlog 7b5195ea,
/// [`boss_jobs::flow_resources::exiting`]). A run's `building` holds a
/// robots-pool claim since agent-run v11, and the landings that end it
/// sent a bare status the PUT refuses.
pub(crate) async fn complete_read_step(
    client: &boss_core::machine_token::Client,
    base: &str,
    jid: &str,
    sid: &str,
    step: &Value,
    fields: serde_json::Map<String, Value>,
    rule_name: &str,
) -> Result<(), HandlerError> {
    for (method, path, body) in step_completion_writes(jid, sid, fields) {
        let body = match method {
            reqwest::Method::PUT => boss_jobs::flow_resources::exiting(step, body),
            _ => body,
        };
        write_json(client, method, &format!("{base}{path}"), &body, rule_name).await?;
    }
    Ok(())
}

/// The step a machine completes to close a `backlog-item` alarm it
/// raised. The kind routes on `triage.disposition`, and `stale` is the
/// terminal whose title is literally "Closed — the claim no longer
/// holds" (infra/platform/workflows/backlog-item.toml).
pub(crate) const TRIAGE_SLUG: &str = "triage";

/// The id of the `triage` step of one alarm packet.
///
/// Lived in `cadence_silence` until `estate.recover` closed alarms the
/// same way (backlog ef421cd3) — one definition of "the step that
/// closes an alarm", not a second copy (CLAUDE.md §9a). It used to hand
/// back the step's metadata too, for a completion PUT that replaced it
/// wholesale; a completion now merges its own keys through the step
/// merge door ([`complete_step`], e39a9d2a), so nothing needs it.
pub(crate) fn triage_step(job: &Value) -> Option<String> {
    job.get("steps")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .find(|s| s.get("spec_slug").and_then(Value::as_str) == Some(TRIAGE_SLUG))
        .and_then(|s| s.get("id").and_then(Value::as_str).map(str::to_string))
}

/// The routed steps a machine may complete to withdraw an alarm once a
/// person has triaged it — the two whose fields carry a `stale`
/// disposition (infra/platform/workflows/backlog-item.toml: `measure`
/// since 2802ba8c, `build` since 6c114a23). The design route has none:
/// `draft-design` needs a design id and `design-review` is a decision.
const WITHDRAWING_SLUGS: [&str; 2] = ["measure", "build"];

/// How a machine withdraws an alarm it raised once the condition has
/// cleared (backlog a2d8bad3). `pub` because the train conductor closes
/// its own alarms by the same judgement (backlog e61093a1) rather than a
/// copy of it.
#[derive(Debug, Clone, PartialEq)]
pub enum Retraction {
    /// Complete this step with `disposition = stale`, through
    /// [`complete_step`] — the step's own keys are not carried, because
    /// the merge door keeps every key it is not sent (e39a9d2a).
    Complete { slug: String, step_id: String },
    /// No step the machine may complete: say on the PACKET that the
    /// condition cleared, and why it is still open.
    Annotate { why_open: String },
}

/// THE step a recovered alarm withdraws at — one definition for every
/// machine that closes its own alarm (CLAUDE.md §9a).
///
/// WHY IT IS NOT ALWAYS `triage` (backlog a2d8bad3). Both closers
/// completed the triage step, which is right only while nobody has
/// routed the alarm. Once a person triaged it, that step was already
/// complete, the jobs API refused the PUT as a write to a terminal
/// step, and the machine had no other exit: alarm a6a4ae18 (CADENCE
/// SILENT: ops-request/github-mirror), routed to `build` on 2026-09-20,
/// had its cadence back on 2026-09-21 and sat open with `build` ready,
/// its measurement frozen at the raise, until a person closed it on
/// 2026-09-23.
///
/// So the machine withdraws at the step the packet is WAITING ON:
/// - `triage`, while it is open — unchanged;
/// - a READY `measure` or `build`, the step the route opened, which
///   carries `stale` for exactly this — the claim no longer holds;
/// - otherwise it annotates. An ACTIVE step has an executor on it, and
///   completing a dispatched run's step fires
///   `agent-run-delivers-when-its-step-is-done`, which lands the run as
///   `delivered` for work it did not deliver; the design route has no
///   withdrawal at all. The executor, or the person deciding, reads the
///   note and closes it — a machine alarm a human has touched can still
///   tell the human it is over.
///
/// A human's route is not overridden by this: the `stale` completion is
/// stamped `cleared_by`, which both raisers' settled-suppression reads
/// as a machine clear, so a condition that returns re-raises instead of
/// hiding behind the close — and a HUMAN's close still holds for its
/// settle window, exactly as before.
///
/// `None` is a packet with no triage step, which is not an alarm this
/// crate can judge.
///
/// THE TRAIN CONDUCTOR READS IT TOO (backlog e61093a1). Its LEFT BEHIND
/// and stranded-green alarms closed at `triage` only — the defect this
/// function was written to cure, rebuilt a crate away — and on
/// 2026-09-28 two LEFT BEHIND alarms routed to `build` sat open urgent
/// on a 409 every pass. One judgement, two callers (CLAUDE.md §9a).
pub fn retraction(job: &Value) -> Option<Retraction> {
    let steps: Vec<&Value> = job
        .get("steps")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .collect();
    let slug_of = |s: &Value| {
        s.get("spec_slug")
            .and_then(Value::as_str)
            .map(str::to_string)
    };
    let status_of = |s: &Value| s.get("status").and_then(Value::as_str).map(str::to_string);
    let triage_id = triage_step(job)?;
    let triage = steps
        .iter()
        .find(|s| slug_of(s).as_deref() == Some(TRIAGE_SLUG))?;
    // A triage with no status is read as open: the listing always
    // carries one, and the older fixtures do not.
    if !matches!(
        status_of(triage).as_deref(),
        Some("completed") | Some("skipped")
    ) {
        return Some(Retraction::Complete {
            slug: TRIAGE_SLUG.to_string(),
            step_id: triage_id,
        });
    }
    let routed = triage
        .pointer("/metadata/disposition")
        .and_then(Value::as_str)
        .unwrap_or("an unrecorded route");
    for slug in WITHDRAWING_SLUGS {
        let Some(step) = steps.iter().find(|s| slug_of(s).as_deref() == Some(slug)) else {
            continue;
        };
        match status_of(step).as_deref() {
            Some("ready") => {
                return Some(Retraction::Complete {
                    slug: slug.to_string(),
                    step_id: step.get("id").and_then(Value::as_str)?.to_string(),
                });
            }
            Some("active") => {
                return Some(Retraction::Annotate {
                    why_open: format!(
                        "triage routed it to `{routed}` and `{slug}` is active — an executor \
                         holds it, and the machine does not complete a step from under its \
                         executor"
                    ),
                });
            }
            _ => {}
        }
    }
    Some(Retraction::Annotate {
        why_open: format!(
            "triage routed it to `{routed}`, and no step that route has open carries a \
             withdrawal the machine may complete"
        ),
    })
}

/// The fields that withdraw a recovered alarm at the step
/// [`retraction`] names, for a machine that judged the condition from a
/// reading: `stale` — the backlog-item terminal "the claim no longer
/// holds" — with the evidence, who cleared it and when. ONLY these: they
/// ride the step merge door ([`complete_step`]), which keeps every key
/// the step holds (e39a9d2a).
///
/// One definition, since the queue alarm, the flight overdue and the
/// agent-step overdue each carried the same body a copy apiece, each
/// with the step's own metadata cloned in for a PUT that replaced it.
pub(crate) fn withdrawal_fields(
    evidence: &str,
    cleared_by: &str,
    at: &str,
) -> serde_json::Map<String, Value> {
    let mut m = serde_json::Map::new();
    m.insert("disposition".into(), Value::String("stale".into()));
    m.insert(
        "evidence".into(),
        Value::String(format!(
            "{evidence} Closed by machine from the reading, not by judgement."
        )),
    );
    m.insert("cleared_by".into(), Value::String(cleared_by.into()));
    m.insert(RECOVERED_AT.into(), Value::String(at.into()));
    m
}

/// The packet-metadata key a recovery is stamped under — the same key
/// `estate.recover` has written onto the packets it closes since
/// ef421cd3, so a reader asks one question of every alarm.
pub const RECOVERED_AT: &str = "recovered_at";

/// The recovery's window and recorded ending (595e32f6). Neither is
/// inferred from prose: old plain recovery notes remain readable.
pub(crate) const HELD_DAYS: &str = "held_days";
pub(crate) const HOLD_ENDED: &str = "hold_ended";

/// The merge that tells a person the condition is over when the machine
/// may not close the packet itself ([`Retraction::Annotate`]).
pub fn recovery_note(
    evidence: &str,
    cleared_by: &str,
    recovered_at: &str,
    why_open: &str,
) -> serde_json::Map<String, Value> {
    let mut m = serde_json::Map::new();
    m.insert(HELD_DAYS.into(), Value::Null);
    m.insert(HOLD_ENDED.into(), Value::Null);
    m.insert(RECOVERED_AT.into(), Value::String(recovered_at.into()));
    m.insert("recovered_by".into(), Value::String(cleared_by.into()));
    m.insert(
        "recovery".into(),
        Value::String(format!(
            "RECOVERED — {evidence} Still open because {why_open}. The condition this alarm \
             was raised for no longer holds; close it as `stale` unless the route it took is \
             still wanted without it (backlog a2d8bad3)."
        )),
    );
    m
}

/// The merge that withdraws a [`recovery_note`] when the condition comes
/// back: `null` deletes a key on `PATCH /api/jobs/{id}/metadata`, and a
/// packet whose condition returned must not go on saying RECOVERED.
pub(crate) fn relapse_patch() -> Value {
    serde_json::json!({RECOVERED_AT: null, "recovered_by": null, "recovery": null,
        HELD_DAYS: null, HOLD_ENDED: null})
}

/// A positively marked window, not a legacy recovery note (595e32f6).
pub(crate) fn held_recovery(alarm: &Value) -> bool {
    told_recovered(alarm)
        && alarm
            .pointer(&format!("/metadata/{HELD_DAYS}"))
            .and_then(Value::as_i64)
            .is_some_and(|days| days > 0)
}

/// End only the hold. Preserve its original recovery stamps and every
/// step, holder and priority; the metadata merge records this decision.
pub(crate) fn hold_end_note(evidence: &str, why_open: &str) -> serde_json::Map<String, Value> {
    serde_json::Map::from_iter([
        (HELD_DAYS.into(), Value::Null),
        (HOLD_ENDED.into(), Value::String(why_open.into())),
        (
            "recovery".into(),
            Value::String(format!(
                "RECOVERED — {evidence} HOLD ENDED because {why_open}. The item stays on its \
             chosen route; the machine does not complete its step (595e32f6)."
            )),
        ),
    ])
}

/// PURE: has a machine told this alarm its condition cleared — does it
/// carry a [`RECOVERED_AT`] the relapse patch has not withdrawn?
pub(crate) fn told_recovered(alarm: &Value) -> bool {
    alarm
        .pointer(&format!("/metadata/{RECOVERED_AT}"))
        .is_some_and(|v| !v.is_null())
}

/// THE HOLD (backlog f4b0ad41; decided by David 2026-11-17 on design
/// 25511015 Q4, superseding a seven-day REOPEN, which a closed packet
/// cannot take — backlog 36352452). A machine that reads an alarm it
/// raised as clear does not withdraw it at once. It keeps the item OPEN
/// with the RECOVERED note ([`hold_note`]) for a declared window, so a
/// relapse inside the window is appended to THIS item through
/// `alarm_item` — the occurrence counted, the note withdrawn — instead
/// of filed as a new item beside a closed twin. Only a window that
/// passes with no relapse withdraws the item, at the step
/// [`retraction`] names. Measured by run 3f4ed839 over the record on
/// 2026-11-17: closing on clear had filed 62 silent-session and 55
/// estate items that the hold would have appended instead.
///
/// The window is the rule's to declare: its `hold_days` arg, in whole
/// days, [`HOLD_DAYS_DEFAULT`] when the rule names none.
pub(crate) const HOLD_DAYS: &str = "hold_days";

/// The window a rule that declares none holds a recovered alarm open
/// for — the seven days of David's decision.
pub(crate) const HOLD_DAYS_DEFAULT: i64 = 7;

/// Parse the rule's `hold_days`. Absent is the default; anything but a
/// positive whole number of days is refused by name — a window nobody
/// can read is a hold that never ends, or never starts.
pub(crate) fn hold_window(
    args: &[(String, boss_dispatcher::rules::expr::Value)],
) -> Result<chrono::Duration, String> {
    use boss_dispatcher::rules::expr::Value as Arg;
    let days = match boss_dispatcher::rules::handler::arg(args, HOLD_DAYS) {
        None => HOLD_DAYS_DEFAULT,
        Some(Arg::Int(n)) if *n > 0 => *n,
        Some(other) => {
            return Err(format!(
                "`{HOLD_DAYS}` must be a positive whole number of days, got a {} ({other:?})",
                other.kind()
            ));
        }
    };
    chrono::Duration::try_days(days)
        .ok_or_else(|| format!("`{HOLD_DAYS}` {days} reaches outside the calendar"))
}

/// What a machine that read its alarm clear writes this pass ([`hold`]).
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum Hold {
    /// Newly clear, and the machine may withdraw it: write the
    /// [`hold_note`]. The item stays open for the window.
    Note,
    /// Newly clear, and no step the machine may complete (a2d8bad3):
    /// write the [`recovery_note`] with this reason. There is nothing to
    /// hold — a person closes it.
    Annotate { why_open: String },
    /// An executor or design route has taken this marked hold. Record
    /// its end once; even a later READY step is no quiet-window exit.
    End { why_open: String },
    /// Already told: the window is still running, or the machine may not
    /// withdraw it. Nothing is written — a pass that fires every few
    /// minutes tells a packet once.
    Holding,
    /// Told at `recovered_at`, and the window passed with no relapse
    /// (a relapse withdraws the note, so a told item has had none):
    /// complete this step `stale`.
    Withdraw {
        slug: String,
        step_id: String,
        recovered_at: String,
    },
}

/// PURE: the hold, for an alarm its machine reads clear on this pass.
/// `retraction` is the alarm's own ([`retraction`]); `now` is the
/// firing's instant, and with none — or a `recovered_at` that does not
/// parse — a told alarm holds: an elapsed window is never assumed.
pub(crate) fn hold(
    alarm: &Value,
    retraction: Retraction,
    now: Option<chrono::DateTime<chrono::Utc>>,
    window: chrono::Duration,
) -> Hold {
    if !told_recovered(alarm) {
        return match retraction {
            Retraction::Complete { .. } => Hold::Note,
            Retraction::Annotate { why_open } => Hold::Annotate { why_open },
        };
    }
    if alarm
        .pointer(&format!("/metadata/{HOLD_ENDED}"))
        .and_then(Value::as_str)
        .is_some_and(|why| !why.trim().is_empty())
    {
        return Hold::Holding;
    }
    // The older retraction judgement permits an open triage, even when
    // active. A marked hold must yield to its executor before that
    // judgement can age it out (595e32f6).
    if held_recovery(alarm)
        && let Some(step) = alarm
            .get("steps")
            .and_then(Value::as_array)
            .and_then(|steps| {
                steps
                    .iter()
                    .find(|step| step.get("status").and_then(Value::as_str) == Some("active"))
            })
    {
        let slug = step
            .get("spec_slug")
            .and_then(Value::as_str)
            .unwrap_or("a step");
        return Hold::End {
            why_open: format!("`{slug}` is active — an executor holds it"),
        };
    }
    if held_recovery(alarm)
        && let Retraction::Annotate { why_open } = &retraction
    {
        return Hold::End {
            why_open: why_open.clone(),
        };
    }
    let told = alarm
        .pointer(&format!("/metadata/{RECOVERED_AT}"))
        .and_then(Value::as_str)
        .unwrap_or_default();
    let at = chrono::DateTime::parse_from_rfc3339(told)
        .ok()
        .map(|t| t.with_timezone(&chrono::Utc));
    match (retraction, at, now) {
        (Retraction::Complete { slug, step_id }, Some(at), Some(now)) if now - at >= window => {
            Hold::Withdraw {
                slug,
                step_id,
                recovered_at: told.to_string(),
            }
        }
        _ => Hold::Holding,
    }
}

/// The RECOVERED note a held alarm carries — the same keys as
/// [`recovery_note`], so [`relapse_patch`] withdraws it and `alarm_item`
/// reads it as told, with the hold said in words.
pub(crate) fn hold_note(
    evidence: &str,
    cleared_by: &str,
    recovered_at: &str,
    window: chrono::Duration,
) -> serde_json::Map<String, Value> {
    let mut m = recovery_note(
        evidence,
        cleared_by,
        recovered_at,
        "the recovery window is running",
    );
    m.insert(HELD_DAYS.into(), Value::from(window.num_days()));
    m.insert(
        "recovery".into(),
        Value::String(format!(
            "RECOVERED — {evidence} HELD OPEN for {days} day(s) from `recovered_at` (backlog \
             f4b0ad41): a relapse inside that window is appended to this item and withdraws this \
             note, and a window that passes with no relapse is withdrawn by the machine as \
             `stale`. Nothing to do: a hand close before then files the next relapse as a new \
             item, which the hold exists to prevent. While held it waits at `scheduled`, out of \
             the pull, and a relapse or work taking it restores its priority (595e32f6).",
            days = window.num_days(),
        )),
    );
    m
}

/// What a withdrawal at the end of a quiet window says: who held it,
/// from when, for how long, and the note it held.
pub(crate) fn held_evidence(
    alarm: &Value,
    cleared_by: &str,
    window: chrono::Duration,
    now: chrono::DateTime<chrono::Utc>,
) -> String {
    let meta = |k: &str| {
        alarm
            .pointer(&format!("/metadata/{k}"))
            .and_then(Value::as_str)
            .unwrap_or("(unrecorded)")
            .to_string()
    };
    format!(
        "{cleared_by} held this item open {days} day(s) from its recovery at {at} (backlog \
         f4b0ad41), and read it again at {now} with no relapse appended inside the window. The \
         note it held: {note}",
        days = window.num_days(),
        at = meta(RECOVERED_AT),
        now = now.to_rfc3339(),
        note = meta("recovery"),
    )
}

/// A HELD ITEM LEAVES THE PULL (595e32f6 part 3; David's decision on
/// f4b0ad41, 2026-11-17). An alarm held open RECOVERED still sat in
/// every pull at the priority it was raised with — urgent, mostly — so
/// an agent took it, read the note, and hand-closed it inside its
/// window: 221 idle-actor items went that way, and the hand close is
/// exactly what the hold exists to prevent (the next relapse files a
/// new item beside a closed twin). So while an item is held its
/// priority drops to `scheduled`, through the priority door
/// (`PUT /api/jobs/{id}/priority`, eb26059d), which records the move in
/// the packet's own `priority_changes`; when the hold stops — a relapse
/// withdraws the note, or work takes the item (`Hold::End`) — the
/// priority the hold lowered from comes back.
///
/// The lowering's `why` starts with this marker and names the
/// recovery it was for, so the record says which move was the hold's
/// and restoration never needs a clock: a lowering is made once per
/// recovery (an item an operator raises again inside the window stays
/// raised), and only the hold's OWN lowering, while it is still the
/// newest move, is ever undone (a station-row exclusion could not reach
/// the tree pull, which is why this is the priority and not a filter).
pub(crate) const HOLD_LOWERING: &str = "recovery hold (595e32f6)";

/// The `why` prefix that names the recovery a lowering was for.
fn hold_lowering_for(recovered_at: &str) -> String {
    format!("{HOLD_LOWERING}: held RECOVERED from {recovered_at}")
}

/// PURE: the priority move, if any, that makes `alarm` — as it stands
/// after this pass's note — agree with its hold: the body for
/// `PUT /api/jobs/{id}/priority`, each move guarded on the row as read
/// (`expected`), so a move anyone made meanwhile turns this one into a
/// 409 that writes nothing. Idempotent: a pass over an item that
/// already agrees moves nothing, and a pass that failed is repaired by
/// the next one.
pub(crate) fn hold_priority_move(alarm: &Value) -> Option<Value> {
    use boss_jobs::priority::PRIORITY_CHANGES_KEY;
    let priority = alarm.get("priority").and_then(Value::as_str)?;
    let stored = alarm
        .pointer(&format!("/metadata/{PRIORITY_CHANGES_KEY}"))
        .cloned()
        .unwrap_or(Value::Null);
    let changes: &[Value] = match &stored {
        Value::Null => &[],
        Value::Array(list) => list,
        // Not the door's record: nothing is judged from it.
        _ => return None,
    };
    let why_of = |entry: &Value| {
        entry
            .get("why")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string()
    };
    let ended = alarm
        .pointer(&format!("/metadata/{HOLD_ENDED}"))
        .and_then(Value::as_str)
        .is_some_and(|why| !why.trim().is_empty());
    if held_recovery(alarm) && !ended {
        let recovered_at = alarm
            .pointer(&format!("/metadata/{RECOVERED_AT}"))
            .and_then(Value::as_str)?;
        let marker = hold_lowering_for(recovered_at);
        if priority == "scheduled" || changes.iter().any(|e| why_of(e).starts_with(&marker)) {
            return None;
        }
        return Some(serde_json::json!({
            "priority": "scheduled",
            "why": format!(
                "{marker}: out of the pull while its window runs, so nobody hand-closes it; \
                 a relapse or work taking it restores {priority}."
            ),
            "expected": {
                "priority": priority,
                "metadata": {
                    RECOVERED_AT: recovered_at,
                    HELD_DAYS: alarm.pointer(&format!("/metadata/{HELD_DAYS}")).cloned(),
                    HOLD_ENDED: null,
                    PRIORITY_CHANGES_KEY: stored,
                },
            },
        }));
    }
    // Not held: undo only the hold's own lowering, and only while it is
    // still the newest move and the item still reads as it left it.
    let last = changes.last()?;
    let from = last.get("from").and_then(Value::as_str)?;
    if priority != "scheduled"
        || last.get("to").and_then(Value::as_str) != Some("scheduled")
        || !why_of(last).starts_with(HOLD_LOWERING)
        || !matches!(from, "emergency" | "urgent" | "standard")
    {
        return None;
    }
    let because = if ended {
        "work took the item, which ended the hold"
    } else {
        "the hold stopped — the condition came back, or its note was withdrawn"
    };
    Some(serde_json::json!({
        "priority": from,
        "why": format!("{HOLD_LOWERING}: restored to {from}: {because}."),
        "expected": {
            "priority": "scheduled",
            "metadata": {PRIORITY_CHANGES_KEY: stored},
        },
    }))
}

/// PURE: `alarm` as it reads once `note` has merged into its metadata —
/// what [`hold_priority_move`] judges after a note is written. A `null`
/// in the note deletes the key, as the merge door does.
pub(crate) fn with_note(alarm: &Value, note: &Value) -> Value {
    let mut merged = alarm.clone();
    if let (Some(metadata), Some(note)) = (
        merged
            .as_object_mut()
            .map(|m| {
                m.entry("metadata")
                    .or_insert_with(|| Value::Object(Default::default()))
            })
            .and_then(Value::as_object_mut),
        note.as_object(),
    ) {
        for (k, v) in note {
            if v.is_null() {
                metadata.remove(k);
            } else {
                metadata.insert(k.clone(), v.clone());
            }
        }
    }
    merged
}

/// Make `alarm`'s priority agree with its hold ([`hold_priority_move`]).
/// A 409 — the row moved since it was read, or the packet finished — is
/// an answer, not a failure: nothing was written, and the next pass
/// judges the row as it then stands.
pub(crate) async fn hold_priority(
    client: &boss_core::machine_token::Client,
    jobs_base: &str,
    alarm: &Value,
    rule_name: &str,
) -> Result<(), HandlerError> {
    let Some(body) = hold_priority_move(alarm) else {
        return Ok(());
    };
    let Some(id) = alarm
        .get("id")
        .and_then(Value::as_str)
        .filter(|id| !id.is_empty())
    else {
        return Ok(());
    };
    let url = format!("{}/api/jobs/{id}/priority", jobs_base.trim_end_matches('/'));
    let resp = client
        .request(reqwest::Method::PUT, &url)
        .header("content-type", "application/json")
        .header("x-boss-user", dispatcher_actor_header(rule_name))
        .header("x-sim-origin", sim_origin_value())
        .json(&body)
        .send()
        .await
        .map_err(|e| HandlerError::Downstream(format!("PUT {url}: {e}")))?;
    let status = resp.status();
    if status == reqwest::StatusCode::CONFLICT {
        let text = resp.text().await.unwrap_or_default();
        tracing::info!(rule = rule_name, packet = id, %text, "the recovery hold's priority move found the row moved; nothing written");
        return Ok(());
    }
    if !status.is_success() {
        let text = resp.text().await.unwrap_or_default();
        return Err(HandlerError::Downstream(format!(
            "PUT {url} returned {status}: {text}"
        )));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// The owner's message is a `direct` one, addressed to the owner,
    /// pointing at the alarm, under an id fixed by (scope, alarm,
    /// recipient) so a re-send collapses (backlog 35dabc3d).
    #[test]
    fn an_alarm_reaches_its_owner_as_a_direct_message_with_a_collapsing_id() {
        let alarm = json!({"title": "SILENT SESSION: agent-claude",
            "metadata": {"detail": "agent-claude has written nothing for 53m."}});
        let m = owner_message(
            "silent-factory-session",
            "a79f9097",
            "a79f9097",
            &alarm,
            "emp-owner",
        );
        assert_eq!(m["id"], "silent-factory-session:a79f9097:emp-owner");
        assert_eq!(m["kind"], "direct");
        assert_eq!(m["recipient_id"], "emp-owner");
        assert_eq!(m["sender_id"], "automation:dispatcher");
        assert_eq!(m["subject"], "SILENT SESSION: agent-claude");
        assert!(
            m["body"]
                .as_str()
                .unwrap()
                .starts_with("agent-claude has written nothing for 53m."),
            "{m}"
        );
        assert_eq!(m["entity_ref"]["entity_id"], "a79f9097");
        assert_eq!(m["entity_ref"]["entity_path"], "/jobs/a79f9097");
        assert_eq!(
            owner_message(
                "silent-factory-session",
                "a79f9097",
                "a79f9097",
                &json!({}),
                "emp-owner"
            )["id"],
            m["id"],
            "the id is the episode and the recipient, never the text"
        );
    }

    /// The handlers that speak to a party OUTSIDE the estate, and whom.
    /// Each holds a `reqwest::Client` of its own and must never reach
    /// the machine client: `webhook.notify` sent on `api_client()` until
    /// backlog ee96c839 (2026-09-28), so the configured host received
    /// the estate token on every forwarded event.
    const THIRD_PARTY: &[(&str, &str, &str)] = &[
        (
            "webhook_notify.rs",
            include_str!("webhook_notify.rs"),
            "the configured webhook host",
        ),
        (
            "stripe_charges.rs",
            include_str!("stripe_charges.rs"),
            "Stripe",
        ),
        (
            "stripe_payouts.rs",
            include_str!("stripe_payouts.rs"),
            "Stripe",
        ),
        (
            "credential_issuer.rs",
            include_str!("credential_issuer.rs"),
            "the forge, Cloudflare and GitHub",
        ),
    ];

    /// Design 6805c764 car 2, the handlers slice: `api_client()` is now
    /// a `machine_token::Client`, and a third party's client stays a
    /// plain `reqwest::Client`, so passing one for the other is a
    /// compile error. What the type cannot stop is a third-party
    /// handler BUILDING its client from `api_client()` — the ee96c839
    /// shape — so each is read here, comments stripped (a doc comment
    /// saying why it does not is not a use).
    #[test]
    fn a_third_party_handler_never_holds_the_machine_client() {
        let doors = ["api_client(", "machine_token::"];
        let leaks: Vec<String> = THIRD_PARTY
            .iter()
            .filter_map(|(file, src, whom)| {
                let prod = boss_testing::production_source::production_text(src)
                    .unwrap_or_else(|e| panic!("{file} does not parse: {e}"));
                let code = boss_testing::production_source::without_comments(&prod);
                let hit: Vec<&str> = doors.iter().copied().filter(|d| code.contains(d)).collect();
                (!hit.is_empty()).then(|| format!("{file} ({whom}) names {hit:?}"))
            })
            .collect();
        assert!(
            leaks.is_empty(),
            "a handler that sends to a party outside the estate reaches the machine client, \
             so that party would receive the estate token:\n  {}",
            leaks.join("\n  ")
        );
        // The control: the scan sees the door in a file that uses it.
        let own = boss_testing::production_source::without_comments(
            &boss_testing::production_source::production_text(include_str!("common.rs")).unwrap(),
        );
        assert!(own.contains("machine_token::"), "the scan reads nothing");
    }

    /// Backlog 833e2d0a — THE JUDGEMENT LIVES ONCE. A listing with no
    /// `data` array is no answer: an error shape, a changed contract, a
    /// policy-narrowed read, a far side that restarted. Reading it as
    /// zero rows makes a handler do nothing and report the pass
    /// healthy. Three handlers were cured of exactly this one at a
    /// time (80a77466 departments, 6c4c432a sensors, 0767c830
    /// readings) and each was found by accident. An EMPTY array is
    /// honest — a registry may legitimately hold nothing — so no floor
    /// is put on the count.
    #[test]
    fn a_listing_with_no_data_array_refuses_and_an_empty_one_is_honest() {
        let rows: Vec<Value> =
            rows_or_refuse(&json!({ "data": [], "total": 0 }), "GET /api/things")
                .expect("an empty array is honest");
        assert!(rows.is_empty());

        // `null` and an object refuse too: `as_array` alone reads both
        // as absent, and an absence that defaults is the whole disease.
        for dark in [
            json!({ "total": 0 }),
            json!({ "error": "forbidden" }),
            json!({ "data": null }),
            json!({ "data": {} }),
        ] {
            let why = rows_or_refuse::<Value>(&dark, "GET /api/things")
                .expect_err("no `data` array is a refusal");
            assert!(why.contains("no `data` array"), "{why}");
            assert!(why.contains("GET /api/things"), "{why}");
        }
    }

    /// A row out of the caller's shape is a refusal carrying the serde
    /// error and the reading's name — never a shorter list.
    #[test]
    fn a_row_out_of_shape_refuses_and_names_the_reading() {
        let ok: Vec<u64> =
            rows_or_refuse(&json!({ "data": [1, 2] }), "GET /api/things").expect("rows");
        assert_eq!(ok, vec![1, 2]);
        let why = rows_or_refuse::<u64>(&json!({ "data": ["x"] }), "GET /api/things")
            .expect_err("a row out of shape refuses");
        assert!(why.contains("GET /api/things"), "{why}");
        assert!(why.contains("not in shape"), "{why}");
    }

    /// Backlog f2eac973 — the single-row neighbour of 833e2d0a. Every
    /// single-row door these handlers read (`GET /api/jobs/{id}`, a
    /// flattened `JobDetail`; `GET /api/credentials/{id}`, a bare
    /// `CredentialRow`) answers the row BARE, so the old
    /// `.get("data").cloned().unwrap_or(job)` hedged for an envelope no
    /// server sends and its fallback was the only live path: any 200
    /// body was read as the row, and a kind check then skipped it
    /// silently. A row is what carries its `id`; everything else —
    /// an error body, an envelope, null, a list — refuses and says
    /// what it did carry.
    #[test]
    fn a_single_row_read_with_no_id_refuses_and_a_bare_row_is_the_answer() {
        let job = json!({ "id": "j-1", "kind": "maintenance-sweep", "steps": [] });
        assert_eq!(
            row_or_refuse(job.clone(), "GET /api/jobs/j-1").expect("a bare row is the answer"),
            job,
            "the row comes back whole"
        );
        for dark in [
            json!({ "error": "forbidden" }),
            json!({ "data": { "id": "j-1" } }),
            json!({ "id": "" }),
            json!({ "id": 7 }),
            json!(null),
            json!([{ "id": "j-1" }]),
        ] {
            let why = row_or_refuse(dark.clone(), "GET /api/jobs/j-1")
                .expect_err("a body with no row id is a refusal");
            assert!(why.contains("GET /api/jobs/j-1"), "{why}");
            assert!(why.contains("no row"), "{why}");
        }
        // The refusal names what the body DID carry, so the next
        // reader does not re-derive it (CLAUDE.md §Diagnosis).
        let why = row_or_refuse(json!({ "error": "forbidden" }), "GET /api/jobs/j-1")
            .expect_err("refuses");
        assert!(why.contains("error"), "{why}");
    }

    /// The judgement AT THE CONSUMING LAYER. `open_jobs` is the board
    /// walk almost every packet-reading rule goes through, and it held
    /// the same chain: a page with no `data` array left `got == 0`,
    /// which broke the paging loop and returned an EMPTY BOARD. Every
    /// caller then found nothing to do and said so calmly — the
    /// widest-blast-radius instance of 833e2d0a, and the fourth.
    async fn board_from(page: Value) -> Result<Vec<Value>, HandlerError> {
        use axum::{Json as AxJson, Router, routing::get};
        let app = Router::new().route(
            "/api/jobs",
            get(move || {
                let page = page.clone();
                async move { AxJson(page) }
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        open_jobs(
            &api_client(),
            &format!("http://{addr}"),
            Some("backlog-item"),
            "a-rule",
        )
        .await
    }

    #[tokio::test]
    async fn a_board_page_with_no_data_array_refuses_rather_than_reading_empty() {
        let board = board_from(json!({ "data": [], "total": 0 }))
            .await
            .expect("an empty board is honest");
        assert!(board.is_empty());

        let why = board_from(json!({ "total": 3 }))
            .await
            .expect_err("a page with no `data` array is a refusal");
        assert!(matches!(why, HandlerError::Downstream(_)), "{why:?}");
        assert!(format!("{why:?}").contains("no `data` array"), "{why:?}");
    }

    /// THE FLOOR, stated once: a roster read that answers zero rows is
    /// refused, and the refusal names the handler, the roster and why
    /// zero means broken — the three things an operator reading a
    /// dead-letter has to be told.
    #[test]
    fn an_empty_roster_is_a_permanent_refusal_that_names_the_roster() {
        let e = empty_roster_refusal(
            "retro.open",
            "departments",
            "a company with no departments is broken in a way a retro cannot fix",
        );
        assert!(
            e.is_permanent(),
            "a roster that is empty is empty identically on every redelivery"
        );
        let text = e.to_string();
        assert!(text.contains("retro.open"), "{text}");
        assert!(text.contains("departments roster is empty"), "{text}");
        assert!(
            text.contains("broken in a way a retro cannot fix"),
            "the caller's reason rides the refusal: {text}"
        );
        assert!(
            text.contains("a check that is not running"),
            "the floor's own sentence, stated here and not per handler: {text}"
        );
    }

    /// A step as the jobs API lists it back, for the retraction tests.
    fn step(slug: &str, status: &str, metadata: Value) -> Value {
        json!({"id": format!("s-{slug}"), "spec_slug": slug, "status": status, "metadata": metadata})
    }

    fn alarm(steps: Vec<Value>) -> Value {
        json!({"id": "alarm-1", "status": "open", "steps": steps})
    }

    /// Nobody has routed it: the machine withdraws it where it has
    /// always withdrawn it, at `triage`. A fixture with no `status` at
    /// all is read as open — the shape every pre-existing test uses.
    #[test]
    fn an_untriaged_alarm_is_withdrawn_at_its_triage_step() {
        for triage in [
            step(
                "triage",
                "ready",
                json!({"authority_role": "platform-admin"}),
            ),
            step(
                "triage",
                "active",
                json!({"authority_role": "platform-admin"}),
            ),
            json!({"id": "s-triage", "spec_slug": "triage", "metadata": {"authority_role": "platform-admin"}}),
        ] {
            let job = alarm(vec![step("filed", "completed", json!({})), triage]);
            match retraction(&job) {
                Some(Retraction::Complete { slug, step_id }) => {
                    assert_eq!(slug, TRIAGE_SLUG);
                    assert_eq!(step_id, "s-triage");
                }
                other => panic!("an untriaged alarm completes triage, got {other:?}"),
            }
        }
    }

    /// Backlog a2d8bad3 — THE WORKED EXAMPLE, a6a4ae18. The alarm was
    /// triaged to `build` on 2026-09-20 and its cadence came back on
    /// 2026-09-21; the sweep's only exit was the triage step, already
    /// complete, so the jobs API refused the PUT and the alarm sat open
    /// with `build` ready until a person closed it on 2026-09-23. The
    /// step the packet is WAITING ON is `build`, and `build` carries a
    /// `stale` disposition for exactly this — the claim no longer holds.
    #[test]
    fn an_alarm_routed_to_build_is_withdrawn_at_its_ready_build_step() {
        let job = alarm(vec![
            step("filed", "completed", json!({})),
            step(
                "triage",
                "completed",
                json!({"disposition": "build", "evidence": "routed by a human"}),
            ),
            step("measure", "skipped", json!({})),
            step("draft-design", "skipped", json!({})),
            step("design-review", "skipped", json!({})),
            step(
                "build",
                "ready",
                json!({"authority_role": "platform-admin", "agent_profile": "builder"}),
            ),
        ]);
        match retraction(&job) {
            Some(Retraction::Complete { slug, step_id }) => {
                assert_eq!(slug, "build");
                assert_eq!(step_id, "s-build");
            }
            other => panic!("a ready build is where the alarm withdraws, got {other:?}"),
        }
    }

    /// The verify route withdraws at `measure`, which carries the same
    /// `stale` disposition (2802ba8c).
    #[test]
    fn an_alarm_routed_to_verify_is_withdrawn_at_its_ready_measure_step() {
        let job = alarm(vec![
            step("triage", "completed", json!({"disposition": "verify"})),
            step("measure", "ready", json!({})),
            step("build", "skipped", json!({})),
        ]);
        assert!(matches!(
            retraction(&job),
            Some(Retraction::Complete { slug, .. }) if slug == "measure"
        ));
    }

    /// An executor HOLDS an active build: completing it from under a
    /// dispatched run fires `agent-run-delivers-when-its-step-is-done`
    /// and lands the run as `delivered` for work it did not deliver. So
    /// the machine does not complete it — it says on the packet that
    /// the condition cleared, and the executor's verify-the-claim reads
    /// that.
    #[test]
    fn a_build_an_executor_holds_is_annotated_not_completed() {
        let job = alarm(vec![
            step("triage", "completed", json!({"disposition": "build"})),
            step("build", "active", json!({})),
        ]);
        match retraction(&job) {
            Some(Retraction::Annotate { why_open }) => {
                assert!(why_open.contains("`build`"), "{why_open}");
                assert!(why_open.contains("active"), "{why_open}");
            }
            other => panic!("a held build is annotated, got {other:?}"),
        }
    }

    /// The design route has no withdrawal on it: `draft-design` needs a
    /// design id and `design-review` is a decision a person owns. The
    /// machine completes neither, and tells them instead.
    #[test]
    fn a_design_route_is_annotated_because_no_step_on_it_can_withdraw() {
        let job = alarm(vec![
            step("triage", "completed", json!({"disposition": "design"})),
            step("draft-design", "completed", json!({"design_id": "d-1"})),
            step("design-review", "ready", json!({})),
            step("build", "pending", json!({})),
        ]);
        match retraction(&job) {
            Some(Retraction::Annotate { why_open }) => {
                assert!(why_open.contains("design"), "{why_open}");
            }
            other => panic!("a design route is annotated, got {other:?}"),
        }
    }

    /// No triage step is not an alarm this crate can judge.
    #[test]
    fn a_packet_with_no_triage_step_has_no_retraction() {
        assert_eq!(
            retraction(&alarm(vec![step("filed", "completed", json!({}))])),
            None
        );
    }

    /// The note says it is over and why the packet is still open, and
    /// the relapse patch deletes exactly the keys the note wrote — a
    /// packet whose condition came back must not go on saying
    /// RECOVERED.
    #[test]
    fn the_recovery_note_and_its_relapse_patch_name_the_same_keys() {
        let note = recovery_note(
            "the cadence is arriving again",
            "cadence.silence.sweep",
            "2026-09-21T00:00:00+00:00",
            "triage routed it to `design`",
        );
        assert_eq!(note[RECOVERED_AT], "2026-09-21T00:00:00+00:00");
        assert_eq!(note["recovered_by"], "cadence.silence.sweep");
        let text = note["recovery"].as_str().expect("a sentence");
        assert!(text.contains("the cadence is arriving again"), "{text}");
        assert!(text.contains("triage routed it to `design`"), "{text}");
        let relapse = relapse_patch();
        let relapse = relapse.as_object().expect("an object");
        assert_eq!(
            relapse.keys().collect::<Vec<_>>(),
            note.keys().collect::<Vec<_>>()
        );
        assert!(
            relapse.values().all(Value::is_null),
            "null deletes on PATCH"
        );
        // The hold's note is the same note in other words: a relapse
        // withdraws it, and `alarm_item` reads it as told (f4b0ad41).
        let held = hold_note(
            "the actor wrote again",
            "jobs.silent_session_watch",
            "2026-11-17T21:00:00+00:00",
            chrono::Duration::days(7),
        );
        assert_eq!(
            relapse.keys().collect::<Vec<_>>(),
            held.keys().collect::<Vec<_>>()
        );
        let text = held["recovery"].as_str().expect("a sentence");
        assert!(
            text.starts_with("RECOVERED — the actor wrote again"),
            "{text}"
        );
        assert!(text.contains("HELD OPEN for 7 day(s)"), "{text}");
    }

    #[test]
    fn a_recovery_hold_marks_its_window_and_a_relapse_withdraws_the_marker() {
        let note = hold_note(
            "clear",
            "machine",
            "2026-11-17T21:00:00Z",
            chrono::Duration::days(7),
        );
        assert_eq!(note.get("held_days"), Some(&json!(7)));
        assert_eq!(relapse_patch().get("held_days"), Some(&Value::Null));
    }

    #[test]
    fn a_recovery_hold_ends_when_work_has_taken_the_item() {
        let mut a = alarm(vec![step("triage", "active", json!({}))]);
        a["metadata"] = json!({RECOVERED_AT:"2026-11-17T21:00:00Z", "held_days":7});
        let decide = |a: &Value| {
            hold(
                a,
                Retraction::Annotate {
                    why_open: "`build` is active".into(),
                },
                Some(instant("2026-11-18T00:00:00Z")),
                chrono::Duration::days(7),
            )
        };
        assert!(
            format!("{:?}", decide(&a)).starts_with("End"),
            "{:?}",
            decide(&a)
        );
        a["metadata"]["held_days"] = Value::Null;
        assert_eq!(
            decide(&a),
            Hold::Holding,
            "a plain recovery note is not a hold"
        );
    }

    #[test]
    fn a_recovery_hold_that_ended_never_withdraws_work_returned_to_ready() {
        let mut a = alarm(vec![step("triage", "ready", json!({}))]);
        a["metadata"] =
            json!({RECOVERED_AT:"2026-11-17T21:00:00Z", "hold_ended":"an executor claimed build"});
        assert_eq!(
            hold(
                &a,
                Retraction::Complete {
                    slug: "triage".into(),
                    step_id: "s".into()
                },
                Some(instant("2026-12-01T00:00:00Z")),
                chrono::Duration::days(7)
            ),
            Hold::Holding,
            "ending the hold is permanent for this recovery, even if work later returns ready"
        );
    }

    /// A held alarm as the idle watch reads it: urgent, told RECOVERED
    /// with a seven-day window, nobody's priority move on it yet.
    fn held_urgent() -> Value {
        json!({"id": "alarm-1", "status": "open", "priority": "urgent",
               "metadata": {RECOVERED_AT: "2026-11-17T21:00:00+00:00", "held_days": 7}})
    }

    /// The row after the hold's lowering landed: what the priority door
    /// records for the body [`hold_priority_move`] sent.
    fn lowered(mut alarm: Value) -> Value {
        let body = hold_priority_move(&alarm).expect("a held urgent alarm is lowered");
        let entry = json!({"from": alarm["priority"], "to": "scheduled",
                           "by": "automation:dispatcher", "why": body["why"],
                           "at": "2026-11-17T21:00:01+00:00"});
        alarm["priority"] = json!("scheduled");
        alarm["metadata"]["priority_changes"] = json!([entry]);
        alarm
    }

    /// 595e32f6 part 3: a held item leaves the pull. Its priority drops
    /// to `scheduled` through the door, guarded on every key the
    /// judgement read, and once per recovery — never again after the
    /// lowering landed, nor for an item already scheduled.
    #[test]
    fn a_held_alarm_is_lowered_to_scheduled_once_guarded_on_what_it_read() {
        let body = hold_priority_move(&held_urgent()).expect("lowered");
        assert_eq!(body["priority"], "scheduled");
        assert!(
            body["why"]
                .as_str()
                .unwrap()
                .starts_with("recovery hold (595e32f6): held RECOVERED from 2026-11-17T21:00:00"),
            "{body}"
        );
        assert_eq!(
            body["expected"],
            json!({"priority": "urgent", "metadata": {
                RECOVERED_AT: "2026-11-17T21:00:00+00:00", "held_days": 7,
                "hold_ended": null, "priority_changes": null}})
        );
        assert_eq!(hold_priority_move(&lowered(held_urgent())), None);

        let mut scheduled = held_urgent();
        scheduled["priority"] = json!("scheduled");
        assert_eq!(hold_priority_move(&scheduled), None);

        // An operator raised it again inside the window: that stands.
        let mut raised = lowered(held_urgent());
        raised["priority"] = json!("urgent");
        let mut changes = raised["metadata"]["priority_changes"]
            .as_array()
            .unwrap()
            .clone();
        changes.push(
            json!({"from": "scheduled", "to": "urgent", "by": "emp-owner",
                            "why": "I want it seen", "at": "2026-11-18T09:00:00+00:00"}),
        );
        raised["metadata"]["priority_changes"] = json!(changes);
        assert_eq!(hold_priority_move(&raised), None);

        // A plain recovery note (no window) is not a hold.
        let mut told = held_urgent();
        told["metadata"]["held_days"] = Value::Null;
        assert_eq!(hold_priority_move(&told), None);
    }

    /// A relapse withdraws the note and the priority the hold lowered
    /// from comes back, guarded on the record it read; so does work
    /// taking the item (`Hold::End`). Only the hold's own lowering, only
    /// while it is the newest move.
    #[test]
    fn the_hold_restores_only_its_own_lowering_when_it_stops() {
        let relapsed = with_note(&lowered(held_urgent()), &relapse_patch());
        let body = hold_priority_move(&relapsed).expect("restored on relapse");
        assert_eq!(body["priority"], "urgent");
        assert_eq!(body["expected"]["priority"], "scheduled");
        assert_eq!(
            body["expected"]["metadata"]["priority_changes"],
            relapsed["metadata"]["priority_changes"]
        );

        let ended = with_note(
            &lowered(held_urgent()),
            &Value::Object(hold_end_note("x", "`build` is active")),
        );
        let body = hold_priority_move(&ended).expect("restored when work took it");
        assert_eq!(body["priority"], "urgent");
        assert!(
            body["why"].as_str().unwrap().contains("work took"),
            "{body}"
        );

        // Restored already: the newest move is no longer the hold's.
        let mut restored = relapsed.clone();
        restored["priority"] = json!("urgent");
        assert_eq!(hold_priority_move(&restored), None);

        // Someone scheduled it on purpose: not the hold's to undo.
        let mut theirs = relapsed.clone();
        theirs["metadata"]["priority_changes"] = json!([{"from": "urgent", "to": "scheduled",
            "by": "emp-owner", "why": "parked for next week", "at": "2026-11-18T00:00:00Z"}]);
        assert_eq!(hold_priority_move(&theirs), None);

        // A row that never carried the hold's move, or not the door's list.
        let mut plain = held_urgent();
        plain["metadata"] = json!({});
        assert_eq!(hold_priority_move(&plain), None);
        plain["metadata"]["priority_changes"] = json!("garbled");
        assert_eq!(hold_priority_move(&plain), None);
    }

    fn instant(s: &str) -> chrono::DateTime<chrono::Utc> {
        chrono::DateTime::parse_from_rfc3339(s)
            .unwrap()
            .with_timezone(&chrono::Utc)
    }

    /// THE HOLD, pure (backlog f4b0ad41). A clear the machine may
    /// withdraw is a NOTE, never a close; a told item inside its window
    /// is left alone; the first pass at or past the window withdraws it
    /// at the step `retraction` names, carrying the instant it was told.
    #[test]
    fn a_clear_is_held_open_for_its_window_and_a_quiet_window_withdraws() {
        let week = chrono::Duration::days(7);
        let triage = || Retraction::Complete {
            slug: TRIAGE_SLUG.into(),
            step_id: "s-triage".into(),
        };
        let fresh = alarm(vec![step("triage", "ready", json!({}))]);
        assert_eq!(
            hold(
                &fresh,
                triage(),
                Some(instant("2026-11-17T21:00:00Z")),
                week
            ),
            Hold::Note,
            "a clear keeps the item open and RECOVERED"
        );

        let mut told = fresh.clone();
        told["metadata"] = json!({RECOVERED_AT: "2026-11-10T21:00:00+00:00"});
        assert_eq!(
            hold(&told, triage(), Some(instant("2026-11-17T20:59:59Z")), week),
            Hold::Holding,
            "a second short of the window holds"
        );
        assert_eq!(
            hold(&told, triage(), Some(instant("2026-11-17T21:00:00Z")), week),
            Hold::Withdraw {
                slug: TRIAGE_SLUG.into(),
                step_id: "s-triage".into(),
                recovered_at: "2026-11-10T21:00:00+00:00".into(),
            },
            "a quiet window withdraws, stamped with the instant it was told"
        );
        assert_eq!(
            hold(&told, triage(), None, week),
            Hold::Holding,
            "with no instant to judge by, the window is never assumed to have passed"
        );
        let mut garbled = told.clone();
        garbled["metadata"][RECOVERED_AT] = json!("last tuesday");
        assert_eq!(
            hold(
                &garbled,
                triage(),
                Some(instant("2027-01-01T00:00:00Z")),
                week
            ),
            Hold::Holding,
            "a told instant that does not parse is no evidence the window passed"
        );

        // No step the machine may complete: the a2d8bad3 note, once.
        let annotate = || Retraction::Annotate {
            why_open: "`build` is active".into(),
        };
        assert_eq!(
            hold(
                &fresh,
                annotate(),
                Some(instant("2026-11-17T21:00:00Z")),
                week
            ),
            Hold::Annotate {
                why_open: "`build` is active".into()
            }
        );
        assert_eq!(
            hold(
                &told,
                annotate(),
                Some(instant("2027-01-01T00:00:00Z")),
                week
            ),
            Hold::Holding,
            "the machine never completes a step from under its executor, window or not"
        );
    }

    /// The window is the rule's: absent is seven days, and anything but a
    /// positive whole number of days is refused by name.
    #[test]
    fn the_hold_window_is_a_rule_arg_defaulting_to_seven_days() {
        use boss_dispatcher::rules::expr::Value as Arg;
        assert_eq!(hold_window(&[]), Ok(chrono::Duration::days(7)));
        assert_eq!(
            hold_window(&[(HOLD_DAYS.into(), Arg::Int(3))]),
            Ok(chrono::Duration::days(3))
        );
        for bad in [Arg::Int(0), Arg::Int(-1), Arg::String("7".into())] {
            let refused = hold_window(&[(HOLD_DAYS.into(), bad)]).unwrap_err();
            assert!(refused.contains(HOLD_DAYS), "{refused}");
        }
    }

    /// THE SHIPPED ROWS, through the REAL loader. A rule arg is an
    /// expression source, so `hold_days = "7"` reaches the handler as
    /// whatever `Registry::from_toml` compiles and `eval` makes of it —
    /// and `hold_window` refuses anything but an integer. Read by hand,
    /// the file says `"7"`; only the loader says whether that is Int(7)
    /// or the string a refusal would leave every firing on. Each rule
    /// file that drives a held class is loaded, evaluated as the runner
    /// evaluates it, and must declare exactly the seven days.
    #[test]
    fn the_shipped_hold_rules_load_to_a_seven_day_window() {
        use boss_dispatcher::rules::expr::{Context, NoHelpers, Value as Arg, eval};
        use boss_dispatcher::rules::registry::Registry;
        let root = boss_testing::repo_root();
        for (file, handler) in [
            (
                "infra/dispatcher/rules/a-silent-factory-holding-claimed-work-is-alarmed.toml",
                "jobs.silent_session_watch",
            ),
            (
                "infra/dispatcher/rules/estate-recover-on-comparison.toml",
                "estate.recover",
            ),
        ] {
            let src = std::fs::read_to_string(root.join(file)).unwrap();
            let registry = Registry::from_toml(&src).unwrap();
            let step = registry
                .rules()
                .iter()
                .flat_map(|r| r.do_steps.iter())
                .find(|d| d.handler == handler)
                .unwrap_or_else(|| panic!("{file} invokes {handler}"));
            let payload = json!({"_at": "2026-11-17T22:00:00Z"});
            let args: Vec<(String, Arg)> = step
                .args
                .iter()
                .map(|(key, expression)| {
                    let ctx = Context {
                        payload: &payload,
                        helpers: &NoHelpers,
                    };
                    (key.clone(), eval(expression, &ctx).unwrap())
                })
                .collect();
            let declared = args.iter().find(|(k, _)| k == HOLD_DAYS).map(|(_, v)| v);
            assert!(
                matches!(declared, Some(Arg::Int(7))),
                "{file}: `{HOLD_DAYS}` loads as {declared:?}"
            );
            assert_eq!(hold_window(&args), Ok(chrono::Duration::days(7)), "{file}");
        }
    }

    #[test]
    fn overhead_source_id_matches_endpoint_contract() {
        assert_eq!(
            overhead_source_id("step-1", "6100"),
            "overhead-absorbed@step-1:6100"
        );
    }
}

#[cfg(test)]
mod lane_pin {
    use super::*;

    /// EVERY MACHINE-FILED BACKLOG-ITEM IN THIS CRATE STAMPS ITS LANE
    /// (backlog b2d9b432).
    ///
    /// Stamping the eight sites that existed was the easy half; the half
    /// that lasts is that the NINTH cannot be written without one. From
    /// `FIELD_LIVE_AT` a packet with no lane reads as "the filer did not
    /// say", and a machine filer is the one filer whose origin is never
    /// in doubt — so a silent one does not merely lose a field, it
    /// poisons the count that is supposed to measure operators.
    ///
    /// This reads the crate's own source rather than exercising each
    /// handler, because the defect is a MISSING line: no amount of
    /// running the eight that are right can show that a ninth is wrong.
    /// The `mod tests` truncation is what keeps fixtures out — a test
    /// packet standing in for what the jobs API lists back is not a
    /// filing, and there are five of those.
    // crate pin — it counts this crate's own filing sites, so a car that
    // adds one learns it in `infra/gate.sh --lint` (backlog e2d091f7)
    #[test]
    fn every_backlog_item_this_crate_files_stamps_an_input_lane() {
        let dir = concat!(env!("CARGO_MANIFEST_DIR"), "/src/handlers");
        let mut filings = 0;
        let mut silent: Vec<String> = Vec::new();
        for entry in std::fs::read_dir(dir).expect("handlers dir") {
            let path = entry.expect("entry").path();
            if path.extension().and_then(|e| e.to_str()) != Some("rs") {
                continue;
            }
            let name = path
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or_default()
                .to_string();
            let src = std::fs::read_to_string(&path).expect("read handler");
            // Fixtures live under `mod tests`; a filing never does.
            let production = match src.find("\nmod tests {") {
                Some(i) => &src[..i],
                None => &src[..],
            };
            let lines: Vec<&str> = production.lines().collect();
            for (i, line) in lines.iter().enumerate() {
                if !line.contains(r#""kind": "backlog-item""#) {
                    continue;
                }
                filings += 1;
                let window = lines[i..lines.len().min(i + 30)].join("\n");
                if !window.contains("input_channel")
                    && !window.contains("with_lane")
                    && !window.contains("lane_label")
                {
                    silent.push(format!("{name}:{}", i + 1));
                }
            }
        }
        assert!(
            silent.is_empty(),
            "these machine filers file a backlog-item with NO input lane, so from \
             FIELD_LIVE_AT each reads as `the filer did not say` though its origin is \
             known exactly: {silent:?}. Stamp it — `super::common::with_lane(metadata, \
             InputChannel::<lane>)` when the metadata is a value, or \
             `\"input_channel\": super::common::lane_label(InputChannel::<lane>)` inside \
             an inline object."
        );
        // Nine since `ops.queue.alarm` (a45b38c1), which files
        // `ops_queue:<host>` stamped `Telemetry`; ten since
        // `jobs.agent_step_overdue` (078ddcb0), whose alarm per late
        // real-work step is stamped `Telemetry` too; eleven since
        // `jobs.flight_overdue` (73c31776), whose stale-flight alarm is
        // `Telemetry` for the same reason; twelve since the door alarm
        // (`estate.alarm`'s `door_dark`, e6406701), stamped `Telemetry`
        // like the estate alarms beside it; thirteen since `ops.judge`'s
        // WATCH alarm (backlog 8d77d670), a watched unattended verb that
        // did not do its job, stamped `PipelineFailure` like the chain's;
        // fourteen since `maintenance.chore.file_reds` files ONE item for
        // a night whose reds share a cause (backlog c8c6b9a8), stamped
        // `PipelineFailure` like its per-route item; fifteen since
        // `estate.alarm`'s BLIND alarm (a276f7c2), a hard class left
        // unmeasured three rows running, stamped `Telemetry` like the
        // estate alarms beside it; sixteen since `policy.coverage.alarm`
        // (design 1c4e42e1), a control no real person holds, stamped
        // `Telemetry` — a reading, like the estate alarms; seventeen since
        // its `coverage-unreadable` alarm (review M2), a dark read said
        // rather than swallowed, stamped `Telemetry` beside it; eighteen
        // since the boot seed's rule-drift alarm (backlog 732c3cf9), a
        // rule file edited without a version bump, stamped `Telemetry` —
        // a reading of the registry against the tree; nineteen since
        // `policy.check.refusals.alarm` (backlog b8e75382 R3), a lapsed
        // service grant read off the policy check's tally, stamped
        // `Telemetry` like the coverage backstop beside it; twenty since
        // the registry-declared failed chore without RED lines (e4a9a9b3)
        // reaches its owner, stamped `PipelineFailure` by the fallback;
        // twenty-one since `machine.gate.asked.alarm` (backlog d653e090),
        // an ask from a caller nobody expects read off the machine gate's
        // durable window, stamped `Telemetry` like the two beside it;
        // twenty-two since `maintenance.verdict.streak` (backlog
        // ada4d019), a chore verdict held on N consecutive runs — boss-gcp's
        // deposit-key audit reading unverified — stamped `Telemetry`, a
        // reading integrated over time like the estate alarms;
        // twenty-three since `jobs.urgent_item_watch` (backlog 9788e22d),
        // an urgent item past its age bound with landed cars or many runs
        // asked for a finish line, stamped `Telemetry` — a reading of the
        // record, like the agent-step overdue alarm beside it;
        // twenty-four since `jobs.idle_actor_watch` (backlog 861360bc),
        // an actor holding ready urgent nominations with no run for its
        // bound, stamped `Telemetry` — a reading of the record, like the
        // urgent-item watch beside it; twenty-five since
        // `jobs.silent_session_watch` (backlog 628dc540), an actor holding
        // claimed work whose open session has had no write of its own for
        // its bound, stamped `Telemetry` like the idle-actor watch;
        // twenty-six since `jobs.unheld_review_watch` (backlog f0e604dc),
        // a station whose ready reviews nobody holds passed their pinned
        // bound, stamped `Telemetry` like the agent-step overdue alarm;
        // twenty-seven since `jobs.factory_idle_watch` (backlog 5627a4e1),
        // no car gate for its bound while declared ready work waits,
        // stamped `Telemetry` like the unheld-review watch beside it;
        // twenty-eight since `jobs.changes_repair_watch` (backlog
        // 205ce78a), a held CHANGES car with no repair gate-run past its
        // bound, stamped `Telemetry` like the unheld-review watch.
        assert_eq!(
            filings, 28,
            "the number of machine filing sites changed. That is fine — but check the new \
             one stamps a lane, then update this count, which exists so a filing that \
             DISAPPEARS from the scan (a renamed key, a reshaped body) cannot read as \
             `all clear`."
        );
    }

    #[test]
    fn with_lane_records_the_label_and_keeps_what_was_there() {
        let out = with_lane(
            serde_json::json!({"area": "estate", "host": "w-1"}),
            InputChannel::Telemetry,
        );
        assert_eq!(out["input_channel"], "telemetry/monitoring");
        assert_eq!(out["area"], "estate", "existing keys survive");
        assert_eq!(out["host"], "w-1");
        // The key is the one the CLI reads, not a second spelling.
        assert_eq!(RECORDED_KEY, "input_channel");
        // A filer with nothing to keep still records the lane.
        let bare = with_lane(serde_json::Value::Null, InputChannel::PipelineFailure);
        assert_eq!(bare["input_channel"], "pipeline-failure");
    }

    /// Backlog e39a9d2a: a completion is the fields through the step
    /// merge door FIRST (required-at-done fields are judged at the
    /// flip), then a PUT whose body carries the status and nothing
    /// else — the one form that survives the step PUT refusing any
    /// metadata body.
    #[test]
    fn a_completion_merges_its_fields_then_flips_the_status_alone() {
        use serde_json::json;
        let mut fields = serde_json::Map::new();
        fields.insert("disposition".into(), json!("stale"));
        let writes = step_completion_writes("j1", "s1", fields);
        assert_eq!(
            writes,
            vec![
                (
                    reqwest::Method::PATCH,
                    "/api/jobs/j1/steps/s1/metadata".to_string(),
                    json!({ "disposition": "stale" }),
                ),
                (
                    reqwest::Method::PUT,
                    "/api/jobs/j1/steps/s1".to_string(),
                    json!({ "status": "completed" }),
                ),
            ]
        );
        assert_eq!(
            step_completion_writes("j1", "s1", serde_json::Map::new()),
            vec![(
                reqwest::Method::PUT,
                "/api/jobs/j1/steps/s1".to_string(),
                json!({ "status": "completed" }),
            )],
            "no fields is the flip alone — never an empty merge"
        );
    }
}
