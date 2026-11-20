//! A builder's slot is released when its car is HANDED TO THE GATE, not
//! after the car has waited in the gate line (backlog a99dd722; design
//! 012822ea allocation item 3, David 2026-11-06 15:13Z: "Let's see how it
//! works releasing at gate submit" — a TRIAL).
//!
//! THE MEASUREMENT. The per-actor cap counted an actor's OPEN `agent-run`
//! packets, and a run stays open until its report lands, which is after
//! its gate went green. So a run whose car sat 45+ minutes in the gate
//! line held one of the actor's slots while the actor did nothing on it.
//! Under the three-layer frame a car in the gate line is held in the gate
//! station's queue; the actor is idle.
//!
//! THE FACT. `boss gate` hands the car over through the durable submission:
//! a conditional, immutable `gate_submission` on the gate-run, naming the
//! run as its source and receipted by the server. Once that receipt is in
//! hand it copies it onto the RUN as [`KEY`] ([`record`]: receipt copied,
//! not retyped) and completes `building` `handed`, which exits the run's
//! `robots:<model>` claim. It is a recorded state, never a timer.
//!
//! THE CAP THAT FIRST READ IT IS RETIRED (backlog 3e927996; David, design
//! a3c5e58a Q3): the pool is the one bound on a robot, so the record is
//! read for the setup measurement below and the run's own surfaces, and
//! no counter takes it.
//!
//! WHAT IT DOES NOT CHANGE. The verdict still reaches the run: the green
//! completes `building` through `agent-run-lands-on-gate-green`, which
//! follows the edge on the GATE-RUN and never reads this key, and the run
//! still reports. A red gate's restart is a FRESH run on the released
//! source step (conductor decision 1507Z (2)), and that run takes a slot
//! of its pool through the normal claim. A malformed record, or one naming
//! another run, is not a hand-off.
//!
//! THE COST IT MAKES READABLE (David, same decision: "if it turns out we
//! are wasting a lot of time setting back up the station, maybe we cache
//! the station resources for a bit"). A restarted run sets its station up
//! again, so [`setup_record`] reads, from the run's own server stamps and
//! event log, how long that took from the claim, part by part, and names
//! each part the record does not carry rather than inventing it. No cache
//! is built here; that is a later decision on this measurement.

use chrono::{DateTime, Utc};
use serde_json::{Value, json};

use crate::conditional_metadata::Receipt;

/// The key on an `agent-run` packet recording that its car was handed to
/// the gate. One spelling, here: `boss gate` writes it.
pub const KEY: &str = "handed_to_gate";

/// The shape [`record`] writes and [`stamp_for`] accepts.
pub const SCHEMA: u64 = 1;

/// The `result` a run's `building` ends with when its car is handed to
/// the gate (backlog 87d1ae40): the builder station's work is done, its
/// robots:<class> claim exits, and the run waits on [`VERDICT_SLUG`].
/// One spelling for `boss gate`, which writes it, and the gate's source
/// claim, which reads a handed `building` as a claim still standing.
pub const HANDED: &str = "handed";

/// The agent-run step a handed run waits on, billing nothing, until its
/// gate's verdict completes it (`infra/platform/workflows/agent-run.toml`).
pub const VERDICT_SLUG: &str = "verdict";

/// The key on a RESTARTED run recording how long its station took to set
/// up again ([`setup_record`]); written by `boss dispatch --started`.
pub const SETUP_KEY: &str = "station_setup";

/// The hand-off record for `run`, copied from the server's receipt of the
/// durable gate submission. Refused (`Err`) when the receipt is not an
/// exact accepted submission, or its source names another run.
pub fn record(run: &str, receipt: &Receipt) -> Result<Value, String> {
    // The one acceptance check every actuator and projection shares: the
    // receipt's command, actor, immutable binding and committed packet
    // must all be the submission's own, or there is no hand-off to copy.
    accepted_record(run, &receipt.job, receipt)
}

/// Is `claimed` exactly the hand-off record of `run` that `gate` — the
/// gate-run row AS STORED — and the receipt the server RETAINED for its
/// submission vouch for? The merge door asks this of every write of
/// [`KEY`] (adversarial finding 1 of run 77fa2b8e): a record a client
/// typed, borrowed from another run's gate, or altered in any copied
/// field is refused, so the run's row holds only what the gate's own
/// record says.
pub fn verify(
    run: &boss_core::job::Job,
    claimed: &Value,
    gate: &boss_core::job::Job,
    receipt: &Receipt,
) -> Result<(), String> {
    if run.kind != crate::agent_budget::RUN_KIND {
        return Err(format!(
            "{KEY} is a fact about an agent run, and {} is a {}",
            run.id, run.kind
        ));
    }
    let expected = accepted_record(&run.id.to_string(), gate, receipt)?;
    if claimed != &expected {
        return Err(format!(
            "{KEY} on run {} does not match the record gate-run {} and its retained \
             receipt vouch for; it is written only as `boss gate` copies it",
            run.id, gate.id
        ));
    }
    Ok(())
}

/// The record of `run` handed to `gate`, built from the receipt after the
/// acceptance check holds it to `gate` as given.
fn accepted_record(
    run: &str,
    gate: &boss_core::job::Job,
    receipt: &Receipt,
) -> Result<Value, String> {
    let submission = crate::gate_reservation::acceptance(gate, receipt)?;
    let source = &submission.source;
    if source
        .get(crate::agent_runs::EDGE_KEY)
        .and_then(Value::as_str)
        != Some(run)
    {
        return Err(format!(
            "the submission accepted on {} does not name run {run}",
            receipt.job_id
        ));
    }
    let receipt_event = receipt
        .receipt_event_id
        .ok_or("the acceptance receipt names no receipt event")?;
    Ok(json!({
        "schema": SCHEMA,
        "run": run,
        "gate_run": receipt.job_id.to_string(),
        "command": submission.command.to_string(),
        "accepted_at": receipt.applied_at,
        "accepted_by": receipt.actor,
        "event_id": receipt.event_id.to_string(),
        "receipt_event_id": receipt_event.to_string(),
        "branch": source.get("branch").cloned().unwrap_or(Value::Null),
        "sha": source.get("sha").cloned().unwrap_or(Value::Null),
    }))
}

/// The gate-run a hand-off record names: the row the merge door reads to
/// [`verify`] it. Everything else it reads comes from that row — the
/// submitter and command whose retained receipt it then fetches — never
/// from the record. `None` for a record naming none, refused unread.
pub fn gate_named(claimed: &Value) -> Option<boss_core::job::JobId> {
    let gate = uuid::Uuid::parse_str(claimed.get("gate_run")?.as_str()?).ok()?;
    (!gate.is_nil()).then(|| boss_core::job::JobId::from_uuid(gate))
}

/// What the merge door, the job PUT and the create door say of a write of
/// [`KEY`] they will not take.
pub const WRITE_REFUSAL: &str = "`handed_to_gate` is written only through the metadata merge \
     door, as `boss gate` copies the server's receipt of its durable gate submission, and the \
     door verifies it against that gate-run and its retained receipt";

/// The hand-off record on `metadata` when it is well formed and names
/// `run_id` — `None` for anything else, which is not a hand-off.
pub fn stamp_for<'a>(metadata: &'a Value, run_id: &str) -> Option<&'a Value> {
    let stamp = metadata.get(KEY)?;
    let id = |field: &str| {
        stamp
            .get(field)
            .and_then(Value::as_str)
            .and_then(|text| uuid::Uuid::parse_str(text).ok())
            .is_some_and(|id| !id.is_nil())
    };
    let well_formed = stamp.get("schema").and_then(Value::as_u64) == Some(SCHEMA)
        && stamp.get("run").and_then(Value::as_str) == Some(run_id)
        && id("gate_run")
        && id("command")
        && id("event_id")
        && id("receipt_event_id")
        && instant(stamp.get("accepted_at")).is_some()
        && stamp
            .get("accepted_by")
            .and_then(Value::as_str)
            .is_some_and(|actor| !actor.trim().is_empty());
    well_formed.then_some(stamp)
}

/// The run a restarted run follows: the source step's `released.from_run`
/// as the claim door returned the step to `boss dispatch`, which copies
/// it onto the run unchanged as `claim_receipt`.
pub fn restarted_from(run: &Value) -> Option<&str> {
    let own = run.get("id").and_then(Value::as_str);
    run.pointer(&format!(
        "/metadata/claim_receipt/metadata/{}/{}",
        crate::step_release::KEY,
        crate::step_release::FROM_RUN
    ))
    .and_then(Value::as_str)
    .map(str::trim)
    .filter(|prior| !prior.is_empty() && Some(*prior) != own)
}

/// How long a restarted run's station took to set up again, read off its
/// own record: `run` as the jobs API returns it (steps included), its
/// event log, and the `prior` run it restarts. `None` when `prior` was
/// never handed to the gate or the run carries no claim stamp to measure
/// from.
pub fn setup_record(run: &Value, events: &[Value], prior: &Value) -> Option<Value> {
    let prior_id = prior.get("id").and_then(Value::as_str)?;
    let stamp = stamp_for(prior.get("metadata")?, prior_id)?;
    let step = |slug: &str| {
        run.get("steps")
            .and_then(Value::as_array)
            .and_then(|steps| {
                steps
                    .iter()
                    .find(|s| s.get("spec_slug").and_then(Value::as_str) == Some(slug))
            })
    };
    // The anchor: the `claimed` trigger `boss dispatch` completes as it
    // files the run, right after the claim door admitted the claim.
    let (claimed, claimed_text) = instant(step("claimed")?.get("completed_at"))?;
    // Building's claim, stamped by the claim CAS alone (records_before_claim).
    let building = step("building").and_then(|s| {
        instant(
            s.get("metadata")
                .and_then(|m| m.get(crate::records_before_claim::CLAIMED_KEY)),
        )
    });
    // The worker bind: the first write on this run's log that carried the
    // binding `boss dispatch --started` merges once the controller answers.
    let bound = events
        .iter()
        .filter(|e| e.get("kind").and_then(Value::as_str) == Some(crate::events::JOB_UPDATED))
        .filter(|e| {
            e.pointer("/payload/metadata/worker_session")
                .is_some_and(Value::is_object)
        })
        .filter_map(|e| instant(e.get("at")))
        .min_by_key(|(at, _)| *at);

    let mut parts = serde_json::Map::new();
    parts.insert(
        "building_claim".into(),
        match building {
            Some((at, text)) => json!({"at": text, "seconds_after_claim": seconds(claimed, at)}),
            None => json!({"missing": "not recorded: this run's building step carries no \
                server claim stamp (records_before_claim_closed_at)"}),
        },
    );
    parts.insert(
        "worker_bind".into(),
        match (bound, building) {
            (Some((at, text)), Some((from, _))) => {
                json!({"at": text, "seconds_after_building_claim": seconds(from, at)})
            }
            (Some((at, text)), None) => {
                json!({"at": text, "seconds_after_claim": seconds(claimed, at)})
            }
            (None, _) => json!({"missing": "not recorded: no write carrying worker_session is on \
                this run's event log (a START without --local-root binds no worker)"}),
        },
    );
    parts.insert(
        "checkout".into(),
        json!({"missing": "not recorded: the worktree is made before the claim, and the worker's \
            copy of it happens inside the bind; worker_session names its source_head and no time"}),
    );
    parts.insert(
        "warm_target".into(),
        json!({"missing": "not recorded: wt-cargo seeds the worktree's target on its first build \
            inside the worker and writes nothing to the record"}),
    );
    let mut setup = json!({
        "schema": SCHEMA,
        "restart_of": {
            "run": prior_id,
            "gate_run": stamp.get("gate_run").cloned().unwrap_or(Value::Null),
            "handed_at": stamp.get("accepted_at").cloned().unwrap_or(Value::Null),
        },
        "claimed_at": claimed_text,
        "parts": Value::Object(parts),
        "first_work": {"missing": "not recorded: nothing on the record marks the first act of \
            work after setup, so claim-to-first-work cannot be computed yet"},
    });
    if let Some((at, _)) = bound {
        setup["claim_to_worker_bound_seconds"] = seconds(claimed, at);
    }
    Some(setup)
}

fn instant(value: Option<&Value>) -> Option<(DateTime<Utc>, &str)> {
    let text = value?.as_str()?;
    let at = DateTime::parse_from_rfc3339(text).ok()?.with_timezone(&Utc);
    Some((at, text))
}

fn seconds(from: DateTime<Utc>, to: DateTime<Utc>) -> Value {
    json!((to - from).num_milliseconds() as f64 / 1000.0)
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::conditional_metadata::Command;
    use crate::gate_reservation::{LaunchIntent, Submission};
    use boss_core::job::{Job, JobId, JobStatus, Priority, Subject};
    use uuid::Uuid;

    pub(crate) const RUN: &str = "77fa2b8e-0000-4000-8000-000000000001";
    pub(crate) const ACTOR: &str = "agent-claude-b";
    const LOGIN: &str = "claude-b@algedonic.dev";

    /// The server's receipt of a durable gate submission naming `run`,
    /// submitted by `submitter`, in the exact shape the acceptance check
    /// holds a receipt to.
    pub(crate) fn accepted(run: &str, submitter: &str) -> Receipt {
        let gate = JobId::new();
        let command = Uuid::new_v4();
        let source = json!({"branch": "feat/x", "sha": "abc123", "agent_run": run,
            "worktree": "/scratch/wt"});
        let submission = Submission {
            contract_version: 1,
            command,
            submitted_by: submitter.into(),
            supervisor: "automation:train-conductor".into(),
            intent: LaunchIntent {
                contract_version: 1,
                packet: gate.to_string(),
                namespace: "boss-gates".into(),
                name: "gate-x".into(),
                head: "abc123".into(),
                mode: "auto".into(),
                transition_generation: None,
                file_id: "file".into(),
                sha256: "0".repeat(64),
                size_bytes: 1,
            },
            source: source.as_object().cloned().unwrap(),
        };
        let encoded = serde_json::to_value(&submission).unwrap();
        let mut job = Job::new(
            "gate-run",
            Subject::new("custom", "bosspipeline"),
            "gate: feat/x",
            "emp-owner",
            Priority::Standard,
            chrono::NaiveDate::from_ymd_opt(2026, 11, 6).unwrap(),
        );
        job.id = gate;
        job.status = JobStatus::Open;
        job.metadata = json!({"branch": "feat/x", "sha": "abc123", "agent_run": run,
            "worktree": "/scratch/wt", "gate_submission": encoded,
            "_immutable_metadata": {"gate_submission": encoded}});
        let mut patch = serde_json::Map::new();
        patch.insert("gate_submission".into(), encoded);
        Receipt {
            contract_version: 2,
            job_id: gate,
            actor: submitter.into(),
            command: Command {
                contract_version: 2,
                idempotency_key: command,
                expected: Default::default(),
                patch,
                immutable_keys: vec!["gate_submission".into()],
                guarded_keys: vec![],
            },
            applied_at: "2026-11-06T17:00:00Z".parse().unwrap(),
            event_id: Uuid::new_v4(),
            receipt_event_id: Some(Uuid::new_v4()),
            job,
        }
    }

    fn run_job(id: &str, metadata: Value) -> Job {
        let mut job = Job::new(
            "agent-run",
            Subject::new("custom", "bosspipeline"),
            "run",
            "emp-owner",
            Priority::Standard,
            chrono::NaiveDate::from_ymd_opt(2026, 11, 6).unwrap(),
        );
        job.id = JobId::from_uuid(Uuid::parse_str(id).unwrap());
        job.status = JobStatus::Open;
        job.metadata = metadata;
        job
    }

    /// RECEIPT COPIED, NOT RETYPED. Every field of the hand-off is the
    /// server's: the gate-run, the submission's command, when and by whom
    /// the server accepted it, and the branch and head it vouches for.
    #[test]
    fn the_hand_off_is_the_gates_receipt_copied_onto_the_run() {
        let receipt = accepted(RUN, ACTOR);
        let stamp = record(RUN, &receipt).expect("an exact acceptance naming this run");
        assert_eq!(stamp["schema"], SCHEMA);
        assert_eq!(stamp["run"], RUN);
        assert_eq!(stamp["gate_run"], receipt.job_id.to_string());
        assert_eq!(
            stamp["command"],
            receipt.command.idempotency_key.to_string()
        );
        assert_eq!(stamp["accepted_at"], "2026-11-06T17:00:00Z");
        assert_eq!(stamp["accepted_by"], ACTOR);
        assert_eq!(stamp["event_id"], receipt.event_id.to_string());
        assert_eq!(
            stamp["receipt_event_id"],
            receipt.receipt_event_id.unwrap().to_string()
        );
        assert_eq!(stamp["branch"], "feat/x");
        assert_eq!(stamp["sha"], "abc123");
        assert_eq!(stamp_for(&json!({ KEY: stamp }), RUN), Some(&stamp));
    }

    /// THE SERVER CHECKS THE COPY (adversarial finding 1). The record is
    /// verified against the gate-run as stored and the receipt the server
    /// retained: the exact copy passes; a copy for another run, one with an
    /// altered receipt event id, one checked against another gate-run, and
    /// one onto a packet that is not a run are refused.
    #[test]
    fn a_hand_off_is_verified_against_the_gate_run_and_its_receipt() {
        let receipt = accepted(RUN, ACTOR);
        let gate = receipt.job.clone();
        let run = run_job(RUN, json!({"agent": LOGIN}));
        let exact = record(RUN, &receipt).unwrap();
        assert_eq!(verify(&run, &exact, &gate, &receipt), Ok(()));

        let elsewhere = run_job(
            "77fa2b8e-0000-4000-8000-000000000002",
            json!({"agent": LOGIN}),
        );
        let mut moved = exact.clone();
        moved["run"] = json!(elsewhere.id.to_string());
        assert!(verify(&elsewhere, &moved, &gate, &receipt).is_err());

        for (field, value) in [
            ("receipt_event_id", json!(Uuid::new_v4().to_string())),
            ("event_id", json!(Uuid::new_v4().to_string())),
            ("command", json!(Uuid::new_v4().to_string())),
            ("accepted_by", json!("agent-other")),
            ("sha", json!("def456")),
        ] {
            let mut bent = exact.clone();
            bent[field] = value;
            assert!(verify(&run, &bent, &gate, &receipt).is_err(), "{field}");
        }

        let other = accepted(RUN, ACTOR);
        assert!(verify(&run, &exact, &other.job, &other).is_err());
        assert!(verify(&run, &exact, &other.job, &receipt).is_err());

        let mut not_a_run = run.clone();
        not_a_run.kind = "backlog-item".into();
        assert!(verify(&not_a_run, &exact, &gate, &receipt).is_err());
    }

    /// A receipt is not a hand-off of THIS run unless the accepted source
    /// names it, and an altered receipt is not an acceptance at all.
    #[test]
    fn a_receipt_for_another_run_or_an_altered_one_records_no_hand_off() {
        let other = "77fa2b8e-0000-4000-8000-000000000002";
        assert!(record(other, &accepted(RUN, ACTOR)).is_err());
        let mut altered = accepted(RUN, ACTOR);
        altered.actor = "agent-other".into();
        assert!(record(RUN, &altered).is_err());
        let mut unbound = accepted(RUN, ACTOR);
        unbound.job.metadata["_immutable_metadata"] = json!({});
        assert!(record(RUN, &unbound).is_err());
    }

    /// Only a well-formed hand-off of THIS run reads as one.
    #[test]
    fn only_a_well_formed_hand_off_of_this_run_reads_as_one() {
        let stamp = record(RUN, &accepted(RUN, ACTOR)).unwrap();
        let on_run = json!({"agent": LOGIN, KEY: stamp});
        assert_eq!(stamp_for(&on_run, RUN), Some(&stamp));
        // Copied onto another run's row: names a different run.
        assert!(stamp_for(&on_run, "77fa2b8e-0000-4000-8000-000000000002").is_none());
        // No record, a null, a shape this reader does not know, an
        // unreadable instant, a blank actor, a nil gate-run.
        assert!(stamp_for(&json!({"agent": LOGIN}), RUN).is_none());
        assert!(stamp_for(&json!({"agent": LOGIN, KEY: null}), RUN).is_none());
        for (field, value) in [
            ("schema", json!(2)),
            ("accepted_at", json!("soon")),
            ("accepted_by", json!(" ")),
            ("gate_run", json!(Uuid::nil().to_string())),
            ("command", json!("not-a-uuid")),
        ] {
            let mut bent = stamp.clone();
            bent[field] = value;
            assert!(
                stamp_for(&json!({"agent": LOGIN, KEY: bent}), RUN).is_none(),
                "{field}"
            );
        }
    }

    /// The source step's release names the run it was taken from, and the
    /// claim door hands that step back to `boss dispatch` verbatim.
    #[test]
    fn a_restarted_run_names_the_run_its_released_step_came_from() {
        let prior = "77fa2b8e-0000-4000-8000-0000000000aa";
        let run = json!({"id": RUN, "metadata": {"claim_receipt": {"metadata": {
            "released": {"why": "red gate", "from_run": prior, "from": ACTOR}}}}});
        assert_eq!(restarted_from(&run), Some(prior));
        for absent in [
            json!({"id": RUN, "metadata": {}}),
            json!({"id": RUN, "metadata": {"claim_receipt": {"metadata": {}}}}),
            json!({"id": RUN, "metadata": {"claim_receipt": {"metadata": {
                "released": {"from_run": null}}}}}),
            json!({"id": RUN, "metadata": {"claim_receipt": {"metadata": {
                "released": {"from_run": RUN}}}}}),
        ] {
            assert_eq!(restarted_from(&absent), None, "{absent}");
        }
    }

    pub(crate) fn restarted_run(prior: &str) -> Value {
        json!({"id": RUN, "kind": "agent-run", "status": "open",
        "metadata": {"agent": LOGIN, "claim_receipt": {"metadata": {
            "released": {"from_run": prior}}}},
        "steps": [
            {"spec_slug": "claimed", "status": "completed",
             "completed_at": "2026-11-06T18:00:00.000Z", "metadata": {}},
            {"spec_slug": "briefed", "status": "completed",
             "completed_at": "2026-11-06T18:00:00.200Z", "metadata": {}},
            {"spec_slug": "building", "status": "active", "metadata": {
                "records_before_claim_closed_at": "2026-11-06T18:00:01.500Z"}},
        ]})
    }

    pub(crate) fn bind_event() -> Value {
        json!({"kind": "jobs.job.updated", "at": "2026-11-06T18:00:31.000Z", "job_id": RUN,
            "payload": {"metadata": {"worker_session": {"session": "dev-build-0"},
                "worker_started": {"schema": 1}}}})
    }

    pub(crate) fn handed_prior(prior: &str) -> Value {
        let stamp = record(prior, &accepted(prior, ACTOR)).unwrap();
        json!({"id": prior, "kind": "agent-run", "status": "open",
            "metadata": {"agent": LOGIN, KEY: stamp}})
    }

    /// THE SETUP COST, READ OFF THE RECORD. Claim, building's claim and
    /// the worker bind each carry a server stamp, so their spans are
    /// computed; the checkout, the warm target and the first act of work
    /// carry none today, so each is NAMED missing and claim-to-first-work
    /// is not invented.
    #[test]
    fn a_restarted_runs_setup_is_read_from_its_stamps_and_names_what_is_missing() {
        let prior = "77fa2b8e-0000-4000-8000-0000000000aa";
        let events = vec![
            json!({"kind": "jobs.job.created", "at": "2026-11-06T18:00:00.000Z",
                "payload": {"metadata": {}}}),
            bind_event(),
            json!({"kind": "jobs.job.updated", "at": "2026-11-06T18:09:00.000Z",
                "payload": {"metadata": {"worker_session": {"session": "dev-build-0"}}}}),
        ];
        let handed_run = handed_prior(prior);
        let setup = setup_record(&restarted_run(prior), &events, &handed_run)
            .expect("a restart after a hand-off is measured");
        assert_eq!(setup["schema"], 1);
        assert_eq!(setup["restart_of"]["run"], prior);
        assert_eq!(
            setup["restart_of"]["gate_run"],
            handed_run["metadata"][KEY]["gate_run"]
        );
        assert_eq!(
            setup["restart_of"]["handed_at"],
            handed_run["metadata"][KEY]["accepted_at"]
        );
        assert_eq!(setup["claimed_at"], "2026-11-06T18:00:00.000Z");
        let parts = &setup["parts"];
        assert_eq!(parts["building_claim"]["at"], "2026-11-06T18:00:01.500Z");
        assert_eq!(parts["building_claim"]["seconds_after_claim"], 1.5);
        assert_eq!(parts["worker_bind"]["at"], "2026-11-06T18:00:31.000Z");
        assert_eq!(parts["worker_bind"]["seconds_after_building_claim"], 29.5);
        assert_eq!(setup["claim_to_worker_bound_seconds"], 31.0);
        for missing in ["checkout", "warm_target"] {
            assert!(
                parts[missing]["missing"]
                    .as_str()
                    .is_some_and(|why| why.starts_with("not recorded")),
                "{missing}: {setup}"
            );
        }
        assert!(
            setup["first_work"]["missing"]
                .as_str()
                .is_some_and(|why| why.starts_with("not recorded")),
            "{setup}"
        );
        assert!(
            setup.get("claim_to_first_work_seconds").is_none(),
            "{setup}"
        );
    }

    /// A part the run's record does not carry is named, not zeroed.
    #[test]
    fn a_run_with_no_bind_on_its_log_names_the_bind_missing() {
        let prior = "77fa2b8e-0000-4000-8000-0000000000aa";
        let mut run = restarted_run(prior);
        run["steps"][2]["metadata"] = json!({});
        let setup = setup_record(&run, &[], &handed_prior(prior)).unwrap();
        assert!(
            setup["parts"]["worker_bind"]["missing"].is_string(),
            "{setup}"
        );
        assert!(
            setup["parts"]["building_claim"]["missing"].is_string(),
            "{setup}"
        );
        assert!(setup.get("claim_to_worker_bound_seconds").is_none());
    }

    /// A run that follows a run never handed to the gate is not a restart
    /// after a hand-off, and gets no setup record.
    #[test]
    fn a_restart_of_a_run_never_handed_to_the_gate_records_no_setup() {
        let prior = "77fa2b8e-0000-4000-8000-0000000000aa";
        let unhanded = json!({"id": prior, "metadata": {"agent": LOGIN}});
        assert_eq!(
            setup_record(&restarted_run(prior), &[bind_event()], &unhanded),
            None
        );
        let mut unclaimed = restarted_run(prior);
        unclaimed["steps"][0]["completed_at"] = Value::Null;
        assert_eq!(
            setup_record(&unclaimed, &[bind_event()], &handed_prior(prior)),
            None
        );
    }
}
