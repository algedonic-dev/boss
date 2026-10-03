//! `policy.check.refusals.alarm` — the reader of the policy check's
//! lapsed-grant warn (R3 of the 2026-09-29 follow-ups on backlog
//! b8e75382, carried into F7 when car 09c0bfb9 was released).
//!
//! THE WARN IT READS. Every service asks `POST /api/policy/check` signed
//! as `automation:<svc>` at platform-admin (13 of them, measured on the
//! review 1c2860f4 of car c81280cc). If that service's Read on
//! `policy-rule` lapses — `platform-admin:policy-rule:read` retired, or a
//! scope-none override on the service id — the check is still ANSWERED,
//! because refusing it would hang every request in the estate on one
//! mutable grant; the gap was only a WARN in the policy service's log,
//! and nothing read it. Under `enforce` that same lapse is every door
//! 503 (checklist F3), so the lapse must be seen while it is still only
//! a warn. With the policy check's mode mounted at `report` (checklist
//! F4), each such check is tallied at `/api/policy/check/refusals` as an
//! arm `service` row with its `last_seen`, and this handler reads that
//! tally hourly.
//!
//! WHAT IT FILES. At most one open alarm per finding, keyed on
//! [`FINDING_KEY`] — never one per caller, because the tally's callers
//! are ASSERTED ids and anyone on the port can spray a thousand of them
//! (checklist F2); one packet lists them, bounded:
//!
//! * [`LAPSED`] — a service caller the read bound refused within the
//!   last [`LOOKBACK_MINUTES`]. Withdrawn by a read that shows none AND
//!   whose tally began before the lookback opened: the tally lives in
//!   memory and every train Recreates the boss pod, so a younger empty
//!   tally watched only part of the window and holds the alarm (review
//!   b14f0e1b, B1).
//! * [`OVERFLOW`] — the tally is full, so a lapsed grant that arrives now
//!   is counted where it names nobody, and the tally is not clean.
//!   Withdrawn once the tally begins again (a restart or a mode move).
//! * [`UNREADABLE`] — the read could not be judged, said rather than
//!   swallowed, as `policy.coverage.alarm` says its own. Withdrawn by the
//!   next whole read; the firing still fails.
//!
//! A tally in mode `off` records nothing, so it judges nothing: no alarm
//! is raised or withdrawn on it — silence there is not recovery.
//!
//! TWO CLOCKS. The tick's `_at` comes from the clock service, while
//! `last_seen` and `recording_since` are the policy service's wall clock.
//! They agree in production (wall mode); on an instance whose clock is
//! warped (a playground sim clock) the lookback compares unrelated
//! instants and means nothing there.
//!
//! IT REFUSES NOTHING. It reads, files and withdraws its own alarms.

use std::collections::BTreeMap;
use std::sync::Arc;

use async_trait::async_trait;
use boss_dispatcher::rules::expr::Value;
use boss_dispatcher::rules::handler::{Handler, HandlerError, InvocationContext};
use boss_jobs::channels::InputChannel;
use chrono::{DateTime, Duration, Utc};
use serde_json::{Value as Json, json};

use super::common::{
    RECOVERED_AT, Retraction, api_client, complete_step, get_json, jobs_where, owner_for_filing,
    post_json, recovery_note, relapse_patch, retraction, with_lane, withdrawal_fields, write_json,
};

/// The handler's registered name.
pub const HANDLER: &str = "policy.check.refusals.alarm";

/// The tally this handler reads, on the policy service — the same path
/// as `boss_policy::check_mode::REFUSALS_PATH`, held equal by a test.
pub const REFUSALS_PATH: &str = "/api/policy/check/refusals";

/// The packet-metadata key naming which finding an alarm is — the dedup
/// key.
pub const FINDING_KEY: &str = "policy_check_finding";

/// A service's Read on `policy-rule` has lapsed.
pub const LAPSED: &str = "lapsed-service-grant";
/// The tally is full: what arrives now names no caller.
pub const OVERFLOW: &str = "refusal-tally-overflow";
/// The tally could not be read as one.
pub const UNREADABLE: &str = "refusals-unreadable";

/// How recently a service row must have been counted to be lapsing NOW:
/// two of the rule's hourly cadences, so one late tick cannot withdraw a
/// lapse still happening, and a grant restored is withdrawn within two
/// hours of its last refused check.
pub const LOOKBACK_MINUTES: i64 = 120;

/// How many callers one alarm lists; the rest are counted.
const LISTED: usize = 12;

/// The key the schedule runner writes the firing instant under on every
/// sub-day tick.
const TICK_AT: &str = "_at";

/// One service caller whose check the read bound refused.
#[derive(Debug, Clone, PartialEq)]
pub struct Lapsed {
    pub caller: String,
    pub role: String,
    pub peer: String,
    pub count: u64,
    pub last_seen: String,
}

/// The tally, judged against the firing's instant.
#[derive(Debug, Clone, PartialEq)]
pub struct Judged {
    pub mode: String,
    pub recording_since: String,
    /// The tally began at or before the lookback opened, so an empty one
    /// is evidence for the whole of it. False after a restart or a mode
    /// move inside the lookback (review b14f0e1b, B1).
    pub watched_lookback: bool,
    pub lapsed: Vec<Lapsed>,
    pub overflow: u64,
}

fn instant(raw: &str, what: &str) -> Result<DateTime<Utc>, String> {
    DateTime::parse_from_rfc3339(raw)
        .map(|t| t.with_timezone(&Utc))
        .map_err(|e| format!("{what} `{raw}` is not an RFC 3339 instant: {e}"))
}

/// The read, judged: refuses a body that is not a whole answer, so a
/// changed contract cannot withdraw every alarm as recovered.
pub fn judge(body: &Json, now: DateTime<Utc>) -> Result<Judged, String> {
    let text = |v: &Json, k: &str| v.get(k).and_then(Json::as_str).map(str::to_string);
    let whole = |what: &str| format!("GET {REFUSALS_PATH} answered no `{what}`");
    let mode = text(body, "mode").ok_or_else(|| whole("mode"))?;
    let recording_since = text(body, "recording_since").ok_or_else(|| whole("recording_since"))?;
    let began = instant(&recording_since, "recording_since")?;
    let overflow = body
        .get("overflow")
        .and_then(Json::as_u64)
        .ok_or_else(|| whole("overflow"))?;
    let rows = body
        .get("rows")
        .and_then(Json::as_array)
        .ok_or_else(|| whole("rows"))?;
    let since = now - Duration::minutes(LOOKBACK_MINUTES);
    let mut lapsed = Vec::new();
    for row in rows
        .iter()
        .filter(|r| text(r, "arm").as_deref() == Some("service"))
    {
        let last_seen = text(row, "last_seen").ok_or_else(|| {
            format!("a `service` row of GET {REFUSALS_PATH} carries no `last_seen`: {row}")
        })?;
        if instant(&last_seen, "a row's last_seen")? < since {
            continue;
        }
        lapsed.push(Lapsed {
            caller: text(row, "caller").unwrap_or_default(),
            role: text(row, "role").unwrap_or_default(),
            peer: text(row, "peer").unwrap_or_default(),
            count: row.get("count").and_then(Json::as_u64).unwrap_or_default(),
            last_seen,
        });
    }
    Ok(Judged {
        mode,
        recording_since,
        watched_lookback: began <= since,
        lapsed,
        overflow,
    })
}

fn body(finding: &str, title: String, detail: String, extra: Json, owner: &str) -> Json {
    let mut metadata = json!({
        "area": "policy",
        "scope": "policy-check",
        FINDING_KEY: finding,
        "detail": detail,
    });
    if let (Some(m), Json::Object(extra)) = (metadata.as_object_mut(), extra) {
        m.extend(extra);
    }
    json!({
        "kind": "backlog-item",
        "title": title,
        "subject": {"subject_kind": "custom", "id": finding},
        "owner_id": owner,
        "priority": "urgent",
        "status": "open",
        "tags": [],
        "metadata": with_lane(metadata, InputChannel::Telemetry),
    })
}

/// The alarm a lapsed service grant files.
pub fn lapsed_body(j: &Judged, owner: &str, now: &str, rule: &str) -> Json {
    let named: Vec<String> = j
        .lapsed
        .iter()
        .take(LISTED)
        .map(|l| {
            format!(
                "{} (role {}, from {}, {} check(s), last {})",
                l.caller, l.role, l.peer, l.count, l.last_seen
            )
        })
        .collect();
    let more = j.lapsed.len().saturating_sub(LISTED);
    let callers = if more > 0 {
        format!("{} — and {more} more", named.join("; "))
    } else {
        named.join("; ")
    };
    let detail = format!(
        "Raised by {HANDLER} (rule {rule}, backlog b8e75382 R3) at {now}: GET {REFUSALS_PATH} \
         (mode {mode}, recording since {since}) shows a service's POST /api/policy/check refused \
         by the read bound within the last {LOOKBACK_MINUTES} minutes: {callers}. Each is \
         ANSWERED today, as an unsigned check; under mode `enforce` each would be 403 and that \
         service's every door would fail (enforce checklist F3). Restore the grant: \
         platform-admin:policy-rule:read at scope all, and no scope-none override on the \
         service's automation: id. A caller that is no service of this estate is an asserted id \
         from its peer address. This alarm refuses nothing; it is withdrawn when a read shows \
         no service refused for {LOOKBACK_MINUTES} minutes.",
        mode = j.mode,
        since = j.recording_since,
    );
    body(
        LAPSED,
        format!(
            "LAPSED POLICY READ: {} service caller(s) refused Read on policy-rule",
            j.lapsed.len()
        ),
        detail,
        json!({
            "callers": j.lapsed.iter().take(LISTED).map(|l| l.caller.clone()).collect::<Vec<_>>(),
            "caller_count": j.lapsed.len(),
            "measured_at": now,
        }),
        owner,
    )
}

/// The alarm a full tally files.
pub fn overflow_body(j: &Judged, owner: &str, now: &str, rule: &str) -> Json {
    let detail = format!(
        "Raised by {HANDLER} (rule {rule}, backlog b8e75382 checklist F2) at {now}: GET \
         {REFUSALS_PATH} (mode {mode}, recording since {since}) counts overflow {overflow}: the \
         tally's keys are full, so a lapsed service grant that arrives now names no caller, and \
         the policy check's window is not clean. Read the rows' callers and peers — ids nobody \
         runs, from one address, are a spray on the policy port. It begins again on a restart of \
         the policy service or a move of its mode; this alarm is withdrawn when a read shows no \
         overflow.",
        mode = j.mode,
        since = j.recording_since,
        overflow = j.overflow,
    );
    body(
        OVERFLOW,
        format!(
            "POLICY CHECK TALLY FULL: overflow {} — a lapsed grant can go unnamed",
            j.overflow
        ),
        detail,
        json!({"overflow": j.overflow, "measured_at": now}),
        owner,
    )
}

/// The alarm a dark read files, naming the error it got.
pub fn unreadable_body(error: &str, owner: &str, now: &str, rule: &str) -> Json {
    let detail = format!(
        "Raised by {HANDLER} (rule {rule}, backlog b8e75382 R3) at {now}: GET {REFUSALS_PATH} \
         could not be judged — {error}. While it stays so, a service whose Read on policy-rule \
         lapses is seen by nobody; the alarms already open stay open. A 403 here is itself the \
         lapse this alarm watches for, on the dispatcher's own id. Withdrawn by the next whole \
         read."
    );
    body(
        UNREADABLE,
        "POLICY CHECK TALLY UNREADABLE: nobody can see a lapsed service grant".to_string(),
        detail,
        json!({"last_error": error, "measured_at": now}),
        owner,
    )
}

/// Every OPEN alarm this handler filed, keyed by its finding.
pub fn alarms_by_finding(rows: &[Json]) -> BTreeMap<String, Json> {
    rows.iter()
        .filter(|r| r.get("status").and_then(Json::as_str) == Some("open"))
        .filter_map(|r| {
            let f = r
                .pointer(&format!("/metadata/{FINDING_KEY}"))
                .and_then(Json::as_str)
                .filter(|s| !s.is_empty())?;
            Some((f.to_string(), r.clone()))
        })
        .collect()
}

pub struct PolicyCheckRefusalsAlarm {
    client: boss_core::machine_token::Client,
    policy_base: String,
    jobs_base: String,
    owner: Arc<dyn boss_core::platform_owner::PlatformOwner>,
}

impl PolicyCheckRefusalsAlarm {
    pub fn new(
        policy_base: impl Into<String>,
        jobs_base: impl Into<String>,
        owner: Arc<dyn boss_core::platform_owner::PlatformOwner>,
    ) -> Arc<Self> {
        Arc::new(Self {
            client: api_client(),
            policy_base: policy_base.into().trim_end_matches('/').to_string(),
            jobs_base: jobs_base.into().trim_end_matches('/').to_string(),
            owner,
        })
    }

    /// File `body` unless an open alarm already names `finding`; an open
    /// one told it had recovered is told it did not.
    async fn raise(
        &self,
        finding: &str,
        body: Json,
        filed: &BTreeMap<String, Json>,
        rule: &str,
    ) -> Result<(), HandlerError> {
        match filed.get(finding) {
            Some(alarm)
                if alarm
                    .pointer(&format!("/metadata/{RECOVERED_AT}"))
                    .is_some_and(|v| !v.is_null()) =>
            {
                let id = alarm.get("id").and_then(Json::as_str).unwrap_or_default();
                write_json(
                    &self.client,
                    reqwest::Method::PATCH,
                    &format!("{}/api/jobs/{id}/metadata", self.jobs_base),
                    &relapse_patch(),
                    rule,
                )
                .await
            }
            Some(_) => Ok(()),
            None => {
                post_json(
                    &self.client,
                    &format!("{}/api/jobs", self.jobs_base),
                    &body,
                    rule,
                )
                .await
            }
        }
    }

    /// Withdraw the open alarm for `finding`, if there is one, at the
    /// step it waits on (or by a note when no step may be completed).
    async fn withdraw(
        &self,
        finding: &str,
        filed: &BTreeMap<String, Json>,
        evidence: &str,
        now: &str,
        rule: &str,
    ) -> Result<(), HandlerError> {
        let Some(alarm) = filed.get(finding) else {
            return Ok(());
        };
        let id = alarm.get("id").and_then(Json::as_str).unwrap_or_default();
        match retraction(alarm) {
            None => {
                tracing::warn!(rule, packet = %id, "{HANDLER}: the open alarm has no triage step to close");
            }
            Some(Retraction::Complete { step_id, .. }) => {
                complete_step(
                    &self.client,
                    &self.jobs_base,
                    id,
                    &step_id,
                    withdrawal_fields(evidence, HANDLER, now),
                    rule,
                )
                .await?;
            }
            Some(Retraction::Annotate { .. })
                if alarm
                    .pointer(&format!("/metadata/{RECOVERED_AT}"))
                    .is_some_and(|v| !v.is_null()) => {}
            Some(Retraction::Annotate { why_open }) => {
                write_json(
                    &self.client,
                    reqwest::Method::PATCH,
                    &format!("{}/api/jobs/{id}/metadata", self.jobs_base),
                    &Json::Object(recovery_note(evidence, HANDLER, now, &why_open)),
                    rule,
                )
                .await?;
            }
        }
        Ok(())
    }
}

#[async_trait]
impl Handler for PolicyCheckRefusalsAlarm {
    fn name(&self) -> &'static str {
        HANDLER
    }

    async fn invoke(
        &self,
        _args: &[(String, Value)],
        ctx: &InvocationContext,
    ) -> Result<(), HandlerError> {
        let rule = ctx.rule_name.as_str();
        let now = ctx
            .event_payload
            .get(TICK_AT)
            .and_then(Json::as_str)
            .map(str::to_string)
            .ok_or_else(|| {
                HandlerError::Permanent(format!(
                    "the firing carries no `{TICK_AT}` — {HANDLER} judges `last_seen` against the \
                     tick's instant and needs a sub-day cadence (hourly, every-<n>-minutes)"
                ))
            })?;
        let at = instant(&now, TICK_AT).map_err(HandlerError::Permanent)?;

        // The dedup read first: a dark tally read still needs it, to keep
        // its one UNREADABLE alarm.
        let filed = alarms_by_finding(
            &jobs_where(
                &self.client,
                &self.jobs_base,
                &format!("kind=backlog-item&status=open&metadata_has={FINDING_KEY}"),
                rule,
            )
            .await?,
        );
        let owner = owner_for_filing(self.owner.as_ref(), rule).await;

        let read = match get_json(
            &self.client,
            &format!("{}{REFUSALS_PATH}", self.policy_base),
            rule,
        )
        .await
        {
            Ok(body) => judge(&body, at).map_err(HandlerError::Downstream),
            Err(e) => Err(e),
        };
        let judged = match read {
            Ok(judged) => judged,
            Err(e) => {
                let body = unreadable_body(&e.to_string(), &owner, &now, rule);
                self.raise(UNREADABLE, body, &filed, rule).await?;
                return Err(e);
            }
        };
        let whole = format!("{HANDLER} read {REFUSALS_PATH} whole at {now}");
        self.withdraw(
            UNREADABLE,
            &filed,
            &format!("{whole}: the lapsed-grant watch judges again."),
            &now,
            rule,
        )
        .await?;

        if judged.mode == "off" {
            tracing::info!(
                rule,
                "{HANDLER}: the policy check mode is `off`, which tallies nothing; no alarm is \
                 raised or withdrawn on a tally that watched nothing"
            );
            return Ok(());
        }

        if judged.lapsed.is_empty() && !judged.watched_lookback {
            // A tally that began inside the lookback (every train
            // Recreates the boss pod) watched only part of it: its
            // silence is no recovery, so the open alarm holds (review
            // b14f0e1b, B1). Nothing is raised either: no row, no lapse.
            tracing::info!(
                rule,
                recording_since = %judged.recording_since,
                "{HANDLER}: the tally began inside the last {LOOKBACK_MINUTES} minutes; an \
                 open lapsed-grant alarm is neither withdrawn nor raised on it"
            );
        } else if judged.lapsed.is_empty() {
            self.withdraw(
                LAPSED,
                &filed,
                &format!(
                    "{whole}: no service caller refused within the last {LOOKBACK_MINUTES} \
                     minutes (mode {}, recording since {}).",
                    judged.mode, judged.recording_since
                ),
                &now,
                rule,
            )
            .await?;
        } else {
            let body = lapsed_body(&judged, &owner, &now, rule);
            self.raise(LAPSED, body, &filed, rule).await?;
        }
        if judged.overflow == 0 {
            self.withdraw(
                OVERFLOW,
                &filed,
                &format!(
                    "{whole}: overflow 0 (recording since {}).",
                    judged.recording_since
                ),
                &now,
                rule,
            )
            .await?;
        } else {
            let body = overflow_body(&judged, &owner, &now, rule);
            self.raise(OVERFLOW, body, &filed, rule).await?;
        }
        tracing::info!(
            rule,
            mode = %judged.mode,
            lapsed = judged.lapsed.len(),
            overflow = judged.overflow,
            "{HANDLER}: read the policy check's refusals"
        );
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::super::listing_stub;
    use super::*;

    const NOW: &str = "2026-10-01T03:00:00+00:00";
    const ALARMS: &str =
        "/api/jobs?kind=backlog-item&status=open&metadata_has=policy_check_finding";

    fn ctx() -> InvocationContext {
        InvocationContext {
            event_timestamp: None,
            rule_name: "policy-check-refusals-are-read-hourly".into(),
            triggering_event_id: format!("clock-tick:{NOW}"),
            triggering_topic: "clock.tick".into(),
            event_payload: json!({"_day": "2026-10-01", "_at": NOW}),
        }
    }

    fn row(arm: &str, caller: &str, last_seen: &str) -> Json {
        json!({"arm": arm, "caller": caller, "role": "platform-admin", "peer": "10.20.0.7",
               "count": 4, "first_seen": "2026-10-01T00:00:00Z", "last_seen": last_seen})
    }

    fn tally(mode: &str, rows: Vec<Json>, overflow: u64) -> Json {
        json!({"switch": "policy check", "file": "/etc/boss/machine-gate/policy-check",
               "mode": mode, "mode_error": null, "rows": rows, "overflow": overflow,
               "recording_since": "2026-09-30T23:00:00Z", "clean_since": null,
               "not_clean": []})
    }

    fn alarm_row(finding: &str) -> Json {
        json!({
            "id": "al-1", "kind": "backlog-item", "status": "open",
            "metadata": {FINDING_KEY: finding},
            "steps": [{"id": "al-1-triage", "spec_slug": "triage", "status": "ready"}],
        })
    }

    fn listing(rows: Vec<Json>) -> Json {
        let n = rows.len();
        json!({"data": rows, "total": n})
    }

    fn handler(stub: &listing_stub::Stub) -> Arc<PolicyCheckRefusalsAlarm> {
        PolicyCheckRefusalsAlarm::new(
            stub.base.clone(),
            stub.base.clone(),
            Arc::new(boss_core::platform_owner::Fixed("emp-owner".into())),
        )
    }

    fn at() -> DateTime<Utc> {
        instant(NOW, "now").unwrap()
    }

    /// Only a SERVICE row counted within the lookback is a lapse now: an
    /// unsigned row is the enforce window's business, and a service row
    /// last seen three hours ago is a grant since restored.
    #[test]
    fn a_lapse_is_a_service_row_seen_within_the_lookback() {
        let j = judge(
            &tally(
                "report",
                vec![
                    row("service", "automation:jobs", "2026-10-01T02:30:00Z"),
                    row("service", "automation:ledger", "2026-10-01T00:00:00Z"),
                    row("unsigned", "anonymous", "2026-10-01T02:59:00Z"),
                ],
                0,
            ),
            at(),
        )
        .unwrap();
        assert_eq!(j.lapsed.len(), 1, "{j:?}");
        assert_eq!(j.lapsed[0].caller, "automation:jobs");
    }

    #[test]
    fn a_read_that_is_not_a_whole_answer_is_refused() {
        for k in ["mode", "rows", "overflow", "recording_since"] {
            let mut t = tally("report", vec![], 0);
            t.as_object_mut().unwrap().remove(k);
            assert!(judge(&t, at()).unwrap_err().contains(k), "{k}");
        }
        // A service row with no time cannot be judged as recent or not.
        let mut r = row("service", "automation:jobs", "x");
        r.as_object_mut().unwrap().remove("last_seen");
        assert!(
            judge(&tally("report", vec![r], 0), at())
                .unwrap_err()
                .contains("last_seen")
        );
    }

    #[tokio::test]
    async fn a_lapsed_service_grant_files_one_alarm_naming_it() {
        let stub = listing_stub::serve(vec![
            (
                REFUSALS_PATH,
                tally(
                    "report",
                    vec![row("service", "automation:jobs", "2026-10-01T02:55:00Z")],
                    0,
                ),
            ),
            (ALARMS, listing(vec![])),
        ])
        .await;
        handler(&stub).invoke(&[], &ctx()).await.unwrap();
        let sent = stub.sent();
        assert_eq!(sent.len(), 1, "{sent:?}");
        assert_eq!(sent[0].0, "POST /api/jobs");
        let b = &sent[0].1;
        assert_eq!(b["priority"], "urgent");
        assert_eq!(b["metadata"][FINDING_KEY], LAPSED);
        assert_eq!(b["metadata"]["area"], "policy");
        let detail = b["metadata"]["detail"].as_str().unwrap();
        assert!(
            detail.contains("automation:jobs")
                && detail.contains("platform-admin:policy-rule:read"),
            "{detail}"
        );
        assert!(detail.contains("refuses nothing"), "{detail}");
    }

    /// Forged ids from one peer are ONE alarm, bounded — a spray on the
    /// port cannot file a thousand packets (checklist F2).
    #[tokio::test]
    async fn a_spray_of_callers_is_one_bounded_alarm() {
        let rows = (0..40)
            .map(|i| {
                row(
                    "service",
                    &format!("automation:spray-{i}"),
                    "2026-10-01T02:59:00Z",
                )
            })
            .collect();
        let stub = listing_stub::serve(vec![
            (REFUSALS_PATH, tally("report", rows, 0)),
            (ALARMS, listing(vec![])),
        ])
        .await;
        handler(&stub).invoke(&[], &ctx()).await.unwrap();
        let sent = stub.sent();
        assert_eq!(sent.len(), 1, "{sent:?}");
        let m = &sent[0].1["metadata"];
        assert_eq!(m["caller_count"], 40);
        assert_eq!(m["callers"].as_array().map(Vec::len), Some(LISTED));
        assert!(m["detail"].as_str().unwrap().contains("and 28 more"));
    }

    #[tokio::test]
    async fn an_open_alarm_is_not_filed_again() {
        let stub = listing_stub::serve(vec![
            (
                REFUSALS_PATH,
                tally(
                    "report",
                    vec![row("service", "automation:jobs", "2026-10-01T02:55:00Z")],
                    0,
                ),
            ),
            (ALARMS, listing(vec![alarm_row(LAPSED)])),
        ])
        .await;
        handler(&stub).invoke(&[], &ctx()).await.unwrap();
        assert!(stub.writes().is_empty(), "{:?}", stub.writes());
    }

    #[tokio::test]
    async fn a_restored_grant_withdraws_its_alarm() {
        let stub = listing_stub::serve(vec![
            (
                REFUSALS_PATH,
                tally(
                    "report",
                    vec![row("service", "automation:jobs", "2026-09-30T20:00:00Z")],
                    0,
                ),
            ),
            (ALARMS, listing(vec![alarm_row(LAPSED)])),
        ])
        .await;
        handler(&stub).invoke(&[], &ctx()).await.unwrap();
        let sent = stub.sent();
        assert_eq!(sent.len(), 2, "the merge, then the flip: {sent:?}");
        assert_eq!(sent[0].0, "PATCH /api/jobs/al-1/steps/al-1-triage/metadata");
        assert_eq!(sent[0].1["disposition"], "stale");
        assert_eq!(sent[0].1["cleared_by"], HANDLER);
    }

    /// Review b14f0e1b, B1: every train Recreates the boss pod, which
    /// empties the policy service's tally. An empty tally that began
    /// INSIDE the lookback watched only part of it, so it holds the
    /// open LAPSED alarm — no withdrawal, no "nobody refused" written
    /// over a refusal the alarm itself recorded an hour earlier.
    #[tokio::test]
    async fn a_tally_younger_than_the_lookback_withdraws_no_lapse() {
        let mut young = tally("report", vec![], 0);
        young["recording_since"] = json!("2026-10-01T02:30:00Z");
        let stub = listing_stub::serve(vec![
            (REFUSALS_PATH, young),
            (ALARMS, listing(vec![alarm_row(LAPSED)])),
        ])
        .await;
        handler(&stub).invoke(&[], &ctx()).await.unwrap();
        assert!(stub.writes().is_empty(), "{:?}", stub.writes());
    }

    /// Overflow is its own alarm: the tally is full, so a lapse now
    /// names nobody, and the window is not clean (checklist F2).
    #[tokio::test]
    async fn a_full_tally_files_the_overflow_alarm() {
        let stub = listing_stub::serve(vec![
            (REFUSALS_PATH, tally("report", vec![], 3)),
            (ALARMS, listing(vec![])),
        ])
        .await;
        handler(&stub).invoke(&[], &ctx()).await.unwrap();
        let sent = stub.sent();
        assert_eq!(sent.len(), 1, "{sent:?}");
        assert_eq!(sent[0].1["metadata"][FINDING_KEY], OVERFLOW);
        assert_eq!(sent[0].1["metadata"]["overflow"], 3);
    }

    /// `off` tallies nothing: its empty tally withdraws no alarm.
    #[tokio::test]
    async fn an_off_tally_withdraws_nothing() {
        let stub = listing_stub::serve(vec![
            (REFUSALS_PATH, tally("off", vec![], 0)),
            (
                ALARMS,
                listing(vec![alarm_row(LAPSED), {
                    let mut a = alarm_row(OVERFLOW);
                    a["id"] = json!("al-2");
                    a
                }]),
            ),
        ])
        .await;
        handler(&stub).invoke(&[], &ctx()).await.unwrap();
        assert!(stub.writes().is_empty(), "{:?}", stub.writes());
    }

    #[tokio::test]
    async fn a_refused_read_is_said_once_and_withdraws_nothing() {
        // No route for the tally: the stub answers 404.
        let stub = listing_stub::serve(vec![(ALARMS, listing(vec![alarm_row(LAPSED)]))]).await;
        assert!(handler(&stub).invoke(&[], &ctx()).await.is_err());
        let sent = stub.sent();
        assert_eq!(sent.len(), 1, "one alarm, and no withdrawal: {sent:?}");
        assert_eq!(sent[0].1["metadata"][FINDING_KEY], UNREADABLE);
        assert!(
            sent[0].1["metadata"]["last_error"]
                .as_str()
                .unwrap()
                .contains("404")
        );
    }

    #[tokio::test]
    async fn a_whole_read_withdraws_the_unreadable_alarm() {
        let stub = listing_stub::serve(vec![
            (REFUSALS_PATH, tally("report", vec![], 0)),
            (ALARMS, listing(vec![alarm_row(UNREADABLE)])),
        ])
        .await;
        handler(&stub).invoke(&[], &ctx()).await.unwrap();
        let sent = stub.sent();
        assert_eq!(sent.len(), 2, "{sent:?}");
        assert!(
            sent[0].1["evidence"]
                .as_str()
                .unwrap()
                .contains("read /api/policy/check/refusals whole")
        );
    }

    #[tokio::test]
    async fn a_firing_without_an_instant_is_a_permanent_refusal() {
        let stub = listing_stub::serve(vec![]).await;
        let mut c = ctx();
        c.event_payload = json!({"_day": "2026-10-01"});
        let err = handler(&stub).invoke(&[], &c).await.unwrap_err();
        assert!(matches!(err, HandlerError::Permanent(ref m) if m.contains("_at")));
    }

    /// §9a: the path and the wire shape are the policy service's own —
    /// the tally a real `CheckMode` serializes is what `judge` reads.
    #[test]
    fn the_handler_reads_what_the_policy_service_serves() {
        use boss_policy::check_mode::{self, Arm, CheckMode, Mode};
        use boss_policy_client::types::User;
        assert_eq!(REFUSALS_PATH, check_mode::REFUSALS_PATH);
        let mode = CheckMode::fixed(Mode::Report);
        mode.record(Arm::Service, &User::service("jobs"), None);
        mode.record(Arm::Unsigned, &User::anonymous(), None);
        let wire = serde_json::to_value(mode.refusals()).unwrap();
        // Judged at the instant the rows were counted, so the test reads
        // no clock of its own.
        let counted = wire["rows"][0]["last_seen"].as_str().unwrap();
        let j = judge(&wire, instant(counted, "last_seen").unwrap())
            .expect("the served tally is a whole answer");
        assert_eq!(j.mode, "report");
        assert_eq!(j.lapsed.len(), 1, "{j:?}");
        assert_eq!(j.lapsed[0].caller, "automation:jobs");
    }

    #[test]
    fn the_handler_is_registered_under_its_name() {
        let h = PolicyCheckRefusalsAlarm::new(
            "http://unused",
            "http://unused",
            Arc::new(boss_core::platform_owner::Fixed("emp-owner".into())),
        );
        assert_eq!(h.name(), HANDLER);
        assert!(
            crate::cascade::handler_emits().contains_key(HANDLER),
            "the cascade table knows what this handler emits"
        );
        let raw =
            boss_dispatcher::rules::registry::parse_raw_path(boss_testing::dispatcher_rules_dir())
                .expect("parse the shipped rule registry");
        assert!(
            raw.rules
                .iter()
                .any(|r| r.do_steps.iter().any(|d| d.handler == HANDLER)),
            "a shipped rule invokes this handler"
        );
    }
}
