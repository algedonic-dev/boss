//! `policy.coverage.alarm` — the backstop: a control no real person
//! holds files an alarm (design 1c4e42e1; backlog 47aed706).
//!
//! WHY A BACKSTOP. The coverage invariant — every control a door checks
//! is held by an active employee with a bound passkey — can break by
//! routes no write path sees: a migration, a seed, a restore, a
//! `tenant publish --force`, a person's last key revoked. The guards
//! that will refuse an orphaning write wait for DR readiness
//! (62dac114); this read does not refuse anything, so it ships first.
//! Hourly it reads `GET /api/policy/coverage` and files ONE urgent
//! backlog-item per orphaned control, deduped on the control's id
//! ([`CONTROL_KEY`]) against the OPEN alarms, and withdraws an open one
//! once the read shows the control held again.
//!
//! WHY ONLY THE OPEN ALARMS DEDUP. An orphaned control is a standing
//! fact of the record, not an event that happened once: a person who
//! closes the alarm while nobody still holds the control gets it back
//! on the next read. The remedy for a gap is a holder, never a close.
//!
//! WHAT IT FILES TODAY. On 2026-09-29 the read's one orphan is the
//! operator tier: no real person holds an operator-tier passkey, so no
//! person can raise a session to operator. Its alarm names the remedy
//! the coverage core carries — the passkey-promotion door (2a228d0c)
//! and the register fix (1d9970d1).
//!
//! A READ THAT CANNOT BE TRUSTED IS AN ERROR, NEVER "ALL COVERED". The
//! coverage read refuses a dark roster itself (502); here a body with no
//! `orphans` array, or no `controls` at all, is refused by name, so a
//! changed contract cannot withdraw every alarm as recovered.
//!
//! AND IT IS SAID, NOT SWALLOWED (review of this car, M2). A dark read
//! keeps ONE alarm, [`UNREADABLE`], naming the error, and the firing
//! still fails — so a successful firing on the record means a whole read
//! was judged, which is what the car's proof reads. The next whole read
//! withdraws it. The alarms already open are left as they are: a dark
//! read is no evidence that anything recovered.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use async_trait::async_trait;
use boss_dispatcher::rules::expr::Value;
use boss_dispatcher::rules::handler::{Handler, HandlerError, InvocationContext};
use boss_jobs::channels::InputChannel;
use serde_json::{Value as Json, json};

use super::common::{
    RECOVERED_AT, Retraction, api_client, complete_step, get_json, jobs_where, owner_for_filing,
    post_json, recovery_note, relapse_patch, retraction, with_lane, withdrawal_fields, write_json,
};

/// The handler's registered name.
pub const HANDLER: &str = "policy.coverage.alarm";

/// The read this handler judges from, on the policy service.
pub const COVERAGE_PATH: &str = "/api/policy/coverage";

/// The `scope` every alarm this handler files carries.
pub const SCOPE: &str = "orphaned-control";

/// The packet-metadata key holding the orphaned control's id — the
/// dedup key.
pub const CONTROL_KEY: &str = "orphaned_control";

/// The one alarm a dark coverage read keeps, under [`CONTROL_KEY`]
/// (review of this car, M2). Without it a read that stayed 502 — the
/// tier counts turned 403 by a gate change, the people service down —
/// stopped all new orphan detection and said nothing: a dead-letter
/// raises no packet, and this rule is on the silence sweep's exempt
/// list. Withdrawn by the next whole read.
pub const UNREADABLE: &str = "coverage-unreadable";

/// The alarm a dark read files, naming the error it got.
pub fn unreadable_body(error: &str, owner: &str, now: &str, rule: &str) -> Json {
    let detail = format!(
        "Raised by {HANDLER} (rule {rule}, design 1c4e42e1, backlog 47aed706) at {now}: GET \
         {COVERAGE_PATH} could not be judged — {error}. While it stays so, no control that loses \
         its last real holder is alarmed; the alarms already open stay open. Read the policy \
         service's answer (a 502 names the source it could not read) and repair that read. \
         Withdrawn by the next whole read."
    );
    json!({
        "kind": "backlog-item",
        "title": "COVERAGE UNREADABLE: the backstop cannot say who holds the controls",
        "subject": {"subject_kind": "custom", "id": UNREADABLE},
        "owner_id": owner,
        "priority": "urgent",
        "status": "open",
        "tags": [],
        "metadata": with_lane(json!({
            "area": "policy",
            "scope": SCOPE,
            CONTROL_KEY: UNREADABLE,
            "control_kind": "backstop",
            "last_error": error,
            "measured_at": now,
            "detail": detail,
        }), InputChannel::Telemetry),
    })
}

/// The key the schedule runner writes the firing instant under on every
/// sub-day tick.
const TICK_AT: &str = "_at";

/// One control the read reports no real person holds.
#[derive(Debug, Clone, PartialEq)]
pub struct Orphan {
    pub control: String,
    pub kind: String,
    pub wants: String,
    pub remedy: Option<String>,
}

/// The read, judged: the orphans, and every control id it reports with
/// its holders. Refuses a body that is not a whole answer.
pub fn judge(body: &Json) -> Result<(Vec<Orphan>, BTreeMap<String, Vec<String>>), String> {
    let text = |v: &Json, k: &str| v.get(k).and_then(Json::as_str).map(str::to_string);
    let controls = body
        .get("controls")
        .and_then(Json::as_array)
        .filter(|c| !c.is_empty())
        .ok_or_else(|| {
            format!(
                "GET {COVERAGE_PATH} answered no `controls` — a read that lists no control is \
                 no answer, and judging it would withdraw every alarm as covered"
            )
        })?;
    let orphans = body
        .get("orphans")
        .and_then(Json::as_array)
        .ok_or_else(|| format!("GET {COVERAGE_PATH} answered no `orphans` array"))?;
    let held = controls
        .iter()
        .filter_map(|c| {
            let holders = c
                .get("holders")
                .and_then(Json::as_array)?
                .iter()
                .filter_map(Json::as_str)
                .map(str::to_string)
                .collect();
            Some((text(c, "control")?, holders))
        })
        .collect();
    let orphans = orphans
        .iter()
        .filter_map(|o| {
            Some(Orphan {
                control: text(o, "control")?,
                kind: text(o, "kind").unwrap_or_default(),
                wants: text(o, "wants").unwrap_or_default(),
                remedy: text(o, "remedy"),
            })
        })
        .collect();
    Ok((orphans, held))
}

/// The alarm one orphaned control files.
pub fn alarm_body(o: &Orphan, owner: &str, now: &str, rule: &str) -> Json {
    let remedy = o.remedy.clone().unwrap_or_else(|| {
        "grant it to a real person — a rule for their role, or an override, through \
         /api/policy/rules — or give a person who holds it a passkey"
            .to_string()
    });
    let detail = format!(
        "Raised by {HANDLER} (rule {rule}, design 1c4e42e1, backlog 47aed706) at {now}: GET \
         {COVERAGE_PATH} reports the {kind} control `{control}` held by no real person — an \
         active employee with at least one bound passkey. It wants {wants}. Remedy: {remedy}. \
         This alarm refuses nothing; it is withdrawn when the next read shows the control held.",
        kind = o.kind,
        control = o.control,
        wants = o.wants,
    );
    json!({
        "kind": "backlog-item",
        "title": format!("ORPHANED CONTROL: `{}` — no real person holds it", o.control),
        "subject": {"subject_kind": "custom", "id": o.control},
        "owner_id": owner,
        "priority": "urgent",
        "status": "open",
        "tags": [],
        "metadata": with_lane(json!({
            "area": "policy",
            "scope": SCOPE,
            CONTROL_KEY: o.control,
            "control_kind": o.kind,
            "wants": o.wants,
            "remedy": remedy,
            "measured_at": now,
            "detail": detail,
        }), InputChannel::Telemetry),
    })
}

/// Every OPEN alarm this handler filed, keyed by the control it names.
pub fn alarms_by_control(rows: &[Json]) -> BTreeMap<String, Json> {
    rows.iter()
        .filter(|r| r.get("status").and_then(Json::as_str) == Some("open"))
        .filter_map(|r| {
            let c = r
                .pointer(&format!("/metadata/{CONTROL_KEY}"))
                .and_then(Json::as_str)
                .filter(|s| !s.is_empty())?;
            Some((c.to_string(), r.clone()))
        })
        .collect()
}

pub struct PolicyCoverageAlarm {
    /// The machine client: every call goes to a BOSS host (the policy
    /// service, the jobs API), which the estate token is for.
    client: boss_core::machine_token::Client,
    policy_base: String,
    jobs_base: String,
    owner: Arc<dyn boss_core::platform_owner::PlatformOwner>,
}

impl PolicyCoverageAlarm {
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

    /// File `body` for `control` unless an open alarm already names it;
    /// an open one told it had recovered is told it did not. True when
    /// a new alarm was filed.
    async fn raise(
        &self,
        control: &str,
        body: Json,
        filed: &BTreeMap<String, Json>,
        rule: &str,
    ) -> Result<bool, HandlerError> {
        match filed.get(control) {
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
                .await?;
                Ok(false)
            }
            Some(_) => Ok(false),
            None => {
                post_json(
                    &self.client,
                    &format!("{}/api/jobs", self.jobs_base),
                    &body,
                    rule,
                )
                .await?;
                Ok(true)
            }
        }
    }

    async fn withdraw(
        &self,
        control: &str,
        holders: Option<&Vec<String>>,
        alarm: &Json,
        now: &str,
        rule: &str,
    ) -> Result<(), HandlerError> {
        let id = alarm.get("id").and_then(Json::as_str).unwrap_or_default();
        let evidence = match holders {
            _ if control == UNREADABLE => {
                format!("{HANDLER} read {COVERAGE_PATH} whole at {now}: the backstop judges again.")
            }
            Some(h) => format!(
                "{HANDLER} read {COVERAGE_PATH} at {now}: `{control}` is held by {}.",
                h.join(", ")
            ),
            None => format!(
                "{HANDLER} read {COVERAGE_PATH} at {now}: `{control}` is no longer a control \
                 any door or active workflow asks for."
            ),
        };
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
                    withdrawal_fields(&evidence, HANDLER, now),
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
                    &Json::Object(recovery_note(&evidence, HANDLER, now, &why_open)),
                    rule,
                )
                .await?;
            }
        }
        Ok(())
    }
}

#[async_trait]
impl Handler for PolicyCoverageAlarm {
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
                    "the firing carries no `{TICK_AT}` — {HANDLER} stamps its reading with the \
                     tick's instant and needs a sub-day cadence (hourly, every-<n>-minutes)"
                ))
            })?;

        // The dedup read first: a dark coverage read still needs it, to
        // keep its one UNREADABLE alarm (review M2).
        let filed = alarms_by_control(
            &jobs_where(
                &self.client,
                &self.jobs_base,
                &format!("kind=backlog-item&status=open&metadata_has={CONTROL_KEY}"),
                rule,
            )
            .await?,
        );
        let owner = owner_for_filing(self.owner.as_ref(), rule).await;

        let read = match get_json(
            &self.client,
            &format!("{}{COVERAGE_PATH}", self.policy_base),
            rule,
        )
        .await
        {
            Ok(body) => judge(&body).map_err(HandlerError::Downstream),
            Err(e) => Err(e),
        };
        let (orphans, held) = match read {
            Ok(judged) => judged,
            Err(e) => {
                // A dark read is said, not swallowed: ONE alarm keyed
                // UNREADABLE, naming the error. The firing still fails,
                // so no successful firing is recorded for a read that
                // judged nothing — which is what the proof probe reads.
                let body = unreadable_body(&e.to_string(), &owner, &now, rule);
                self.raise(UNREADABLE, body, &filed, rule).await?;
                return Err(e);
            }
        };

        let mut raised = 0usize;
        for o in &orphans {
            if self
                .raise(&o.control, alarm_body(o, &owner, &now, rule), &filed, rule)
                .await?
            {
                raised += 1;
            }
        }

        let orphaned: BTreeSet<&str> = orphans.iter().map(|o| o.control.as_str()).collect();
        let mut withdrawn = 0usize;
        for (control, alarm) in &filed {
            if orphaned.contains(control.as_str()) {
                continue;
            }
            self.withdraw(control, held.get(control), alarm, &now, rule)
                .await?;
            withdrawn += 1;
        }
        tracing::info!(
            rule,
            controls = held.len(),
            orphans = orphans.len(),
            raised,
            withdrawn,
            "{HANDLER}: read the coverage"
        );
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::super::listing_stub;
    use super::*;

    const NOW: &str = "2026-09-29T03:00:00+00:00";
    const ALARMS: &str = "/api/jobs?kind=backlog-item&status=open&metadata_has=orphaned_control";

    fn ctx() -> InvocationContext {
        InvocationContext {
            event_timestamp: None,
            rule_name: "every-control-has-a-real-person-hourly".into(),
            triggering_event_id: format!("clock-tick:{NOW}"),
            triggering_topic: "clock.tick".into(),
            event_payload: json!({"_day": "2026-09-29", "_at": NOW}),
        }
    }

    /// The read as the policy service answers it on this instance: the
    /// founder holds everything but the operator tier.
    fn coverage(orphans: Vec<Json>) -> Json {
        json!({
            "active_people": 2,
            "real_people": ["emp-founder"],
            "total": 3,
            "controls": [
                {"control": "operator-tier", "kind": "operator-tier", "wants": "w", "holders": []},
                {"control": "platform-owner", "kind": "platform-owner", "wants": "w",
                 "holders": ["emp-founder"]},
                {"control": "policy:create:class", "kind": "policy", "wants": "w",
                 "holders": ["emp-founder"]},
            ],
            "orphans": orphans,
        })
    }

    fn operator_tier() -> Json {
        json!({"control": "operator-tier", "kind": "operator-tier",
               "wants": "a real person with role platform-admin and an operator-tier passkey",
               "remedy": "promote a key (backlog 2a228d0c; 1d9970d1)"})
    }

    fn alarm_row(control: &str, recovered: bool) -> Json {
        let mut metadata = json!({"scope": SCOPE, CONTROL_KEY: control});
        if recovered {
            metadata[RECOVERED_AT] = json!("2026-09-29T02:00:00Z");
        }
        json!({
            "id": "al-1", "kind": "backlog-item", "status": "open", "metadata": metadata,
            "steps": [{"id": "al-1-triage", "spec_slug": "triage", "status": "ready"}],
        })
    }

    fn listing(rows: Vec<Json>) -> Json {
        let n = rows.len();
        json!({"data": rows, "total": n})
    }

    fn handler(stub: &listing_stub::Stub) -> Arc<PolicyCoverageAlarm> {
        PolicyCoverageAlarm::new(
            stub.base.clone(),
            stub.base.clone(),
            Arc::new(boss_core::platform_owner::Fixed("emp-owner".into())),
        )
    }

    #[test]
    fn the_alarm_names_the_control_what_it_wants_and_the_remedy() {
        let (orphans, _) = judge(&coverage(vec![operator_tier()])).unwrap();
        let body = alarm_body(&orphans[0], "emp-owner", NOW, "r");
        assert_eq!(body["kind"], "backlog-item");
        assert_eq!(body["priority"], "urgent");
        assert_eq!(body["owner_id"], "emp-owner");
        let m = &body["metadata"];
        assert_eq!(m[CONTROL_KEY], "operator-tier");
        assert_eq!(m["scope"], SCOPE);
        assert_eq!(m["area"], "policy");
        let detail = m["detail"].as_str().unwrap();
        assert!(
            detail.contains("2a228d0c") && detail.contains("1d9970d1"),
            "{detail}"
        );
        assert!(detail.contains("refuses nothing"));
        assert!(
            body["title"].as_str().unwrap().contains("`operator-tier`"),
            "{}",
            body["title"]
        );
    }

    #[test]
    fn a_read_that_is_not_a_whole_answer_is_refused() {
        assert!(
            judge(&json!({"orphans": []}))
                .unwrap_err()
                .contains("controls")
        );
        assert!(
            judge(&json!({"controls": [], "orphans": []}))
                .unwrap_err()
                .contains("controls")
        );
        let mut no_orphans = coverage(vec![]);
        no_orphans.as_object_mut().unwrap().remove("orphans");
        assert!(judge(&no_orphans).unwrap_err().contains("orphans"));
    }

    #[tokio::test]
    async fn an_orphaned_control_files_one_alarm() {
        let stub = listing_stub::serve(vec![
            (COVERAGE_PATH, coverage(vec![operator_tier()])),
            (ALARMS, listing(vec![])),
        ])
        .await;
        handler(&stub).invoke(&[], &ctx()).await.unwrap();
        let sent = stub.sent();
        assert_eq!(sent.len(), 1, "{sent:?}");
        assert_eq!(sent[0].0, "POST /api/jobs");
        assert_eq!(sent[0].1["metadata"][CONTROL_KEY], "operator-tier");
    }

    #[tokio::test]
    async fn an_open_alarm_for_the_control_is_not_filed_again() {
        let stub = listing_stub::serve(vec![
            (COVERAGE_PATH, coverage(vec![operator_tier()])),
            (ALARMS, listing(vec![alarm_row("operator-tier", false)])),
        ])
        .await;
        handler(&stub).invoke(&[], &ctx()).await.unwrap();
        assert!(stub.writes().is_empty(), "{:?}", stub.writes());
    }

    /// A control held again withdraws its open alarm at the step it
    /// waits on, naming who holds it now.
    #[tokio::test]
    async fn a_covered_control_withdraws_its_open_alarm() {
        let stub = listing_stub::serve(vec![
            (COVERAGE_PATH, coverage(vec![])),
            (ALARMS, listing(vec![alarm_row("platform-owner", false)])),
        ])
        .await;
        handler(&stub).invoke(&[], &ctx()).await.unwrap();
        let sent = stub.sent();
        assert_eq!(sent.len(), 2, "the merge, then the flip: {sent:?}");
        assert_eq!(sent[0].0, "PATCH /api/jobs/al-1/steps/al-1-triage/metadata");
        assert_eq!(sent[0].1["disposition"], "stale");
        assert_eq!(sent[0].1["cleared_by"], HANDLER);
        assert!(
            sent[0].1["evidence"]
                .as_str()
                .unwrap()
                .contains("held by emp-founder")
        );
        assert_eq!(sent[1].0, "PUT /api/jobs/al-1/steps/al-1-triage");
    }

    /// Orphaned again after a recovery note: the note is withdrawn, and
    /// no second alarm is filed.
    #[tokio::test]
    async fn a_relapse_takes_back_the_recovery_note() {
        let stub = listing_stub::serve(vec![
            (COVERAGE_PATH, coverage(vec![operator_tier()])),
            (ALARMS, listing(vec![alarm_row("operator-tier", true)])),
        ])
        .await;
        handler(&stub).invoke(&[], &ctx()).await.unwrap();
        let sent = stub.sent();
        assert_eq!(sent.len(), 1, "{sent:?}");
        assert_eq!(sent[0].0, "PATCH /api/jobs/al-1/metadata");
        assert!(sent[0].1[RECOVERED_AT].is_null());
    }

    #[tokio::test]
    async fn a_dark_coverage_read_files_one_unreadable_alarm_and_withdraws_nothing() {
        let stub = listing_stub::serve(vec![
            (COVERAGE_PATH, json!({"error": "coverage cannot be judged"})),
            (ALARMS, listing(vec![alarm_row("platform-owner", false)])),
        ])
        .await;
        let err = handler(&stub).invoke(&[], &ctx()).await.unwrap_err();
        assert!(matches!(err, HandlerError::Downstream(ref m) if m.contains("controls")));
        let sent = stub.sent();
        assert_eq!(sent.len(), 1, "one alarm, and no withdrawal: {sent:?}");
        assert_eq!(sent[0].0, "POST /api/jobs");
        let m = &sent[0].1["metadata"];
        assert_eq!(m[CONTROL_KEY], UNREADABLE);
        assert!(
            m["last_error"].as_str().unwrap().contains("controls"),
            "the alarm names the error: {m}"
        );
    }

    /// A read the policy service refuses (502, 403) is the same: said.
    #[tokio::test]
    async fn a_refused_coverage_read_is_said_once() {
        // No route for the coverage path: the stub answers 404.
        let stub = listing_stub::serve(vec![(ALARMS, listing(vec![]))]).await;
        assert!(handler(&stub).invoke(&[], &ctx()).await.is_err());
        let sent = stub.sent();
        assert_eq!(sent.len(), 1, "{sent:?}");
        assert_eq!(sent[0].1["metadata"][CONTROL_KEY], UNREADABLE);
        assert!(
            sent[0].1["metadata"]["last_error"]
                .as_str()
                .unwrap()
                .contains("404"),
            "{:?}",
            sent[0].1
        );

        // Still dark on the next tick, its alarm open: nothing new.
        let stub =
            listing_stub::serve(vec![(ALARMS, listing(vec![alarm_row(UNREADABLE, false)]))]).await;
        assert!(handler(&stub).invoke(&[], &ctx()).await.is_err());
        assert!(stub.writes().is_empty(), "{:?}", stub.writes());
    }

    /// The next whole read withdraws the unreadable alarm.
    #[tokio::test]
    async fn a_whole_read_withdraws_the_unreadable_alarm() {
        let stub = listing_stub::serve(vec![
            (COVERAGE_PATH, coverage(vec![])),
            (ALARMS, listing(vec![alarm_row(UNREADABLE, false)])),
        ])
        .await;
        handler(&stub).invoke(&[], &ctx()).await.unwrap();
        let sent = stub.sent();
        assert_eq!(sent.len(), 2, "{sent:?}");
        assert_eq!(sent[0].1["disposition"], "stale");
        assert!(
            sent[0].1["evidence"]
                .as_str()
                .unwrap()
                .contains("read /api/policy/coverage whole"),
            "{:?}",
            sent[0].1
        );
    }

    #[tokio::test]
    async fn a_daily_firing_without_an_instant_is_a_permanent_refusal() {
        let stub = listing_stub::serve(vec![]).await;
        let mut c = ctx();
        c.event_payload = json!({"_day": "2026-09-29"});
        let err = handler(&stub).invoke(&[], &c).await.unwrap_err();
        assert!(matches!(err, HandlerError::Permanent(ref m) if m.contains("_at")));
    }

    #[test]
    fn the_handler_is_registered_under_its_name() {
        let h = PolicyCoverageAlarm::new(
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
