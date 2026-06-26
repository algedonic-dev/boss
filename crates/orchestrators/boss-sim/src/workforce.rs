//! `workforce` — the sim-as-workforce executor.
//!
//! The simulator stops holding a job/step mirror. Instead it acts as the
//! workforce the way real executors do: it reads the live system (the
//! clock, its open assignments, real inventory) and works them through
//! the public API. This is the executor side of BOSS's
//! "human-powered state machine" framing — the sim is just CPUs driving
//! the same state machine a real deployment runs.
//!
//! ## Clock-coordinated, never clock-driving
//!
//! Time comes from the clock-api's freely-advancing formula clock
//! (`sim = epoch_start + (wall_now − wall_anchor) × warp_factor`). The
//! workforce *reads* `/api/clock/now`; it never manipulates the clock.
//! A step that became Active at `started_at` completes only once
//! `now ≥ started_at + duration` — so a 5-day fermentation is genuinely
//! held for five sim-days of clock time while a 15-minute QC finishes
//! almost immediately. Durations are real, paced by the clock, not a
//! per-tick completion-probability roll.
//!
//! ## Pull the whole assigned backlog in one query
//!
//! Routing and execution are separate layers. The dispatcher (and, later,
//! managers) ASSIGN Ready steps to employees; the workforce EXECUTES what
//! it's been assigned. Each pass pulls the entire assigned-and-workable
//! backlog in a single query — `GET /api/jobs/assignments?all_assigned=true`
//! — and drives every step in it, attributing the work to each step's own
//! assignee. The workforce holds no roster and makes no routing decision:
//! it works whatever has been assigned, regardless of who assigned it or
//! to whom. That keeps it decoupled from assignment policy (load
//! distribution can change without touching the executor) and replaces a
//! per-employee query fan-out — which dominated pass time — with one
//! round-trip.
//!
//! Each `work_once`:
//! 1. read `/api/clock/now`
//! 2. `GET /api/jobs/assignments?all_assigned=true` — every assigned
//!    Ready step to start + Active step to finish
//! 3. for each:
//!    - Ready: gate (production-consume checks *real* inventory and
//!      defers if short; demand-gate is handled at completion) → claim
//!      (Ready→Active, stamp `metadata.started_at`)
//!    - Active: if `now ≥ started_at + duration`, complete
//!      (Active→Completed; demand-gate reads real finished-goods stock to
//!      decide brew vs oversupply); otherwise leave it in progress
//!
//! The driver calls `work_once` on a wall-cadence until the clock
//! reaches `epoch_end`.

use std::collections::HashMap;

use anyhow::{Context, Result};
use chrono::{DateTime, Duration, Utc};
use serde::Deserialize;
use serde_json::{Value, json};
use tracing::{debug, warn};

use crate::api_activity::{self, ActorKind, ApiActivity};

/// Default duration for step kinds with no `typical_duration_hours`
/// (generic / task / sub-job) — paces an untyped step as a full
/// working day.
const DEFAULT_STEP_HOURS: f64 = 8.0;

/// Resolve a service URL the same way `LiveApiOutput` does: `direct://`
/// host hits per-service prod ports, `scratch://` adds the +1000 offset,
/// anything else is treated as a gateway base prefix.
fn service_url(api_base: &str, path: &str) -> String {
    let (host, offset) = if let Some(rest) = api_base.strip_prefix("direct://") {
        (rest, 0i32)
    } else if let Some(rest) = api_base.strip_prefix("scratch://") {
        (rest, 1000i32)
    } else {
        return format!("{api_base}{path}");
    };
    let base_port: i32 = if path.starts_with("/api/jobs") {
        7900
    } else if path.starts_with("/api/people") {
        7500
    } else if path.starts_with("/api/inventory") {
        7300
    } else if path.starts_with("/api/products") {
        7840
    } else if path.starts_with("/api/clock") {
        7060
    } else {
        4443
    };
    format!("http://{host}:{}{path}", base_port + offset)
}

/// A required-at-done field the executor must supply when completing a
/// step — its `name` and the StepType `field_type` (enum types arrive
/// pipe-joined, e.g. `"pass|fail|conditional"`). The workforce synthesizes
/// a type-appropriate value when the JobKind didn't default it — the
/// simulated worker filling the step's form the way a human would before
/// marking it done. Built from the StepRegistry's `FieldSpec`s and passed
/// to [`Workforce::new`].
#[derive(Debug, Clone)]
pub struct RequiredField {
    pub name: String,
    pub field_type: String,
}

#[derive(Debug, Default, Clone, serde::Serialize)]
pub struct WorkforceStats {
    pub checkins: u64,
    pub claimed: u64,
    pub completed: u64,
    /// Steps left Ready because real inventory was short (the
    /// auto-reorder catches up).
    pub deferred: u64,
    /// Steps left Active because their duration hasn't elapsed yet.
    pub in_progress: u64,
    pub errors: u64,
}

/// How many steps the workforce drives in parallel per check-in.
/// Completion is the sim's bottleneck: each step claim/complete is a
/// blocking HTTP round-trip, so a serial loop caps throughput at
/// ~10 completions/wall-sec, which trails job generation at warp and
/// grows WIP over a long regen. Completion capacity ≈ (polls ×
/// workers) has to cover the run's step-completions or open Jobs pile
/// up. Completions hit jobs-api + the side-effect services, whose
/// connection pools have the headroom, and any transient blip is
/// NAK'd + redelivered by the JetStream layer rather than lost. The
/// invariant: saturate the DB, don't error it — if a service pool
/// starts queueing, dead-letters reappear and this comes back down.
const WORKFORCE_WORKERS: usize = 16;

/// Per-step tally returned by `work_step` so the parallel completion loop
/// sums results without sharing `&mut self.stats` across threads.
#[derive(Debug, Default)]
struct StepDelta {
    claimed: u64,
    completed: u64,
    deferred: u64,
    in_progress: u64,
    errors: u64,
}

#[derive(Debug, Deserialize)]
struct ClockNowResp {
    now: DateTime<Utc>,
    #[serde(default)]
    epoch_end: Option<chrono::NaiveDate>,
}

/// The sim-as-workforce executor. Holds no job/step state — every
/// decision reads the live system.
pub struct Workforce {
    client: reqwest::blocking::Client,
    api_base: String,
    /// StepType kind → typical duration hours, sourced from the
    /// StepRegistry. Drives the duration-gated completion.
    durations: HashMap<String, f64>,
    /// StepType kind → its required-at-done fields, sourced from the
    /// StepRegistry. On completion the workforce supplies any the JobKind
    /// didn't default — the executor filling the step's form.
    required_fields: HashMap<String, Vec<RequiredField>>,
    /// Shared per-actor API-call tally (cockpit telemetry). The daemon
    /// injects the shared handle via `with_actor_telemetry`; default is a
    /// detached fresh handle so non-daemon callers + tests work unchanged.
    api_activity: ApiActivity,
    /// Employee id → role. Attributes this executor's `PUT /steps` calls
    /// to the worker's role (sign-offs carry their own role). The workforce
    /// holds no *routing* roster — this is display attribution only.
    emp_roles: HashMap<String, String>,
    pub stats: WorkforceStats,
}

impl Workforce {
    /// `api_base` accepts the same forms as `LiveApiOutput::new`
    /// (`direct://host`, `scratch://host`, or a gateway base).
    /// `durations` maps StepType kind → typical_duration_hours;
    /// `required_fields` maps StepType kind → its required-at-done fields
    /// (so the worker can fill any the JobKind didn't default).
    pub fn new(
        api_base: &str,
        durations: HashMap<String, f64>,
        required_fields: HashMap<String, Vec<RequiredField>>,
    ) -> Self {
        // Same actor identity as LiveApiOutput: id=system, role=
        // system-sim, plus x-sim-origin so the receiving services'
        // policy gate takes the sim bypass. Attribution to the real
        // employee rides on the body's assignee_id / completed_by.
        let mut headers = reqwest::header::HeaderMap::new();
        let actor = json!({
            "id": "automation:sim",
            "role": "system-sim",
            "access_tier": "operator",
            "territory_account_ids": [],
            "direct_report_ids": [],
            "department": "platform",
        })
        .to_string();
        if let Ok(v) = reqwest::header::HeaderValue::from_str(&actor) {
            headers.insert("x-boss-user", v);
        }
        headers.insert(
            "x-sim-origin",
            reqwest::header::HeaderValue::from_static("true"),
        );
        let client = reqwest::blocking::Client::builder()
            .default_headers(headers)
            .timeout(std::time::Duration::from_secs(30))
            .build()
            .expect("HTTP client");
        Self {
            client,
            api_base: api_base.to_string(),
            durations,
            required_fields,
            api_activity: api_activity::new_handle(),
            emp_roles: HashMap::new(),
            stats: WorkforceStats::default(),
        }
    }

    /// Inject the shared per-actor API-activity handle + the emp→role map
    /// (cockpit telemetry). The daemon creates one handle, hands it to
    /// both the workforce + the live output, and snapshots it each tick.
    pub fn with_actor_telemetry(
        mut self,
        handle: ApiActivity,
        emp_roles: HashMap<String, String>,
    ) -> Self {
        self.api_activity = handle;
        self.emp_roles = emp_roles;
        self
    }

    /// The role to attribute an employee's API calls to (cockpit display).
    fn role_of(&self, emp: &str) -> &str {
        self.emp_roles
            .get(emp)
            .map(String::as_str)
            .unwrap_or("unassigned-role")
    }

    /// Record one workforce call on its ack, under the Employee actor —
    /// rolled up by `role` (the cockpit panel grouping) and counting `emp`
    /// toward that role's distinct-people tally.
    fn record_employee_call(&self, method: &str, path: &str, emp: &str, role: &str, ok: bool) {
        let endpoint = api_activity::endpoint_label(method, path);
        api_activity::record(
            &self.api_activity,
            ActorKind::Employee,
            role,
            &endpoint,
            ok,
            emp,
        );
    }

    fn duration_hours(&self, kind: &str) -> f64 {
        self.durations
            .get(kind)
            .copied()
            .unwrap_or(DEFAULT_STEP_HOURS)
    }

    /// Read the freely-advancing sim clock. Returns `(now, epoch_end)`.
    pub fn clock_now(&self) -> Result<(DateTime<Utc>, Option<chrono::NaiveDate>)> {
        let url = service_url(&self.api_base, "/api/clock/now");
        let resp = self
            .client
            .get(&url)
            .send()
            .with_context(|| format!("GET {url}"))?;
        let parsed: ClockNowResp = resp.json().context("decode /api/clock/now")?;
        Ok((parsed.now, parsed.epoch_end))
    }

    /// Configure the formula clock ONCE at run kickoff: sim-time starts
    /// at `epoch_start` (as of wall-now) and free-runs at `warp_factor`
    /// sim-seconds per wall-second up to `epoch_end`. After this the
    /// clock is never touched again — the whole system coordinates
    /// against it. `warp_factor` is the single pacing knob (wall-time ≈
    /// sim-span ÷ warp); pick the fastest the system sustains.
    pub fn configure_clock(
        &self,
        epoch_start: chrono::NaiveDate,
        epoch_end: chrono::NaiveDate,
        warp_factor: f64,
    ) -> Result<()> {
        let url = service_url(&self.api_base, "/api/clock/configure");
        let body = json!({
            "epoch_start": epoch_start.to_string(),
            "epoch_end": epoch_end.to_string(),
            "warp_factor": warp_factor,
        });
        let resp = self
            .client
            .post(&url)
            .json(&body)
            .send()
            .with_context(|| format!("POST {url}"))?;
        if !resp.status().is_success() {
            let status = resp.status();
            let text = resp.text().unwrap_or_default();
            anyhow::bail!("clock configure {url} -> {status}: {text}");
        }
        Ok(())
    }

    /// One workforce check-in pass against the current clock instant.
    ///
    /// Pulls the entire assigned-and-workable backlog in ONE query and
    /// drives each step. No roster, no per-employee fan-out: the executor
    /// works whatever has been assigned (see the module docs).
    pub fn work_once(&mut self) -> Result<()> {
        let (now, _) = self.clock_now()?;
        self.stats.checkins += 1;
        let url = service_url(
            &self.api_base,
            "/api/jobs/assignments?all_assigned=true&limit=50000",
        );
        let resp = self
            .client
            .get(&url)
            .send()
            .with_context(|| format!("GET {url}"))?;
        let body: Value = resp.json().context("decode /api/jobs/assignments")?;
        let rows = body
            .get("data")
            .and_then(|d| d.as_array())
            .cloned()
            .unwrap_or_default();

        // Drive steps with bounded parallelism (see WORKFORCE_WORKERS). The
        // blocking client is Clone+Send+Sync and each step claims/completes
        // its own row independently, so N workers pull from a shared queue
        // and tally local deltas — no `&mut self` shared across threads. A
        // single Job's steps still surface one at a time (each goes Ready
        // only after its predecessor completes), so this doesn't reorder a
        // Job's pipeline.
        let this: &Self = self;
        let next = std::sync::atomic::AtomicUsize::new(0);
        let deltas: Vec<StepDelta> = std::thread::scope(|scope| {
            let handles: Vec<_> = (0..WORKFORCE_WORKERS)
                .map(|_| {
                    scope.spawn(|| {
                        let mut d = StepDelta::default();
                        loop {
                            let i = next.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                            let Some(row) = rows.get(i) else { break };
                            match this.work_step(row, now) {
                                Ok(sd) => {
                                    d.claimed += sd.claimed;
                                    d.completed += sd.completed;
                                    d.deferred += sd.deferred;
                                    d.in_progress += sd.in_progress;
                                }
                                Err(e) => {
                                    d.errors += 1;
                                    warn!(error = %e, "workforce: step failed");
                                }
                            }
                        }
                        d
                    })
                })
                .collect();
            handles
                .into_iter()
                .map(|h| h.join().unwrap_or_default())
                .collect()
        });
        for d in deltas {
            self.stats.claimed += d.claimed;
            self.stats.completed += d.completed;
            self.stats.deferred += d.deferred;
            self.stats.in_progress += d.in_progress;
            self.stats.errors += d.errors;
        }
        Ok(())
    }

    fn work_step(&self, row: &Value, now: DateTime<Utc>) -> Result<StepDelta> {
        let mut delta = StepDelta::default();
        let job_id = row.get("job_id").and_then(|v| v.as_str()).unwrap_or("");
        let step = row.get("step").unwrap_or(&Value::Null);
        let step_id = step.get("id").and_then(|v| v.as_str()).unwrap_or("");
        let kind = step.get("kind").and_then(|v| v.as_str()).unwrap_or("");
        let status = step.get("status").and_then(|v| v.as_str()).unwrap_or("");
        let authored_fields: Vec<(String, String)> = step
            .get("fields")
            .and_then(|v| v.as_array())
            .map(|a| {
                a.iter()
                    .filter_map(|f| {
                        let required = f.get("required").and_then(|r| r.as_bool()).unwrap_or(false);
                        if !required {
                            return None;
                        }
                        Some((
                            f.get("name")?.as_str()?.to_string(),
                            f.get("field_type")?.as_str()?.to_string(),
                        ))
                    })
                    .collect()
            })
            .unwrap_or_default();
        let sign_offs_required: Vec<String> = step
            .get("sign_offs_required")
            .and_then(|v| v.as_array())
            .map(|a| {
                a.iter()
                    .filter_map(|r| r.as_str().map(str::to_string))
                    .collect()
            })
            .unwrap_or_default();
        let metadata = step.get("metadata").cloned().unwrap_or_else(|| json!({}));
        if job_id.is_empty() || step_id.is_empty() {
            return Ok(delta);
        }
        // Execute only ASSIGNED work, attributed to the actual assignee.
        // An unassigned row is someone else's to route (the dispatcher
        // assigns role-bearing steps; the marker handler completes the
        // no-role markers) — skip it.
        let emp = match assignee_of(step) {
            Some(e) => e.to_string(),
            None => return Ok(delta),
        };

        match status {
            "ready" => {
                // Don't start a brew's consume without the ingredients —
                // leave it Ready so the auto-reorder can catch up; the
                // role queue re-surfaces it next pass.
                if kind == "production-consume" && self.short_on_ingredients(&metadata)? {
                    delta.deferred += 1;
                    return Ok(delta);
                }
                self.claim(job_id, step_id, &emp, &metadata, now)?;
                delta.claimed += 1;
                // An assigned zero-duration step completes the same pass —
                // no point holding it. (Structural markers complete
                // elsewhere: triggers are resolved at materialization, and
                // outcome / milestone are completed by the dispatcher's
                // marker handler — none are ever assigned to a worker.)
                if self.duration_hours(kind) <= 0.0 {
                    self.complete(
                        job_id,
                        step_id,
                        kind,
                        &metadata,
                        &emp,
                        &sign_offs_required,
                        &authored_fields,
                        now,
                    )?;
                    delta.completed += 1;
                }
            }
            "active" => {
                let started = metadata
                    .get("started_at")
                    .and_then(|v| v.as_str())
                    .and_then(|s| DateTime::parse_from_rfc3339(s).ok())
                    .map(|d| d.with_timezone(&Utc));
                let elapsed_enough = match started {
                    Some(start) => now >= start + duration_from_hours(self.duration_hours(kind)),
                    // No stamp (claimed before this model / orphan) — let
                    // it complete rather than wedge forever.
                    None => true,
                };
                if elapsed_enough {
                    self.complete(
                        job_id,
                        step_id,
                        kind,
                        &metadata,
                        &emp,
                        &sign_offs_required,
                        &authored_fields,
                        now,
                    )?;
                    delta.completed += 1;
                } else {
                    delta.in_progress += 1;
                }
            }
            _ => {}
        }
        Ok(delta)
    }

    /// Ready → Active. Stamps `started_at` so the completion gate can
    /// measure elapsed sim-time. Metadata is sent whole (PATCH-on-PUT
    /// replaces it wholesale) so no existing keys are lost.
    fn claim(
        &self,
        job_id: &str,
        step_id: &str,
        emp: &str,
        metadata: &Value,
        now: DateTime<Utc>,
    ) -> Result<()> {
        let mut md = metadata.clone();
        if let Some(obj) = md.as_object_mut() {
            obj.insert("started_at".to_string(), json!(now.to_rfc3339()));
        }
        let body = json!({
            "status": "active",
            "assignee_id": emp,
            "metadata": md,
        });
        self.put_step(job_id, step_id, &body, emp)
    }

    /// Active → Completed, attributed to `emp`. For a demand-gate step,
    /// reads real finished-goods stock to stamp the brew/oversupply
    /// outcome the JobKind forks on. Co-signs in the same PUT when the
    /// step needs sign-off (the sim-origin bypass authorizes it).
    fn complete(
        &self,
        job_id: &str,
        step_id: &str,
        kind: &str,
        metadata: &Value,
        emp: &str,
        sign_offs_required: &[String],
        authored_fields: &[(String, String)],
        now: DateTime<Utc>,
    ) -> Result<()> {
        let mut body = json!({
            "status": "completed",
            "completed_by": emp,
        });
        let obj = body.as_object_mut().expect("object");
        // Supply the step's required-at-done fields the executor would
        // fill in. Start from the current metadata so PATCH-on-PUT keeps
        // existing keys, then fill any required field the JobKind didn't
        // already default.
        let mut md = metadata.as_object().cloned().unwrap_or_default();
        self.fill_required_fields(kind, &mut md, now);
        // Inline authoring: the step's own required fields
        // are part of the completion contract too.
        for (name, field_type) in authored_fields {
            if !md.contains_key(name) {
                md.insert(name.clone(), synth_field_value(field_type, name, now));
            }
        }
        // Gates (demand-gate / availability-gate) are agent-executed: the
        // dispatcher reads real stock and stamps the outcome on step.ready.
        // The workforce only drives assigned steps and agent steps are never
        // assigned, so the workforce never sees a gate — the gate decision
        // lives in boss-dispatcher's gate.resolve handler (the sim drives
        // only labor; see docs/design/brewery-model-completeness.md).
        obj.insert("metadata".to_string(), Value::Object(md.clone()));
        if sign_offs_required.is_empty() {
            return self.put_step(job_id, step_id, &body, emp);
        }
        // Sign-off contract: stamps attest the step's FINAL shape, so the
        // metadata the executor fills lands first, then the stamps,
        // then the status flip. Stamping happens as the role-matched
        // human — policy (the seeded step-signoff:<role> rules)
        // decides, no sim exemption.
        self.put_step(
            job_id,
            step_id,
            &json!({ "metadata": Value::Object(md) }),
            emp,
        )?;
        for role in sign_offs_required {
            self.post_sign_off(job_id, step_id, emp, role)?;
        }
        self.put_step(
            job_id,
            step_id,
            &json!({ "status": "completed", "completed_by": emp }),
            emp,
        )
    }

    /// Fill the kind's required-at-done fields the JobKind didn't default,
    /// with type-appropriate values — the simulated executor supplying the
    /// inputs a human would type before marking the step done. Existing
    /// keys (JobKind defaults, values set on an earlier transition) are
    /// never overwritten.
    fn fill_required_fields(
        &self,
        kind: &str,
        md: &mut serde_json::Map<String, Value>,
        now: DateTime<Utc>,
    ) {
        let Some(fields) = self.required_fields.get(kind) else {
            return;
        };
        for f in fields {
            if md.contains_key(&f.name) {
                continue;
            }
            md.insert(
                f.name.clone(),
                synth_field_value(&f.field_type, &f.name, now),
            );
        }
    }

    /// Stamp a step (POST .../sign-offs) as the executing employee in
    /// the required role. Idempotent server-side per (role, shape).
    fn post_sign_off(&self, job_id: &str, step_id: &str, emp: &str, role: &str) -> Result<()> {
        let url = service_url(
            &self.api_base,
            &format!("/api/jobs/{job_id}/steps/{step_id}/sign-offs"),
        );
        let user = json!({
            "id": emp,
            "role": role,
            "access_tier": "user",
            "territory_account_ids": [],
            "direct_report_ids": [],
            "department": null,
        })
        .to_string();
        let resp = self
            .client
            .post(&url)
            .header("x-boss-user", user)
            .json(&json!({ "role": role }))
            .send()
            .with_context(|| format!("POST {url}"))?;
        let ok = resp.status().is_success();
        self.record_employee_call(
            "POST",
            &format!("/api/jobs/{job_id}/steps/{step_id}/sign-offs"),
            emp,
            role,
            ok,
        );
        if !ok {
            let status = resp.status();
            let body = resp.text().unwrap_or_default();
            anyhow::bail!("POST {url} returned {status}: {body}");
        }
        Ok(())
    }

    fn put_step(&self, job_id: &str, step_id: &str, body: &Value, emp: &str) -> Result<()> {
        let path = format!("/api/jobs/{job_id}/steps/{step_id}");
        let url = service_url(&self.api_base, &path);
        let resp = self
            .client
            .put(&url)
            .json(body)
            .send()
            .with_context(|| format!("PUT {url}"))?;
        let ok = resp.status().is_success();
        let role = self.role_of(emp);
        self.record_employee_call("PUT", &path, emp, role, ok);
        if !ok {
            let status = resp.status();
            let text = resp.text().unwrap_or_default();
            anyhow::bail!("PUT {url} -> {status}: {text}");
        }
        debug!(%url, "workforce step PUT ok");
        Ok(())
    }

    /// True if any `ingredients_consumed` line exceeds real on-hand.
    fn short_on_ingredients(&self, metadata: &Value) -> Result<bool> {
        let Some(items) = metadata
            .get("ingredients_consumed")
            .and_then(|v| v.as_array())
        else {
            return Ok(false);
        };
        for it in items {
            let Some(sku) = it.get("part_sku").and_then(|v| v.as_str()) else {
                continue;
            };
            let qty = it.get("qty").and_then(|v| v.as_i64()).unwrap_or(0);
            if self.inventory_on_hand(sku)? < qty {
                return Ok(true);
            }
        }
        Ok(false)
    }

    fn inventory_on_hand(&self, sku: &str) -> Result<i64> {
        let url = service_url(&self.api_base, &format!("/api/inventory/items/{sku}"));
        let resp = self
            .client
            .get(&url)
            .send()
            .with_context(|| format!("GET {url}"))?;
        if !resp.status().is_success() {
            // Unknown SKU reads as zero on-hand → treat as short.
            return Ok(0);
        }
        let v: Value = resp.json().context("decode inventory item")?;
        Ok(v.get("on_hand").and_then(|x| x.as_i64()).unwrap_or(0))
    }
}

/// `chrono::Duration` from fractional hours (seconds granularity).
fn duration_from_hours(hours: f64) -> Duration {
    Duration::seconds((hours * 3600.0).round() as i64)
}

/// The step's assignee id, or `None` when unassigned/empty. The workforce
/// executes only assigned steps — routing (who does the work) is the
/// dispatcher's layer, not the executor's.
fn assignee_of(step: &Value) -> Option<&str> {
    step.get("assignee_id")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
}

/// A reasonable value for a required field, by its StepType `field_type` —
/// what a human executor would put in the box. Type-valid against
/// `boss_jobs::step_registry::validate_field_type`: dates/times get a real
/// instant, enums (pipe-joined) pick the leading variant (the happy-path
/// outcome for most: `pass`, `completed`, …), and free text gets a
/// readable label derived from the field name.
fn synth_field_value(field_type: &str, name: &str, now: DateTime<Utc>) -> Value {
    match field_type {
        "number" | "integer" => json!(1),
        "boolean" => json!(true),
        "array" => json!([]),
        "object" => json!({}),
        "date" => json!(now.date_naive().to_string()), // YYYY-MM-DD (len 10)
        "date-time" => json!(now.to_rfc3339()),        // len ≥ 19
        "uri" => json!("https://docs.example.internal/sop"),
        // Enum types are pipe-joined; the first variant is the conventional
        // happy-path value.
        s if s.contains('|') => json!(s.split('|').next().unwrap_or("")),
        // "string" / "id-ref" / anything else accepts any string.
        _ => json!(humanize(name)),
    }
}

/// `"document_title"` → `"Document title"`: a readable label for a synthetic
/// free-text value.
fn humanize(name: &str) -> String {
    let spaced = name.replace(['_', '-'], " ");
    let mut chars = spaced.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().collect::<String>() + chars.as_str(),
        None => spaced,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn service_url_maps_ports() {
        assert_eq!(
            service_url("direct://127.0.0.1", "/api/jobs/assignments"),
            "http://127.0.0.1:7900/api/jobs/assignments"
        );
        assert_eq!(
            service_url("scratch://h", "/api/inventory/items/X"),
            "http://h:8300/api/inventory/items/X"
        );
        assert_eq!(
            service_url("https://gw.example", "/api/clock/now"),
            "https://gw.example/api/clock/now"
        );
    }

    #[test]
    fn assignee_of_returns_assigned_skips_unassigned() {
        // Assigned -> Some (the workforce executes it, attributed to them).
        let assigned = json!({ "id": "s1", "assignee_id": "emp-aa-007", "status": "ready" });
        assert_eq!(assignee_of(&assigned), Some("emp-aa-007"));
        // Null / missing / empty -> None: unassigned, so the workforce
        // skips it (the dispatcher or a manager routes it).
        assert_eq!(
            assignee_of(&json!({ "id": "s2", "assignee_id": null })),
            None
        );
        assert_eq!(assignee_of(&json!({ "id": "s3" })), None);
        assert_eq!(assignee_of(&json!({ "id": "s4", "assignee_id": "" })), None);
    }

    fn fixed_now() -> DateTime<Utc> {
        DateTime::parse_from_rfc3339("2025-04-03T12:00:00Z")
            .unwrap()
            .with_timezone(&Utc)
    }

    #[test]
    fn synth_field_value_is_type_valid_per_kind() {
        let now = fixed_now();
        // Free text -> a readable label derived from the field name.
        assert_eq!(
            synth_field_value("string", "document_title", now),
            json!("Document title")
        );
        // date-time -> RFC3339 (the validator requires len >= 19).
        assert!(
            synth_field_value("date-time", "scheduled_at", now)
                .as_str()
                .unwrap()
                .len()
                >= 19
        );
        // date -> YYYY-MM-DD (the validator requires len == 10).
        assert_eq!(
            synth_field_value("date", "due", now)
                .as_str()
                .unwrap()
                .len(),
            10
        );
        // enum -> leading variant.
        assert_eq!(
            synth_field_value("pass|fail|conditional", "result", now),
            json!("pass")
        );
        // scalars are the right JSON shape.
        assert!(synth_field_value("integer", "n", now).is_i64());
        assert!(synth_field_value("boolean", "b", now).is_boolean());
        assert!(synth_field_value("array", "xs", now).is_array());
        assert!(synth_field_value("object", "o", now).is_object());
        assert!(synth_field_value("uri", "u", now).is_string());
    }

    #[test]
    fn fill_required_fields_fills_missing_keeps_existing() {
        let mut req = HashMap::new();
        req.insert(
            "acknowledgment".to_string(),
            vec![
                RequiredField {
                    name: "document_title".into(),
                    field_type: "string".into(),
                },
                RequiredField {
                    name: "acknowledged_at".into(),
                    field_type: "date-time".into(),
                },
            ],
        );
        let wf = Workforce::new("direct://127.0.0.1", HashMap::new(), req);
        let now = fixed_now();

        let mut md = serde_json::Map::new();
        md.insert("document_title".to_string(), json!("Q2 Safety Policy"));
        wf.fill_required_fields("acknowledgment", &mut md, now);
        // Pre-set key is preserved (the JobKind's default wins).
        assert_eq!(
            md.get("document_title").unwrap(),
            &json!("Q2 Safety Policy")
        );
        // Missing required key is filled.
        assert!(md.get("acknowledged_at").is_some());

        // Unknown kind -> no-op.
        let mut empty = serde_json::Map::new();
        wf.fill_required_fields("not-a-kind", &mut empty, now);
        assert!(empty.is_empty());
    }
}
