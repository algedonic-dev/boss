//! `jobs.run-car-probes` — a train arrived: run each boarded car's
//! recorded probe against production, by filing the ops-request the
//! forge host answers.
//!
//! THE ACTOR HALF OF CARRIED PROOF (backlog 28ac45ab). `boss gate
//! --park-probe/--park-expect` writes the probe at park time and the
//! auto-park handler copies it onto the car as `proof_probe` /
//! `proof_expect` (the input half, feat/the-car-carries-its-probe).
//! Until this rule, every recorded probe still waited for a person to
//! type `boss prove <car> --from-car`: 43 landed cars sat at
//! `proven: ready` for 2.5 days, and 12 were proven by hand in one
//! sitting. A pr-train closing `arrived` is the moment every car
//! aboard is merged AND converged, so it is the moment the probe
//! means something — and this handler files one `ops-request` per
//! probed car, `host=forge verb=run-car-probe args=[car]`.
//!
//! WHERE THE PROBE RUNS, AND WHY NOT HERE. The dispatcher's image does
//! carry a shell, so a shell-out from this handler would compile. It
//! is the wrong seat three times over: the dispatcher is the queue
//! watcher — clock, threshold, matchmaking, nothing else (David,
//! 2026-08-14) — and running a probe is an actor's work, not routing;
//! a probe that hangs would hold a JetStream consumer, and the
//! dispatcher owes the rest of the system its liveness; and the
//! vantage a probe needs (the converged checkout, the forge journal,
//! the SoR over the LAN) lives on the forge host, not in the `boss`
//! namespace. What the forge does NOT have is the cluster's vantage:
//! no kubectl, no kubeconfig. A probe is written on the dev pod and
//! run there, so it can be correct and unrunnable — measured on this
//! rule's first live run (f9304366), where both cars came back as an
//! exit code with empty streams. `boss gate` now refuses a probe
//! naming a tool in infra/forge/host-absent-tools.txt, and the runner
//! records `unrunnable` with the tool named rather than a bare exit
//! code. This rule does not restate every gate-side refusal; it applies
//! the ones nothing downstream can (the unidentified read, the offset
//! git date, a caller cleared from the gate log alone) — see
//! [`ship_refusal`], which is where that call is argued rather than
//! described (23b2dffa, 71ec5a58, ce72aea9).
//! The forge already answers
//! ops-request packets through a reviewed verb allowlist
//! (`infra/ops/verbs/run-car-probe.json`), so the run goes through that door:
//! `boss prove <car> --from-car --unattended` (the tree's CLI on the
//! forge, backlog 9f00a805) re-reads the car, refuses unless it
//! has merged and recorded a probe, runs the probe as `boss-probe`
//! (never root, never the checkout's owner; infra/forge/probe-account.sh)
//! with a timeout, judges it by the two rules `boss prove`
//! applies, and writes the verdict on the car — `proven` completed
//! with the recorded proof shape on green, `proof_attempt` on the car
//! and `proven` left ready otherwise.
//!
//! WHAT IT LEAVES ALONE. A car with no `proof_probe` — including an
//! EVENT-BOUND car that recorded `proof_event` instead — stays
//! hand-proven; a car already proven is not re-proven; a car that
//! already has an open run-car-probe request is not asked twice
//! (JetStream is at-least-once and the close marker is emitted from
//! three sites, so this WILL run more than once per arrival) — unless
//! that request's `execute` has completed: it was answered and is only
//! waiting to be settled, which the hourly firing does (17bf6f3e).

use std::sync::Arc;

use async_trait::async_trait;
use serde_json::{Value, json};

use boss_dispatcher::rules::handler::{Handler, HandlerError, InvocationContext};
use boss_jobs::car;

use super::common::{api_client, jobs_where, open_jobs_of_kind, post_json, write_json};

/// The allowlisted verb (a file under `infra/ops/verbs/`) and the host that
/// answers it. One definition each, read by the request builder and
/// the twice-guard.
pub const VERB: &str = "run-car-probe";
pub const HOST: &str = "forge";

/// Every request this handler files is titled with this prefix, and the
/// `owed` scope reads it back: the close of one of OUR requests is never
/// the event an owed proof waits on, or a car keyed to `ops-request`
/// closes would re-run on its own probe's answer, and two such cars on
/// each other's, forever. One definition, written and read here.
pub(crate) const REQUEST_TITLE: &str = "run the recorded probe for ";

/// The car metadata key the probe rides under. Its ONE definition is
/// `boss_jobs::car::PROOF_PROBE`, which lands with the companion car
/// feat/the-car-carries-its-probe; this branch is gated alone against
/// main, where that symbol does not yet exist, so the literal is
/// repeated here for exactly as long as the two cars are apart.
/// COLLAPSE once both have landed: replace with `car::PROOF_PROBE`
/// and delete this constant (CLAUDE.md 9a).
const PROOF_PROBE: &str = "proof_probe";

pub struct JobsRunCarProbes {
    client: boss_core::machine_token::Client,
    jobs_base: String,
}

impl JobsRunCarProbes {
    pub fn new(jobs_base: impl Into<String>) -> Arc<Self> {
        Arc::new(Self {
            client: api_client(),
            jobs_base: jobs_base.into(),
        })
    }

    fn base(&self) -> &str {
        self.jobs_base.trim_end_matches('/')
    }

    /// Every open Job of `kind`, paged on `total` so a car sorted past
    /// one page is still found — the same paging `boss prove` does,
    /// for the same reason (a capped page is a false negative that
    /// grows with the pipeline's age). One definition, in `common`,
    /// since `jobs.complete_step_matching` walks the same list.
    async fn all_open(&self, kind: &str, rule: &str) -> Result<Vec<Value>, HandlerError> {
        open_jobs_of_kind(&self.client, self.base(), kind, rule).await
    }
}

/// PURE: the id of the train that just ARRIVED, or `None` for every
/// other close this shared topic carries (the rule's `when` filters
/// them; the handler re-checks so it is correct on its own terms).
pub(crate) fn arrived_train(payload: &Value) -> Option<&str> {
    if payload.get("kind").and_then(Value::as_str) != Some("pr-train")
        || payload.get("outcome").and_then(Value::as_str) != Some("arrived")
    {
        return None;
    }
    payload.get("id").and_then(Value::as_str)
}

fn md<'a>(job: &'a Value, key: &str) -> Option<&'a str> {
    job.get("metadata")
        .and_then(|m| m.get(key))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
}

/// PURE: is this car's `proven` step still waiting to be filled?
fn proven_is_open(car: &Value) -> bool {
    car::find_step(car, "proven", "Proven in prod")
        .and_then(|s| s.get("status"))
        .and_then(Value::as_str)
        .is_some_and(|s| matches!(s, "ready" | "active"))
}

/// What one arrival produces: requests to file, and cars this door will
/// not ship.
#[derive(Debug, Default)]
pub(crate) struct Arrival {
    /// The ops-request bodies to file, one per probed car.
    pub requests: Vec<Value>,
    /// Cars whose recorded probe this rule refuses, each with the
    /// `proof_attempt` to record on the car — `(car id, attempt)`. A
    /// refusal nobody can read is the same as not checking, so every one
    /// of these is written where the car's other proof evidence lives.
    pub refusals: Vec<(String, Value)>,
}

/// WHY THIS DOOR RE-CHECKS THE FAILS-OPEN RULES AND NOT THE OTHER
/// (backlog 23b2dffa, settled here rather than described).
///
/// `boss gate --park-probe` refuses every shape of bad probe at park
/// time, so most cars reaching this rule were already checked. Most is
/// not all: a car's `proof_probe` can be written straight onto it with a
/// metadata PATCH (a documented door), and cars parked before the
/// gate-side refusals existed still carry whatever they carried. So the
/// question is what this rule owes a probe no gate ever saw.
///
/// THE ABSENT-TOOL RULE IS NOT RE-CHECKED, deliberately. The forge
/// runner catches it empirically: fd 9 collects every command bash could
/// not resolve, on a channel the probe's own redirections cannot reach,
/// and the attempt is stamped `unrunnable` with the tool named. A
/// measurement of the actual host beats this handler predicting it from
/// a manifest — and a prediction that is wrong (a tool installed since
/// the list was measured) would strand a car with nobody watching.
///
/// THE UNIDENTIFIED READ IS RE-CHECKED, because nothing downstream can.
/// The probe runs, policy answers a NARROWER WORLD in silence, the
/// absence assertion passes, and the runner records it as a PROOF on a
/// car that then closes (61085a9e). By the time the text reaches the
/// forge it is too late for anything but the refusal, and this is the
/// last place that can make one.
///
/// THE OFFSET GIT DATE IS RE-CHECKED for the same reason (71ec5a58,
/// left by the builder of c0ac92b8). A string compare of `%cI` against
/// the SoR's UTC timestamps lies in BOTH directions — FAILED for a
/// not-yet on 746a1fac, and PASS for an event that never happened with
/// the offsets the other way round — and the runner sees only a compare
/// that succeeded. The gate and `boss prove` refuse the token; a probe
/// PATCHed onto a car met neither, so this door refuses it too, under
/// the rule id both of them record.
///
/// THE GATE-LOG-ALONE CLEARANCE IS REFUSED HERE, though `boss prove`
/// only WARNS of it (backlog ce72aea9; the gate refuses it at park since
/// 5605c2b5, so this arm now meets only older cars). It warns because the
/// text cannot tell an absence assertion, which the gap turns green, from
/// a presence assertion, which it does not — and a person is standing
/// there to read which one they wrote. This door has nobody to warn, so
/// its choice is refuse or record, and recording is what went wrong:
/// after the warning landed, this rule recorded car 033c5c2f PROVEN at
/// 2026-10-08T23:00Z and car 099f8a9f at 2026-10-10T02:01Z on log-only
/// probes. A presence probe refused here is not lost — the refusal sits
/// on the car and `boss prove` runs it by hand, warning and all.
///
/// WHAT THE REFUSAL TELLS ITS READER TO DO (review 4ce5ca85 N3). Every
/// car this door sees has LANDED (`merged=true`, `proven` open), so it
/// cannot be re-parked, and a bare `boss prove <car> --from-car` would
/// run this same recorded text. The one act that moves the car is a
/// hand proof with a replacement probe, so each arm names its fix and
/// the closing sentence names that verb on this car.
fn ship_refusal(car_id: &str, probe: &str, expect: Option<&str>) -> Option<Value> {
    let (rule, why, fix) = if let Some(client) = boss_jobs::probe::reads_the_sor_unidentified(probe)
    {
        (
            boss_jobs::probe::UNIDENTIFIED_RULE,
            format!(
                "THE PROBE WAS NOT RUN: it reads the system of record with `{client}` and no \
                 identity, so it would read as operator:unidentified and be answered with a \
                 NARROWER WORLD, silently. {evidence}",
                evidence = boss_jobs::probe::UNIDENTIFIED_READ_EVIDENCE,
            ),
            format!(
                "a probe that reads as a named reader ({reader} /api/...)",
                reader = boss_jobs::probe::SOR_READER,
            ),
        )
    } else if let Some(kind) = boss_jobs::probe::clears_a_caller_from_the_gate_log_alone(probe) {
        (
            boss_jobs::probe::GATE_LOG_ALONE_RULE,
            format!(
                "THE PROBE WAS NOT RUN: it {heading} — {reading}. {evidence}",
                heading = boss_jobs::probe::gate_log_alone_heading(kind).to_lowercase(),
                reading = boss_jobs::probe::gate_log_alone_reading(kind),
                evidence = boss_jobs::probe::GATE_LOG_ALONE_EVIDENCE,
            ),
            "a probe that also reads the live rows with their coverage (the jq above)".to_string(),
        )
    } else {
        let token = boss_jobs::probe::reads_git_time_with_an_offset(probe)?;
        (
            boss_jobs::probe::GIT_TIME_RULE,
            format!(
                "THE PROBE WAS NOT RUN: it reads a git date with `{token}`, which carries the \
                 committer's UTC offset, and a probe that compares that string against the \
                 system of record's UTC timestamps lies in BOTH directions. {evidence}",
                evidence = boss_jobs::probe::GIT_TIME_STRING_EVIDENCE,
            ),
            "a probe that compares epochs (git log -1 --format=%ct against date -u -d \"$ts\" \
             +%s, the empty guard first)"
                .to_string(),
        )
    };
    let why = format!(
        "{why}\nThe car has landed, so it is not re-parked, and `--from-car` would run this \
         same text. Prove it by hand with {fix}: {by_hand}.",
        by_hand = boss_jobs::probe::hand_proof_command(car_id),
    );
    // No `at`: this rule holds no clock (the dispatcher's time comes
    // from the clock port, which this handler does not carry), and the
    // PATCH that records the attempt is itself an audit-log event with
    // one. The same attempt re-written on a redelivered arrival is
    // idempotent by content, which is what at-least-once needs.
    Some(json!({
        "refused": rule,
        "probe": probe,
        "expect": expect.unwrap_or(""),
        "unrunnable": false,
        "why": why,
    }))
}

/// PURE: the ops-request bodies to file for one arrival — one per car
/// aboard `train_id` that recorded a probe, still has `proven` open,
/// and has no run-car-probe request already open. Everything the
/// forge script needs rides in metadata: `host`/`verb`/`args` for the
/// ops-runner, `car`/`branch`/`train` for a reader.
///
/// A car whose recorded probe this door refuses ([`ship_refusal`]) is
/// not shipped, and comes back in `refusals` so the reason lands on the
/// car instead of vanishing.
/// Which cars a firing asks after.
///
/// `Train`: the cars aboard the train that just arrived — the first
/// honest moment for their probes. `Failing`: every landed car whose
/// probe has RUN and not settled it (a `proof_attempt` is on the car),
/// whatever train it rode. On 2026-09-12 two such cars sat at `proven`
/// waiting on tomorrow's timer firing, correct but early, and the only
/// way their probe would run again was a human refiling
/// `run-car-probe` by hand (rule `recheck-failing-probes-hourly`).
///
/// `Owed`: the close marker of a packet that just closed, asking after
/// every car that closed LANDED with its proof owed and declared THIS
/// close as the event it waits on (backlog b9005734, rule
/// `rerun-owed-proofs-on-job-closed`). Those cars are closed and their
/// `proven` was skipped by the landing, so neither scope above can
/// reach them: the owed proof is an obligation keyed to its event, and
/// this is the obligation firing.
///
/// `Declared`: the close marker again, asking after every OPEN landed car
/// whose probe ran and said not-yet (a `proof_attempt`, `proven` still
/// open) and that declared THIS close as the event it waits on (backlog
/// 9e9db2ea, rule `rerun-waiting-probes-on-their-declared-close`). The
/// owed obligation, before the car closes: measured 2026-10-19, 6 of the
/// 50 newest proven cars were proven 24 to 315 minutes after arrival
/// because only the hourly `Failing` firing would ask again — four of
/// them waiting for a gate-run to close after the converge. `Failing`
/// stays as the safety net for a car that declares nothing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Scope<'a> {
    Train(&'a str),
    Failing,
    Owed(&'a Value),
    Declared(&'a Value),
}

impl Scope<'_> {
    fn admits(&self, car: &Value) -> bool {
        match self {
            Scope::Train(t) => md(car, "train") == Some(t),
            Scope::Failing => car.pointer("/metadata/proof_attempt").is_some(),
            Scope::Owed(closed) => {
                let m = car.get("metadata").unwrap_or(&Value::Null);
                !closes_a_probe_request(closed)
                    && car::owes_proof(m)
                    && car::wait_event(m).is_some_and(|e| e.fired_by(closed))
            }
            Scope::Declared(closed) => {
                let m = car.get("metadata").unwrap_or(&Value::Null);
                !closes_a_probe_request(closed)
                    && m.get(car::PROOF_ATTEMPT).is_some()
                    && car::wait_event(m).is_some_and(|e| e.fired_by(closed))
            }
        }
    }
    fn train_of(&self, car: &Value) -> String {
        match self {
            Scope::Train(t) => t.to_string(),
            Scope::Failing | Scope::Owed(_) | Scope::Declared(_) => {
                md(car, "train").unwrap_or("").to_string()
            }
        }
    }
    /// Is the car's `proven` step one this scope may ask about? Open for
    /// the two scopes over open cars; for an owed car, anything but
    /// completed — the landing close skipped it, and a car marked owed
    /// before its protocol's exit was published is still open at it.
    fn proven_admits(&self, car: &Value) -> bool {
        match self {
            Scope::Owed(_) => !proven_is_completed(car),
            _ => proven_is_open(car),
        }
    }
    /// The packet whose close fired an owed run, for the request's reader.
    fn event_packet(&self) -> Option<&str> {
        match self {
            Scope::Owed(closed) | Scope::Declared(closed) => {
                closed.get("id").and_then(Value::as_str)
            }
            _ => None,
        }
    }
}

/// PURE: is this close marker the close of a request this handler filed?
fn closes_a_probe_request(closed: &Value) -> bool {
    closed.get("kind").and_then(Value::as_str) == Some("ops-request")
        && closed
            .get("title")
            .and_then(Value::as_str)
            .is_some_and(|t| t.starts_with(REQUEST_TITLE))
}

/// PURE: has this car's `proven` step been completed?
fn proven_is_completed(car: &Value) -> bool {
    car::find_step(car, "proven", "Proven in prod")
        .and_then(|s| s.get("status"))
        .and_then(Value::as_str)
        == Some("completed")
}

/// PURE: the id of this request's `execute` step when that step has
/// completed — the request has been ANSWERED, whether or not its packet
/// has closed yet.
fn completed_execute(request: &Value) -> Option<&str> {
    request
        .get("steps")
        .and_then(Value::as_array)?
        .iter()
        .find(|s| s.get("spec_slug").and_then(Value::as_str) == Some("execute"))
        .filter(|s| s.get("status").and_then(Value::as_str) == Some("completed"))
        .and_then(|s| s.get("id"))
        .and_then(Value::as_str)
}

/// PURE: every open ops-request whose `execute` completed, as `(request
/// id, execute step id)` — a request answered and never settled, which
/// the hourly recheck settles by re-sending that completion (backlog
/// 17bf6f3e). Whatever the verb: this rule is the one hourly reader of
/// the open request board, and the re-send writes nothing where nothing
/// is owed, so naming a request of another verb costs one 204.
pub(crate) fn unsettled_requests(open_requests: &[Value]) -> Vec<(String, String)> {
    open_requests
        .iter()
        .filter_map(|r| {
            let id = r.get("id").and_then(Value::as_str)?;
            Some((id.to_string(), completed_execute(r)?.to_string()))
        })
        .collect()
}

pub(crate) fn probe_requests(
    scope: Scope<'_>,
    cars: &[Value],
    open_requests: &[Value],
    rule_name: &str,
    event_id: &str,
    topic: &str,
) -> Arrival {
    // A request whose `execute` completed has been ANSWERED: it holds
    // its car only while the forge has not yet run it. Read as "still
    // asking" whatever its steps said, one request left open after its
    // run retired its car's hourly recheck for good — car 176d5c9d, on
    // request e8dd5479 from 2026-10-09T04:00Z (backlog 17bf6f3e).
    let already_asked = |car_id: &str| {
        open_requests.iter().any(|r| {
            md(r, "verb") == Some(VERB)
                && md(r, "car") == Some(car_id)
                && completed_execute(r).is_none()
        })
    };
    let mut refusals: Vec<(String, Value)> = Vec::new();
    let requests = cars
        .iter()
        .filter(|c| scope.admits(c))
        .filter(|c| md(c, PROOF_PROBE).is_some())
        .filter(|c| scope.proven_admits(c))
        .filter_map(|c| {
            let id = c.get("id").and_then(Value::as_str)?;
            if already_asked(id) {
                return None;
            }
            if let Some(probe) = md(c, PROOF_PROBE)
                && let Some(attempt) = ship_refusal(id, probe, md(c, car::PROOF_EXPECT))
            {
                refusals.push((id.to_string(), attempt));
                return None;
            }
            let title = c.get("title").and_then(Value::as_str).unwrap_or("");
            Some(json!({
                "kind": "ops-request",
                "title": format!("{REQUEST_TITLE}{title}"),
                // The CAR is the subject: what this packet is about, and
                // the cheap handle for anyone asking "was this car's
                // probe run?"
                "subject": {"subject_kind": "custom", "id": id},
                "owner_id": format!("rule:{rule_name}"),
                "priority": "standard",
                "status": "open",
                "tags": ["dispatcher-spawned"],
                "metadata": {
                    "host": HOST,
                    "verb": VERB,
                    "args": [id],
                    "car": id,
                    "branch": md(c, "branch").unwrap_or(""),
                    "train": scope.train_of(c),
                    "recheck": matches!(scope, Scope::Failing),
                    "owed": matches!(scope, Scope::Owed(_)),
                    "event_packet": scope.event_packet(),
                    "spawned_by_rule": rule_name,
                    "triggered_by_event_id": event_id,
                    "triggered_by_topic": topic,
                },
            }))
        })
        .collect();
    Arrival { requests, refusals }
}

#[async_trait]
impl Handler for JobsRunCarProbes {
    fn name(&self) -> &'static str {
        "jobs.run-car-probes"
    }

    async fn invoke(
        &self,
        args: &[(String, boss_dispatcher::rules::expr::Value)],
        ctx: &InvocationContext,
    ) -> Result<(), HandlerError> {
        // The rule says which cars: `scope = "failing"` for the daily
        // recheck; no arg means the arrival rule, which names its train
        // in the closing event.
        let scope = match boss_dispatcher::rules::handler::arg_string(args, "scope") {
            Ok("failing") => Scope::Failing,
            Ok("owed") => Scope::Owed(&ctx.event_payload),
            Ok("declared") => Scope::Declared(&ctx.event_payload),
            Ok(other) => {
                return Err(HandlerError::Permanent(format!(
                    "jobs.run-car-probes: scope must be \"failing\", \"owed\" or \"declared\" (or absent for the arrival rule), not {other:?}"
                )));
            }
            Err(HandlerError::MissingArg(_)) => match arrived_train(&ctx.event_payload) {
                Some(t) => Scope::Train(t),
                None => return Ok(()),
            },
            Err(e) => return Err(e),
        };
        // An owed car is CLOSED, so the open board cannot hold it: read
        // the cars carrying the marker, whatever their status — one
        // indexed, usually-empty query per close, the shape
        // `jobs-clear-waiting-on` already runs on the same topic. And
        // the close of one of our own requests is dropped before any
        // read, since `admits` would refuse every car for it anyway.
        let cars = match scope {
            Scope::Owed(closed) if closes_a_probe_request(closed) => return Ok(()),
            Scope::Owed(_) => {
                jobs_where(
                    &self.client,
                    self.base(),
                    &format!("kind=ship-a-change&metadata_has={}", car::PROOF_OWED),
                    &ctx.rule_name,
                )
                .await?
            }
            // A waiting car is OPEN and carries a declared wait: the same
            // one indexed read per close, narrowed to the open cars that
            // declare one (1 on 2026-10-19). When none of them declared
            // THIS close, the firing ends before it reads the request
            // board — almost every close.
            Scope::Declared(closed) if closes_a_probe_request(closed) => return Ok(()),
            Scope::Declared(_) => {
                let cars = jobs_where(
                    &self.client,
                    self.base(),
                    &format!(
                        "kind=ship-a-change&status=open&metadata_has={}",
                        car::WAITS_ON
                    ),
                    &ctx.rule_name,
                )
                .await?;
                if !cars.iter().any(|c| scope.admits(c)) {
                    return Ok(());
                }
                cars
            }
            _ => self.all_open("ship-a-change", &ctx.rule_name).await?,
        };
        let open_requests = self.all_open("ops-request", &ctx.rule_name).await?;
        let arrival = probe_requests(
            scope,
            &cars,
            &open_requests,
            &ctx.rule_name,
            &ctx.triggering_event_id,
            &ctx.triggering_topic,
        );
        // The refusals go first, and they are recorded rather than
        // logged: a car this rule will not probe must LOOK like one, on
        // the car, beside the attempts the forge runner writes — a
        // refusal only a journal knows about is indistinguishable from
        // a rule that never ran (CLAUDE.md §Diagnosis).
        //
        // EVERY WRITE IN THIS FIRING IS ATTEMPTED, THEN THE FAILURES ARE
        // NAMED TOGETHER (backlog 8b76fab9, the shape a2aff635 gave the
        // re-sends below). A `?` in either of these loops let the first
        // car whose write failed — a 5xx, a rollout gap, a 422 on one
        // malformed car — end the firing: every car after it went
        // unwritten and the hourly re-send never ran.
        let mut failed: Vec<(String, HandlerError)> = Vec::new();
        for (car_id, attempt) in &arrival.refusals {
            if let Err(e) = write_json(
                &self.client,
                reqwest::Method::PATCH,
                &format!("{}/api/jobs/{car_id}/metadata", self.base()),
                &json!({ car::PROOF_ATTEMPT: attempt }),
                &ctx.rule_name,
            )
            .await
            {
                failed.push((format!("refusal on car {car_id}"), e));
            }
        }
        for body in &arrival.requests {
            if let Err(e) = post_json(
                &self.client,
                &format!("{}/api/jobs", self.base()),
                body,
                &ctx.rule_name,
            )
            .await
            {
                let car_id = body.pointer("/metadata/car").and_then(Value::as_str);
                failed.push((format!("request for car {}", car_id.unwrap_or("?")), e));
            }
        }
        // AN ANSWERED REQUEST LEFT OPEN IS SETTLED, NOT JUST STEPPED
        // AROUND (backlog 17bf6f3e). Ignoring it above lets its car be
        // asked again; this closes it. The hourly firing only — the
        // arrival and owed scopes run on every train and every close, and
        // an hour is soon enough for a packet that sat open a day. The
        // status alone, re-sent to the completed `execute`: the step PUT
        // writes nothing to a completed step, and on an open packet runs
        // the readiness pass and the declared-terminal close it missed.
        // A re-send that cannot settle answers 5xx, and this firing
        // retries and then dead-letters under its rule's name — a
        // request that cannot close is loud here, never a quiet hold on
        // its car.
        //
        // EVERY RE-SEND IS ATTEMPTED, THEN THE FAILURES ARE NAMED
        // TOGETHER (backlog a2aff635, landed-review fc7b75f6 finding 2).
        // A `?` inside this loop let the first request that cannot settle
        // — its pinned version will not resolve (500), it sits outside
        // the dispatcher's scope (404) — end the firing, so it
        // dead-lettered hourly AND every answered-but-open request after
        // it was never re-sent at all: one stuck packet held the rest.
        if matches!(scope, Scope::Failing) {
            for (request, execute) in unsettled_requests(&open_requests) {
                if let Err(e) = write_json(
                    &self.client,
                    reqwest::Method::PUT,
                    &format!("{}/api/jobs/{request}/steps/{execute}", self.base()),
                    &json!({"status": "completed"}),
                    &ctx.rule_name,
                )
                .await
                {
                    failed.push((format!("re-send of request {request}"), e));
                }
            }
        }
        resend_verdict(failed)
    }
}

/// PURE: the firing's verdict over the writes that failed — refusals,
/// requests and re-sends alike — `Ok` when none did, else ONE error
/// naming each and why. Permanent only when every failure is: a
/// retryable one among them earns the firing its redelivery. A
/// redelivery repeats the writes that succeeded: a refusal PATCH merges
/// the same key, a re-send to a completed step writes nothing, and a
/// filed request reads as already asked on the next pass.
fn resend_verdict(failed: Vec<(String, HandlerError)>) -> Result<(), HandlerError> {
    if failed.is_empty() {
        return Ok(());
    }
    let permanent = failed.iter().all(|(_, e)| e.is_permanent());
    let named = failed
        .iter()
        .map(|(what, e)| format!("{what}: {e}"))
        .collect::<Vec<_>>()
        .join("; ");
    let why = format!("{} write(s) could not be made: {named}", failed.len());
    Err(if permanent {
        HandlerError::Permanent(why)
    } else {
        HandlerError::Downstream(why)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn car(id: &str, train: &str, extra: Value, proven: &str) -> Value {
        let mut m = json!({"branch": format!("feat/{id}"), "merged": "true", "train": train});
        if let (Some(dst), Some(src)) = (m.as_object_mut(), extra.as_object()) {
            for (k, v) in src {
                dst.insert(k.clone(), v.clone());
            }
        }
        json!({
            "id": id, "kind": "ship-a-change", "status": "open",
            "title": format!("Car {id}"),
            "metadata": m,
            "steps": [{"spec_slug": "proven", "title": "Proven in prod", "status": proven}]
        })
    }

    /// A car carrying a GOOD probe: the named reader the forge puts on
    /// the probe's PATH. This fixture used to be
    /// `curl -s http://sor/api/yard | grep -c x` — exactly the shape
    /// 61085a9e measured, an unidentified read answered with a narrower
    /// world — so the fixture was itself an example of the defect.
    fn probed() -> Value {
        json!({"proof_probe": "boss-sor-read /api/yard/status | grep -q dock_depth",
               "proof_expect": "dock_depth"})
    }

    /// THE TRIGGER: only a pr-train closing `arrived` names a train.
    #[test]
    fn only_an_arrived_train_is_an_arrival() {
        let closed = |kind: &str, outcome: Value| json!({"id": "t1", "kind": kind, "outcome": outcome, "closed_on": "2026-09-08"});
        assert_eq!(
            arrived_train(&closed("pr-train", json!("arrived"))),
            Some("t1")
        );
        assert_eq!(arrived_train(&closed("pr-train", json!("cancelled"))), None);
        assert_eq!(arrived_train(&closed("pr-train", Value::Null)), None);
        assert_eq!(
            arrived_train(&closed("ship-a-change", json!("merged"))),
            None
        );
    }

    /// A boarded, probed car with `proven` open gets exactly one
    /// request, shaped for the ops-runner (host/verb/args) and for a
    /// reader (car/branch/train), with the car as its subject.
    #[test]
    fn a_probed_car_aboard_the_train_gets_one_bounded_request() {
        let cars = [car("c1", "t1", probed(), "ready")];
        let got = probe_requests(
            Scope::Train("t1"),
            &cars,
            &[],
            "run-car-probes-on-train-arrived",
            "ev",
            "jobs.job.closed",
        );
        assert_eq!(got.requests.len(), 1);
        let r = &got.requests[0];
        assert_eq!(r["kind"], "ops-request");
        assert_eq!(r["subject"]["id"], "c1");
        assert_eq!(r["metadata"]["host"], "forge");
        assert_eq!(r["metadata"]["verb"], "run-car-probe");
        assert_eq!(r["metadata"]["args"], json!(["c1"]));
        assert_eq!(r["metadata"]["car"], "c1");
        assert_eq!(r["metadata"]["branch"], "feat/c1");
        assert_eq!(r["metadata"]["train"], "t1");
        assert_eq!(
            r["metadata"]["spawned_by_rule"],
            "run-car-probes-on-train-arrived"
        );
        assert!(r["title"].as_str().unwrap().contains("Car c1"));
    }

    /// THE DAILY RECHECK: a landed car whose probe ran and did not settle
    /// it (a `proof_attempt` on the car) is asked again whatever train it
    /// rode; a car whose probe never ran, an event-bound car, a proven
    /// car and an already-asked car are not. The request says it is a
    /// recheck and names the car's own train.
    #[test]
    fn the_failing_scope_asks_again_for_every_landed_car_whose_probe_ran_and_failed() {
        let failed = json!({"proof_attempt": {"exit": "1", "why": "printed nothing"}});
        let mut f1 = probed();
        f1["proof_attempt"] = failed["proof_attempt"].clone();
        let mut f2 = probed();
        f2["proof_attempt"] = failed["proof_attempt"].clone();
        let cars = [
            car("f1", "t1", f1, "ready"),
            car("f2", "t7", f2, "ready"),
            car("never-ran", "t1", probed(), "ready"),
            car(
                "event",
                "t1",
                json!({"proof_event": "the next red train", "proof_attempt": failed["proof_attempt"].clone()}),
                "ready",
            ),
            car(
                "done",
                "t1",
                {
                    let mut m = probed();
                    m["proof_attempt"] = failed["proof_attempt"].clone();
                    m
                },
                "completed",
            ),
        ];
        let open = [json!({"metadata": {"verb": VERB, "car": "f2"}})];
        let got = probe_requests(
            Scope::Failing,
            &cars,
            &open,
            "recheck-failing-probes-hourly",
            "clock-day:2026-09-13",
            "clock.day",
        );
        let asked: Vec<&str> = got
            .requests
            .iter()
            .map(|r| r["metadata"]["car"].as_str().unwrap())
            .collect();
        assert_eq!(asked, vec!["f1"], "{got:?}");
        assert_eq!(got.requests[0]["metadata"]["train"], "t1");
        assert_eq!(got.requests[0]["metadata"]["recheck"], true);
        assert_eq!(
            got.requests[0]["metadata"]["spawned_by_rule"],
            "recheck-failing-probes-hourly"
        );
    }

    #[test]
    fn an_arrival_request_is_not_a_recheck() {
        let cars = [car("c1", "t1", probed(), "ready")];
        let got = probe_requests(Scope::Train("t1"), &cars, &[], "r", "ev", "jobs.job.closed");
        assert_eq!(got.requests[0]["metadata"]["recheck"], false);
    }

    /// Everything that must NOT be asked: another train's car, an
    /// event-bound car, a car with no proof intent at all, a car
    /// already proven, and a car already asked (at-least-once
    /// delivery).
    #[test]
    fn unprobed_foreign_proven_and_already_asked_cars_are_left_alone() {
        let cars = [
            car("other", "t2", probed(), "ready"),
            car(
                "event",
                "t1",
                json!({"proof_event": "event-bound — a stalled train"}),
                "ready",
            ),
            car("bare", "t1", json!({}), "ready"),
            car("done", "t1", probed(), "completed"),
            car("asked", "t1", probed(), "ready"),
            car("fresh", "t1", probed(), "active"),
        ];
        let open = [json!({
            "id": "r1", "kind": "ops-request", "status": "open",
            "metadata": {"host": "forge", "verb": "run-car-probe", "car": "asked"}
        })];
        let got = probe_requests(
            Scope::Train("t1"),
            &cars,
            &open,
            "r",
            "ev",
            "jobs.job.closed",
        );
        let ids: Vec<&str> = got
            .requests
            .iter()
            .map(|r| r["metadata"]["car"].as_str().unwrap())
            .collect();
        assert_eq!(ids, vec!["fresh"], "{ids:?}");
    }

    /// THE RULE WITH NO BACKSTOP DOWNSTREAM (backlog 23b2dffa). The
    /// forge runner catches a missing TOOL empirically — fd 9 collects
    /// what bash could not resolve — so a probe naming `kubectl` comes
    /// back as `unrunnable` with the tool named, which is better than
    /// any prediction this handler could make. Nothing downstream
    /// catches an UNIDENTIFIED READ: the probe runs, policy answers a
    /// narrower world, the absence assertion passes, and the runner
    /// records a green proof of nothing (61085a9e). So this door checks
    /// that one rule, on the car's own recorded text, and refuses to
    /// ship it — because a car can carry a probe no gate ever saw (a
    /// metadata PATCH is a door, and cars parked before the gate-side
    /// refusal existed still carry theirs).
    #[test]
    fn a_car_whose_probe_reads_the_sor_unidentified_is_refused_not_shipped() {
        let bad = json!({
            "proof_probe": "curl -fsS $BOSS_JOBS_URL/api/jobs?kind=gate-run | grep -q c0ffee",
            "proof_expect": "c0ffee",
        });
        let cars = [car("c1", "t1", bad, "ready")];
        let got = probe_requests(Scope::Train("t1"), &cars, &[], "r", "ev", "jobs.job.closed");
        assert!(
            got.requests.is_empty(),
            "a probe that reads unidentified must not be shipped to the forge: {:?}",
            got.requests
        );
        assert_eq!(got.refusals.len(), 1);
        let (id, attempt) = &got.refusals[0];
        assert_eq!(id, "c1");
        assert_eq!(attempt["refused"], boss_jobs::probe::UNIDENTIFIED_RULE);
        assert!(
            attempt["why"]
                .as_str()
                .unwrap_or_default()
                .contains("unidentified"),
            "the attempt must say what was wrong: {attempt}"
        );
        assert!(
            attempt["why"]
                .as_str()
                .unwrap_or_default()
                .contains("boss prove c1 --probe '"),
            "the attempt must name the hand proof that can be followed: {attempt}"
        );
        assert_eq!(
            attempt["probe"],
            "curl -fsS $BOSS_JOBS_URL/api/jobs?kind=gate-run | grep -q c0ffee"
        );
    }

    /// THE OTHER RULE WITH NO BACKSTOP DOWNSTREAM (backlog 71ec5a58,
    /// left by the builder of c0ac92b8). A probe that compares `git log
    /// --format=%cI` — a committer date carrying a -07:00 offset —
    /// against the SoR's UTC timestamps as STRINGS lies in both
    /// directions: it answered FAILED for a not-yet on car 746a1fac,
    /// and with the offsets the other way it answers PASS for an event
    /// that never happened. The forge runner cannot see that: the probe
    /// runs, the compare succeeds, and a proof of nothing is recorded
    /// on a car that then closes. The gate and `boss prove` refuse the
    /// token; a PATCHed probe meets neither, so this door refuses it
    /// too, under the same rule id.
    #[test]
    fn a_car_whose_probe_reads_git_time_with_an_offset_is_refused_not_shipped() {
        let probe = "c=$(git log -1 --format=%cI HEAD); t=$(boss-sor-read /api/audit | jq -r \
                     '.data[0].at // empty'); [ \"$t\" \\> \"$c\" ] && echo retire:after";
        let cars = [car(
            "c1",
            "t1",
            json!({"proof_probe": probe, "proof_expect": "retire:after"}),
            "ready",
        )];
        let got = probe_requests(Scope::Train("t1"), &cars, &[], "r", "ev", "jobs.job.closed");
        assert!(
            got.requests.is_empty(),
            "a probe that compares an offset git date as a string must not be shipped: {:?}",
            got.requests
        );
        assert_eq!(got.refusals.len(), 1);
        let (id, attempt) = &got.refusals[0];
        assert_eq!(id, "c1");
        assert_eq!(attempt["refused"], boss_jobs::probe::GIT_TIME_RULE);
        assert_eq!(attempt["unrunnable"], false);
        assert_eq!(attempt["probe"], probe);
        assert_eq!(attempt["expect"], "retire:after");
        let why = attempt["why"].as_str().unwrap_or_default();
        assert!(
            why.contains("%cI") && why.contains("%ct"),
            "the attempt must name the token and the epoch rewrite: {attempt}"
        );
        assert!(
            why.contains("boss prove c1 --probe '"),
            "the attempt must name the hand proof that can be followed: {attempt}"
        );
    }

    /// A THIRD RULE WITH NO BACKSTOP DOWNSTREAM (backlog ce72aea9). The
    /// machine gate writes one would-refuse fact per caller key per
    /// PROCESS, so a probe that clears a caller by counting post-cut facts
    /// in the window's `log.dirty`, and never reads the live tally's
    /// `snapshot.rows`, passes for a caller the running process had
    /// already tallied — whatever it sends. `boss gate` and `boss prove`
    /// say so to the person standing there; this door has nobody to say
    /// it to, and the forge runner sees only a count of zero. Measured
    /// live 2026-10-10: car 033c5c2f was recorded PROVEN by this door at
    /// 2026-10-08T23:00Z and car 099f8a9f at 2026-10-10T02:01Z, each on
    /// a log-only probe of this shape, after the warning had landed. The
    /// fixture is the shape both carried; the second car is the same
    /// probe that ALSO reads the live rows, and it is still shipped.
    #[test]
    fn a_car_whose_probe_clears_a_caller_from_the_gate_log_alone_is_refused_not_shipped() {
        let log_only = "n=$(boss-sor-read \"/api/events/gate-window?gate=machine-gate&hours=$hours\" \
                        | jq -r --arg from \"$jf\" '[.log.dirty[] | select(.kind == \
                        \"machine_gate.would_refuse\" and .payload.key.user == \
                        \"automation:gate-runner\" and .at > $from)] | length')\n\
                        case ${n:-empty} in empty|*[!0-9]*) echo 'not yet'; exit 75;; esac\n\
                        [ \"$n\" -eq 0 ] && echo cleared";
        let with_live = format!(
            "{log_only}\nlive=$(boss-sor-read \"/api/events/gate-window?gate=machine-gate&hours=$hours\" \
             | jq -r --arg from \"$jf\" '[.live[] | select(.snapshot.mode != \"off\" and .snapshot.overflow == 0 \
             and .snapshot.recording_since <= $from) | .snapshot.rows[] | select(.presented == \"none\")] | length')"
        );
        // The same count with NO kind named, verbatim from car 59e0d3dd's
        // recorded probe (review 4ce5ca85 B2): `log.dirty` holds only the
        // once-per-key kinds, so naming none of them is the same claim.
        let no_kind = "w=$(boss-sor-read '/api/events/gate-window?gate=machine-gate&hours=168')\n\
                       n=$(printf '%s' \"$w\" | jq --argjson s \"$since\" '[.log.dirty[] | \
                       select(.payload.key.user == \"automation:probe-admission-source-access\") \
                       | select((.at | sub(\"\\\\.[0-9]+Z$\"; \"Z\") | fromdate) > ($s + 1800))] \
                       | length')\n\
                       [ \"$n\" -eq 0 ] || { echo \"FAILED: $n tokenless reads\"; exit 1; }";
        // The live rows with NO log read and no start of the tally read
        // (backlog 002f5f31): a tally a train restarted after the cut
        // clears the caller just the same, and it used to meet no rule.
        let live_only = "live=$(boss-sor-read \"/api/events/gate-window?gate=machine-gate&hours=$hours\" \
                         | jq -r --arg from \"$jf\" '[.live[] | select(.snapshot.mode != \"off\" and \
                         .snapshot.overflow == 0) | .snapshot.rows[] | select(.user == \
                         \"automation:gate-runner\" and .presented == \"none\" and .last_seen > \
                         $from)] | length')\n\
                         [ \"$live\" -eq 0 ] && echo cleared";
        let cars = [
            car(
                "log-only",
                "t1",
                json!({"proof_probe": log_only, "proof_expect": "cleared"}),
                "ready",
            ),
            car(
                "reads-live",
                "t1",
                json!({"proof_probe": with_live, "proof_expect": "cleared"}),
                "ready",
            ),
            car(
                "no-kind",
                "t1",
                json!({"proof_probe": no_kind, "proof_expect": "cleared"}),
                "ready",
            ),
            car(
                "live-only",
                "t1",
                json!({"proof_probe": live_only, "proof_expect": "cleared"}),
                "ready",
            ),
        ];
        let got = probe_requests(Scope::Train("t1"), &cars, &[], "r", "ev", "jobs.job.closed");
        let shipped: Vec<&str> = got
            .requests
            .iter()
            .map(|r| r["metadata"]["car"].as_str().unwrap_or_default())
            .collect();
        assert_eq!(
            shipped,
            vec!["reads-live"],
            "only the probe that also reads the live tally may reach the forge"
        );
        let refused: Vec<&str> = got.refusals.iter().map(|(id, _)| id.as_str()).collect();
        assert_eq!(refused, vec!["log-only", "no-kind", "live-only"]);
        for (id, attempt) in &got.refusals {
            assert_eq!(attempt["refused"], boss_jobs::probe::GATE_LOG_ALONE_RULE);
            assert_eq!(attempt["unrunnable"], false);
            let why = attempt["why"].as_str().unwrap_or_default();
            // What was read is named as it was read: the fact kind for a
            // log count, the live rows for a live-only reading — and both
            // name the coverage the rows needed (002f5f31 item 2).
            let read = if id == "live-only" {
                boss_jobs::probe::LIVE_ROWS_ALONE
            } else {
                "machine_gate.would_refuse"
            };
            assert!(
                why.contains(read)
                    && why.contains("snapshot.rows")
                    && why.contains("`recording_since` compared with the cut"),
                "the attempt must name what was read and the live rows to read whole: {attempt}"
            );
            // N3 of review 4ce5ca85: every car this door sees has LANDED,
            // so "re-park" names an act nobody can take. The refusal
            // names the one that can be: the hand door, on this car, with
            // a replacement probe.
            // And it names it WHOLE (review 446039a0 F1): `--verified` is
            // required by the hand door whenever `--probe` is given, and
            // the command this refusal used to print left it out.
            assert!(
                why.contains(&boss_jobs::probe::hand_proof_command(id))
                    && why.contains("--verified '"),
                "the attempt must name the hand proof that can be followed: {attempt}"
            );
        }
        assert_eq!(got.refusals[0].1["probe"], log_only);
    }

    /// And the absent-tool rule is deliberately NOT re-checked here: the
    /// runner measures the actual host, which is strictly better than
    /// this handler predicting it from a manifest. A `kubectl` probe is
    /// still shipped, and comes back `unrunnable` naming the tool.
    #[test]
    fn a_probe_naming_a_tool_the_forge_lacks_is_still_shipped_for_the_runner_to_measure() {
        let cars = [car(
            "c1",
            "t1",
            json!({"proof_probe": "kubectl -n boss get deploy | grep -q boss-jobs",
                   "proof_expect": "boss-jobs"}),
            "ready",
        )];
        let got = probe_requests(Scope::Train("t1"), &cars, &[], "r", "ev", "jobs.job.closed");
        assert_eq!(got.requests.len(), 1);
        assert!(got.refusals.is_empty());
    }

    /// A car that closed LANDED with its proof owed (b9005734): closed,
    /// `proven` skipped by the close, the marker set, and a declared wait
    /// whose event a machine can match.
    fn owed_car(id: &str, event: Value) -> Value {
        let mut m = probed();
        m[car::PROOF_OWED] = json!("true");
        m[car::WAITS_ON] = json!({"on": "a cut-a-release tag", "seen": "true",
                                  "owner": "emp-david", car::WAITS_ON_EVENT: event});
        let mut c = car(id, "t1", m, "skipped");
        c["status"] = json!("closed");
        c
    }

    fn closed(id: &str, kind: &str, title: &str, subject: &str) -> Value {
        json!({"id": id, "kind": kind, "title": title, "outcome": "answered",
               "closed_on": "2026-09-24", "subject_id": subject, "parent_step_id": null})
    }

    /// THE OBLIGATION (b9005734): the close a car declared it waits on
    /// runs its recorded probe again — for a CLOSED car whose `proven`
    /// was skipped by the landing, which no other scope would ask after.
    /// The request says it is owed and names the packet whose close fired
    /// it, so a reader of the ops-request sees which event it answers.
    #[test]
    fn the_declared_close_runs_an_owed_proof_again() {
        let cars = [owed_car(
            "o1",
            json!({"closes": "ops-request", "title": "tag-release"}),
        )];
        let event = closed(
            "p9",
            "ops-request",
            "tag-release on forge — cut-a-release v0.4.0",
            "forge",
        );
        let got = probe_requests(
            Scope::Owed(&event),
            &cars,
            &[],
            "rerun-owed-proofs-on-job-closed",
            "ev",
            "jobs.job.closed",
        );
        assert_eq!(got.requests.len(), 1, "{got:?}");
        let md = &got.requests[0]["metadata"];
        assert_eq!(md["verb"], VERB);
        assert_eq!(md["car"], "o1");
        assert_eq!(md["owed"], true);
        assert_eq!(md["event_packet"], "p9");
        assert_eq!(md["recheck"], false);
    }

    /// Every close that is NOT the declared event asks nothing: another
    /// kind, the right kind under another title, and a car that owes
    /// nothing (never marked, or paid) however exactly its event matches.
    #[test]
    fn a_close_that_is_not_the_declared_event_or_a_car_that_owes_nothing_asks_nothing() {
        let event = json!({"closes": "maintenance-playground-crawl"});
        let mut paid = owed_car("paid", event.clone());
        paid["metadata"][car::PROOF_OWED] = json!("paid");
        let mut unmarked = owed_car("unmarked", event.clone());
        unmarked["metadata"]
            .as_object_mut()
            .unwrap()
            .remove(car::PROOF_OWED);
        let cars = [owed_car("o1", event), paid, unmarked];
        let fire = |e: &Value| {
            probe_requests(Scope::Owed(e), &cars, &[], "r", "ev", "jobs.job.closed")
                .requests
                .iter()
                .map(|r| r["metadata"]["car"].as_str().unwrap().to_string())
                .collect::<Vec<_>>()
        };
        assert_eq!(
            fire(&closed("p1", "maintenance-playground-crawl", "crawl", "x")),
            vec!["o1"]
        );
        assert!(fire(&closed("p2", "maintenance-sweep", "crawl", "x")).is_empty());
        let tag = [owed_car(
            "t",
            json!({"closes": "ops-request", "title": "tag-release"}),
        )];
        let converge = closed("p3", "ops-request", "converge on forge — a train", "forge");
        assert!(
            probe_requests(Scope::Owed(&converge), &tag, &[], "r", "ev", "t")
                .requests
                .is_empty()
        );
    }

    /// THE LOOP THIS SCOPE COULD MAKE, refused. The request it files is
    /// an `ops-request` whose close is itself a `jobs.job.closed`, so a
    /// car keyed to ops-request closes would re-run on its own probe's
    /// answer, and two such cars on each other's, forever. The close of
    /// ANY request this handler filed is never an owed event.
    #[test]
    fn the_close_of_a_probe_request_this_handler_filed_is_never_an_owed_event() {
        let cars = [
            owed_car("a", json!({"closes": "ops-request"})),
            owed_car("b", json!({"closes": "ops-request"})),
        ];
        for subject in ["a", "b", "some-open-car"] {
            let own = closed(
                "p1",
                "ops-request",
                &format!("{REQUEST_TITLE}Car {subject}"),
                subject,
            );
            let got = probe_requests(Scope::Owed(&own), &cars, &[], "r", "ev", "t");
            assert!(
                got.requests.is_empty(),
                "a probe request's close must not re-run owed proofs: {got:?}"
            );
        }
        // Any other ops-request close is the event both declared.
        let other = closed("p2", "ops-request", "publish-drift on boss-gcp", "boss-gcp");
        let got = probe_requests(Scope::Owed(&other), &cars, &[], "r", "ev", "t");
        assert_eq!(got.requests.len(), 2);
    }

    /// A LANDED, OPEN car whose probe ran and said not-yet, declaring the
    /// close it waits on (backlog 9e9db2ea): `proven` still ready, a
    /// `proof_attempt` on the car, and a `waits_on.event`.
    fn waiting_car(id: &str, event: Value) -> Value {
        let mut m = probed();
        m["proof_attempt"] = json!({"exit": "75", "stdout": "not yet: no gate closed since"});
        m[car::WAITS_ON] = json!({"on": "a full gate closing after the converge",
                                  "seen": "true", car::WAITS_ON_EVENT: event});
        car(id, "t1", m, "ready")
    }

    /// THE DECLARED CLOSE RE-RUNS A WAITING PROBE (backlog 9e9db2ea).
    /// Measured 2026-10-19 over the 50 newest proven cars: 6 were proven
    /// 24 to 315 minutes after their train arrived, every one on a probe
    /// that said not-yet at arrival and waited for the hourly recheck —
    /// four of them for a gate-run to close after the converge. The close
    /// the car declared fires its probe again at once; the request names
    /// the packet whose close fired it and is neither a recheck nor owed.
    #[test]
    fn the_declared_close_reruns_a_landed_cars_waiting_probe() {
        let cars = [waiting_car("w1", json!({"closes": "gate-run"}))];
        let event = closed("g7", "gate-run", "gate main@abc123", "main");
        let got = probe_requests(
            Scope::Declared(&event),
            &cars,
            &[],
            "rerun-waiting-probes-on-their-declared-close",
            "ev",
            "jobs.job.closed",
        );
        assert_eq!(got.requests.len(), 1, "{got:?}");
        let md = &got.requests[0]["metadata"];
        assert_eq!(md["car"], "w1");
        assert_eq!(md["train"], "t1");
        assert_eq!(md["event_packet"], "g7");
        assert_eq!(md["recheck"], false);
        assert_eq!(md["owed"], false);
    }

    /// Everything the declared scope must leave alone: a close that is
    /// not the declared one; a car whose probe never ran (not landed, or
    /// its train has not arrived — the arrival rule runs it first); a car
    /// already proven; a car that declares no machine event; a car with a
    /// request already open; and the close of one of OUR requests, which
    /// would loop a car keyed to ops-request closes on its own answer.
    #[test]
    fn the_declared_scope_asks_only_a_waiting_car_on_its_own_event() {
        let gate = json!({"closes": "gate-run"});
        let mut never_ran = waiting_car("never-ran", gate.clone());
        never_ran["metadata"]
            .as_object_mut()
            .unwrap()
            .remove("proof_attempt");
        let mut proven = waiting_car("proven", gate.clone());
        proven["steps"][0]["status"] = json!("completed");
        // `proven` not yet reached: the declared scope asks only a car
        // whose `proven` is ready or active. Without this row, loosening
        // it to the owed scope's "anything but completed" survived
        // mutation (backlog 7838df7d, review 4890fdfd N5).
        let mut pending = waiting_car("pending", gate.clone());
        pending["steps"][0]["status"] = json!("pending");
        let mut undeclared = waiting_car("undeclared", gate.clone());
        undeclared["metadata"][car::WAITS_ON]
            .as_object_mut()
            .unwrap()
            .remove(car::WAITS_ON_EVENT);
        let cars = [
            waiting_car("w1", gate.clone()),
            never_ran,
            proven,
            pending,
            undeclared,
            waiting_car("asked", gate),
        ];
        let open = [request("r1", VERB, "asked", "ready")];
        let fire = |e: &Value| {
            probe_requests(Scope::Declared(e), &cars, &open, "r", "ev", "t")
                .requests
                .iter()
                .map(|r| r["metadata"]["car"].as_str().unwrap().to_string())
                .collect::<Vec<_>>()
        };
        assert_eq!(fire(&closed("g1", "gate-run", "gate", "main")), vec!["w1"]);
        assert!(fire(&closed("p1", "pr-train", "train", "main")).is_empty());
        let ops = [waiting_car("o", json!({"closes": "ops-request"}))];
        let own = closed("p2", "ops-request", &format!("{REQUEST_TITLE}Car o"), "o");
        assert!(
            probe_requests(Scope::Declared(&own), &ops, &[], "r", "ev", "t")
                .requests
                .is_empty(),
            "a probe request's close must not re-run a waiting probe"
        );
    }

    /// The request's title is built from the one prefix the loop guard
    /// reads, so the two cannot drift apart (CLAUDE.md §9a).
    #[test]
    fn every_filed_request_carries_the_title_the_loop_guard_reads() {
        let cars = [car("c1", "t1", probed(), "ready")];
        let got = probe_requests(Scope::Train("t1"), &cars, &[], "r", "ev", "jobs.job.closed");
        assert!(
            got.requests[0]["title"]
                .as_str()
                .unwrap()
                .starts_with(REQUEST_TITLE)
        );
    }

    /// An open ops-request as the board lists it: its `execute` step in
    /// `execute_status`.
    fn request(id: &str, verb: &str, car_id: &str, execute_status: &str) -> Value {
        json!({
            "id": id, "kind": "ops-request", "status": "open",
            "metadata": {"host": "forge", "verb": verb, "car": car_id},
            "steps": [
                {"id": format!("{id}-filed"), "spec_slug": "filed", "status": "completed"},
                {"id": format!("{id}-execute"), "spec_slug": "execute", "status": execute_status},
                {"id": format!("{id}-answered"), "spec_slug": "answered", "status": "pending"},
            ]
        })
    }

    /// BACKLOG 17bf6f3e. Car 176d5c9d was last probed at
    /// 2026-10-09T04:00:42Z while every other failing car was rechecked
    /// hourly: its request e8dd5479 had RUN — `execute` completed at
    /// 04:01:20Z — and stayed open because its packet was never settled,
    /// and this guard read any open request as one still being asked. A
    /// request whose execute completed has been answered; it asks nothing.
    #[test]
    fn a_request_whose_execute_completed_is_not_still_asking() {
        let mut attempt = probed();
        attempt["proof_attempt"] = json!({"exit": "75", "why": "not yet"});
        let cars = [
            car("ran", "t1", attempt.clone(), "ready"),
            car("waiting", "t1", attempt, "ready"),
        ];
        let open = [
            request("r-ran", VERB, "ran", "completed"),
            request("r-waiting", VERB, "waiting", "ready"),
        ];
        let got = probe_requests(
            Scope::Failing,
            &cars,
            &open,
            "recheck-failing-probes-hourly",
            "ev",
            "clock.tick",
        );
        let asked: Vec<&str> = got
            .requests
            .iter()
            .map(|r| r["metadata"]["car"].as_str().unwrap())
            .collect();
        assert_eq!(
            asked,
            vec!["ran"],
            "the answered request no longer holds its car; the waiting one still does"
        );
    }

    /// The other half: the answered-but-open request is not merely
    /// ignored, it is settled. Every open ops-request whose `execute`
    /// completed, whatever its verb (85cd57f1 was a disk-report), gets
    /// its execute completion re-sent — the step PUT's re-send settles
    /// an open packet (readiness, then the declared-terminal close), and
    /// writes nothing to a step or packet that needs nothing. A request
    /// still waiting on its execute is left to the runner.
    #[test]
    fn every_open_request_whose_execute_completed_is_named_for_its_resend() {
        let open = [
            request("r1", VERB, "c1", "completed"),
            request("r2", "disk-report", "", "completed"),
            request("r3", VERB, "c3", "ready"),
            json!({"id": "r4", "kind": "ops-request", "status": "open", "metadata": {}}),
        ];
        assert_eq!(
            unsettled_requests(&open),
            vec![
                ("r1".to_string(), "r1-execute".to_string()),
                ("r2".to_string(), "r2-execute".to_string()),
            ]
        );
    }

    /// BACKLOG a2aff635 (landed-review fc7b75f6, finding 2). The hourly
    /// re-send stopped at the first request that could not settle, so
    /// one packet whose pinned version will not resolve (500) or that
    /// sits outside the dispatcher's scope (404) dead-lettered every
    /// firing AND kept every answered request after it open. Three
    /// answered requests against a stub board: the first and third
    /// refuse, the middle one settles. Every re-send is attempted, and
    /// the one error names both refusals and not the request that
    /// settled.
    #[tokio::test]
    async fn one_request_that_cannot_settle_does_not_stop_the_re_sends_after_it() {
        use axum::Router;
        use axum::http::{Method, StatusCode, Uri};
        use axum::response::IntoResponse;
        use std::sync::Mutex;

        let board = Arc::new(json!({
            "data": [
                request("r1", VERB, "c1", "completed"),
                request("r2", VERB, "c2", "completed"),
                request("r3", "disk-report", "", "completed"),
            ],
            "total": 3,
        }));
        let puts: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
        let (b, p) = (board.clone(), puts.clone());
        let app = Router::new().fallback(move |method: Method, uri: Uri| {
            let (board, puts) = (b.clone(), p.clone());
            async move {
                let query = uri.query().unwrap_or("");
                if method == Method::GET && uri.path() == "/api/jobs" {
                    return if query.contains("kind=ops-request") {
                        axum::Json((*board).clone()).into_response()
                    } else {
                        axum::Json(json!({"data": [], "total": 0})).into_response()
                    };
                }
                if method == Method::PUT {
                    let path = uri.path().to_string();
                    puts.lock().unwrap().push(path.clone());
                    return match path.as_str() {
                        "/api/jobs/r1/steps/r1-execute" => (
                            StatusCode::INTERNAL_SERVER_ERROR,
                            "pinned version v3 not found",
                        )
                            .into_response(),
                        "/api/jobs/r3/steps/r3-execute" => {
                            (StatusCode::NOT_FOUND, "not in scope").into_response()
                        }
                        _ => axum::Json(json!({})).into_response(),
                    };
                }
                StatusCode::NOT_FOUND.into_response()
            }
        });
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });

        let handler = JobsRunCarProbes::new(format!("http://{addr}"));
        let args = [(
            "scope".to_string(),
            boss_dispatcher::rules::expr::Value::String("failing".into()),
        )];
        let ctx = InvocationContext {
            rule_name: "recheck-failing-probes-hourly".into(),
            triggering_event_id: "ev".into(),
            triggering_topic: "clock.tick".into(),
            event_payload: json!({}),
            event_timestamp: None,
        };
        let err = handler
            .invoke(&args, &ctx)
            .await
            .expect_err("two re-sends refused, so the firing fails");

        assert_eq!(
            *puts.lock().unwrap(),
            vec![
                "/api/jobs/r1/steps/r1-execute",
                "/api/jobs/r2/steps/r2-execute",
                "/api/jobs/r3/steps/r3-execute",
            ],
            "every answered request is re-sent, the ones after a refusal included"
        );
        let said = err.to_string();
        assert!(
            said.contains("r1") && said.contains("r3") && !said.contains("r2"),
            "the one error names each request that failed, and only those: {said}"
        );
        assert!(
            !err.is_permanent(),
            "a 500 among the failures keeps the firing retryable: {said}"
        );
    }

    /// A stub board for the write loops: GET answers `cars` for the
    /// ship-a-change walk and `requests` for the ops-request walk; every
    /// write is recorded as `"<METHOD> <car or path>"` (a POST by the car
    /// its body names), and a write whose record is in `refuse` answers
    /// 500. Returns the base URL and the record.
    async fn write_board(
        cars: Vec<Value>,
        requests: Vec<Value>,
        refuse: &'static [&'static str],
    ) -> (String, Arc<std::sync::Mutex<Vec<String>>>) {
        use axum::Router;
        use axum::http::{Method, StatusCode, Uri};
        use axum::response::IntoResponse;
        use std::sync::Mutex;

        let (cars, requests) = (Arc::new(cars), Arc::new(requests));
        let writes: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
        let w = writes.clone();
        let app = Router::new().fallback(move |method: Method, uri: Uri, body: String| {
            let (cars, requests, writes) = (cars.clone(), requests.clone(), w.clone());
            async move {
                let query = uri.query().unwrap_or("");
                if method == Method::GET && uri.path() == "/api/jobs" {
                    let rows = if query.contains("kind=ship-a-change") {
                        (*cars).clone()
                    } else if query.contains("kind=ops-request") {
                        (*requests).clone()
                    } else {
                        Vec::new()
                    };
                    let total = rows.len();
                    return axum::Json(json!({"data": rows, "total": total})).into_response();
                }
                let record = if method == Method::POST {
                    let sent: Value = serde_json::from_str(&body).unwrap_or(Value::Null);
                    format!("POST {}", sent["metadata"]["car"].as_str().unwrap_or("?"))
                } else {
                    format!("{method} {}", uri.path())
                };
                writes.lock().unwrap().push(record.clone());
                if refuse.contains(&record.as_str()) {
                    return (StatusCode::INTERNAL_SERVER_ERROR, "rollout gap").into_response();
                }
                axum::Json(json!({})).into_response()
            }
        });
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        (format!("http://{addr}"), writes)
    }

    fn hourly_ctx() -> InvocationContext {
        InvocationContext {
            rule_name: "recheck-failing-probes-hourly".into(),
            triggering_event_id: "ev".into(),
            triggering_topic: "clock.tick".into(),
            event_payload: json!({}),
            event_timestamp: None,
        }
    }

    fn failing_scope() -> [(String, boss_dispatcher::rules::expr::Value); 1] {
        [(
            "scope".to_string(),
            boss_dispatcher::rules::expr::Value::String("failing".into()),
        )]
    }

    /// ONE CAR'S REFUSAL THAT CANNOT BE WRITTEN DOES NOT STOP THE OTHERS
    /// (backlog 8b76fab9). The refusal PATCH sat behind a `?` in its
    /// loop, so the first car whose write failed — a 5xx, a rollout gap —
    /// ended the firing: every car after it went unrecorded, and the
    /// hourly re-send below never ran. Three refused cars, the first
    /// PATCH answers 500: all three are written, the answered request is
    /// still re-sent, and the one error names car c1 alone.
    #[tokio::test]
    async fn one_refusal_that_cannot_be_written_does_not_stop_the_cars_after_it() {
        let refused = || {
            json!({
                "proof_probe": "curl -fsS $BOSS_JOBS_URL/api/jobs?kind=gate-run | grep -q c0ffee",
                "proof_expect": "c0ffee",
                "proof_attempt": {"exit": "1", "why": "printed nothing"},
            })
        };
        let cars = ["c1", "c2", "c3"]
            .map(|id| car(id, "t1", refused(), "ready"))
            .to_vec();
        let (base, writes) = write_board(
            cars,
            vec![request("r9", VERB, "c9", "completed")],
            &["PATCH /api/jobs/c1/metadata"],
        )
        .await;
        let err = JobsRunCarProbes::new(base)
            .invoke(&failing_scope(), &hourly_ctx())
            .await
            .expect_err("car c1's refusal could not be written");

        assert_eq!(
            *writes.lock().unwrap(),
            vec![
                "PATCH /api/jobs/c1/metadata",
                "PATCH /api/jobs/c2/metadata",
                "PATCH /api/jobs/c3/metadata",
                "PUT /api/jobs/r9/steps/r9-execute",
            ],
            "every refusal is written and the re-send still runs"
        );
        let said = err.to_string();
        assert!(
            said.contains("c1") && !said.contains("c2") && !said.contains("c3"),
            "the error names the car whose write failed, and only it: {said}"
        );
        assert!(!err.is_permanent(), "a 500 is retryable: {said}");
    }

    /// The same for the request POST (backlog 8b76fab9): car c1's
    /// ops-request cannot be filed, and cars c2 and c3 still get theirs.
    #[tokio::test]
    async fn one_request_that_cannot_be_filed_does_not_stop_the_cars_after_it() {
        let failed = || {
            let mut p = probed();
            p["proof_attempt"] = json!({"exit": "1", "why": "printed nothing"});
            p
        };
        let cars = ["c1", "c2", "c3"]
            .map(|id| car(id, "t1", failed(), "ready"))
            .to_vec();
        let (base, writes) = write_board(
            cars,
            vec![request("r9", VERB, "c9", "completed")],
            &["POST c1"],
        )
        .await;
        let err = JobsRunCarProbes::new(base)
            .invoke(&failing_scope(), &hourly_ctx())
            .await
            .expect_err("car c1's request could not be filed");

        assert_eq!(
            *writes.lock().unwrap(),
            vec![
                "POST c1",
                "POST c2",
                "POST c3",
                "PUT /api/jobs/r9/steps/r9-execute",
            ],
            "every request is filed and the re-send still runs"
        );
        let said = err.to_string();
        assert!(
            said.contains("c1") && !said.contains("c2") && !said.contains("c3"),
            "the error names the car whose request failed, and only it: {said}"
        );
        assert!(!err.is_permanent(), "a 500 is retryable: {said}");
    }

    /// The verdict's other two arms: nothing failed is `Ok`, and a set
    /// of failures that are ALL permanent (422s) stays permanent, as the
    /// first one alone was under the old `?`.
    #[test]
    fn the_re_send_verdict_is_ok_when_none_failed_and_permanent_only_when_all_were() {
        assert!(resend_verdict(Vec::new()).is_ok());
        let all_permanent = resend_verdict(vec![
            ("r1".into(), HandlerError::Permanent("422".into())),
            ("r2".into(), HandlerError::Permanent("422".into())),
        ])
        .expect_err("two failures");
        assert!(all_permanent.is_permanent(), "{all_permanent}");
        let said = all_permanent.to_string();
        assert!(said.contains("r1: ") && said.contains("r2: "), "{said}");
    }
}
