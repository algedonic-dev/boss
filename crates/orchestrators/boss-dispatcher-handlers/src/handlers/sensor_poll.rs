//! `sensor.poll` — the first sensor: a declared poll of the world
//! outside BOSS, recorded outside the audit log, opened as work.
//!
//! Design 14c9b2ad (decided with David 2026-09-17: "Polling is fine,
//! use a restricted read-only key"), backlog 2d33e111. ONE platform
//! cadence rule (`sensors-poll-every-5-minutes`) fires this handler; it
//! reads the sensor registry (`GET /api/sensors`) and, for every
//! enabled sensor whose `every_minutes` has elapsed since its last
//! attempt (`SensorRow::due_at` — never true of a push-only source
//! such as `site`, whose readings the gateway records itself,
//! 0b5c5081), asks the source adapter for everything since the sensor's
//! cursor, records the readings (insert-if-absent), opens ONE packet
//! per reading that still owes one, stamps it, and marks the poll.
//! The product knows how to read a source; WHAT to open for it is the
//! sensor row — tenant data.
//!
//! THE READING IS NOT THE FACT; THE PACKET IS. David, 2026-09-16: "the
//! audit_log just needs to capture all work that is done; sensors
//! record data that don't flow into the audit_log." Nothing here
//! writes the log directly. The packet the poll opens — signed
//! `automation:sensor:<id>`, subject `<subject_kind>/<external id>` —
//! is the audit-log fact, and the reading is the evidence it was opened
//! from.
//!
//! IDEMPOTENT BY EXTERNAL ID. The source is read from the cursor
//! INCLUSIVE (a charge created in the cursor's own second must not be
//! lost), so a redelivery re-reads what it read before; the readings
//! door inserts nothing twice, and a reading is opened only while its
//! `packet_id` is null and stamped before the next one is opened. So a
//! firing that died between opening and stamping opens that ONE
//! packet again on the next pass — the honest bound; a firing that
//! died anywhere else opens nothing twice. Pinned by the test that
//! runs the handler twice and counts one packet.
//!
//! FAILURE IS LOUD ON A PACKET. A sensor has no packet of its own to
//! dead-letter on, so an UNREADABLE credential — the env var unset, a
//! 401/403 from the source, a source the product has no adapter for —
//! files the estate-style alarm `sensor_unreadable:<id>` (an urgent
//! backlog-item deduped by `estate_finding`, REFRESHED only when the
//! reason changes, so a persisting condition is one packet and not a
//! metadata write every five minutes) and marks the poll so the next
//! attempt waits the sensor's own period. The next GOOD read closes it
//! (`disposition = stale`, the estate.recover idiom) at the step it is
//! waiting on — `triage`, or the `build`/`measure` a person routed it
//! to — or, where the machine may not close it, tells it RECOVERED and
//! withdraws that note if the condition returns (`common::retraction`,
//! backlog 6072ff60). A network blip is a `Downstream` error: the
//! firing NAKs, the poll is NOT marked, and the next tick retries.
//!
//! AN UNOPENABLE PACKET IS THE SAME SHAPE, ONE STEP LATER (backlog
//! f50a9ec1, 2026-09-17). A source that reads fine can still declare a
//! packet the jobs API refuses — the `opens_kind` not published, its
//! `subject_kind` unknown to the registry, the described metadata
//! failing the kind's schema. Until this car that refusal was only
//! logged: a 400 rode the transient lane and retried every tick, a 422
//! terminated the whole firing, and nothing on any packet said why the
//! reading sat unstamped. Now a refusal (a 4xx that is not a miss, a
//! conflict or a throttle — `is_refusal`) files `sensor_unopenable:<id>`
//! through the SAME raise / refresh / recover mechanics as the
//! unreadable alarm, carrying the API's answer verbatim (status + body,
//! bounded by `REFUSAL_BOUND`) and the reading's id. The reading stays
//! unstamped, so it is still owed; the poll is marked so the next
//! attempt waits the period; and the first open that succeeds closes
//! the alarm. The two are two findings on one sensor, each recovered by
//! its own later success: a source that reads again closes the
//! unreadable one on the read even while its packets are still refused.
//!
//! THE VALUE IS NEVER READ EXCEPT TO SEND IT. The handler holds the
//! credential values the deployment handed it by registry id
//! (`BOSS_BROKER_STRIPE_KEY` for `stripe-restricted-read`, the same
//! optional-secretKeyRef idiom as the Cloudflare root) and passes one
//! to the adapter as a bearer. It is never logged, never on a packet,
//! never in an error string.

use boss_jobs::channels::InputChannel;
use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;

use async_trait::async_trait;
use boss_dispatcher::rules::expr::Value;
use boss_dispatcher::rules::handler::{Handler, HandlerError, InvocationContext};
use boss_jobs::sensors::{
    MAX_POLL_REFUSALS, NewReading, PollRefusalClasses, PollRefusals, PollStamp,
    READINGS_HTTP_BODY_LIMIT_BYTES, Reading, RefusedMessage, SensorRow,
};
use chrono::{DateTime, Utc};
use serde_json::{Value as Json, json};

use super::alarm_item::{self, Occurrence, Plan};
use super::common::{
    RECOVERED_AT, Retraction, api_client, dispatcher_reader_header, get_json, owner_for_filing,
    post_json, recovery_note, retraction, rows_or_refuse, sim_origin_value, step_completion_writes,
};

/// The two standing conditions a sensor alarms on. Each is its own
/// `estate_finding` (so one sensor can carry both at once — a source
/// that reads but whose packets are refused is exactly the second and
/// not the first) and each is recovered by its own later success.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Condition {
    /// The credential or the source cannot be read.
    Unreadable,
    /// The source reads, but the packet a reading declares is refused
    /// by the jobs API (backlog f50a9ec1).
    Unopenable,
}

impl Condition {
    /// The alarm key one sensor carries for this condition — the
    /// `estate_finding` the dedup lens reads.
    pub fn key(self, sensor_id: &str) -> String {
        match self {
            Self::Unreadable => format!("sensor_unreadable:{sensor_id}"),
            Self::Unopenable => format!("sensor_unopenable:{sensor_id}"),
        }
    }

    /// What a later success proves about this condition, for the
    /// recovery evidence.
    fn recovered_by(self) -> &'static str {
        match self {
            Self::Unreadable => "read",
            Self::Unopenable => "opened a packet for",
        }
    }
}

/// How much of a refusal body an alarm carries verbatim. A jobs-API
/// refusal is one line naming the kind or the field; a few hundred
/// chars holds all of it, and the bound keeps a proxy's HTML error
/// page off a packet.
pub const REFUSAL_BOUND: usize = 400;

/// What one alarm carries: the condition, the reason (what the failing
/// call answered — never a value), and for an unopenable packet the
/// reading it was about.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Alarm {
    pub condition: Condition,
    pub reason: String,
    pub external_id: Option<String>,
}

impl Alarm {
    pub fn unreadable(reason: impl Into<String>) -> Self {
        Self {
            condition: Condition::Unreadable,
            reason: reason.into(),
            external_id: None,
        }
    }

    /// The API's refusal verbatim — status and body — bounded, and
    /// saying how much was cut when it was. The reading's id is in the
    /// reason too, so a NEW reading meeting the same refusal reads as a
    /// changed reason and refreshes the packet once.
    pub fn unopenable(external_id: &str, status: u16, body: &str) -> Self {
        let total = body.chars().count();
        let shown: String = body.chars().take(REFUSAL_BOUND).collect();
        let body = if total > REFUSAL_BOUND {
            format!("{shown} …[{total} chars; first {REFUSAL_BOUND} shown]")
        } else {
            shown
        };
        Self {
            condition: Condition::Unopenable,
            reason: format!("reading {external_id}: POST /api/jobs answered {status}: {body}"),
            external_id: Some(external_id.to_string()),
        }
    }
}

/// Which answers to a packet open are a standing refusal — request
/// data the API will refuse identically on every retry — and which are
/// weather. By the house contract on `HandlerError::Permanent`, 404
/// (not yet projected), 409 (convergent conflict), 408 and 429 stay
/// retryable; every other 4xx is the API saying no to THIS body.
pub fn is_refusal(status: reqwest::StatusCode) -> bool {
    status.is_client_error()
        && !matches!(
            status,
            reqwest::StatusCode::NOT_FOUND
                | reqwest::StatusCode::REQUEST_TIMEOUT
                | reqwest::StatusCode::CONFLICT
                | reqwest::StatusCode::TOO_MANY_REQUESTS
        )
}

/// The actor a sensor's writes sign as. The packet's owner, its
/// `opened_by`, and the readings' provenance are all this one id —
/// spelled once, in boss-jobs, because the readings door admits a
/// polled sensor's readings from this actor only (0b5c5081).
pub fn sensor_actor(sensor_id: &str) -> String {
    boss_jobs::sensors::sensor_actor(sensor_id)
}

fn sensor_actor_header(sensor_id: &str) -> String {
    json!({
        "id": sensor_actor(sensor_id),
        "role": "platform-admin",
        "access_tier": "operator",
        "territory_account_ids": [],
        "direct_report_ids": [],
        "department": "platform",
    })
    .to_string()
}

/// The dedup read's page. Open backlog-items carrying `estate_finding`
/// number in the tens (all open backlog-items were 370 on 2026-09-26,
/// which is why the read asks only for those, c5ac71de); a truncated
/// page HOLDS the raise (never twins blind).
const DEDUP_PAGE: usize = 1000;
const READINGS_BOUND_VERSION: &str = "readings-json-v1";

// ---------------------------------------------------------------------------
// The port — one source adapter per `source`
// ---------------------------------------------------------------------------

/// Why a source could not be read. The two legs the handler tells
/// apart: an UNREADABLE source is a standing condition (alarmed,
/// retried at the sensor's own period); a TRANSIENT one is weather
/// (NAKed, retried at the next tick).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SourceError {
    Unreadable(String),
    Transient(String),
    /// A bounded refused SOURCE poll; no typed reading has been recorded.
    RefusedPoll(PollRefusals),
}

/// One observation as the source reports it. `payload` is the source's
/// own record (a Stripe charge), kept whole; the packet's title and
/// metadata are derived from it by [`SensorSource::describe`] so a
/// reading recorded on one firing can be opened on a later one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Observation {
    pub external_id: String,
    pub observed_at: DateTime<Utc>,
    pub payload: Json,
}

/// What a reading's packet says.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Described {
    pub title: String,
    pub metadata: Json,
}

#[async_trait]
pub trait SensorSource: Send + Sync {
    /// Intrinsic input capability, not authority. Legacy sources keep both
    /// their read arguments and their existing poll-mark behavior.
    fn supports_read_context(&self) -> bool {
        false
    }
    /// Every observation at or after `since` (inclusive — the caller
    /// dedups), in any order. `credential` is the value to send.
    async fn read(
        &self,
        credential: &str,
        since: Option<DateTime<Utc>>,
    ) -> Result<Vec<Observation>, SourceError>;

    /// Optional A211233 context. Existing sources retain their original read;
    /// bounded sources consume witnesses before their own count/byte bounds.
    async fn read_with_context(
        &self,
        credential: &str,
        since: Option<DateTime<Utc>>,
        _context: Option<&SourceReadContext>,
    ) -> Result<Vec<Observation>, SourceError> {
        self.read(credential, since).await
    }

    /// The packet one recorded payload opens. Pure.
    fn describe(&self, payload: &Json) -> Described;
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SourceReadContext {
    pub witnessed: Vec<boss_jobs::sensors::CursorWitness>,
}

/// In-memory source: the readings it will answer, or the error it
/// will raise, set by the test. Every `read` is recorded.
pub struct InMemorySource {
    pub answer: std::sync::Mutex<Result<Vec<Observation>, SourceError>>,
    pub reads: std::sync::Mutex<Vec<(String, Option<DateTime<Utc>>)>>,
}

impl InMemorySource {
    pub fn answering(obs: Vec<Observation>) -> Arc<Self> {
        Arc::new(Self {
            answer: std::sync::Mutex::new(Ok(obs)),
            reads: Default::default(),
        })
    }

    pub fn failing(e: SourceError) -> Arc<Self> {
        Arc::new(Self {
            answer: std::sync::Mutex::new(Err(e)),
            reads: Default::default(),
        })
    }

    pub fn set(&self, answer: Result<Vec<Observation>, SourceError>) {
        *self.answer.lock().expect("source lock") = answer;
    }
}

#[async_trait]
impl SensorSource for InMemorySource {
    async fn read(
        &self,
        credential: &str,
        since: Option<DateTime<Utc>>,
    ) -> Result<Vec<Observation>, SourceError> {
        self.reads
            .lock()
            .expect("reads lock")
            .push((credential.to_string(), since));
        self.answer.lock().expect("source lock").clone()
    }

    fn describe(&self, payload: &Json) -> Described {
        Described {
            title: format!(
                "Reading {}",
                payload.get("id").and_then(Json::as_str).unwrap_or("?")
            ),
            metadata: json!({ "amount_cents": payload.get("amount").cloned().unwrap_or(Json::Null) }),
        }
    }
}

// ---------------------------------------------------------------------------
// Credential values — by registry id, from the deployment
// ---------------------------------------------------------------------------

/// The credential values the deployment handed this process, keyed by
/// `credentials` registry id, each beside the env var it came from so
/// an absent one is alarmed BY NAME. Built once in the binary from
/// config; the tests build it directly.
#[derive(Default, Clone)]
pub struct CredentialValues {
    values: BTreeMap<String, (String, Option<String>)>,
}

impl CredentialValues {
    pub fn new() -> Self {
        Self::default()
    }

    /// Declare that registry id `credential` is read from env var
    /// `env_name`, whose value (if the deployment set it) is `value`.
    pub fn with(mut self, credential: &str, env_name: &str, value: Option<String>) -> Self {
        self.values.insert(
            credential.to_string(),
            (env_name.to_string(), value.filter(|v| !v.trim().is_empty())),
        );
        self
    }

    /// The value to send, or the reason it cannot be: no env var is
    /// declared for this registry id, or the declared one is unset.
    pub fn value(&self, credential: &str) -> Result<String, String> {
        match self.values.get(credential) {
            None => Err(format!(
                "no env var carries credential `{credential}` into the dispatcher — the product \
                 knows {} — declare it in the binary beside the others",
                if self.values.is_empty() {
                    "none".to_string()
                } else {
                    self.values.keys().cloned().collect::<Vec<_>>().join(", ")
                }
            )),
            Some((env, None)) => Err(format!(
                "credential `{credential}` is unset: env {env} is empty (the Secret key the \
                 registry row names has not been minted, or the pod was not rolled after it was)"
            )),
            Some((_, Some(v))) => Ok(v.clone()),
        }
    }
}

// ---------------------------------------------------------------------------
// Pure pieces — bodies and decisions
// ---------------------------------------------------------------------------

/// The packet one reading opens: the sensor's declared kind, the
/// reading as subject, the source's description, and the sensor's
/// provenance on the metadata.
pub fn packet_body(sensor: &SensorRow, r: &Reading, d: &Described) -> Json {
    let mut metadata = d.metadata.clone();
    if let Some(m) = metadata.as_object_mut() {
        m.insert("sensor_id".into(), json!(sensor.id));
        m.insert("sensor_source".into(), json!(sensor.source));
        m.insert("external_id".into(), json!(r.external_id));
        m.insert("observed_at".into(), json!(r.observed_at.to_rfc3339()));
    }
    json!({
        "kind": sensor.opens_kind,
        "subject": {"subject_kind": sensor.subject_kind, "id": r.external_id},
        "title": d.title,
        "owner_id": sensor_actor(&sensor.id),
        "priority": "standard",
        "status": "open",
        "metadata": metadata,
        "tags": ["sensor"],
    })
}

/// The alarm one troubled sensor files. The reason is what the failing
/// call answered — never a value.
pub fn alarm_body(sensor: &SensorRow, alarm: &Alarm, owner: &str) -> Json {
    let reason = &alarm.reason;
    let (title, detail) = match alarm.condition {
        Condition::Unreadable => (
            format!(
                "ESTATE ALARM: sensor {} cannot read its source ({})",
                sensor.id, sensor.source
            ),
            format!(
                "Raised by sensor.poll (design 14c9b2ad): the sensor `{}` (source {}, credential \
                 `{}`) could not be read: {reason}. Until it can, no {} packet is opened for \
                 anything the source records. The poll retries every {} minutes and refreshes \
                 this packet only when the reason changes; the next good read closes it.",
                sensor.id,
                sensor.source,
                sensor.credential,
                sensor.opens_kind,
                sensor.every_minutes
            ),
        ),
        Condition::Unopenable => (
            format!(
                "ESTATE ALARM: sensor {} cannot open its {} packet",
                sensor.id, sensor.opens_kind
            ),
            format!(
                "Raised by sensor.poll (backlog f50a9ec1): the sensor `{}` (source {}) read its \
                 source and recorded the reading, but the jobs API refused the `{}` packet it \
                 declares (subject_kind `{}`): {reason}. The reading is recorded and unstamped, \
                 so it is still owed a packet; the poll retries the open every {} minutes and \
                 refreshes this packet only when the refusal changes; the first open that \
                 succeeds closes it. The usual causes: the workflow `{}` is not published, its \
                 subject_kinds do not admit `{}`, or the described metadata fails its schema.",
                sensor.id,
                sensor.source,
                sensor.opens_kind,
                sensor.subject_kind,
                sensor.every_minutes,
                sensor.opens_kind,
                sensor.subject_kind,
            ),
        ),
    };
    let mut metadata = json!({
        "area": "estate",
        "estate_finding": alarm.condition.key(&sensor.id),
        "scope": "sensor",
        "sensor_id": sensor.id,
        "source": sensor.source,
        "credential": sensor.credential,
        "reason": reason,
        "detail": detail,
    });
    if alarm.condition == Condition::Unopenable
        && let Some(m) = metadata.as_object_mut()
    {
        m.insert("opens_kind".into(), json!(sensor.opens_kind));
        m.insert("subject_kind".into(), json!(sensor.subject_kind));
        m.insert("external_id".into(), json!(alarm.external_id));
    }
    json!({
        "kind": "backlog-item",
        "title": title,
        "subject": {"subject_kind": "custom", "id": sensor.id},
        // The platform owner as the registry answers it, or nobody for
        // the jobs API to resolve from the kind's owner_role (3c23662d).
        "owner_id": owner,
        "priority": "urgent",
        "status": "open",
        "tags": [],
        "metadata": super::common::with_lane(metadata, InputChannel::Telemetry),
    })
}

/// The sensor registry as `GET /api/sensors` answered it.
///
/// NO `data` ARRAY IS A REFUSAL, NOT AN EMPTY LIST (backlog 6c4c432a,
/// the same reading `retro_open::departments` takes of departments,
/// 80a77466). An error shape, a changed contract or a policy-narrowed
/// answer carries no `data` key, and reading that as zero sensors
/// polls nothing and reports the pass healthy — the whole class of
/// outside-world input going dark with the only signal an absence of
/// readings. An EMPTY array is honest: sensors are a registry, not a
/// work queue, and a deployment may legitimately declare none, so it
/// keeps working and no floor is put on the count.
///
/// The refusal is RETRYABLE (`HandlerError::Downstream` at the call
/// site, as the malformed-row case already was): a listing with no
/// `data` is a bad answer from the jobs API, not a deterministic fault
/// in the request this handler sent, so a redelivery can succeed once
/// the service or the policy scope is right, and the firing naks
/// loudly instead of terminating.
pub fn sensors(listing: &Json) -> Result<Vec<SensorRow>, String> {
    rows_or_refuse(listing, "GET /api/sensors")
}

/// The readings still owed a packet, as the sensors API answered them.
///
/// A missing, null or non-array `data` is NO ANSWER and refuses — it
/// used to read as an empty list, i.e. "nothing owes a packet", so a
/// poll against a wrong-shaped answer completed happily while recorded
/// readings never became packets: the readings exist, the sensor looks
/// alive, and the audit-log fact never opens (0767c830). An EMPTY
/// array stays honest — a sensor may genuinely owe nothing.
pub fn owed_readings(listing: &Json) -> Result<Vec<Reading>, String> {
    rows_or_refuse(listing, "the owed-readings read")
}

/// One open alarm as the dedup read found it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OpenAlarm {
    pub id: String,
    /// The reason it was raised or last refreshed with.
    pub reason: String,
    /// It carries a standing recovery note (`common::recovery_note`) —
    /// a routed alarm told the condition cleared, which a relapse must
    /// withdraw (backlog 6072ff60).
    pub told: bool,
    /// The packet as the dedup read listed it, for `alarm_item::plan`.
    pub row: Json,
}

/// Does this packet carry a standing recovery note? One reading of the
/// listing row and of the full packet alike.
fn told(packet: &Json) -> bool {
    packet
        .get("metadata")
        .and_then(|m| m.get(RECOVERED_AT))
        .is_some_and(|v| !v.is_null())
}

/// The open alarm carrying `key`, if any — and whether the page can be
/// trusted (a truncated page holds).
///
/// A missing, null or non-array `data` is NO ANSWER and refuses by
/// that name (445c1494). It used to read as zero rows and was caught
/// only by accident of the truncation check below — misnamed
/// "truncated" — and `{"total": 0}` slipped through it as a completed
/// read that found nothing, so the alarm it was guarding got twinned.
/// The truncation check now means only what it says.
pub fn open_alarm(listing: &Json, key: &str) -> Result<Option<OpenAlarm>, String> {
    let rows: Vec<Json> = rows_or_refuse(listing, "the open-alarm dedup read")?;
    let total = listing
        .get("total")
        .and_then(Json::as_u64)
        .map(|t| usize::try_from(t).unwrap_or(usize::MAX));
    match total {
        Some(t) if rows.len() >= t => {}
        _ => {
            return Err(format!(
                "dedup read truncated ({} rows, total {total:?}); the alarm is held for retry rather than twinned",
                rows.len()
            ));
        }
    }
    Ok(
        alarm_item::open_item(&rows, "estate_finding", key).and_then(|j| {
            Some(OpenAlarm {
                id: j.get("id")?.as_str()?.to_string(),
                reason: j
                    .pointer("/metadata/reason")
                    .and_then(Json::as_str)
                    .unwrap_or_default()
                    .to_string(),
                told: told(j),
                row: j.clone(),
            })
        }),
    )
}

/// Who a sensor alarm's retraction is stamped as.
const CLEARED_BY: &str = "sensor.poll";

/// What a later success proves — the one sentence a close and a
/// recovery note both state.
fn recovery_evidence(sensor_id: &str, condition: Condition, at: DateTime<Utc>) -> String {
    format!(
        "sensor.poll {} sensor `{sensor_id}` successfully at {}; the condition this alarm \
         carried no longer holds.",
        condition.recovered_by(),
        at.to_rfc3339()
    )
}

/// The fields that close a recovered alarm — at `triage`, or at the
/// `build`/`measure` a person routed it to (`common::retraction`,
/// backlog 6072ff60). ONLY these: they ride the step merge door, which
/// keeps every key the step holds (e39a9d2a) — this used to clone the
/// step's own metadata in for a PUT that replaced it wholesale.
pub fn recover_step_fields(
    sensor_id: &str,
    condition: Condition,
    at: DateTime<Utc>,
) -> serde_json::Map<String, Json> {
    let mut fields = serde_json::Map::new();
    fields.insert("disposition".into(), json!("stale"));
    fields.insert(
        "evidence".into(),
        json!(format!(
            "{} Closed by machine from the success, not by judgement.",
            recovery_evidence(sensor_id, condition, at)
        )),
    );
    fields.insert("cleared_by".into(), json!(CLEARED_BY));
    fields.insert(RECOVERED_AT.into(), json!(at.to_rfc3339()));
    fields
}

// ---------------------------------------------------------------------------
// The handler
// ---------------------------------------------------------------------------

pub struct SensorPoll {
    client: boss_core::machine_token::Client,
    jobs_base: String,
    sources: HashMap<String, Arc<dyn SensorSource>>,
    credentials: CredentialValues,
    /// Who the packets this handler files are owned by — the platform
    /// owner through the port (backlog 3c23662d), resolved once per
    /// invocation by `common::owner_for_filing`; never a literal.
    owner: Arc<dyn boss_core::platform_owner::PlatformOwner>,
}

/// What one sensor's poll did — the line the firing logs per sensor.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PollOutcome {
    /// `(readings recorded, packets opened)`.
    Read { recorded: usize, opened: usize },
    /// The alarm was raised, refreshed, or left as it was.
    Unreadable(&'static str),
    /// A permanent readings-POST refusal, retained on the poll mark.
    Refused,
    /// The source read, but a packet open was refused: what happened
    /// to the alarm, and what was recorded and opened before it.
    Unopenable {
        alarm: &'static str,
        recorded: usize,
        opened: usize,
    },
}

impl SensorPoll {
    async fn write_poll_mark(
        &self,
        sensor: &SensorRow,
        stamp: &PollStamp,
    ) -> Result<(), HandlerError> {
        let path = format!("/api/sensors/{}/polled", sensor.id);
        let body = serde_json::to_value(stamp)
            .map_err(|e| HandlerError::Downstream(format!("encoding poll mark: {e}")))?;
        let (status, text) = self
            .send(reqwest::Method::POST, &path, &body, &sensor.id)
            .await?;
        if status == reqwest::StatusCode::BAD_REQUEST {
            // A191726 M3 applies only to this merge door, not reading or
            // packet admission/configuration failures on other endpoints.
            let shown: String = text.chars().take(REFUSAL_BOUND).collect();
            let reason =
                format!("sensor poll mark logic refusal: POST {path} returned 400: {shown}");
            self.raise_or_refresh(sensor, &Alarm::unreadable(&reason), stamp.polled_at)
                .await?;
            return Err(HandlerError::Permanent(reason));
        }
        if !status.is_success() {
            let reason = format!("POST {path} returned {status}: {text}");
            return Err(if status == reqwest::StatusCode::UNPROCESSABLE_ENTITY {
                HandlerError::Permanent(reason)
            } else {
                HandlerError::Downstream(reason)
            });
        }
        Ok(())
    }

    async fn mark_refused(
        &self,
        sensor: &SensorRow,
        now: DateTime<Utc>,
        cursor: Option<DateTime<Utc>>,
        mut refused: PollRefusals,
    ) -> Result<(), HandlerError> {
        // Old absent measurements are read-only. The locked backend retains
        // these exact held witnesses; only new measured entries cross the
        // strict write door. An unknown unmeasured entry is NOT dropped.
        refused.entries.retain(|entry| {
            entry.limit.is_measured()
                || !sensor
                    .refused
                    .iter()
                    .flat_map(|w| &w.entries)
                    .any(|old| old == entry)
        });
        self.write_poll_mark(
            sensor,
            &PollStamp {
                polled_at: now,
                cursor_at: cursor,
                refused: Some(refused),
            },
        )
        .await?;
        Ok(())
    }

    pub fn new(
        jobs_base: impl Into<String>,
        sources: HashMap<String, Arc<dyn SensorSource>>,
        credentials: CredentialValues,
        owner: Arc<dyn boss_core::platform_owner::PlatformOwner>,
    ) -> Arc<Self> {
        Arc::new(Self {
            client: api_client(),
            jobs_base: jobs_base.into(),
            sources,
            credentials,
            owner,
        })
    }

    fn base(&self) -> &str {
        self.jobs_base.trim_end_matches('/')
    }

    async fn get(&self, path: &str) -> Result<Json, HandlerError> {
        let url = format!("{}{path}", self.base());
        let resp = self
            .client
            .get(&url)
            .header("x-boss-user", dispatcher_reader_header())
            .header("x-sim-origin", sim_origin_value())
            .send()
            .await
            .map_err(|e| HandlerError::Downstream(format!("GET {url}: {e}")))?;
        if !resp.status().is_success() {
            let st = resp.status();
            let body = resp.text().await.unwrap_or_default();
            return Err(HandlerError::Downstream(format!(
                "GET {url} returned {st}: {body}"
            )));
        }
        resp.json()
            .await
            .map_err(|e| HandlerError::Downstream(format!("GET {url} not JSON: {e}")))
    }

    /// A write signed as the sensor's own actor, answered as the API
    /// answered it: the status and the body, whatever they were. Only
    /// transport failure is an error here; the caller judges the
    /// status, because one caller (the packet open) needs to tell a
    /// refusal from weather and keep the refusal's text.
    async fn send(
        &self,
        method: reqwest::Method,
        path: &str,
        body: &Json,
        sensor_id: &str,
    ) -> Result<(reqwest::StatusCode, String), HandlerError> {
        let url = format!("{}{path}", self.base());
        let verb = method.to_string();
        let resp = self
            .client
            .request(method, &url)
            .header("content-type", "application/json")
            .header("x-boss-user", sensor_actor_header(sensor_id))
            .header("x-sim-origin", sim_origin_value())
            .json(body)
            .send()
            .await
            .map_err(|e| HandlerError::Downstream(format!("{verb} {url}: {e}")))?;
        let status = resp.status();
        let text = resp.text().await.unwrap_or_default();
        Ok((status, text))
    }

    /// A write that must succeed. Returns the body for the one caller
    /// that reads it (the packet's id).
    async fn write(
        &self,
        method: reqwest::Method,
        path: &str,
        body: &Json,
        sensor_id: &str,
    ) -> Result<Json, HandlerError> {
        let url = format!("{}{path}", self.base());
        let verb = method.to_string();
        let (status, text) = self.send(method, path, body, sensor_id).await?;
        if !status.is_success() {
            return Err(if status == reqwest::StatusCode::UNPROCESSABLE_ENTITY {
                HandlerError::Permanent(format!("{verb} {url} returned {status}: {text}"))
            } else {
                HandlerError::Downstream(format!("{verb} {url} returned {status}: {text}"))
            });
        }
        Ok(serde_json::from_str(&text).unwrap_or(Json::Null))
    }

    async fn mark_polled(
        &self,
        sensor: &SensorRow,
        polled_at: DateTime<Utc>,
        cursor_at: Option<DateTime<Utc>>,
    ) -> Result<(), HandlerError> {
        let stamp = PollStamp {
            polled_at,
            cursor_at,
            refused: None,
        };
        self.write_poll_mark(sensor, &stamp).await?;
        Ok(())
    }

    /// Only the packets carrying `estate_finding`, the one key
    /// [`open_alarm`] compares (backlog c5ac71de): unfiltered, every open
    /// backlog-item counted toward the page, and past it the raise held.
    async fn open_alarm_for(&self, key: &str) -> Result<Option<OpenAlarm>, HandlerError> {
        let listing = self
            .get(&format!(
                "/api/jobs?kind=backlog-item&status=open&metadata_has=estate_finding&limit={DEDUP_PAGE}"
            ))
            .await?;
        open_alarm(&listing, key).map_err(HandlerError::Downstream)
    }

    /// File the alarm, or refresh the open one when the reason moved,
    /// or leave it when nothing changed. One mechanism for both
    /// conditions; the alarm's key is what tells them apart.
    ///
    /// A RELAPSE WITHDRAWS THE RECOVERY NOTE (backlog 6072ff60). A
    /// routed alarm the machine may not close is told RECOVERED
    /// ([`Self::recover`]); if the condition then comes back, that note
    /// is false. The held path writes nothing to the packet by design —
    /// one packet, not a metadata write every five minutes — which is
    /// exactly why the note would stand through the relapse unless the
    /// held path withdraws it, as cadence.silence.sweep and
    /// estate.recover withdraw theirs (a2d8bad3). It is one write, on
    /// the first failing read only: after it the packet is no longer
    /// told, and the held path is silent again.
    ///
    /// The decision is the one every alarm raises by (backlog f4b0ad41,
    /// `alarm_item::plan`): a relapse is an occurrence on the open
    /// alarm, recorded beside the withdrawal of its note. Only the
    /// transport is this handler's own, because a sensor's writes are
    /// signed as the sensor.
    async fn raise_or_refresh(
        &self,
        sensor: &SensorRow,
        alarm: &Alarm,
        now: DateTime<Utc>,
    ) -> Result<&'static str, HandlerError> {
        let key = alarm.condition.key(&sensor.id);
        let reason = &alarm.reason;
        let owner = owner_for_filing(self.owner.as_ref(), self.name()).await;
        let open = self.open_alarm_for(&key).await?;
        let body = alarm_body(sensor, alarm, &owner);
        // A moved reason refreshes the open alarm with the fresh body's
        // metadata, whole, so every key the reason moved with (the
        // detail, the reading's id) moves with it.
        let refresh = match &open {
            Some(o) if o.reason != *reason => {
                body["metadata"].as_object().cloned().unwrap_or_default()
            }
            _ => Default::default(),
        };
        let refreshed = !refresh.is_empty();
        let occ = Occurrence::new(now.to_rfc3339(), reason.as_str());
        match alarm_item::plan(open.as_ref().map(|o| &o.row), body, &occ, refresh) {
            Plan::File(body) => {
                self.write(reqwest::Method::POST, "/api/jobs", &body, &sensor.id)
                    .await?;
                tracing::warn!(finding = %key, reason = %reason, "sensor.poll raised an alarm");
                Ok("raised")
            }
            Plan::Patch {
                id,
                merge,
                appended,
            } => {
                self.write(
                    reqwest::Method::PATCH,
                    &format!("/api/jobs/{id}/metadata"),
                    &Json::Object(merge),
                    &sensor.id,
                )
                .await?;
                if appended {
                    tracing::warn!(finding = %key, packet = %id, "sensor.poll: the condition is back; recorded the occurrence and withdrew the recovery note");
                } else {
                    tracing::info!(finding = %key, packet = %id, "sensor.poll refreshed the open alarm");
                }
                Ok(if refreshed { "refreshed" } else { "held" })
            }
            Plan::Hold(id) => {
                tracing::info!(finding = %key, packet = %id, "sensor.poll: alarm already open with this reason");
                Ok("held")
            }
        }
    }

    /// Withdraw the open alarm for `condition`, if one is, at the step
    /// the packet is waiting on (`common::retraction`).
    ///
    /// It completed `triage` only, so once a person had routed the
    /// alarm the jobs API refused the PUT as a write to a terminal step
    /// — a 409 on every poll of a healthy sensor, and an alarm that
    /// never retracted (backlog 6072ff60, the sensor half of a2d8bad3).
    /// Now a ready `measure`/`build` is completed `stale`, and a route
    /// the machine may not complete is TOLD, once, that the condition
    /// cleared; a relapse withdraws that note ([`Self::raise_or_refresh`]).
    async fn recover(
        &self,
        sensor: &SensorRow,
        condition: Condition,
        at: DateTime<Utc>,
    ) -> Result<(), HandlerError> {
        let key = condition.key(&sensor.id);
        let Some(OpenAlarm { id, .. }) = self.open_alarm_for(&key).await? else {
            return Ok(());
        };
        let job = self.get(&format!("/api/jobs/{id}")).await?;
        match retraction(&job) {
            None => {
                tracing::warn!(finding = %key, packet = %id, "sensor.poll: the open alarm has no triage step to close");
            }
            // The fields through the step merge door, then the flip
            // (e39a9d2a).
            Some(Retraction::Complete { slug, step_id }) => {
                for (method, path, body) in step_completion_writes(
                    &id,
                    &step_id,
                    recover_step_fields(&sensor.id, condition, at),
                ) {
                    self.write(method, &path, &body, &sensor.id).await?;
                }
                tracing::info!(finding = %key, packet = %id, step = %slug, "sensor.poll closed the alarm: the condition no longer holds");
            }
            // A packet already told is not told again on every poll.
            Some(Retraction::Annotate { .. }) if told(&job) => {}
            Some(Retraction::Annotate { why_open }) => {
                self.write(
                    reqwest::Method::PATCH,
                    &format!("/api/jobs/{id}/metadata"),
                    &Json::Object(recovery_note(
                        &recovery_evidence(&sensor.id, condition, at),
                        CLEARED_BY,
                        &at.to_rfc3339(),
                        &why_open,
                    )),
                    &sensor.id,
                )
                .await?;
                tracing::info!(finding = %key, packet = %id, why_open = %why_open, "sensor.poll: the condition cleared; told the routed alarm it has recovered");
            }
        }
        Ok(())
    }

    /// The whole obligation for one due sensor.
    async fn poll_one(
        &self,
        sensor: &SensorRow,
        now: DateTime<Utc>,
    ) -> Result<PollOutcome, HandlerError> {
        let unreadable = |reason: String| async move {
            let what = self
                .raise_or_refresh(sensor, &Alarm::unreadable(reason), now)
                .await?;
            self.mark_polled(sensor, now, None).await?;
            Ok::<_, HandlerError>(PollOutcome::Unreadable(what))
        };
        let credential = match self.credentials.value(&sensor.credential) {
            Ok(v) => v,
            Err(why) => return unreadable(why).await,
        };
        let Some(source) = self.sources.get(&sensor.source) else {
            return unreadable(format!(
                "no source adapter named `{}` — the product reads: {}",
                sensor.source,
                self.sources.keys().cloned().collect::<Vec<_>>().join(", ")
            ))
            .await;
        };
        let context_aware = source.supports_read_context();
        let context = context_aware.then(|| SourceReadContext {
            witnessed: sensor
                .refused
                .iter()
                .flat_map(|w| {
                    w.processed.iter().cloned().chain(
                        w.entries
                            .iter()
                            .filter(|e| e.reason.starts_with("readings_post_413"))
                            .map(|e| boss_jobs::sensors::CursorWitness {
                                message_key: e.message_key.clone(),
                                at: e.at,
                            }),
                    )
                })
                .filter(|key| Some(key.at) == sensor.cursor_at)
                .collect(),
        });
        let observations = match source
            .read_with_context(&credential, sensor.cursor_at, context.as_ref())
            .await
        {
            Err(SourceError::RefusedPoll(mut witness)) => {
                witness.validate().map_err(HandlerError::Permanent)?;
                let count = witness.entries.len() as u64 + witness.truncated;
                for entry in &mut witness.entries {
                    entry.reason = format!("source_poll:{}", entry.reason);
                }
                let permanent = sensor
                    .refused
                    .iter()
                    .flat_map(|w| &w.entries)
                    .filter(|e| {
                        e.reason.starts_with("readings_post_413")
                            && sensor.cursor_at.is_none_or(|cursor| e.at >= cursor)
                    })
                    .cloned()
                    .collect::<Vec<_>>();
                let permanent_count = permanent.len() as u64;
                let processed = sensor
                    .refused
                    .iter()
                    .flat_map(|w| &w.processed)
                    .filter(|key| Some(key.at) == sensor.cursor_at)
                    .cloned()
                    .collect::<Vec<_>>();
                if permanent_count > 0 || !processed.is_empty() {
                    // The source only supplies its first64 names. Its omitted
                    // cohort may overlap permanent refusals: do not add counts.
                    let mut entries = permanent
                        .into_iter()
                        .map(|e| ((e.at, e.message_key.clone()), e))
                        .collect::<std::collections::BTreeMap<_, _>>();
                    for entry in &witness.entries {
                        if entries.len() + processed.len() == MAX_POLL_REFUSALS {
                            break;
                        }
                        entries
                            .entry((entry.at, entry.message_key.clone()))
                            .or_insert_with(|| entry.clone());
                    }
                    witness.entries = entries.into_values().collect();
                    witness.classes = Some(PollRefusalClasses {
                        source_count: count,
                        permanent_413_count: permanent_count,
                        union_count: None,
                    });
                    // No union omitted-count can be derived. The fixed class
                    // summary itself holds the latch, not a fabricated count.
                    witness.truncated = 0;
                }
                witness.entries.retain(|entry| {
                    !processed
                        .iter()
                        .any(|key| key.at == entry.at && key.message_key == entry.message_key)
                });
                let processed_count = processed.len();
                witness.processed = processed;
                witness.validate().map_err(HandlerError::Permanent)?;
                self.mark_refused(sensor, now, None, witness).await?;
                self.raise_or_refresh(sensor, &Alarm::unreadable(format!(
                    "source poll refused: source count={count}, witnessed permanent413 count={permanent_count}, processed count={processed_count}, union {}; cursor={:?}; bounded identities retained; remeasurement required, valid tied messages remain owed", if permanent_count>0 || processed_count>0 {"unknown".into()} else {count.to_string()}, sensor.cursor_at
                )), now).await?;
                return Ok(PollOutcome::Refused);
            }
            Ok(o) => o,
            Err(SourceError::Unreadable(why)) => return unreadable(why).await,
            Err(SourceError::Transient(why)) => {
                return Err(HandlerError::Downstream(format!(
                    "sensor {}: {why}",
                    sensor.id
                )));
            }
        };
        let mut measured_sensor = sensor.clone();
        if sensor
            .refused
            .as_ref()
            .is_some_and(PollRefusals::is_latched)
        {
            let source_latch = sensor.refused.as_ref().is_some_and(|w| {
                w.classes.as_ref().is_some_and(|c| c.source_count > 0)
                    || (!w.entries.is_empty()
                        && w.entries
                            .iter()
                            .all(|e| e.reason.starts_with("source_poll:")))
            });
            if source_latch {
                let cleared = PollRefusals {
                    processed: sensor
                        .refused
                        .iter()
                        .flat_map(|w| &w.processed)
                        .cloned()
                        .collect(),
                    classes: None,
                    entries: sensor
                        .refused
                        .iter()
                        .flat_map(|w| &w.entries)
                        .filter(|e| {
                            e.reason.starts_with("readings_post_413")
                                && sensor.cursor_at.is_none_or(|cursor| e.at >= cursor)
                        })
                        .cloned()
                        .collect(),
                    truncated: 0,
                    cleared: Some(format!(
                        "cleared at {}: source certified readable bounded cohort of {} keys at cursor {:?}",
                        now.to_rfc3339(),
                        observations.len(),
                        sensor.cursor_at
                    )),
                };
                self.mark_refused(sensor, now, None, cleared.clone())
                    .await?;
                measured_sensor.refused = Some(cleared);
            }
        }
        let sensor = &measured_sensor;
        // The cursor is inclusive. Only witnessed identities are excluded;
        // a new valid message at the SAME instant is still eligible.
        let observations: Vec<_> = observations
            .into_iter()
            .filter(|observation| {
                !sensor.refused.as_ref().is_some_and(|witness| {
                    witness.processed.iter().any(|key| {
                        key.at == observation.observed_at
                            && key.message_key == observation.external_id
                    }) || witness.entries.iter().any(|entry| {
                        entry.reason.starts_with("readings_post_413")
                            && entry.at == observation.observed_at
                            && entry.message_key == observation.external_id
                    })
                })
            })
            .collect();
        let readings: Vec<NewReading> = observations
            .iter()
            .map(|o| NewReading {
                external_id: o.external_id.clone(),
                observed_at: o.observed_at,
                payload: o.payload.clone(),
            })
            .collect();
        let mut recorded = 0;
        let mut acknowledged = Vec::new();
        let was_latched = sensor
            .refused
            .as_ref()
            .is_some_and(PollRefusals::is_latched);
        let mut refused_keys = sensor
            .refused
            .as_ref()
            .into_iter()
            .flat_map(|w| &w.entries)
            .filter(|e| {
                e.reason.starts_with("readings_post_413")
                    && sensor.cursor_at.is_none_or(|cursor| e.at >= cursor)
            })
            .map(|e| ((e.at, e.message_key.clone()), e.clone()))
            .collect::<std::collections::BTreeMap<_, _>>();
        let prior_keys = refused_keys
            .keys()
            .cloned()
            .collect::<std::collections::BTreeSet<_>>();
        let mut chunks = if readings.is_empty() {
            vec![]
        } else {
            vec![readings]
        };
        while let Some(chunk) = chunks.pop() {
            let body = serde_json::to_value(&chunk)
                .map_err(|e| HandlerError::Downstream(format!("encoding readings: {e}")))?;
            let size = serde_json::to_vec(&body)
                .map_err(|e| HandlerError::Downstream(format!("sizing readings: {e}")))?
                .len();
            let (status, text) = self
                .send(
                    reqwest::Method::POST,
                    &format!("/api/sensors/{}/readings", sensor.id),
                    &body,
                    &sensor.id,
                )
                .await?;
            if status == reqwest::StatusCode::PAYLOAD_TOO_LARGE {
                if chunk.len() > 1 {
                    // A114958: an opaque batch refusal is not evidence that
                    // any one message is invalid. Measure singleton outcomes.
                    chunks.extend(chunk.into_iter().rev().map(|r| vec![r]));
                    continue;
                }
                for reading in chunk {
                    refused_keys.insert(
                        (reading.observed_at, reading.external_id.clone()),
                        RefusedMessage {
                            message_key: reading.external_id.clone(),
                            size_bytes: size as u64,
                            limit: boss_jobs::sensors::RefusalLimit::bytes(
                                READINGS_HTTP_BODY_LIMIT_BYTES as u64,
                                size as u64,
                            ),
                            reason: format!("readings_post_413:{READINGS_BOUND_VERSION}"),
                            at: reading.observed_at,
                        },
                    );
                }
                continue;
            }
            if !status.is_success() {
                let why = format!(
                    "POST {}/api/sensors/{}/readings returned {status}: {text}",
                    self.base(),
                    sensor.id
                );
                return Err(if status == reqwest::StatusCode::UNPROCESSABLE_ENTITY {
                    HandlerError::Permanent(why)
                } else {
                    HandlerError::Downstream(why)
                });
            }
            let out: Json = serde_json::from_str(&text).unwrap_or(Json::Null);
            if context_aware
                && (out.get("received").and_then(Json::as_u64) != Some(chunk.len() as u64)
                    || out
                        .get("inserted")
                        .and_then(Json::as_u64)
                        .is_none_or(|n| n > chunk.len() as u64))
            {
                return Err(HandlerError::Downstream(format!(
                    "sensor {}: readings POST answered without a complete BatchOutcome; processing witness withheld",
                    sensor.id
                )));
            }
            acknowledged.extend(chunk.iter().map(|r| boss_jobs::sensors::CursorWitness {
                message_key: r.external_id.clone(),
                at: r.observed_at,
            }));
            recorded += out
                .get("inserted")
                .and_then(Json::as_u64)
                .map(|n| n as usize)
                .unwrap_or(0);
        }
        let has_refusals = !refused_keys.is_empty();
        if !context_aware && (has_refusals || was_latched) {
            let count = refused_keys.len();
            let truncated = count.saturating_sub(MAX_POLL_REFUSALS) as u64;
            let mut entries = refused_keys
                .iter()
                .filter(|(key, _)| prior_keys.contains(*key))
                .map(|(_, entry)| entry.clone())
                .collect::<Vec<_>>();
            entries.extend(
                refused_keys
                    .iter()
                    .filter(|(key, _)| !prior_keys.contains(*key))
                    .take(MAX_POLL_REFUSALS.saturating_sub(entries.len()))
                    .map(|(_, entry)| entry.clone()),
            );
            entries.sort_by(|a, b| (a.at, &a.message_key).cmp(&(b.at, &b.message_key)));
            let keys = entries
                .iter()
                .map(|e| e.message_key.as_str())
                .collect::<Vec<_>>();
            let reason = format!(
                "sensor configuration fault: single-reading POST413, bound version {READINGS_BOUND_VERSION}, limit {} bytes; retained {} witnessed keys {keys:?}, known/measured refusal count {count}, omitted {truncated}; only witnessed keys are suppressed",
                READINGS_HTTP_BODY_LIMIT_BYTES,
                entries.len()
            );
            self.mark_refused(sensor,now,None,PollRefusals { processed: Vec::new(),entries,truncated,classes:None,cleared:if was_latched && truncated==0 {Some(format!("cleared at {}: source remeasured; singleton POSTs resolved every unwitnessed key; {count} current refused keys fit",now.to_rfc3339()))} else {None}}).await?;
            if has_refusals {
                self.raise_or_refresh(sensor, &Alarm::unreadable(reason), now)
                    .await?;
            }
            if truncated > 0 {
                return Ok(PollOutcome::Refused);
            }
        }
        let cursor = observations.iter().map(|o| o.observed_at).max();
        if context_aware {
            // The reading ACK makes a durable owed obligation. Commit its
            // inclusive-source witness before attempting the separate packet
            // open, so a recoverable packet failure cannot strand a suffix.
            let at = cursor.or(sensor.cursor_at);
            let mut processed = sensor
                .refused
                .iter()
                .flat_map(|w| &w.processed)
                .cloned()
                .chain(acknowledged)
                .filter(|key| Some(key.at) == at)
                .collect::<std::collections::BTreeSet<_>>();
            let entries = refused_keys
                .values()
                .filter(|e| at.is_none_or(|at| e.at >= at))
                .cloned()
                .collect::<Vec<_>>();
            processed.retain(|key| {
                !entries
                    .iter()
                    .any(|e| e.at == key.at && e.message_key == key.message_key)
            });
            let witness = PollRefusals {
                processed: processed.into_iter().collect(),
                entries,
                truncated: 0,
                classes: None,
                cleared: None,
            };
            witness.validate().map_err(HandlerError::Permanent)?;
            self.mark_refused(sensor, now, cursor, witness).await?;
            if has_refusals {
                self.raise_or_refresh(sensor,&Alarm::unreadable(format!("sensor configuration fault: single-reading POST413, bound version {READINGS_BOUND_VERSION}; witnessed keys retained")), now).await?;
            }
        }
        // Open a packet per reading still owed one — the ones just
        // recorded and any an earlier firing recorded but could not
        // open — and stamp each before the next.
        let listing = self
            .get(&format!(
                "/api/sensors/{}/readings?unstamped=true",
                sensor.id
            ))
            .await?;
        // Downstream, not Permanent: a bad answer from the jobs API is
        // not a fault in the request this handler sent, so a
        // redelivery can succeed once the far side is right
        // (boss-dispatcher/src/rules/handler.rs documents Permanent as
        // a deterministic request-data error that terminates).
        let owed: Vec<Reading> = owed_readings(&listing)
            .map_err(|why| HandlerError::Downstream(format!("sensor {}: {why}", &sensor.id)))?;
        // The cursor is the newest observation whether or not every
        // packet opens: the readings are recorded, and an owed one is
        // found by its missing stamp, not by re-reading the source.
        let mut opened = 0;
        for r in &owed {
            let described = source.describe(&r.payload);
            let (status, text) = self
                .send(
                    reqwest::Method::POST,
                    "/api/jobs",
                    &packet_body(sensor, r, &described),
                    &sensor.id,
                )
                .await?;
            if is_refusal(status) {
                // The unopenable leg: the same raise / mark as the
                // unreadable one, one step later. The reading stays
                // unstamped (still owed), and the source having read
                // is itself the unreadable alarm's recovery.
                let alarm = Alarm::unopenable(&r.external_id, status.as_u16(), &text);
                let what = self.raise_or_refresh(sensor, &alarm, now).await?;
                self.mark_polled(sensor, now, cursor).await?;
                self.recover(sensor, Condition::Unreadable, now).await?;
                return Ok(PollOutcome::Unopenable {
                    alarm: what,
                    recorded,
                    opened,
                });
            }
            if !status.is_success() {
                return Err(HandlerError::Downstream(format!(
                    "POST /api/jobs for reading {} returned {status}: {text}",
                    r.external_id
                )));
            }
            let created: Json = serde_json::from_str(&text).unwrap_or(Json::Null);
            let packet_id = created
                .get("id")
                .or_else(|| created.pointer("/data/id"))
                .and_then(Json::as_str)
                .ok_or_else(|| {
                    HandlerError::Downstream(format!(
                        "POST /api/jobs answered without an id for reading {}: {created}",
                        r.external_id
                    ))
                })?
                .to_string();
            self.write(
                reqwest::Method::PUT,
                &format!(
                    "/api/sensors/{}/readings/{}/packet",
                    sensor.id, r.external_id
                ),
                &json!({ "packet_id": packet_id }),
                &sensor.id,
            )
            .await?;
            opened += 1;
        }
        self.mark_polled(sensor, now, cursor).await?;
        if !has_refusals {
            self.recover(sensor, Condition::Unreadable, now).await?;
        }
        self.recover(sensor, Condition::Unopenable, now).await?;
        Ok(PollOutcome::Read { recorded, opened })
    }
}

#[async_trait]
impl Handler for SensorPoll {
    fn name(&self) -> &'static str {
        "sensor.poll"
    }

    async fn invoke(
        &self,
        _args: &[(String, Value)],
        ctx: &InvocationContext,
    ) -> Result<(), HandlerError> {
        // The tick's instant (`_at`); refused, never the wall clock,
        // when the firing carries none (eabc5943).
        let now = ctx.firing_instant()?;
        let listing = get_json(
            &self.client,
            &format!("{}/api/sensors", self.base()),
            &ctx.rule_name,
        )
        .await?;
        let sensors = sensors(&listing).map_err(HandlerError::Downstream)?;
        let mut transient: Vec<String> = Vec::new();
        for sensor in sensors.iter().filter(|s| s.due_at(now)) {
            match self.poll_one(sensor, now).await {
                Ok(outcome) => {
                    tracing::info!(sensor = %sensor.id, ?outcome, "sensor.poll");
                }
                Err(HandlerError::Downstream(why)) => {
                    tracing::warn!(sensor = %sensor.id, error = %why, "sensor.poll: transient, will retry");
                    transient.push(why);
                }
                Err(e) => return Err(e),
            }
        }
        // The readings' retention sweep rides the same firing — one
        // sweep per pass, whatever the sensors did, signed as the rule
        // (it is the platform's chore, not any one sensor's).
        post_json(
            &self.client,
            &format!("{}/api/sensors/sweep", self.base()),
            &json!({}),
            &ctx.rule_name,
        )
        .await?;
        if transient.is_empty() {
            Ok(())
        } else {
            Err(HandlerError::Downstream(transient.join("; ")))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use boss_jobs::sensors::Sensors as _;
    use std::sync::Mutex;

    fn at(s: &str) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339(s).unwrap().with_timezone(&Utc)
    }

    fn sensor() -> SensorRow {
        SensorRow {
            id: "stripe-sponsorships".into(),
            source: "stripe".into(),
            credential: "stripe-restricted-read".into(),
            every_minutes: 15,
            opens_kind: "receive-a-sponsorship".into(),
            subject_kind: "custom".into(),
            enabled: true,
            selector: None,
            tenant_id: "acme".into(),
            published_at: at("2026-09-17T00:00:00Z"),
            last_polled_at: None,
            cursor_at: None,
            refused: None,
        }
    }

    fn obs(id: &str, when: &str, amount: i64) -> Observation {
        Observation {
            external_id: id.into(),
            observed_at: at(when),
            payload: json!({"id": id, "amount": amount, "status": "succeeded"}),
        }
    }

    // ----- pure pieces -----

    #[test]
    fn the_packet_is_the_sensors_declared_kind_about_the_reading_signed_by_the_sensor() {
        let r = Reading {
            sensor_id: "stripe-sponsorships".into(),
            external_id: "ch_1".into(),
            observed_at: at("2026-09-17T10:00:00Z"),
            payload: json!({"id": "ch_1", "amount": 500}),
            packet_id: None,
        };
        let d = Described {
            title: "Sponsorship: $5.00".into(),
            metadata: json!({"amount_cents": 500, "currency": "usd"}),
        };
        let b = packet_body(&sensor(), &r, &d);
        assert_eq!(b["kind"], "receive-a-sponsorship");
        assert_eq!(b["subject"]["subject_kind"], "custom");
        assert_eq!(b["subject"]["id"], "ch_1");
        assert_eq!(b["owner_id"], "automation:sensor:stripe-sponsorships");
        assert_eq!(b["metadata"]["amount_cents"], 500);
        assert_eq!(b["metadata"]["sensor_id"], "stripe-sponsorships");
        assert_eq!(b["metadata"]["external_id"], "ch_1");
        assert_eq!(b["title"], "Sponsorship: $5.00");
    }

    #[test]
    fn the_alarm_is_dedup_keyed_and_carries_the_reason_never_a_value() {
        let b = alarm_body(
            &sensor(),
            &Alarm::unreadable("env BOSS_BROKER_STRIPE_KEY is empty"),
            "emp-owner",
        );
        assert_eq!(b["owner_id"], "emp-owner", "the owner is the one handed in");
        assert_eq!(
            b["metadata"]["estate_finding"],
            "sensor_unreadable:stripe-sponsorships"
        );
        assert_eq!(b["metadata"]["area"], "estate");
        assert_eq!(b["priority"], "urgent");
        assert!(
            b["metadata"]["detail"]
                .as_str()
                .unwrap()
                .contains("BOSS_BROKER_STRIPE_KEY")
        );
        assert!(b["title"].as_str().unwrap().contains("stripe-sponsorships"));
    }

    #[test]
    fn the_open_alarm_is_found_by_key_and_a_truncated_page_holds() {
        let listing = json!({
            "data": [
                {"id": "other", "metadata": {"estate_finding": "dns_drift:x"}},
                {"id": "alarm-1", "metadata": {"estate_finding": "sensor_unreadable:stripe-sponsorships", "reason": "401"}}
            ],
            "total": 2
        });
        assert_eq!(
            open_alarm(&listing, "sensor_unreadable:stripe-sponsorships").unwrap(),
            Some(OpenAlarm {
                id: "alarm-1".into(),
                reason: "401".into(),
                told: false,
                row: listing["data"][1].clone(),
            })
        );
        assert_eq!(
            open_alarm(&listing, "sensor_unreadable:other").unwrap(),
            None
        );
        let truncated = json!({"data": [], "total": 5});
        assert!(
            open_alarm(&truncated, "k")
                .unwrap_err()
                .contains("truncated")
        );
    }

    /// Backlog 445c1494. The dedup read's truncation check caught a
    /// missing `data` only by accident, named it "truncated", and let
    /// `{"total": 0}` through as a completed read that found nothing —
    /// so the handler would file an alarm that may already be open.
    /// A missing, null or non-array `data` is NO ANSWER and refuses by
    /// that name; an empty array with total 0 is honest.
    #[test]
    fn the_dedup_read_refuses_a_listing_with_no_data_array_rather_than_twin_the_alarm() {
        assert_eq!(
            open_alarm(&json!({"data": [], "total": 0}), "k").unwrap(),
            None
        );
        for no_answer in [
            json!({"total": 0}),
            json!({"data": null, "total": 0}),
            json!({"data": {}, "total": 0}),
            json!({"error": "forbidden"}),
        ] {
            let why = open_alarm(&no_answer, "k")
                .expect_err("a listing with no rows array must hold the alarm, not twin it");
            assert!(why.contains("no `data` array"), "{no_answer}: {why}");
            assert!(!why.contains("truncated"), "{no_answer}: {why}");
        }
    }

    /// Backlog 6c4c432a. A listing with NO `data` array is no answer —
    /// an error shape, a changed contract, a policy-narrowed read — and
    /// it must refuse, not poll nothing and report healthy. An EMPTY
    /// array is honest: a deployment may legitimately declare no
    /// sensors, so it keeps working. Same distinction, same words, as
    /// `retro_open::departments` (80a77466).
    #[test]
    fn a_listing_with_no_data_array_is_a_refusal_and_an_empty_one_is_honest() {
        assert_eq!(
            sensors(&json!({ "data": [], "total": 0 })).expect("empty is honest"),
            vec![]
        );
        let why = sensors(&json!({ "total": 0 })).expect_err("no data array is a refusal");
        assert!(why.contains("no `data` array"), "{why}");
        let why = sensors(&json!({ "error": "forbidden" })).expect_err("an error shape refuses");
        assert!(why.contains("no `data` array"), "{why}");
        let why = sensors(&json!({ "data": [{ "id": 7 }] })).expect_err("a bad row refuses");
        assert!(why.contains("not in shape"), "{why}");
    }

    /// A missing `data` is NO ANSWER, not "nothing owes a packet"
    /// (0767c830). The distinction is the whole point: an EMPTY array
    /// is honest — a sensor may genuinely owe nothing — while a
    /// missing, null or non-array one means the far side did not
    /// answer the question, and reading it as zero leaves recorded
    /// readings that silently never become packets.
    #[test]
    fn owed_readings_refuses_a_listing_with_no_data_array_and_allows_an_empty_one() {
        assert_eq!(
            owed_readings(&json!({"data": [], "total": 0})).unwrap(),
            vec![]
        );
        let one = json!({"data": [{
            "sensor_id": "stripe-sponsorships",
            "external_id": "ch_1",
            "observed_at": "2026-09-17T10:00:00Z",
            "payload": {"id": "ch_1"},
            "packet_id": null
        }], "total": 1});
        assert_eq!(owed_readings(&one).unwrap().len(), 1);
        for no_answer in [
            json!({"total": 0}),
            json!({"data": null}),
            json!({"data": {}}),
        ] {
            assert!(
                owed_readings(&no_answer)
                    .unwrap_err()
                    .contains("no `data` array"),
                "a listing {no_answer} must refuse rather than read as nothing owed"
            );
        }
    }

    #[test]
    fn credential_values_name_the_env_var_when_unset_and_the_id_when_undeclared() {
        let c = CredentialValues::new()
            .with("stripe-restricted-read", "BOSS_BROKER_STRIPE_KEY", None)
            .with("other", "BOSS_OTHER", Some("secret-value".into()));
        let why = c.value("stripe-restricted-read").unwrap_err();
        assert!(why.contains("BOSS_BROKER_STRIPE_KEY"), "{why}");
        assert!(!why.contains("secret-value"));
        assert!(c.value("nobody").unwrap_err().contains("nobody"));
        assert_eq!(c.value("other").unwrap(), "secret-value");
        // Whitespace is unset.
        let c = CredentialValues::new().with("x", "X", Some("  ".into()));
        assert!(c.value("x").unwrap_err().contains("X is empty"));
    }

    // ----- the stub jobs API: sensors door + jobs + alarms, in memory -----

    type Captured = Arc<Mutex<Vec<(String, Json)>>>;

    struct Stub {
        base: String,
        captured: Captured,
        sensors: Arc<boss_jobs::sensors::InMemorySensors>,
        /// Open backlog-items, as the listing answers them.
        alarms: Arc<Mutex<Vec<Json>>>,
        /// When set, every packet open (a POST /api/jobs that is not
        /// a backlog-item) is answered with this status and body.
        refuse_opens: Arc<Mutex<Option<(u16, String)>>>,
        refuse_readings: Arc<Mutex<bool>>,
        split_readings_refusal: Arc<Mutex<Option<String>>>,
        refuse_poll_mark: Arc<Mutex<bool>>,
        unreadable_reading_ack: Arc<Mutex<bool>>,
    }

    impl Stub {
        fn packets(&self) -> Vec<Json> {
            self.captured
                .lock()
                .unwrap()
                .iter()
                .filter(|(k, _)| k == "POST /api/jobs")
                .map(|(_, b)| b.clone())
                .collect()
        }
        fn writes(&self, prefix: &str) -> Vec<(String, Json)> {
            self.captured
                .lock()
                .unwrap()
                .iter()
                .filter(|(k, _)| k.starts_with(prefix))
                .cloned()
                .collect()
        }
    }

    async fn stub_jobs_api(open_alarms: Vec<Json>) -> Stub {
        use axum::extract::{Path, Query};
        use axum::response::IntoResponse;
        use axum::{Json as AxJson, Router, routing::get, routing::post};
        use std::collections::HashMap as Map;

        let captured: Captured = Default::default();
        let sensors = Arc::new(boss_jobs::sensors::InMemorySensors::new());
        let alarms = Arc::new(Mutex::new(open_alarms));
        let refuse_opens: Arc<Mutex<Option<(u16, String)>>> = Default::default();
        let refuse = refuse_opens.clone();
        let refuse_readings = Arc::new(Mutex::new(false));
        let reading_refusal = refuse_readings.clone();
        let split_readings_refusal = Arc::new(Mutex::new(None::<String>));
        let split_refusal = split_readings_refusal.clone();
        let reading_calls = captured.clone();
        let refuse_poll_mark = Arc::new(Mutex::new(false));
        let poll_refusal = refuse_poll_mark.clone();
        let unreadable_reading_ack = Arc::new(Mutex::new(false));
        let unreadable_ack = unreadable_reading_ack.clone();
        let (c1, c2, c3, c4) = (
            captured.clone(),
            captured.clone(),
            captured.clone(),
            captured.clone(),
        );
        let (a1, a2, a3, a4) = (
            alarms.clone(),
            alarms.clone(),
            alarms.clone(),
            alarms.clone(),
        );
        let app = Router::new()
            .merge(boss_jobs::sensors::http::router(
                boss_jobs::sensors::http::SensorsApiState {
                    repo: sensors.clone() as Arc<dyn boss_jobs::sensors::Sensors>,
                },
            ))
            .route(
                "/api/jobs",
                get(move |Query(q): Query<Map<String, String>>| {
                    let alarms = a1.clone();
                    async move {
                        assert_eq!(q.get("kind").map(String::as_str), Some("backlog-item"));
                        assert_eq!(q.get("status").map(String::as_str), Some("open"));
                        // Answered as the jobs API answers it, behind more
                        // unrelated open items than one page (c5ac71de).
                        let rows = alarms.lock().unwrap().clone();
                        AxJson(crate::handlers::listing_stub::backlog_listing(
                            &rows,
                            q.get("metadata_has").map(String::as_str),
                            q.get("limit").and_then(|l| l.parse().ok()),
                        ))
                    }
                })
                .post(
                    move |headers: axum::http::HeaderMap, AxJson(body): AxJson<Json>| {
                        let c = c1.clone();
                        let alarms = a2.clone();
                        let refuse = refuse.clone();
                        async move {
                            let actor: Json = serde_json::from_str(
                                headers.get("x-boss-user").unwrap().to_str().unwrap(),
                            )
                            .unwrap();
                            let mut b = body.clone();
                            b["_actor"] = actor["id"].clone();
                            let refusal = refuse.lock().unwrap().clone();
                            if let Some((status, text)) = refusal
                                && body["kind"] != "backlog-item"
                            {
                                c.lock()
                                    .unwrap()
                                    .push(("POST /api/jobs (refused)".into(), b));
                                return (axum::http::StatusCode::from_u16(status).unwrap(), text)
                                    .into_response();
                            }
                            c.lock().unwrap().push(("POST /api/jobs".into(), b));
                            // Reading captures are independent evidence, not
                            // job-create attempts. Keep fixture job identity
                            // stable when another HTTP endpoint is observed.
                            let n = c.lock().unwrap().iter().filter(|(verb,_)|
                                verb=="POST /api/jobs" || verb=="POST /api/jobs (refused)").count();
                            let id = format!("job-{n}");
                            if body["kind"] == "backlog-item" {
                                let mut row = body.clone();
                                row["id"] = json!(id);
                                row["steps"] = json!([
                                    {"id": format!("{id}-triage"), "spec_slug": "triage", "status": "ready",
                                     "metadata": {"authority_role": "platform-admin"}}
                                ]);
                                alarms.lock().unwrap().push(row);
                            }
                            AxJson(json!({ "id": id })).into_response()
                        }
                    },
                ),
            )
            .route(
                "/api/jobs/{id}",
                get(move |Path(id): Path<String>| {
                    let alarms = a3.clone();
                    async move {
                        let row = alarms
                            .lock()
                            .unwrap()
                            .iter()
                            .find(|r| r["id"] == id)
                            .cloned()
                            .unwrap_or(json!({"id": id, "steps": []}));
                        AxJson(row)
                    }
                }),
            )
            .route(
                "/api/jobs/{id}/metadata",
                axum::routing::patch(move |Path(id): Path<String>, AxJson(body): AxJson<Json>| {
                    let c = c2.clone();
                    async move {
                        c.lock()
                            .unwrap()
                            .push((format!("PATCH /api/jobs/{id}/metadata"), body));
                        AxJson(json!({ "ok": true }))
                    }
                }),
            )
            // The step merge door (e39a9d2a), recorded in order with
            // the PUTs.
            .route(
                "/api/jobs/{id}/steps/{sid}/metadata",
                axum::routing::patch(
                    move |Path((id, sid)): Path<(String, String)>, AxJson(body): AxJson<Json>| {
                        let c = c4.clone();
                        async move {
                            c.lock().unwrap().push((
                                format!("PATCH /api/jobs/{id}/steps/{sid}/metadata"),
                                body,
                            ));
                            AxJson(json!({ "ok": true }))
                        }
                    },
                ),
            )
            .route(
                "/api/jobs/{id}/steps/{sid}",
                axum::routing::put(
                    move |Path((id, sid)): Path<(String, String)>, AxJson(body): AxJson<Json>| {
                        let c = c3.clone();
                        let alarms = a4.clone();
                        async move {
                            // A write to a terminal step is refused, as
                            // the real step API refuses it — the 409 a
                            // routed alarm's triage answered on every
                            // poll (backlog 6072ff60).
                            let terminal = alarms
                                .lock()
                                .unwrap()
                                .iter()
                                .filter(|r| r["id"] == id)
                                .flat_map(|r| r["steps"].as_array().cloned().unwrap_or_default())
                                .any(|s| {
                                    s["id"] == sid
                                        && matches!(
                                            s["status"].as_str(),
                                            Some("completed") | Some("skipped")
                                        )
                                });
                            if terminal {
                                c.lock()
                                    .unwrap()
                                    .push((format!("PUT /api/jobs/{id}/steps/{sid} (409)"), body));
                                return (
                                    axum::http::StatusCode::CONFLICT,
                                    "step is completed; annotate via PATCH /api/jobs/{id}/metadata",
                                )
                                    .into_response();
                            }
                            // The decided end state (e39a9d2a).
                            if let Some(refused) =
                                super::super::listing_stub::end_state_step_put(&id, &sid, &body)
                            {
                                c.lock()
                                    .unwrap()
                                    .push((format!("PUT /api/jobs/{id}/steps/{sid} (409)"), body));
                                return refused;
                            }
                            // A completed step closes the packet, so it
                            // leaves the open listing — as the real
                            // API's terminal does.
                            if body["status"] == "completed" {
                                alarms.lock().unwrap().retain(|r| r["id"] != id);
                            }
                            c.lock()
                                .unwrap()
                                .push((format!("PUT /api/jobs/{id}/steps/{sid}"), body));
                            AxJson(json!({ "ok": true })).into_response()
                        }
                    },
                ),
            )
            .route("/health", post(|| async { "" }))
            .layer(axum::middleware::from_fn(move |request: axum::extract::Request, next: axum::middleware::Next| {
                let refuse = reading_refusal.clone();
                let calls = reading_calls.clone();
                let split=split_refusal.clone();
                let poll=poll_refusal.clone();
                let unreadable_ack=unreadable_ack.clone();
                async move {
                    if request.method() == axum::http::Method::POST
                        && request.uri().path().ends_with("/polled") && *poll.lock().unwrap()
                    {
                        calls.lock().unwrap().push((format!("POST {} (400)",request.uri().path()),json!({})));
                        return (axum::http::StatusCode::BAD_REQUEST,"invalid poll refusal witness").into_response();
                    }
                    if request.method() == axum::http::Method::POST
                        && request.uri().path().ends_with("/readings")
                    {
                        let path = request.uri().path().to_string();
                        let (parts, input)=request.into_parts();
                        let bytes = axum::body::to_bytes(input, 2 * 1024 * 1024).await.unwrap();
                        let body: Json = serde_json::from_slice(&bytes).unwrap();
                        let rejected=*refuse.lock().unwrap() || split.lock().unwrap().as_ref().is_some_and(|id|body.as_array().unwrap().len()>1 || body[0]["external_id"].as_str()==Some(id.as_str()));
                        if rejected {
                            calls.lock().unwrap().push((format!("POST {path} (refused)"), body));
                            return (axum::http::StatusCode::PAYLOAD_TOO_LARGE, "readings body exceeds limit").into_response();
                        }
                        calls.lock().unwrap().push((format!("POST {path}"),body));
                        let response=next.run(axum::extract::Request::from_parts(parts,axum::body::Body::from(bytes))).await;
                        return if *unreadable_ack.lock().unwrap() {AxJson(json!({})).into_response()} else {response};
                    }
                    next.run(request).await
                }
            }));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        Stub {
            base: format!("http://{addr}"),
            captured,
            sensors,
            alarms,
            refuse_opens,
            refuse_readings,
            split_readings_refusal,
            refuse_poll_mark,
            unreadable_reading_ack,
        }
    }

    fn ctx() -> InvocationContext {
        InvocationContext {
            event_timestamp: None,
            rule_name: "sensors-poll-every-5-minutes".into(),
            triggering_event_id: "clock-tick:2026-09-17T10:05:00+00:00".into(),
            triggering_topic: "clock.tick".into(),
            event_payload: json!({"_day": "2026-09-17", "_at": "2026-09-17T10:05:00+00:00"}),
        }
    }

    #[tokio::test]
    async fn review_read_context_preserves_real_reading_packet_and_poll_progress_once() {
        struct BoundedSource {
            observations: Vec<Observation>,
            contexts: std::sync::Mutex<Vec<Option<SourceReadContext>>>,
        }
        impl BoundedSource {
            fn select(
                &self,
                since: Option<DateTime<Utc>>,
                context: Option<&SourceReadContext>,
            ) -> Vec<Observation> {
                let mut used = 2;
                let mut output = Vec::new();
                for observation in &self.observations {
                    if since.is_some_and(|cut| observation.observed_at < cut)
                        || context.is_some_and(|c| {
                            c.witnessed.iter().any(|w| {
                                w.message_key == observation.external_id
                                    && w.at == observation.observed_at
                            })
                        })
                    {
                        continue;
                    }
                    let bytes = serde_json::to_vec(&json!({"external_id":observation.external_id,
                        "observed_at":observation.observed_at,"payload":observation.payload}))
                    .unwrap()
                    .len();
                    if used + bytes + 1 > 1536 * 1024 {
                        break;
                    }
                    used += bytes + 1;
                    output.push(observation.clone());
                }
                output
            }
        }
        #[async_trait]
        impl SensorSource for BoundedSource {
            fn supports_read_context(&self) -> bool {
                true
            }
            async fn read(
                &self,
                _credential: &str,
                since: Option<DateTime<Utc>>,
            ) -> Result<Vec<Observation>, SourceError> {
                self.contexts.lock().unwrap().push(None);
                Ok(self.select(since, None))
            }
            async fn read_with_context(
                &self,
                _credential: &str,
                since: Option<DateTime<Utc>>,
                context: Option<&SourceReadContext>,
            ) -> Result<Vec<Observation>, SourceError> {
                self.contexts.lock().unwrap().push(context.cloned());
                Ok(self.select(since, context))
            }
            fn describe(&self, payload: &Json) -> Described {
                Described {
                    title: payload["id"].as_str().unwrap().into(),
                    metadata: json!({"amount_cents":1}),
                }
            }
        }
        for general in [false, true] {
            let stub = stub_jobs_api(vec![]).await;
            declare(&stub).await;
            let mut observations = (0..3)
                .map(|i| {
                    let mut value = obs(&format!("message-{i}"), "2026-09-17T09:00:00Z", 1);
                    value.payload["body"] =
                        json!("p".repeat(if general { 500 * 1024 } else { 600 * 1024 }));
                    value
                })
                .collect::<Vec<_>>();
            if general {
                let mut later = obs("later", "2026-09-17T09:00:01Z", 1);
                later.payload["body"] = json!("p".repeat(500 * 1024));
                observations.push(later);
            }
            let expected = observations.len();
            let source = Arc::new(BoundedSource {
                observations,
                contexts: Default::default(),
            });
            let h = SensorPoll::new(
                &stub.base,
                HashMap::from([("stripe".into(), source.clone() as Arc<dyn SensorSource>)]),
                CredentialValues::new().with(
                    "stripe-restricted-read",
                    "SYNTHETIC",
                    Some("fixture".into()),
                ),
                Arc::new(boss_core::platform_owner::Fixed("emp-owner".into())),
            );
            for instant in [
                "2026-09-17T10:05:00Z",
                "2026-09-17T10:20:00Z",
                "2026-09-17T10:35:00Z",
            ] {
                h.invoke(&[], &ctx_at(instant)).await.unwrap();
            }
            assert_eq!(
                stub.sensors.readings().await.len(),
                expected,
                "general={general}: no valid suffix stranded"
            );
            assert_eq!(
                stub.packets().len(),
                expected,
                "general={general}: each packet opens once"
            );
            let posted: usize = stub
                .writes("POST /api/sensors/stripe-sponsorships/readings")
                .iter()
                .map(|(_, b)| b.as_array().map_or(0, Vec::len))
                .sum();
            assert_eq!(
                posted, expected,
                "general={general}: successful source messages must not be reposted"
            );
            assert!(
                source
                    .contexts
                    .lock()
                    .unwrap()
                    .iter()
                    .skip(1)
                    .any(|c| c.as_ref().is_some_and(|c| !c.witnessed.is_empty()))
            );
        }
        for status in [400, 422, 503] {
            let stub = stub_jobs_api(vec![]).await;
            declare(&stub).await;
            let source = Arc::new(BoundedSource {
                observations: vec![obs("owed", "2026-09-17T09:00:00Z", 1)],
                contexts: Default::default(),
            });
            let h = SensorPoll::new(
                &stub.base,
                HashMap::from([("stripe".into(), source as Arc<dyn SensorSource>)]),
                CredentialValues::new().with(
                    "stripe-restricted-read",
                    "SYNTHETIC",
                    Some("fixture".into()),
                ),
                Arc::new(boss_core::platform_owner::Fixed("emp-owner".into())),
            );
            *stub.refuse_opens.lock().unwrap() =
                Some((status, "recoverable packet admission fixture".into()));
            let first = h.invoke(&[], &ctx()).await;
            assert_eq!(
                first.is_err(),
                status == 503,
                "status={status}: existing packet error classification"
            );
            let row = stub.sensors.list().await.unwrap().remove(0);
            assert_eq!(row.cursor_at, Some(at("2026-09-17T09:00:00Z")));
            assert_eq!(row.refused.unwrap().processed[0].message_key, "owed");
            let readings = stub.sensors.readings().await;
            assert_eq!(readings.len(), 1);
            assert!(
                readings[0].packet_id.is_none(),
                "recorded reading remains durably owed"
            );
            *stub.refuse_opens.lock().unwrap() = None;
            h.invoke(&[], &ctx_at("2026-09-17T10:20:00Z"))
                .await
                .unwrap();
            h.invoke(&[], &ctx_at("2026-09-17T10:35:00Z"))
                .await
                .unwrap();
            assert_eq!(
                stub.packets()
                    .iter()
                    .filter(|p| p["kind"] == "receive-a-sponsorship")
                    .count(),
                1,
                "status={status}: original owed packet opens once after recovery; alarm is separate"
            );
            assert_eq!(
                stub.writes("POST /api/sensors/stripe-sponsorships/readings")
                    .len(),
                1,
                "status={status}: ACKed reading not resent"
            );
        }
    }

    #[tokio::test]
    async fn review_source_latch_keeps_processed_keys_and_names_unknown_union() {
        let stub = stub_jobs_api(vec![]).await;
        declare(&stub).await;
        let at = at("2026-09-17T09:00:00Z");
        let mut processed = (0..32)
            .map(|i| boss_jobs::sensors::CursorWitness {
                message_key: format!("done-{i}"),
                at,
            })
            .collect::<Vec<_>>();
        processed.sort();
        let witness = PollRefusals {
            processed: Vec::new(),
            entries: (0..64)
                .map(|i| RefusedMessage {
                    message_key: format!("pending-{i}"),
                    at,
                    size_bytes: 50,
                    limit: boss_jobs::sensors::RefusalLimit::Count {
                        value: 64,
                        observed: 65,
                    },
                    reason: "cursor_witness_bound".into(),
                })
                .collect(),
            truncated: 1,
            classes: None,
            cleared: None,
        };
        let source = InMemorySource::failing(SourceError::RefusedPoll(witness));
        let h = handler(&stub, source.clone(), Some("synthetic-read-only-key"));
        h.mark_refused(
            &sensor(),
            at,
            Some(at),
            PollRefusals {
                processed: processed.clone(),
                entries: vec![],
                truncated: 0,
                classes: None,
                cleared: None,
            },
        )
        .await
        .unwrap();
        h.invoke(&[], &ctx()).await.unwrap();
        let row = stub.sensors.list().await.unwrap().remove(0);
        let held = row.refused.unwrap();
        assert_eq!(held.processed, processed);
        assert_eq!(held.entries.len() + held.processed.len(), 64);
        assert_eq!(held.classes.unwrap().union_count, None);
        assert!(
            stub.packets()[0]["metadata"]["reason"]
                .as_str()
                .unwrap()
                .contains("union unknown")
        );
        assert!(stub.sensors.readings().await.is_empty());
        source.set(Ok(vec![
            obs("done-0", "2026-09-17T09:00:00Z", 1),
            obs("new-valid", "2026-09-17T09:00:00Z", 1),
        ]));
        h.invoke(&[], &ctx_at("2026-09-17T10:20:00Z"))
            .await
            .unwrap();
        let held = stub
            .sensors
            .list()
            .await
            .unwrap()
            .remove(0)
            .refused
            .unwrap();
        assert_eq!(held.processed, processed);
        assert!(!held.is_latched());
        assert!(held.entries.is_empty());
        let rows = stub.sensors.readings().await;
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].external_id, "new-valid");
    }

    #[tokio::test]
    async fn review_context_never_mints_processed_witness_from_an_unreadable_recording_ack() {
        struct OneSource;
        #[async_trait]
        impl SensorSource for OneSource {
            fn supports_read_context(&self) -> bool {
                true
            }
            async fn read(
                &self,
                _: &str,
                _: Option<DateTime<Utc>>,
            ) -> Result<Vec<Observation>, SourceError> {
                Ok(vec![obs("one", "2026-09-17T09:00:00Z", 1)])
            }
            fn describe(&self, _: &Json) -> Described {
                Described {
                    title: "one".into(),
                    metadata: json!({"amount_cents":1}),
                }
            }
        }
        let stub = stub_jobs_api(vec![]).await;
        declare(&stub).await;
        *stub.unreadable_reading_ack.lock().unwrap() = true;
        let h = SensorPoll::new(
            &stub.base,
            HashMap::from([(
                "stripe".into(),
                Arc::new(OneSource) as Arc<dyn SensorSource>,
            )]),
            CredentialValues::new().with(
                "stripe-restricted-read",
                "SYNTHETIC",
                Some("fixture".into()),
            ),
            Arc::new(boss_core::platform_owner::Fixed("emp-owner".into())),
        );
        let error = h
            .invoke(&[], &ctx())
            .await
            .expect_err("HTTP200 without a BatchOutcome is not proof of processing");
        assert!(matches!(error, HandlerError::Downstream(_)), "{error}");
        let row = stub.sensors.list().await.unwrap().remove(0);
        assert_eq!(
            (row.last_polled_at, row.cursor_at, row.refused),
            (None, None, None)
        );
        assert_eq!(
            stub.sensors.readings().await.len(),
            1,
            "fixture effect exists but its acknowledgement is unknown to caller"
        );
        assert!(stub.packets().is_empty());
    }

    #[tokio::test]
    async fn review_small_source_byte_cohort_latches_without_processing_and_clears_on_remeasurement()
     {
        let stub = stub_jobs_api(vec![]).await;
        declare(&stub).await;
        let witness = PollRefusals {
            processed: Vec::new(),
            entries: (0..3)
                .map(|i| RefusedMessage {
                    message_key: format!("tied-{i}"),
                    size_bytes: 600 * 1024,
                    limit: boss_jobs::sensors::RefusalLimit::bytes(1536 * 1024, 1800 * 1024),
                    reason: "timestamp_tie_read_byte_bound".into(),
                    at: at("2026-09-17T09:00:00Z"),
                })
                .collect(),
            truncated: 0,
            cleared: None,
            classes: None,
        };
        let source = InMemorySource::failing(SourceError::RefusedPoll(witness));
        let h = handler(&stub, source.clone(), Some("synthetic-read-only-key"));
        h.invoke(&[], &ctx()).await.unwrap();
        let row = stub.sensors.list().await.unwrap().remove(0);
        assert!(
            row.refused.as_ref().unwrap().is_latched(),
            "known byte overflow is a latch even below64 keys"
        );
        assert_eq!(row.cursor_at, None);
        assert!(stub.sensors.readings().await.is_empty());
        source.set(Ok(vec![obs(
            "valid-after-remeasurement",
            "2026-09-17T09:00:00Z",
            1,
        )]));
        h.invoke(&[], &ctx_at("2026-09-17T10:20:00+00:00"))
            .await
            .unwrap();
        let row = stub.sensors.list().await.unwrap().remove(0);
        assert!(!row.refused.as_ref().unwrap().is_latched());
        assert!(row.refused.as_ref().unwrap().entries.is_empty());
        assert_eq!(stub.sensors.readings().await.len(), 1);
    }

    #[tokio::test]
    async fn review_historical_measurement_is_not_invented_or_reposted_as_a_new_witness() {
        let stub = stub_jobs_api(vec![]).await;
        let old=PollRefusals::from_stored_json(json!({"entries":[{"message_key":"old", "size_bytes":80,
            "limit_bytes":64,"reason":"readings_post_413:v1","at":"2026-09-17T09:00:00Z"}],"truncated":0})).unwrap();
        declare(&stub).await;
        let h = handler(
            &stub,
            InMemorySource::answering(vec![]),
            Some("synthetic-read-only-key"),
        );
        let mut row = sensor();
        row.refused = Some(old.clone());
        h.mark_refused(&row, at("2026-09-17T10:05:00Z"), None, old)
            .await
            .unwrap();
        // The backend retains old facts under its lock; this caller must not
        // invent or resubmit an absent observation as a new measured fact.
        assert_eq!(
            stub.sensors.list().await.unwrap()[0].refused,
            None,
            "the controlled empty backend must not receive a new invented historical witness"
        );
    }

    #[tokio::test]
    async fn review_poll_mark_400_is_named_permanent_with_the_existing_sensor_alarm() {
        let stub = stub_jobs_api(vec![]).await;
        declare(&stub).await;
        *stub.refuse_poll_mark.lock().unwrap() = true;
        let source = InMemorySource::answering(vec![]);
        let h = handler(&stub, source, Some("synthetic-read-only-key"));
        let error = h.invoke(&[], &ctx()).await.unwrap_err();
        assert!(matches!(error, HandlerError::Permanent(_)), "{error}");
        assert!(error.to_string().contains("/polled"), "{error}");
        assert!(error.to_string().contains("400"), "{error}");
        assert_eq!(
            stub.writes("POST /api/sensors/stripe-sponsorships/polled (400)")
                .len(),
            1
        );
        let alarms = stub.packets();
        assert_eq!(alarms.len(), 1);
        assert_eq!(
            alarms[0]["metadata"]["estate_finding"],
            "sensor_unreadable:stripe-sponsorships"
        );
        assert!(
            alarms[0]["metadata"]["reason"]
                .as_str()
                .unwrap()
                .contains("/polled")
        );
        assert_eq!(stub.sensors.list().await.unwrap()[0].last_polled_at, None);
    }

    fn ctx_at(s: &str) -> InvocationContext {
        InvocationContext {
            event_payload: json!({"_day": "2026-09-17", "_at": s}),
            ..ctx()
        }
    }

    async fn declare(stub: &Stub) {
        stub.sensors
            .publish(
                "acme",
                &[boss_jobs::sensors::SensorInput {
                    id: "stripe-sponsorships".into(),
                    source: "stripe".into(),
                    credential: "stripe-restricted-read".into(),
                    every_minutes: 15,
                    opens: "receive-a-sponsorship".into(),
                    subject_kind: "custom".into(),
                    enabled: true,
                    selector: None,
                }],
            )
            .await
            .unwrap();
    }

    fn handler(stub: &Stub, source: Arc<InMemorySource>, key: Option<&str>) -> Arc<SensorPoll> {
        let mut sources: HashMap<String, Arc<dyn SensorSource>> = HashMap::new();
        sources.insert("stripe".into(), source);
        SensorPoll::new(
            stub.base.clone(),
            sources,
            CredentialValues::new().with(
                "stripe-restricted-read",
                "BOSS_BROKER_STRIPE_KEY",
                key.map(str::to_string),
            ),
            Arc::new(boss_core::platform_owner::Fixed("emp-owner".into())),
        )
    }

    /// THE IDEMPOTENCE PIN. Two firings against the same source open
    /// ONE packet per reading, the reading is stamped with it, the
    /// cursor is the newest observation, and the packet is signed by
    /// the sensor.
    #[tokio::test]
    async fn a_poll_opens_one_packet_per_reading_and_a_second_poll_opens_none() {
        let stub = stub_jobs_api(vec![]).await;
        declare(&stub).await;
        let source = InMemorySource::answering(vec![
            obs("ch_1", "2026-09-17T09:00:00Z", 500),
            obs("ch_2", "2026-09-17T09:30:00Z", 2500),
        ]);
        let h = handler(&stub, source.clone(), Some("rk_test_x"));

        h.invoke(&[], &ctx()).await.unwrap();
        let packets = stub.packets();
        assert_eq!(packets.len(), 2, "{packets:#?}");
        assert_eq!(packets[0]["kind"], "receive-a-sponsorship");
        assert_eq!(packets[0]["subject"]["id"], "ch_1");
        assert_eq!(
            packets[0]["_actor"],
            "automation:sensor:stripe-sponsorships"
        );
        assert_eq!(packets[1]["subject"]["id"], "ch_2");
        let readings = stub.sensors.readings().await;
        assert_eq!(readings.len(), 2);
        assert!(
            readings.iter().all(|r| r.packet_id.is_some()),
            "{readings:?}"
        );
        let row = &stub.sensors.list().await.unwrap()[0];
        assert_eq!(row.cursor_at, Some(at("2026-09-17T09:30:00Z")));
        assert_eq!(row.last_polled_at, Some(at("2026-09-17T10:05:00Z")));
        // The source was asked from no cursor, with the key.
        let reads = source.reads.lock().unwrap().clone();
        assert_eq!(reads, vec![("rk_test_x".to_string(), None)]);

        // Second firing, 15 minutes on: due again, re-reads (from the
        // cursor, inclusive), opens nothing.
        h.invoke(&[], &ctx_at("2026-09-17T10:20:00+00:00"))
            .await
            .unwrap();
        assert_eq!(stub.packets().len(), 2, "a redelivery opens nothing twice");
        let reads = source.reads.lock().unwrap().clone();
        assert_eq!(reads[1].1, Some(at("2026-09-17T09:30:00Z")));
        assert_eq!(stub.sensors.readings().await.len(), 2);
        // No alarm was ever filed.
        assert!(stub.alarms.lock().unwrap().is_empty());
    }

    /// A sensor polled less than `every_minutes` ago is not read.
    #[tokio::test]
    async fn a_sensor_inside_its_period_is_not_read() {
        let stub = stub_jobs_api(vec![]).await;
        declare(&stub).await;
        let source = InMemorySource::answering(vec![obs("ch_1", "2026-09-17T09:00:00Z", 1)]);
        let h = handler(&stub, source.clone(), Some("k"));
        h.invoke(&[], &ctx()).await.unwrap();
        h.invoke(&[], &ctx_at("2026-09-17T10:10:00+00:00"))
            .await
            .unwrap();
        assert_eq!(
            source.reads.lock().unwrap().len(),
            1,
            "five minutes on, not due"
        );
        h.invoke(&[], &ctx_at("2026-09-17T10:20:00+00:00"))
            .await
            .unwrap();
        assert_eq!(
            source.reads.lock().unwrap().len(),
            2,
            "fifteen minutes on, due"
        );
    }

    /// A reading recorded but never opened (a firing that died between
    /// the record and the open) is opened by the next pass — and only
    /// once.
    #[tokio::test]
    async fn a_reading_recorded_without_a_packet_is_opened_by_the_next_pass() {
        let stub = stub_jobs_api(vec![]).await;
        declare(&stub).await;
        stub.sensors
            .record(
                "stripe-sponsorships",
                &[NewReading {
                    external_id: "ch_old".into(),
                    observed_at: at("2026-09-16T12:00:00Z"),
                    payload: json!({"id": "ch_old", "amount": 100}),
                }],
            )
            .await
            .unwrap();
        let source = InMemorySource::answering(vec![]);
        let h = handler(&stub, source, Some("k"));
        h.invoke(&[], &ctx()).await.unwrap();
        let packets = stub.packets();
        assert_eq!(packets.len(), 1);
        assert_eq!(packets[0]["subject"]["id"], "ch_old");
        assert_eq!(packets[0]["title"], "Reading ch_old");
        h.invoke(&[], &ctx_at("2026-09-17T10:20:00+00:00"))
            .await
            .unwrap();
        assert_eq!(stub.packets().len(), 1);
    }

    /// THE DEDUP READS ONLY ESTATE PACKETS (backlog c5ac71de). Unfiltered,
    /// the open-alarm read counted every open backlog-item, and past one
    /// page it was TRUNCATED: the raise held, so a sensor that stopped
    /// reading went unannounced. The stub answers as the jobs API does,
    /// behind more unrelated open items than the page holds; the new
    /// alarm must file, and the next pass must still find it and not
    /// twin it.
    #[tokio::test]
    async fn a_backlog_past_one_page_does_not_hold_a_new_sensor_alarm() {
        use crate::handlers::listing_stub::UNRELATED_BACKLOG;
        const {
            assert!(
                UNRELATED_BACKLOG > DEDUP_PAGE,
                "the fixture outgrows a page"
            )
        };
        let stub = stub_jobs_api(vec![]).await;
        declare(&stub).await;
        let source = InMemorySource::answering(vec![obs("ch_1", "2026-09-17T09:00:00Z", 1)]);
        let h = handler(&stub, source, None);

        h.invoke(&[], &ctx())
            .await
            .expect("a new finding must raise, not be held");
        let packets = stub.packets();
        assert_eq!(packets.len(), 1, "{packets:#?}");
        assert_eq!(
            packets[0]["metadata"]["estate_finding"],
            "sensor_unreadable:stripe-sponsorships"
        );
        h.invoke(&[], &ctx_at("2026-09-17T10:20:00+00:00"))
            .await
            .expect("the standing alarm is found, not held");
        assert_eq!(
            stub.packets().len(),
            1,
            "found past the page, never twinned"
        );
    }

    /// An unset key files the alarm NAMING the env var, marks the poll
    /// (so the next attempt waits the period), opens nothing; the same
    /// reason on the next pass leaves the alarm as it is; a changed
    /// reason refreshes it; a good read closes it through triage.
    #[tokio::test]
    async fn an_unreadable_credential_alarms_by_name_and_the_next_good_read_recovers() {
        let stub = stub_jobs_api(vec![]).await;
        declare(&stub).await;
        let source = InMemorySource::answering(vec![obs("ch_1", "2026-09-17T09:00:00Z", 1)]);
        let h = handler(&stub, source.clone(), None);

        h.invoke(&[], &ctx()).await.unwrap();
        let packets = stub.packets();
        assert_eq!(packets.len(), 1, "{packets:#?}");
        assert_eq!(packets[0]["kind"], "backlog-item");
        assert_eq!(
            packets[0]["metadata"]["estate_finding"],
            "sensor_unreadable:stripe-sponsorships"
        );
        assert!(
            packets[0]["metadata"]["reason"]
                .as_str()
                .unwrap()
                .contains("BOSS_BROKER_STRIPE_KEY")
        );
        assert!(
            source.reads.lock().unwrap().is_empty(),
            "no read without a key"
        );
        let row = &stub.sensors.list().await.unwrap()[0];
        assert_eq!(row.last_polled_at, Some(at("2026-09-17T10:05:00Z")));
        assert_eq!(row.cursor_at, None);

        // Same reason, next period: held, not twinned, not patched.
        h.invoke(&[], &ctx_at("2026-09-17T10:20:00+00:00"))
            .await
            .unwrap();
        assert_eq!(stub.packets().len(), 1);
        assert!(stub.writes("PATCH").is_empty());

        // A different unreadable reason (the source now answers 401):
        // the open alarm is refreshed, never twinned.
        let h2 = handler(&stub, source.clone(), Some("rk_bad"));
        source.set(Err(SourceError::Unreadable("stripe answered 401".into())));
        h2.invoke(&[], &ctx_at("2026-09-17T10:35:00+00:00"))
            .await
            .unwrap();
        assert_eq!(stub.packets().len(), 1);
        let patches = stub.writes("PATCH");
        assert_eq!(patches.len(), 1, "{patches:?}");
        assert_eq!(patches[0].1["reason"], "stripe answered 401");
        // The stub listing does not merge the patch, so the next pass
        // with the SAME reason would refresh again; it is the reason
        // comparison that is under test, not the stub's memory.

        // A good read: the reading lands, the packet opens, the alarm
        // is closed through its triage step with disposition stale.
        source.set(Ok(vec![obs("ch_1", "2026-09-17T09:00:00Z", 1)]));
        h2.invoke(&[], &ctx_at("2026-09-17T10:50:00+00:00"))
            .await
            .unwrap();
        let packets = stub.packets();
        assert_eq!(packets.len(), 2);
        assert_eq!(packets[1]["kind"], "receive-a-sponsorship");
        let puts = stub.writes("PUT /api/jobs/job-1/steps/job-1-triage");
        assert_eq!(puts.len(), 1, "{:?}", stub.writes("PUT"));
        assert_eq!(puts[0].1, json!({"status": "completed"}), "the flip alone");
        let merged = stub.writes("PATCH /api/jobs/job-1/steps/job-1-triage/metadata");
        assert_eq!(merged.len(), 1, "{:?}", stub.writes("PATCH"));
        assert_eq!(merged[0].1["disposition"], "stale");
        assert!(
            merged[0].1.get("authority_role").is_none(),
            "the step's own keys are not re-sent: the merge door keeps them"
        );
        assert_eq!(merged[0].1["cleared_by"], "sensor.poll");
    }

    // ----- the unopenable path (backlog f50a9ec1) -----

    #[test]
    fn the_two_conditions_are_two_findings_on_one_sensor() {
        assert_eq!(
            Condition::Unreadable.key("stripe-sponsorships"),
            "sensor_unreadable:stripe-sponsorships"
        );
        assert_eq!(
            Condition::Unopenable.key("stripe-sponsorships"),
            "sensor_unopenable:stripe-sponsorships"
        );
    }

    #[test]
    fn the_unopenable_alarm_carries_the_refusal_verbatim_and_the_readings_id() {
        let alarm = Alarm::unopenable(
            "ch_1",
            400,
            "unknown or inactive job kind: receive-a-sponsorship",
        );
        let b = alarm_body(&sensor(), &alarm, "emp-owner");
        assert_eq!(
            b["metadata"]["estate_finding"],
            "sensor_unopenable:stripe-sponsorships"
        );
        assert_eq!(b["metadata"]["external_id"], "ch_1");
        assert_eq!(b["metadata"]["opens_kind"], "receive-a-sponsorship");
        assert_eq!(b["priority"], "urgent");
        let reason = b["metadata"]["reason"].as_str().unwrap();
        assert!(reason.contains("400"), "{reason}");
        assert!(
            reason.contains("unknown or inactive job kind: receive-a-sponsorship"),
            "{reason}"
        );
        assert!(reason.contains("ch_1"), "{reason}");
        assert!(
            b["title"].as_str().unwrap().contains("cannot open"),
            "{}",
            b["title"]
        );
        assert!(
            b["metadata"]["detail"]
                .as_str()
                .unwrap()
                .contains("receive-a-sponsorship"),
            "{}",
            b["metadata"]["detail"]
        );
        // The unreadable body still reads as it did.
        let b = alarm_body(&sensor(), &Alarm::unreadable("env X is empty"), "emp-owner");
        assert_eq!(
            b["metadata"]["estate_finding"],
            "sensor_unreadable:stripe-sponsorships"
        );
        assert_eq!(b["metadata"]["reason"], "env X is empty");
        assert!(b["metadata"].get("external_id").is_none());
    }

    /// A refusal body is carried verbatim up to a bound; past it the
    /// reason says how much was cut, so a packet never carries a
    /// megabyte of HTML error page.
    #[test]
    fn the_refusal_is_bounded_to_a_few_hundred_chars_and_says_so() {
        let short = Alarm::unopenable("ch_1", 422, "bad metadata");
        assert!(short.reason.ends_with("bad metadata"), "{}", short.reason);
        let long_body = "x".repeat(5000);
        let long = Alarm::unopenable("ch_1", 422, &long_body);
        assert!(
            long.reason.chars().count() < REFUSAL_BOUND + 120,
            "{}",
            long.reason.len()
        );
        assert!(long.reason.contains("5000"), "{}", long.reason);
        assert!(long.reason.contains("422"), "{}", long.reason);
    }

    /// Which answers to a packet open are a standing refusal (alarmed,
    /// retried at the sensor's period) and which are weather (NAKed).
    #[test]
    fn a_refusal_is_a_4xx_that_is_not_a_conflict_a_miss_or_a_throttle() {
        for s in [400u16, 401, 403, 422] {
            assert!(is_refusal(reqwest::StatusCode::from_u16(s).unwrap()), "{s}");
        }
        for s in [404u16, 408, 409, 429, 500, 502, 503] {
            assert!(
                !is_refusal(reqwest::StatusCode::from_u16(s).unwrap()),
                "{s}"
            );
        }
    }

    /// The source reads, the reading is recorded, but the jobs API
    /// refuses the packet (the declared kind is not published): the
    /// alarm `sensor_unopenable:<id>` is filed carrying the refusal
    /// and the reading's id, the reading stays unstamped, the poll is
    /// marked (the next attempt waits the period, and the cursor
    /// advances past what was recorded). The same refusal next period
    /// holds the packet; a changed refusal refreshes it; the first
    /// open that succeeds stamps the reading and closes the alarm
    /// through triage.
    #[tokio::test]
    async fn a_refused_packet_open_alarms_with_the_refusal_and_the_next_good_open_recovers() {
        let stub = stub_jobs_api(vec![]).await;
        declare(&stub).await;
        let source = InMemorySource::answering(vec![obs("ch_1", "2026-09-17T09:00:00Z", 500)]);
        let h = handler(&stub, source.clone(), Some("rk_test_x"));
        *stub.refuse_opens.lock().unwrap() = Some((
            400,
            "unknown or inactive job kind: receive-a-sponsorship".into(),
        ));

        h.invoke(&[], &ctx()).await.unwrap();
        let packets = stub.packets();
        assert_eq!(packets.len(), 1, "{packets:#?}");
        assert_eq!(packets[0]["kind"], "backlog-item");
        assert_eq!(
            packets[0]["metadata"]["estate_finding"],
            "sensor_unopenable:stripe-sponsorships"
        );
        assert_eq!(packets[0]["metadata"]["external_id"], "ch_1");
        let reason = packets[0]["metadata"]["reason"]
            .as_str()
            .unwrap()
            .to_string();
        assert!(reason.contains("400"), "{reason}");
        assert!(reason.contains("unknown or inactive job kind"), "{reason}");
        assert_eq!(
            packets[0]["_actor"],
            "automation:sensor:stripe-sponsorships"
        );
        assert_eq!(stub.writes("POST /api/jobs (refused)").len(), 1);
        let readings = stub.sensors.readings().await;
        assert_eq!(readings.len(), 1, "the reading was recorded");
        assert_eq!(readings[0].packet_id, None, "and stays unstamped");
        let row = &stub.sensors.list().await.unwrap()[0];
        assert_eq!(row.last_polled_at, Some(at("2026-09-17T10:05:00Z")));
        assert_eq!(row.cursor_at, Some(at("2026-09-17T09:00:00Z")));

        // Same refusal, next period: the open is attempted again (the
        // reading is still owed), refused again, the alarm held.
        h.invoke(&[], &ctx_at("2026-09-17T10:20:00+00:00"))
            .await
            .unwrap();
        assert_eq!(stub.packets().len(), 1, "not twinned");
        assert_eq!(stub.writes("POST /api/jobs (refused)").len(), 2);
        assert!(stub.writes("PATCH").is_empty(), "not patched");

        // A different refusal (the kind is now published but the
        // metadata is refused): refreshed, not twinned.
        *stub.refuse_opens.lock().unwrap() =
            Some((422, "metadata: amount_cents must be an integer".into()));
        h.invoke(&[], &ctx_at("2026-09-17T10:35:00+00:00"))
            .await
            .unwrap();
        assert_eq!(stub.packets().len(), 1);
        let patches = stub.writes("PATCH");
        assert_eq!(patches.len(), 1, "{patches:?}");
        let reason = patches[0].1["reason"].as_str().unwrap();
        assert!(reason.contains("422"), "{reason}");
        assert!(reason.contains("amount_cents"), "{reason}");
        // The reading's id is unchanged, so the refresh does not re-send
        // it: `alarm_item::plan` writes only what moved (f4b0ad41).
        assert!(patches[0].1.get("external_id").is_none(), "{patches:?}");
        assert_eq!(stub.packets()[0]["metadata"]["external_id"], "ch_1");

        // The open succeeds: the packet opens, the reading is stamped,
        // the alarm is closed through its triage step.
        *stub.refuse_opens.lock().unwrap() = None;
        h.invoke(&[], &ctx_at("2026-09-17T10:50:00+00:00"))
            .await
            .unwrap();
        let packets = stub.packets();
        assert_eq!(packets.len(), 2, "{packets:#?}");
        assert_eq!(packets[1]["kind"], "receive-a-sponsorship");
        assert_eq!(packets[1]["subject"]["id"], "ch_1");
        let readings = stub.sensors.readings().await;
        assert!(readings[0].packet_id.is_some(), "{readings:?}");
        // job-1 was the refused open; the alarm is job-2.
        let puts = stub.writes("PUT /api/jobs/job-2/steps/job-2-triage");
        assert_eq!(puts.len(), 1, "{:?}", stub.writes("PUT"));
        assert_eq!(puts[0].1, json!({"status": "completed"}), "the flip alone");
        let merged = stub.writes("PATCH /api/jobs/job-2/steps/job-2-triage/metadata");
        assert_eq!(merged.len(), 1, "{:?}", stub.writes("PATCH"));
        assert_eq!(merged[0].1["disposition"], "stale");
        assert_eq!(merged[0].1["cleared_by"], "sensor.poll");
        assert!(
            merged[0].1["evidence"].as_str().unwrap().contains("opened"),
            "{}",
            merged[0].1["evidence"]
        );

        // Nothing more to open, nothing more to close.
        h.invoke(&[], &ctx_at("2026-09-17T11:05:00+00:00"))
            .await
            .unwrap();
        assert_eq!(stub.packets().len(), 2);
        assert_eq!(stub.writes("PUT").len(), 1);
    }

    #[tokio::test]
    async fn a_business_packet_413_keeps_the_existing_owed_reading_contract() {
        let stub = stub_jobs_api(vec![]).await;
        declare(&stub).await;
        let source =
            InMemorySource::answering(vec![obs("message-413", "2026-09-17T09:00:00Z", 500)]);
        let h = handler(&stub, source, Some("synthetic-read-only-key"));
        *stub.refuse_opens.lock().unwrap() = Some((413, "body permanently too large".into()));
        h.invoke(&[], &ctx()).await.unwrap();
        let packets = stub.packets();
        assert_eq!(packets.len(), 1, "one named refusal record");
        assert_eq!(packets[0]["metadata"]["external_id"], "message-413");
        assert!(
            packets[0]["metadata"]["reason"]
                .as_str()
                .unwrap()
                .contains("413")
        );
        assert_eq!(
            stub.sensors.list().await.unwrap()[0].cursor_at,
            Some(at("2026-09-17T09:00:00Z"))
        );
        h.invoke(&[], &ctx_at("2026-09-17T10:20:00+00:00"))
            .await
            .unwrap();
        // A110647 narrows the new terminal poll refusal to READINGS POSTs;
        // business packet-open recovery retains the pre-existing contract.
        assert_eq!(stub.writes("POST /api/jobs (refused)").len(), 2);
    }

    #[tokio::test]
    async fn a_readings_413_is_durable_once_and_keeps_new_equal_timestamp_messages() {
        let stub = stub_jobs_api(vec![]).await;
        declare(&stub).await;
        let source =
            InMemorySource::answering(vec![obs("message-413", "2026-09-17T09:00:00Z", 500)]);
        let h = handler(&stub, source.clone(), Some("synthetic-read-only-key"));
        *stub.refuse_readings.lock().unwrap() = true;
        h.invoke(&[], &ctx())
            .await
            .expect("a permanent readings refusal is recorded, not NAKed");
        let packets = stub.packets();
        assert_eq!(packets.len(), 1, "one durable named poll refusal");
        let reason = packets[0]["metadata"]["reason"].as_str().unwrap();
        assert!(reason.contains("message-413") && reason.contains("413"));
        assert!(reason.contains("bytes") && reason.contains("limit"));
        assert_eq!(
            stub.sensors.list().await.unwrap()[0].cursor_at,
            Some(at("2026-09-17T09:00:00Z"))
        );
        h.invoke(&[], &ctx_at("2026-09-17T10:20:00+00:00"))
            .await
            .unwrap();
        assert_eq!(
            stub.writes("POST /api/sensors/stripe-sponsorships/readings (refused)")
                .len(),
            1
        );
        *stub.refuse_readings.lock().unwrap() = false;
        source.set(Ok(vec![
            obs("message-413", "2026-09-17T09:00:00Z", 500),
            obs("new-same-time", "2026-09-17T09:00:00Z", 10),
        ]));
        h.invoke(&[], &ctx_at("2026-09-17T10:35:00+00:00"))
            .await
            .unwrap();
        let readings = stub.sensors.readings().await;
        assert_eq!(
            readings.len(),
            1,
            "skip the refused identity, not the entire timestamp"
        );
        assert_eq!(readings[0].external_id, "new-same-time");
    }

    #[tokio::test]
    async fn a_refusal_overflow_latches_without_skipping_and_remeasures_before_clearing() {
        let stub = stub_jobs_api(vec![]).await;
        declare(&stub).await;
        let tied = (0..65)
            .map(|i| obs(&format!("message-{i}"), "2026-09-17T09:00:00Z", 500))
            .collect::<Vec<_>>();
        let source = InMemorySource::answering(tied.clone());
        let h = handler(&stub, source.clone(), Some("synthetic-read-only-key"));
        *stub.refuse_readings.lock().unwrap() = true;
        h.invoke(&[], &ctx()).await.unwrap();
        let row = stub.sensors.list().await.unwrap().remove(0);
        assert_eq!(row.cursor_at, None, "unwitnessed tied messages remain owed");
        let witness = row.refused.expect("durable named latch");
        assert_eq!(witness.entries.len(), 64);
        assert_eq!(witness.truncated, 1);
        assert!(stub.sensors.readings().await.is_empty());
        h.invoke(&[], &ctx_at("2026-09-17T10:20:00+00:00"))
            .await
            .unwrap();
        assert_eq!(source.reads.lock().unwrap().len(), 2, "remeasure each poll");
        assert_eq!(
            stub.writes("POST /api/sensors/stripe-sponsorships/readings (refused)")
                .len(),
            67,
            "one batch,65 singleton measurements, then only the omitted/unwitnessed key is remeasured"
        );
        let witnessed = stub.sensors.list().await.unwrap()[0]
            .refused
            .as_ref()
            .unwrap()
            .entries
            .iter()
            .map(|e| e.message_key.clone())
            .collect::<std::collections::BTreeSet<_>>();
        for key in witnessed {
            assert_eq!(
                stub.writes("POST /api/sensors/stripe-sponsorships/readings (refused)")
                    .iter()
                    .filter(|(_, body)| body.as_array().unwrap().len() == 1
                        && body[0]["external_id"].as_str() == Some(&key))
                    .count(),
                1,
                "never retries a witnessed refused key"
            );
        }
        assert_eq!(stub.sensors.list().await.unwrap()[0].cursor_at, None);
        // An existing mail-side operation changes the observed window. No
        // BOSS manual-clear verb is invented; these valid ties were never lost.
        source.set(Ok(vec![
            obs("remaining-valid-a", "2026-09-17T09:00:00Z", 1),
            obs("remaining-valid-b", "2026-09-17T09:00:00Z", 1),
        ]));
        *stub.refuse_readings.lock().unwrap() = false;
        h.invoke(&[], &ctx_at("2026-09-17T10:35:00+00:00"))
            .await
            .unwrap();
        let rows = stub.sensors.list().await.unwrap();
        assert_eq!(rows[0].cursor_at, Some(at("2026-09-17T09:00:00Z")));
        let value = serde_json::to_value(&rows[0].refused).unwrap();
        assert_eq!(value["truncated"], 0);
        assert!(
            value["cleared"]
                .as_str()
                .unwrap()
                .contains("singleton POSTs")
        );
        assert_eq!(stub.sensors.readings().await.len(), 2);
    }

    #[tokio::test]
    async fn a_batch_413_is_split_and_only_a_refused_single_key_is_witnessed() {
        let stub = stub_jobs_api(vec![]).await;
        declare(&stub).await;
        *stub.split_readings_refusal.lock().unwrap() = Some("bad".into());
        let source = InMemorySource::answering(vec![
            obs("good-a", "2026-09-17T09:00:00Z", 1),
            obs("bad", "2026-09-17T09:00:00Z", 1),
            obs("good-b", "2026-09-17T09:00:00Z", 1),
        ]);
        let h = handler(&stub, source, Some("synthetic-read-only-key"));
        h.invoke(&[], &ctx()).await.unwrap();
        let readings = stub.sensors.readings().await;
        assert_eq!(
            readings.len(),
            2,
            "successful singleton siblings must be conserved"
        );
        assert!(readings.iter().all(|r| r.external_id != "bad"));
        let row = stub.sensors.list().await.unwrap().remove(0);
        let witness = row.refused.unwrap();
        assert_eq!(witness.entries.len(), 1);
        assert_eq!(witness.entries[0].message_key, "bad");
        assert!(
            witness.entries[0].reason.contains("v1"),
            "configured bound version is part of refusal evidence"
        );
        assert!(stub.packets().iter().any(|p| {
            p["metadata"]["reason"]
                .as_str()
                .is_some_and(|s| s.contains("configuration") && s.contains("v1"))
        }));
        h.invoke(&[], &ctx_at("2026-09-17T10:20:00+00:00"))
            .await
            .unwrap();
        assert_eq!(
            stub.writes("POST /api/sensors/stripe-sponsorships/readings (refused)")
                .iter()
                .filter(|(_, body)| body.as_array().unwrap().len() == 1
                    && body[0]["external_id"] == "bad")
                .count(),
            1,
            "never retries a witnessed refused key"
        );
    }

    #[tokio::test]
    async fn a_mixed_source_cohort_and_permanent_refusals_latch_without_a_nak() {
        let stub = stub_jobs_api(vec![]).await;
        declare(&stub).await;
        let source = InMemorySource::answering(
            (0..64)
                .map(|i| obs(&format!("permanent-{i}"), "2026-09-17T09:00:00Z", 1))
                .collect(),
        );
        let h = handler(&stub, source.clone(), Some("synthetic-read-only-key"));
        *stub.refuse_readings.lock().unwrap() = true;
        h.invoke(&[], &ctx()).await.unwrap();
        let before = stub.sensors.list().await.unwrap().remove(0);
        let keys = before
            .refused
            .as_ref()
            .unwrap()
            .entries
            .iter()
            .map(|e| e.message_key.clone())
            .collect::<std::collections::BTreeSet<_>>();
        source.set(Err(SourceError::RefusedPoll(PollRefusals {
            processed: Vec::new(),
            entries: (0..64)
                .map(|i| RefusedMessage {
                    message_key: format!("source-{i}"),
                    size_bytes: 10,
                    limit: boss_jobs::sensors::RefusalLimit::Count {
                        value: 256,
                        observed: 257,
                    },
                    reason: "timestamp_tie_bound".into(),
                    at: at("2026-09-17T09:00:00Z"),
                })
                .collect(),
            truncated: 193,
            cleared: None,
            classes: None,
        })));
        h.invoke(&[], &ctx_at("2026-09-17T10:20:00+00:00"))
            .await
            .expect("mixed union unknown is a durable named latch, never a NAK");
        let row = stub.sensors.list().await.unwrap().remove(0);
        assert_eq!(row.cursor_at, before.cursor_at);
        assert_eq!(
            row.refused
                .as_ref()
                .unwrap()
                .entries
                .iter()
                .filter(|e| e.reason.starts_with("readings_post_413"))
                .map(|e| e.message_key.clone())
                .collect::<std::collections::BTreeSet<_>>(),
            keys
        );
        assert!(
            stub.writes("PATCH /api/jobs/")
                .iter()
                .any(|(_, p)| p.to_string().contains("union unknown")),
            "the existing alarm refresh names the unknown union rather than adding class counts"
        );
        assert_eq!(stub.sensors.readings().await.len(), 0);
        let classes = row.refused.as_ref().unwrap().classes.as_ref().unwrap();
        assert_eq!(
            (
                classes.source_count,
                classes.permanent_413_count,
                classes.union_count
            ),
            (257, 64, None)
        );
        assert_eq!(
            row.refused.as_ref().unwrap().truncated,
            0,
            "unknown union cannot carry a fabricated omitted union count"
        );
        source.set(Ok(vec![
            obs("permanent-0", "2026-09-17T09:00:00Z", 1),
            obs(
                "valid-after-source-remeasurement",
                "2026-09-17T09:00:00Z",
                1,
            ),
        ]));
        *stub.refuse_readings.lock().unwrap() = false;
        h.invoke(&[], &ctx_at("2026-09-17T10:35:00+00:00"))
            .await
            .unwrap();
        let row = stub.sensors.list().await.unwrap().remove(0);
        assert!(row.refused.as_ref().unwrap().classes.is_none());
        assert_eq!(
            row.refused
                .as_ref()
                .unwrap()
                .entries
                .iter()
                .map(|e| e.message_key.clone())
                .collect::<std::collections::BTreeSet<_>>(),
            keys
        );
        assert_eq!(
            stub.sensors.readings().await.len(),
            1,
            "certified readable source clears only source latch; valid tie remains owed"
        );
    }

    #[tokio::test]
    async fn a_source_refused_tie_is_named_durable_and_clears_only_after_remeasurement() {
        let stub = stub_jobs_api(vec![]).await;
        declare(&stub).await;
        let witness = PollRefusals {
            processed: Vec::new(),
            classes: None,
            entries: (0..64)
                .map(|i| RefusedMessage {
                    message_key: format!("<message-{i}@example.invalid>"),
                    size_bytes: 100,
                    limit: boss_jobs::sensors::RefusalLimit::Count {
                        value: 256,
                        observed: 257,
                    },
                    reason: "timestamp_tie_bound".into(),
                    at: at("2026-09-17T09:00:00Z"),
                })
                .collect(),
            truncated: 193,
            cleared: None,
        };
        let source = InMemorySource::failing(SourceError::RefusedPoll(witness));
        let h = handler(&stub, source.clone(), Some("synthetic-read-only-key"));
        h.invoke(&[], &ctx())
            .await
            .expect("source refusal is a durable poll outcome");
        let row = stub.sensors.list().await.unwrap().remove(0);
        assert_eq!(row.cursor_at, None);
        assert_eq!(row.refused.as_ref().unwrap().truncated, 193);
        assert!(stub.sensors.readings().await.is_empty());
        h.invoke(&[], &ctx_at("2026-09-17T10:20:00+00:00"))
            .await
            .unwrap();
        assert_eq!(stub.packets().len(), 1, "same named refusal is not twinned");
        // The SOURCE now certifies its bound:180 valid tied messages, not
        //180 refused keys. Do not confuse a source cap with the64-key witness.
        source.set(Ok((0..180)
            .map(|i| obs(&format!("now-valid-{i}"), "2026-09-17T09:00:00Z", 1))
            .collect()));
        h.invoke(&[], &ctx_at("2026-09-17T10:35:00+00:00"))
            .await
            .unwrap();
        assert_eq!(stub.sensors.readings().await.len(), 180);
        let row = stub.sensors.list().await.unwrap().remove(0);
        assert_eq!(row.refused.as_ref().unwrap().truncated, 0);
        assert!(row.refused.unwrap().cleared.unwrap().contains("source"));
    }

    #[tokio::test]
    async fn retained_boundary_keys_cannot_make_the_next_413_unrecordable() {
        let stub = stub_jobs_api(vec![]).await;
        declare(&stub).await;
        let tied = (0..64)
            .map(|i| obs(&format!("old-{i}"), "2026-09-17T09:00:00Z", 1))
            .collect::<Vec<_>>();
        let source = InMemorySource::answering(tied.clone());
        let h = handler(&stub, source.clone(), Some("synthetic-read-only-key"));
        *stub.refuse_readings.lock().unwrap() = true;
        h.invoke(&[], &ctx()).await.unwrap();
        let prior_keys = stub.sensors.list().await.unwrap()[0]
            .refused
            .as_ref()
            .unwrap()
            .entries
            .iter()
            .map(|e| e.message_key.clone())
            .collect::<std::collections::BTreeSet<_>>();
        let mut expanded = tied.clone();
        expanded.push(obs("new-valid-tie", "2026-09-17T09:00:00Z", 1));
        source.set(Ok(expanded));
        h.invoke(&[], &ctx_at("2026-09-17T10:20:00+00:00"))
            .await
            .expect("actual single413 is a bounded named latch, not a mark400");
        assert_eq!(
            stub.writes("POST /api/sensors/stripe-sponsorships/readings (refused)")
                .len(),
            66,
            "first batch+64singletons, then one new actual single413"
        );
        let row = stub.sensors.list().await.unwrap().remove(0);
        let retained = row.refused.unwrap();
        assert_eq!(retained.truncated, 1);
        assert_eq!(
            retained
                .entries
                .iter()
                .map(|e| e.message_key.clone())
                .collect::<std::collections::BTreeSet<_>>(),
            prior_keys,
            "a new lexically earlier refusal cannot evict an already witnessed still-present key"
        );
        h.invoke(&[], &ctx_at("2026-09-17T10:35:00+00:00"))
            .await
            .unwrap();
        for key in &prior_keys {
            assert_eq!(
                stub.writes("POST /api/sensors/stripe-sponsorships/readings (refused)")
                    .iter()
                    .filter(|(_, body)| body.as_array().unwrap().len() == 1
                        && body[0]["external_id"].as_str() == Some(key.as_str()))
                    .count(),
                1,
                "a previously witnessed key cannot be retried after overflow"
            );
        }
        // Mail-side removal reduces the measured cohort. The new valid tie
        // remains owed; a bounded witness must never discard it silently.
        source.set(Ok(vec![obs("remaining-valid", "2026-09-17T09:00:00Z", 1)]));
        *stub.refuse_readings.lock().unwrap() = false;
        h.invoke(&[], &ctx_at("2026-09-17T10:50:00+00:00"))
            .await
            .unwrap();
        assert_eq!(stub.sensors.readings().await.len(), 1);
        assert_eq!(
            stub.sensors.readings().await[0].external_id,
            "remaining-valid"
        );
    }

    #[tokio::test]
    async fn retained_refusals_do_not_block_a_successful_new_tied_post() {
        let stub = stub_jobs_api(vec![]).await;
        declare(&stub).await;
        let tied = (0..64)
            .map(|i| obs(&format!("old-{i}"), "2026-09-17T09:00:00Z", 1))
            .collect::<Vec<_>>();
        let source = InMemorySource::answering(tied.clone());
        let h = handler(&stub, source.clone(), Some("synthetic-read-only-key"));
        *stub.refuse_readings.lock().unwrap() = true;
        h.invoke(&[], &ctx()).await.unwrap();
        let mut expanded = tied;
        expanded.push(obs("new-valid-tie", "2026-09-17T09:00:00Z", 1));
        source.set(Ok(expanded));
        *stub.refuse_readings.lock().unwrap() = false;
        h.invoke(&[], &ctx_at("2026-09-17T10:20:00+00:00"))
            .await
            .unwrap();
        let rows = stub.sensors.readings().await;
        assert_eq!(
            rows.len(),
            1,
            "retained refusals must not block a successful new POST"
        );
        assert_eq!(rows[0].external_id, "new-valid-tie");
    }

    /// The payouts source (backlog 21eb9516) through the whole handler:
    /// a `stripe-payouts` sensor row, the real adapter against the stub
    /// Stripe, the same credential row as the sponsorships. The tenant
    /// has not published `receive-a-payout` yet: the reading is
    /// recorded, the open is refused, `sensor_unopenable:stripe-payouts`
    /// is filed naming the reading, and the cursor is the arrival date.
    /// Once the kind is published the same reading opens the packet
    /// `describe` says — title, amount, arrival, payout id, provenance.
    #[tokio::test]
    async fn a_payouts_sensor_whose_kind_is_not_published_alarms_and_opens_once_it_is() {
        use super::super::stripe_payouts::{StripePayouts, stub};

        let stub_jobs = stub_jobs_api(vec![]).await;
        stub_jobs
            .sensors
            .publish(
                "acme",
                &[boss_jobs::sensors::SensorInput {
                    id: "stripe-payouts".into(),
                    source: "stripe-payouts".into(),
                    credential: "stripe-restricted-read".into(),
                    every_minutes: 60,
                    opens: "receive-a-payout".into(),
                    subject_kind: "custom".into(),
                    enabled: true,
                    selector: None,
                }],
            )
            .await
            .unwrap();
        // 1_789_430_400 is 2026-09-15T00:00:00Z — inside the readings'
        // 90-day retention from the firing, so the sweep that rides
        // every firing leaves the reading to be read back.
        let (stripe, seen) = stub::stub_stripe(
            200,
            Some(vec![stub::payout("po_1", 1_789_430_400, "paid", 250_075)]),
        )
        .await;
        let mut sources: HashMap<String, Arc<dyn SensorSource>> = HashMap::new();
        sources.insert(
            "stripe-payouts".into(),
            Arc::new(StripePayouts::new(stripe)),
        );
        let h = SensorPoll::new(
            stub_jobs.base.clone(),
            sources,
            CredentialValues::new().with(
                "stripe-restricted-read",
                "BOSS_BROKER_STRIPE_KEY",
                Some("rk_test_good".into()),
            ),
            Arc::new(boss_core::platform_owner::Fixed("emp-owner".into())),
        );
        *stub_jobs.refuse_opens.lock().unwrap() =
            Some((400, "unknown or inactive job kind: receive-a-payout".into()));

        h.invoke(&[], &ctx()).await.unwrap();
        let packets = stub_jobs.packets();
        assert_eq!(packets.len(), 1, "{packets:#?}");
        assert_eq!(packets[0]["kind"], "backlog-item");
        assert_eq!(
            packets[0]["metadata"]["estate_finding"],
            "sensor_unopenable:stripe-payouts"
        );
        assert_eq!(packets[0]["metadata"]["opens_kind"], "receive-a-payout");
        assert_eq!(packets[0]["metadata"]["external_id"], "po_1");
        assert!(
            packets[0]["metadata"]["reason"]
                .as_str()
                .unwrap()
                .contains("unknown or inactive job kind: receive-a-payout")
        );
        assert_eq!(packets[0]["_actor"], "automation:sensor:stripe-payouts");
        let readings = stub_jobs.sensors.readings().await;
        assert_eq!(readings.len(), 1);
        assert_eq!(readings[0].packet_id, None, "still owed");
        assert_eq!(readings[0].payload["amount_cents"], 250_075);
        let row = &stub_jobs.sensors.list().await.unwrap()[0];
        assert_eq!(
            row.cursor_at,
            Some(at("2026-09-15T00:00:00Z")),
            "the cursor is the arrival date"
        );
        assert_eq!(
            seen.lock().unwrap()[0].0,
            "Bearer rk_test_good",
            "the same credential row, sent as the bearer"
        );

        // The tenant publishes the kind: the owed reading opens the
        // packet describe says, and the alarm closes.
        *stub_jobs.refuse_opens.lock().unwrap() = None;
        h.invoke(&[], &ctx_at("2026-09-17T11:05:00+00:00"))
            .await
            .unwrap();
        let packets = stub_jobs.packets();
        assert_eq!(packets.len(), 2, "{packets:#?}");
        let p = &packets[1];
        assert_eq!(p["kind"], "receive-a-payout");
        assert_eq!(p["title"], "Payout: 2500.75 USD arriving 2026-09-15");
        assert_eq!(
            p["subject"],
            json!({"subject_kind": "custom", "id": "po_1"})
        );
        assert_eq!(p["metadata"]["amount_cents"], 250_075);
        assert_eq!(p["metadata"]["currency"], "usd");
        assert_eq!(p["metadata"]["arrival_date"], "2026-09-15");
        assert_eq!(p["metadata"]["stripe_payout_id"], "po_1");
        assert_eq!(p["metadata"]["sensor_id"], "stripe-payouts");
        assert_eq!(p["metadata"]["sensor_source"], "stripe-payouts");
        assert_eq!(p["_actor"], "automation:sensor:stripe-payouts");
        assert!(stub_jobs.sensors.readings().await[0].packet_id.is_some());
        let puts = stub_jobs.writes("PUT /api/jobs/job-2/steps/job-2-triage");
        assert_eq!(
            puts.len(),
            1,
            "the alarm closed: {:?}",
            stub_jobs.writes("PUT")
        );
        assert_eq!(puts[0].1, json!({"status": "completed"}), "the flip alone");
        let merged = stub_jobs.writes("PATCH /api/jobs/job-2/steps/job-2-triage/metadata");
        assert_eq!(merged.len(), 1, "{:?}", stub_jobs.writes("PATCH"));
        assert_eq!(merged[0].1["disposition"], "stale");
    }

    /// A 5xx / 409 from the open is weather, exactly as it was: the
    /// firing NAKs, no alarm, the poll is not marked.
    #[tokio::test]
    async fn a_transient_answer_to_the_open_naks_without_an_alarm() {
        let stub = stub_jobs_api(vec![]).await;
        declare(&stub).await;
        let source = InMemorySource::answering(vec![obs("ch_1", "2026-09-17T09:00:00Z", 1)]);
        let h = handler(&stub, source, Some("k"));
        *stub.refuse_opens.lock().unwrap() =
            Some((503, "subject existence check unavailable".into()));
        let err = h.invoke(&[], &ctx()).await.unwrap_err();
        assert!(matches!(err, HandlerError::Downstream(_)), "{err}");
        assert!(stub.packets().is_empty(), "{:?}", stub.packets());
        assert_eq!(stub.sensors.list().await.unwrap()[0].last_polled_at, None);
    }

    /// The two alarms are two findings: a source that reads again
    /// closes the unreadable one on the read even while its packets
    /// are still refused, and the unopenable one stands until an open
    /// succeeds.
    #[tokio::test]
    async fn a_good_read_whose_open_is_refused_closes_only_the_unreadable_alarm() {
        let stub = stub_jobs_api(vec![]).await;
        declare(&stub).await;
        let source = InMemorySource::answering(vec![obs("ch_1", "2026-09-17T09:00:00Z", 1)]);
        // First: no key, unreadable alarm.
        let h = handler(&stub, source.clone(), None);
        h.invoke(&[], &ctx()).await.unwrap();
        assert_eq!(
            stub.packets()[0]["metadata"]["estate_finding"],
            "sensor_unreadable:stripe-sponsorships"
        );
        // Then: a key, but the open is refused. The unreadable alarm
        // closes (the source read), the unopenable one is raised.
        *stub.refuse_opens.lock().unwrap() = Some((400, "unknown or inactive job kind".into()));
        let h2 = handler(&stub, source.clone(), Some("k"));
        h2.invoke(&[], &ctx_at("2026-09-17T10:20:00+00:00"))
            .await
            .unwrap();
        let packets = stub.packets();
        assert_eq!(packets.len(), 2, "{packets:#?}");
        assert_eq!(
            packets[1]["metadata"]["estate_finding"],
            "sensor_unopenable:stripe-sponsorships"
        );
        let puts = stub.writes("PUT /api/jobs/job-1/steps/job-1-triage");
        assert_eq!(puts.len(), 1, "the unreadable alarm closed on the read");
        assert_eq!(
            stub.writes("PUT").len(),
            1,
            "the unopenable one stands: {:?}",
            stub.writes("PUT")
        );
    }

    /// A source the product has no adapter for is unreadable by that
    /// name — a declaration error, alarmed, never a panic.
    #[tokio::test]
    async fn an_unknown_source_is_an_alarm_naming_it() {
        let stub = stub_jobs_api(vec![]).await;
        stub.sensors
            .publish(
                "acme",
                &[boss_jobs::sensors::SensorInput {
                    id: "weather".into(),
                    source: "noaa".into(),
                    credential: "stripe-restricted-read".into(),
                    every_minutes: 15,
                    opens: "k".into(),
                    subject_kind: "custom".into(),
                    enabled: true,
                    selector: None,
                }],
            )
            .await
            .unwrap();
        let h = handler(&stub, InMemorySource::answering(vec![]), Some("k"));
        h.invoke(&[], &ctx()).await.unwrap();
        let packets = stub.packets();
        assert_eq!(packets.len(), 1);
        assert!(
            packets[0]["metadata"]["reason"]
                .as_str()
                .unwrap()
                .contains("noaa")
        );
        assert_eq!(
            packets[0]["metadata"]["estate_finding"],
            "sensor_unreadable:weather"
        );
    }

    /// A network blip NAKs: the firing fails Downstream, no alarm is
    /// filed, and the poll is not marked, so the next tick retries.
    #[tokio::test]
    async fn a_transient_error_naks_without_an_alarm_or_a_mark() {
        let stub = stub_jobs_api(vec![]).await;
        declare(&stub).await;
        let source = InMemorySource::failing(SourceError::Transient("connection reset".into()));
        let h = handler(&stub, source, Some("k"));
        let err = h.invoke(&[], &ctx()).await.unwrap_err();
        assert!(matches!(err, HandlerError::Downstream(_)), "{err}");
        assert!(err.to_string().contains("connection reset"));
        assert!(stub.packets().is_empty());
        assert_eq!(stub.sensors.list().await.unwrap()[0].last_polled_at, None);
    }

    /// A disabled sensor is never read.
    #[tokio::test]
    async fn a_disabled_sensor_is_left_alone() {
        let stub = stub_jobs_api(vec![]).await;
        stub.sensors
            .publish(
                "acme",
                &[boss_jobs::sensors::SensorInput {
                    id: "stripe-sponsorships".into(),
                    source: "stripe".into(),
                    credential: "stripe-restricted-read".into(),
                    every_minutes: 15,
                    opens: "receive-a-sponsorship".into(),
                    subject_kind: "custom".into(),
                    enabled: false,
                    selector: None,
                }],
            )
            .await
            .unwrap();
        let source = InMemorySource::answering(vec![obs("ch_1", "2026-09-17T09:00:00Z", 1)]);
        let h = handler(&stub, source.clone(), Some("k"));
        h.invoke(&[], &ctx()).await.unwrap();
        assert!(source.reads.lock().unwrap().is_empty());
        assert!(stub.packets().is_empty());
    }

    // ----- a routed alarm (backlog 6072ff60, the sensor half of a2d8bad3) -----

    const UNREADABLE: &str = "sensor_unreadable:stripe-sponsorships";

    /// An open alarm a person has already triaged: `triage` completed
    /// with the route it took, and the route's `build` step at
    /// `route_status`.
    fn routed_alarm(reason: &str, route_status: &str) -> Json {
        json!({
            "id": "alarm-r",
            "kind": "backlog-item",
            "status": "open",
            "metadata": {"estate_finding": UNREADABLE, "reason": reason},
            "steps": [
                {"id": "alarm-r-triage", "spec_slug": "triage", "status": "completed",
                 "metadata": {"authority_role": "platform-admin", "disposition": "build"}},
                {"id": "alarm-r-build", "spec_slug": "build", "status": route_status,
                 "metadata": {"authority_role": "platform-admin"}}
            ]
        })
    }

    /// THE DEFECT. A good read closed its alarm only by completing
    /// `triage`, so an alarm a person had routed answered 409 on every
    /// poll and never retracted. It withdraws at the step the packet is
    /// waiting on — the ready `build` — and never touches the terminal
    /// triage.
    #[tokio::test]
    async fn a_routed_alarm_is_withdrawn_at_its_ready_step_not_refused_at_triage() {
        let stub = stub_jobs_api(vec![routed_alarm("stripe answered 401", "ready")]).await;
        declare(&stub).await;
        let source = InMemorySource::answering(vec![obs("ch_1", "2026-09-17T09:00:00Z", 1)]);
        handler(&stub, source, Some("k"))
            .invoke(&[], &ctx())
            .await
            .expect("a good read retracts a routed alarm without a 409");
        assert!(
            stub.writes("PUT /api/jobs/alarm-r/steps/alarm-r-triage")
                .is_empty(),
            "the terminal triage is not written: {:?}",
            stub.writes("PUT")
        );
        let puts = stub.writes("PUT /api/jobs/alarm-r/steps/alarm-r-build");
        assert_eq!(puts.len(), 1, "{:?}", stub.writes("PUT"));
        assert_eq!(puts[0].1, json!({"status": "completed"}), "the flip alone");
        let merged = stub.writes("PATCH /api/jobs/alarm-r/steps/alarm-r-build/metadata");
        assert_eq!(merged.len(), 1, "{:?}", stub.writes("PATCH"));
        assert_eq!(merged[0].1["disposition"], "stale");
        assert_eq!(merged[0].1["cleared_by"], "sensor.poll");
        assert!(
            merged[0].1.get("authority_role").is_none(),
            "the step's own keys are not re-sent: the merge door keeps them"
        );
    }

    /// A route an executor holds is not completed from under it: the
    /// packet is TOLD it recovered, once — the next good read, with the
    /// note standing, writes nothing.
    #[tokio::test]
    async fn an_alarm_whose_route_is_held_is_told_it_recovered_once() {
        let stub = stub_jobs_api(vec![routed_alarm("stripe answered 401", "active")]).await;
        declare(&stub).await;
        let source = InMemorySource::answering(vec![obs("ch_1", "2026-09-17T09:00:00Z", 1)]);
        let h = handler(&stub, source, Some("k"));
        h.invoke(&[], &ctx()).await.expect("told, not refused");
        assert!(stub.writes("PUT /api/jobs/alarm-r").is_empty());
        let patches = stub.writes("PATCH /api/jobs/alarm-r/metadata");
        assert_eq!(patches.len(), 1, "{patches:?}");
        let note = &patches[0].1;
        assert_eq!(
            note[super::super::common::RECOVERED_AT],
            "2026-09-17T10:05:00+00:00"
        );
        assert_eq!(note["recovered_by"], "sensor.poll");
        let recovery = note["recovery"].as_str().unwrap();
        assert!(recovery.starts_with("RECOVERED"), "{recovery}");
        assert!(recovery.contains("stripe-sponsorships"), "{recovery}");
        assert!(recovery.contains("`build` is active"), "{recovery}");

        // The note stands (the stub does not merge, so set it as the
        // real API would): the next good read does not tell it again.
        stub.alarms.lock().unwrap()[0]["metadata"]["recovered_at"] =
            json!("2026-09-17T10:05:00+00:00");
        h.invoke(&[], &ctx_at("2026-09-17T10:20:00+00:00"))
            .await
            .unwrap();
        assert_eq!(stub.writes("PATCH /api/jobs/alarm-r/metadata").len(), 1);
    }

    /// THE CATCH. The held path — the same reason again — writes
    /// nothing to the packet, so a RECOVERED note written on recovery
    /// would stand through a relapse. It is withdrawn on the first
    /// failing read, as cadence and estate withdraw theirs; a refresh
    /// (a new reason) withdraws it in the same merge.
    #[tokio::test]
    async fn a_relapse_withdraws_the_recovery_note_on_the_held_path_and_on_a_refresh() {
        let mut told = routed_alarm("stripe answered 401", "active");
        told["metadata"]["recovered_at"] = json!("2026-09-17T09:50:00+00:00");
        told["metadata"]["recovery"] = json!("RECOVERED — ...");
        let stub = stub_jobs_api(vec![told]).await;
        declare(&stub).await;
        let source = InMemorySource::failing(SourceError::Unreadable("stripe answered 401".into()));
        let h = handler(&stub, source.clone(), Some("k"));
        h.invoke(&[], &ctx()).await.unwrap();
        assert!(stub.packets().is_empty(), "held, never twinned");
        let patches = stub.writes("PATCH /api/jobs/alarm-r/metadata");
        assert_eq!(patches.len(), 1, "{patches:?}");
        for (key, withdrawn) in super::super::common::relapse_patch().as_object().unwrap() {
            assert_eq!(&patches[0].1[key], withdrawn, "{key}: {patches:?}");
        }
        // The relapse is an occurrence on the open alarm (backlog f4b0ad41).
        assert_eq!(patches[0].1[super::super::alarm_item::OCCURRENCES], 2);
        assert_eq!(
            patches[0].1[super::super::alarm_item::OCCURRENCE_LOG][0]["evidence"],
            "stripe answered 401"
        );

        source.set(Err(SourceError::Unreadable("stripe answered 403".into())));
        h.invoke(&[], &ctx_at("2026-09-17T10:20:00+00:00"))
            .await
            .unwrap();
        let patches = stub.writes("PATCH /api/jobs/alarm-r/metadata");
        assert_eq!(patches.len(), 2, "{patches:?}");
        assert_eq!(patches[1].1["reason"], "stripe answered 403");
        for key in [
            super::super::common::RECOVERED_AT,
            "recovered_by",
            "recovery",
        ] {
            assert!(
                patches[1].1.as_object().unwrap().contains_key(key) && patches[1].1[key].is_null(),
                "{key} is withdrawn by a null in the merge: {}",
                patches[1].1
            );
        }
    }
}
