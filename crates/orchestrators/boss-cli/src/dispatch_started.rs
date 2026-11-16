//! An own-run receipt records that the worker read START, not physical
//! execution or successful delivery. The metadata merge's audit event
//! supplies server provenance; the receipt carries no client clock.

use anyhow::{Context, Result, anyhow, bail};
use serde_json::{Value, json};

/// The top board reads the same key (backlog 57326714), so it is spelled
/// once, beside the run kind.
pub(crate) const EXPECTATION_KEY: &str = boss_jobs::agent_budget::WORKER_START_EXPECTATION;
pub(crate) const RECEIPT_KEY: &str = "worker_started";

pub(crate) fn expectation(job: &Value) -> Option<&Value> {
    crate::envelope::steps(job)
        .into_iter()
        .find(|s| s.get("spec_slug").and_then(Value::as_str) == Some("building"))
        .and_then(|s| s.get("metadata"))
        .and_then(|m| m.get(EXPECTATION_KEY))
}

fn receipt(job: &Value, run: &str, actor: &str, owned: Option<&str>) -> Result<Value> {
    let job = job.get("data").unwrap_or(job);
    if owned != Some(run) {
        bail!("--started requires BOSS_AGENT_RUN to name this exact own run");
    }
    if job.get("id").and_then(Value::as_str) != Some(run)
        || job.get("kind").and_then(Value::as_str) != Some("agent-run")
        || job.get("status").and_then(Value::as_str) != Some("open")
    {
        bail!("--started requires the exact open native agent-run");
    }
    let step = crate::envelope::steps(job)
        .into_iter()
        .find(|s| s.get("spec_slug").and_then(Value::as_str) == Some("building"))
        .context("--started found no building step")?;
    if step.get("status").and_then(Value::as_str) != Some("active")
        || step.get("assignee_id").and_then(Value::as_str) != Some(actor)
    {
        bail!("--started requires this actor's active assigned building step");
    }
    // metadata.agent is the dispatch login (which can be a registry alias),
    // not the actor assigned to this step. The signed caller must own the step.
    let expected =
        expectation(job).context("this legacy or unmarked run declares no worker receipt")?;
    // A BOUNDED START's receipt (design e72dc7c9): the worker states it
    // read this exact START and every mandatory original of this exact
    // manifest — both digests ride the receipt, and nothing about the
    // available originals does.
    if expected.get("schema").and_then(Value::as_u64)
        == Some(boss_jobs::execution_inputs::START_SCHEMA)
    {
        return boss_jobs::execution_inputs::start_receipt(expected, run, actor)
            .context("the bounded START expectation is incomplete; no worker receipt written");
    }
    if expected.get("schema").and_then(Value::as_u64) != Some(1)
        || expected
            .get("minutes")
            .and_then(Value::as_i64)
            .is_none_or(|n| n <= 0)
    {
        bail!("worker receipt expectation is unsupported");
    }
    Ok(json!({"schema":1,"run":run,"actor":actor}))
}

/// The id of the open run's `building` step when it is READY and
/// nominated to `actor` — the one state [`started_at`] claims from.
fn own_ready_nomination(job: &Value, actor: &str) -> Option<String> {
    let job = job.get("data").unwrap_or(job);
    if job.get("kind").and_then(Value::as_str) != Some("agent-run")
        || job.get("status").and_then(Value::as_str) != Some("open")
    {
        return None;
    }
    crate::envelope::steps(job)
        .into_iter()
        .find(|s| s.get("spec_slug").and_then(Value::as_str) == Some("building"))
        .filter(|s| {
            s.get("status").and_then(Value::as_str) == Some("ready")
                && s.get("assignee_id").and_then(Value::as_str) == Some(actor)
        })
        .and_then(|s| s.get("id").and_then(Value::as_str))
        .map(str::to_string)
}

/// The run as the claim door would leave it once it claims `sid`: that
/// step ACTIVE, nothing else changed. What [`receipt`] is checked against
/// before a worker is asked for (backlog 0c55e31d).
fn as_claimed(job: &Value, sid: &str) -> Value {
    let mut job = job.get("data").unwrap_or(job).clone();
    if let Some(steps) = job.get_mut("steps").and_then(Value::as_array_mut) {
        for step in steps
            .iter_mut()
            .filter(|s| s.get("id").and_then(Value::as_str) == Some(sid))
        {
            step["status"] = json!("active");
        }
    }
    job
}

/// Does the run's `building` read ACTIVE and held by `actor`?
fn holds_building(job: &Value, actor: &str) -> bool {
    let job = job.get("data").unwrap_or(job);
    crate::envelope::steps(job)
        .into_iter()
        .find(|s| s.get("spec_slug").and_then(Value::as_str) == Some("building"))
        .is_some_and(|s| {
            s.get("status").and_then(Value::as_str) == Some("active")
                && s.get("assignee_id").and_then(Value::as_str) == Some(actor)
        })
}

/// The building-step key a claim handed back for want of a worker
/// carries: the door's refusal, whole, and who handed it back. No clock:
/// the merge door's audit event dates it.
pub(crate) const RETURNED_KEY: &str = "returned_to_waiting";

/// A WORKER THE DOOR CANNOT PLACE RETURNS THE RUN TO WAITING (decided
/// 20261106_1507Z (5), backlog 0c55e31d). Reached when `building` is
/// ACTIVE as this verb asks for a worker — claimed by `boss dispatch`
/// before it printed (allocation item 1, 587f395a), by hand, or by this
/// verb's own claim a moment earlier (5608c1a2: the claim precedes the
/// worker, so the door reads the host it drew) — and the worker door
/// then refused. A claim with no worker under it is the half-held state
/// this item removes, so the claim is handed back: the refusal recorded on
/// the step, the step put back READY and still nominated to this actor,
/// and read back. A run that already records a worker binding keeps its
/// claim: that worker is the claim's, and the refusal is about a retry.
///
/// THE BINDING IS JUDGED AT THE WRITE (review e296014b R2). `before` was
/// read before the worker was asked for, and a concurrent `--started` of
/// this run can bind its worker after every read made here. Its binder
/// records the binding on this claim's own step before the run, so both
/// hand-back writes carry `{worker_session: null}` in
/// [`boss_jobs::step_metadata_write::EXPECT_HEADER`] and land only while
/// the step records none: the merge door judges it under the row's lock,
/// and the PUT on the row its version compare freezes. A binding that won
/// the race refuses the hand-back, and the claim stays with it.
///
/// THE EXIT NAMES ITS ACQUISITION (review e042d952 R1). The PUT also
/// carries the `resource_claim_id` `before` holds, the claim this verb
/// read, so the server's exit rule frees exactly that generation and
/// refuses one that was released and claimed again in between.
async fn return_to_waiting(
    http: &crate::machine_http::MachineClient,
    base: &str,
    run: &str,
    actor: &str,
    registered: &str,
    before: &Value,
    refused: anyhow::Error,
) -> anyhow::Error {
    let job = before.get("data").unwrap_or(before);
    if records_a_binding(job) {
        return refused.context(
            "the worker door refused, and this run already records a worker binding, so its \
             building claim stays with that worker; nothing was changed",
        );
    }
    let Some((step, sid)) = building(job).and_then(|s| {
        s.get("id")
            .and_then(Value::as_str)
            .map(|id| (s.clone(), id.to_string()))
    }) else {
        return refused.context("the worker door refused, and the building step has no id");
    };
    let unbound =
        serde_json::Map::from_iter([(crate::dispatch_worker::KEY.to_string(), Value::Null)]);
    let judged = match crate::sor_client::expectation_value(&unbound) {
        Ok(judged) => judged,
        Err(e) => return refused.context(format!("the hand-back could not be judged: {e:#}")),
    };
    let call = |method: reqwest::Method, path: String, body: Option<Value>| {
        // Every write is judged; the read is a read.
        let extra = if method == reqwest::Method::GET {
            vec![]
        } else {
            vec![(
                boss_jobs::step_metadata_write::EXPECT_HEADER,
                judged.clone(),
            )]
        };
        async move {
            crate::sor_client::api_at_signed_with(
                http,
                base,
                method,
                &path,
                body,
                crate::identity::Signature::As(actor.to_string()),
                &extra,
            )
            .await
        }
    };
    let step_path = format!("/api/jobs/{run}/steps/{sid}");
    let handed_back = async {
        call(
            reqwest::Method::PATCH,
            format!("{step_path}/metadata"),
            Some(json!({ RETURNED_KEY: { "why": format!("{refused:#}"), "by": registered } })),
        )
        .await?;
        // THE EXIT NAMES THE CLAIM THIS VERB READ (review e042d952 R1,
        // backlog ad486420): the step PUT refuses to take a billed step
        // out of Active unless the body names the acquisition the
        // executor read — `before`, the read-back of its own claim or the
        // read of the claim `boss dispatch` made — never a fresh read.
        // A bare `{status: ready}` answered 409 and left the claim and
        // its host draw held with no worker under it.
        call(
            reqwest::Method::PUT,
            step_path.clone(),
            Some(boss_jobs::flow_resources::exiting(
                &step,
                json!({ "status": "ready" }),
            )),
        )
        .await?;
        let after = call(reqwest::Method::GET, format!("/api/jobs/{run}"), None)
            .await?
            .context("the run would not read back")?;
        let back = own_ready_nomination(&after, registered).as_deref() == Some(sid.as_str());
        anyhow::ensure!(
            back,
            "the hand-back answered success and the step does not read back READY with {registered}"
        );
        Ok::<(), anyhow::Error>(())
    }
    .await;
    match handed_back {
        Ok(()) => refused.context(format!(
            "the worker door refused this run, so its building claim was HANDED BACK: the step \
             reads back READY, still nominated to {registered}, with the refusal under \
             `{RETURNED_KEY}` — no claim and no worker are held; run --started again once the \
             door can place a worker"
        )),
        Err(e) => {
            // Read for the words only: the guard was each write's own
            // judgement, never this read.
            let bound = call(reqwest::Method::GET, format!("/api/jobs/{run}"), None)
                .await
                .ok()
                .flatten()
                .is_some_and(|after| records_a_binding(after.get("data").unwrap_or(&after)));
            if bound {
                refused.context(format!(
                    "the worker door refused this run, and a worker binding was recorded for its \
                     building claim while the worker was asked for, so the hand-back was refused \
                     at its write ({e:#}) and the claim stays with that worker"
                ))
            } else {
                refused.context(format!(
                    "the worker door refused this run, and handing its building claim back did \
                     NOT read back ({e:#}): the claim is still held with no worker"
                ))
            }
        }
    }
}

/// The `building` step of `job`, if it has one.
fn building(job: &Value) -> Option<&Value> {
    crate::envelope::steps(job)
        .into_iter()
        .find(|s| s.get("spec_slug").and_then(Value::as_str) == Some("building"))
}

/// Does the run record a worker binding — on the run, or on its building
/// claim's own step, where the binder writes it first (review e296014b R2)?
fn records_a_binding(job: &Value) -> bool {
    let key = format!("/metadata/{}", crate::dispatch_worker::KEY);
    job.pointer(&key).is_some() || building(job).is_some_and(|s| s.pointer(&key).is_some())
}

/// A CLAIM THE RUN READS BACK NOT HOLDING GIVES UP ITS WORKER (backlog
/// 0c55e31d; review e296014b R1). The read-back is the fact, whatever the
/// claim door answered: a refusal, a 200 whose step was handed back
/// before the read, or a binder whose claim was handed back before its
/// binding landed. Reached only on a POSITIVE not-held read: a read that
/// failed leaves the worker bound, since whether the claim holds is
/// unknown.
async fn retire_unheld(
    workers: &dyn crate::dispatch_worker::Workers,
    (http, base): (&crate::machine_http::MachineClient, &str),
    run: &str,
    actor: &str,
    worker: &Option<Value>,
    refused: anyhow::Error,
) -> anyhow::Error {
    if worker.is_none() {
        return refused.context("claiming this run's own building step");
    }
    match crate::dispatch_worker::retire_at(workers, http, base, run, actor).await {
        Ok(retired) => refused.context(format!(
            "this run's building step reads back NOT held after its worker was admitted, so that \
             worker is RETIRED and the controller's receipt read back on the run ({}): no claim \
             and no worker are held. The controller gives a stopped run no new worker, so this \
             run cannot start; the work is dispatched again as a fresh run",
            retired
                .as_ref()
                .map_or_else(|| "the route kept no worker".to_string(), Value::to_string)
        )),
        Err(e) => refused.context(format!(
            "this run's building step reads back NOT held after its worker was admitted, and \
             retiring that worker did NOT read back ({e:#}): the worker is still held with no \
             claim — `dev-build stop-run {run}` retires it"
        )),
    }
}

#[cfg(test)]
pub(crate) async fn started_at(
    http: &crate::machine_http::MachineClient,
    base: &str,
    run: &str,
    actor: &str,
    owned: Option<&str>,
) -> Result<()> {
    started_with_worker_at(
        http,
        base,
        run,
        actor,
        owned,
        None,
        &crate::dispatch_worker::Published,
    )
    .await
}

/// Does the agent-run version row `row` waive `profile`'s vCPU — does its
/// `building` bill list the profile in `workerless_profiles`?
fn waived_by(row: &Value, profile: &str) -> bool {
    let row = row.get("data").unwrap_or(row);
    row.get("steps")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter(|s| s.get("title").and_then(Value::as_str) == Some("building"))
        .filter_map(|s| s.pointer("/resources/workerless_profiles"))
        .filter_map(Value::as_array)
        .flatten()
        .any(|p| p.as_str() == Some(profile))
}

/// A PROFILE THE BILL WAIVES BINDS NO WORKER (backlog db88f6a9; review
/// 71005282 B2). The claim door reads the run's `profile` against its
/// PINNED building bill's `workerless_profiles` and, for a listed one,
/// draws no host: the claim holds a model slot alone. A worker admitted
/// under that claim would be a 2.5-vCPU Pod no host pool counts, placed
/// unpinned because the claim names no node — the 5608c1a2 defect, the
/// claim admitting work the box was never asked about. So `--local-root`
/// on such a run is refused here, off the same registry fact the claim
/// reads, before the claim and before the worker door: nothing is
/// claimed, ensured or written. A run naming no profile is billed, so it
/// is not asked about; a pinned version that cannot be read is refused,
/// because whether its claim draws a host is then unknown.
async fn refuse_a_worker_its_bill_waives<F, Fut>(run: &Value, read: F) -> Result<()>
where
    F: FnOnce(String) -> Fut,
    Fut: std::future::Future<Output = Result<Option<Value>>>,
{
    let job = run.get("data").unwrap_or(run);
    let Some(profile) = job
        .pointer(&format!(
            "/metadata/{}",
            boss_jobs::resources::RUN_PROFILE_KEY
        ))
        .and_then(Value::as_str)
    else {
        return Ok(());
    };
    let kind = job
        .get("kind")
        .and_then(Value::as_str)
        .context("own run names no kind; no worker asked for")?;
    let version = crate::start_inputs::pinned_version(job)
        .context("own run names no pinned workflow version; no worker asked for")?;
    let row = read(format!("/api/workflows/{kind}/versions/{version}"))
        .await
        .and_then(|row| row.context("it read back empty"))
        .with_context(|| {
            format!(
                "{kind} v{version}, this run's pinned version, could not be read, so whether its \
                 building bill waives profile `{profile}` (draws no host) is unknown; this verb \
                 claimed nothing and asked for no worker"
            )
        })?;
    if waived_by(&row, profile) {
        bail!(
            "this run's profile `{profile}` is billed no vCPU: {kind} v{version}'s building bill \
             lists it in workerless_profiles, so its claim draws no host, and a worker under it \
             would run where no host pool counts it. A waived run binds no worker: run --started \
             without --local-root. This verb claimed nothing and asked for no worker."
        );
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
pub(crate) async fn started_with_worker_at(
    http: &crate::machine_http::MachineClient,
    base: &str,
    run: &str,
    actor: &str,
    owned: Option<&str>,
    local_root: Option<&std::path::Path>,
    workers: &dyn crate::dispatch_worker::Workers,
) -> Result<()> {
    let registry = crate::sor_client::api_at_signed(
        http,
        base,
        reqwest::Method::GET,
        "/api/agents",
        None,
        crate::identity::Signature::As(actor.to_string()),
    )
    .await?
    .context("agent registry returned no body")?;
    let total = registry
        .get("total")
        .and_then(Value::as_u64)
        .context("agent registry returned no complete count")?;
    let agents = crate::train::rows(Some(registry))?;
    if total != agents.len() as u64 {
        bail!("agent registry is incomplete; no worker receipt written");
    }
    let matches: Vec<_> = agents
        .iter()
        .filter_map(|row| crate::dispatch::resolve_agent(std::slice::from_ref(row), actor))
        .collect();
    if matches.len() != 1 {
        bail!("running principal has no unique registered actor; no worker receipt written");
    }
    let registered = &matches[0];
    if registered.trim().is_empty() {
        bail!("registered actor is blank; no worker receipt written");
    }
    let api = |method: reqwest::Method, body: Option<Value>| async move {
        let path = if method == reqwest::Method::PATCH {
            format!("/api/jobs/{run}/metadata")
        } else {
            format!("/api/jobs/{run}")
        };
        crate::sor_client::api_at_signed(
            http,
            base,
            method,
            &path,
            body,
            crate::identity::Signature::As(actor.to_string()),
        )
        .await
    };
    let mut before = api(reqwest::Method::GET, None)
        .await?
        .context("own run read returned no body")?;
    if local_root.is_none() && crate::door_env::isolation_forwards() {
        let profile = before
            .pointer("/metadata/profile")
            .and_then(Value::as_str)
            .context("own run names no execution profile")?;
        let repo = crate::brief::repo_root()?;
        if crate::dispatch::Isolation::for_profile(&repo, profile)?
            == crate::dispatch::Isolation::Worktree
        {
            bail!(
                "source builder START requires --local-root <your own worktree>; no worker acknowledgment written"
            );
        }
    }
    if local_root.is_some() {
        refuse_a_worker_its_bill_waives(&before, |path| async move {
            crate::sor_client::api_at_signed(
                http,
                base,
                reqwest::Method::GET,
                &path,
                None,
                crate::identity::Signature::As(actor.to_string()),
            )
            .await
        })
        .await?;
    }
    // THE WORKER TAKES ITS OWN NOMINATION (backlog 2f7b8c00, 2026-10-07).
    // Nothing in the protocol claimed a run's Building: dispatch claims
    // the SOURCE step, the dispatcher service nominates Building, and the
    // step then sat READY — so this verb refused every worker that had
    // not claimed by hand (every Claude subagent that day; Codex sessions
    // claimed by hand first). The claim door is the one act that makes a
    // nomination a holding, it compares the signer against the nominee,
    // and it is asked only for a step already nominated to THIS actor: an
    // unassigned Building is one the door would grant to anyone, so it is
    // left for the refusal below.
    let ready = (owned == Some(run))
        .then(|| own_ready_nomination(&before, registered))
        .flatten();
    // NO WRITE BEFORE A REFUSAL (review f7f0b689, N5): a run that
    // declares no worker receipt is refused below whatever its step
    // is, so it is refused HERE, before the claim would have left
    // the step ACTIVE behind that refusal.
    if ready.is_some() && expectation(before.get("data").unwrap_or(&before)).is_none() {
        bail!("this legacy or unmarked run declares no worker receipt");
    }
    // ...and, for the same reason, every refusal the receipt would make
    // once the step is claimed is made before a worker is asked for, read
    // against the run as the claim would leave it.
    receipt(
        &ready
            .as_deref()
            .map_or_else(|| before.clone(), |sid| as_claimed(&before, sid)),
        run,
        registered,
        owned,
    )?;
    // THE CLAIM BEFORE THE WORKER (backlog 5608c1a2; review 593436d2 R1).
    // `building` bills vCPU that the claim door places on a host:<node>
    // pool — the box the run will execute on, drawn from its owner's
    // floating share — and the worker door reads that draw off the run
    // to REQUIRE the worker onto that node. So the claim is made first,
    // read back as the fact, and only a claim the run reads back HOLDING
    // asks the door for a worker: asked for first, the worker had no node
    // to go to, was created unpinned and scheduled before any pool had
    // been consulted, and a claim landing after it charged host:w-1 for a
    // Pod the scheduler had placed on its own.
    //
    // This reverses the order 0c55e31d chose (allocation item 4 of design
    // 012822ea; decided 20261106_1507Z (5)) and keeps what it bought.
    // Measured on afa6db72e, this verb claimed first and a storage
    // refusal left the claim ACTIVE with no worker under it — a model
    // slot held by a run that could not start, until the silence clock
    // aged it out four hours later. That item closed the same state for a
    // building `boss dispatch` had claimed before it printed by HANDING
    // THE CLAIM BACK (`return_to_waiting`), and that is now where every
    // worker refusal ends, whichever verb claimed: the refusal on the
    // step, the step READY and still nominated to this actor, neither
    // side holding. A controller refusal is not a worker acknowledgment.
    // Its durable run ownership makes a retry after a failed API write
    // adopt, not allocate.
    if let Some(sid) = &ready {
        let claimed = crate::sor_client::api_at_signed(
            http,
            base,
            reqwest::Method::POST,
            &format!("/api/jobs/{run}/steps/{sid}/claim"),
            None,
            crate::identity::Signature::As(actor.to_string()),
        )
        .await;
        // The read-back is the fact either way: a refusal whose answer was
        // lost can still have made the claim, and a 200 can be followed by
        // a hand-back before the read (review e296014b R1). No worker is
        // asked for under a claim the run reads back NOT holding, whatever
        // the claim door answered, and a read that fails proves nothing.
        match api(reqwest::Method::GET, None).await {
            Ok(Some(after)) if !holds_building(&after, registered) => {
                let refused = claimed.err().unwrap_or_else(|| {
                    anyhow!(
                        "the claim door answered success, and the run reads back with \
                         building not held by {registered}"
                    )
                });
                return Err(refused.context(
                    "claiming this run's own building step: no worker was asked for, so no \
                     claim and no worker are held",
                ));
            }
            Ok(Some(after)) => before = after,
            read => {
                let read = read
                    .err()
                    .map_or_else(|| "no body".to_string(), |e| format!("{e:#}"));
                let claimed = claimed
                    .err()
                    .unwrap_or_else(|| anyhow!("the claim door answered success"));
                return Err(claimed.context(format!(
                    "claiming this run's own building step, and the run would not read back \
                     ({read}): whether the claim holds is unknown, so no worker was asked for \
                     under it — run --started again to read it"
                )));
            }
        }
    }
    let worker = match local_root {
        Some(root) => match workers.ensure(run, root).await {
            Ok(worker) => worker,
            Err(refused) => {
                return Err(return_to_waiting(
                    http, base, run, actor, registered, &before, refused,
                )
                .await);
            }
        },
        None => None,
    };
    let expected = receipt(&before, run, registered, owned)?;
    let before = before.get("data").unwrap_or(&before);
    let mut patch = serde_json::Map::new();
    if let Some(worker) = &worker {
        match before.pointer(&format!("/metadata/{}", crate::dispatch_worker::KEY)) {
            Some(existing) if existing != worker => {
                bail!("own run already records another worker binding")
            }
            Some(_) => {}
            None => {
                patch.insert(crate::dispatch_worker::KEY.into(), worker.clone());
            }
        }
    }
    match before.get("metadata").and_then(|m| m.get(RECEIPT_KEY)) {
        Some(existing) if existing != &expected => {
            bail!("own run already carries a different or unsupported worker receipt")
        }
        Some(_) => {}
        None => {
            patch.insert(RECEIPT_KEY.into(), expected.clone());
        }
    }
    // THE BINDING ON THE CLAIM'S OWN STEP FIRST (review e296014b R2). A
    // hand-back of this claim — another `--started` of this run whose
    // worker was refused — judges its writes against this step, because
    // the claim and the binding can only be held to each other on one
    // row. So the binding is written here, judged absent, before the run
    // records it: landed first, it refuses the hand-back at the jobs API;
    // landed after one, the read-back below finds the claim gone and
    // retires the worker.
    let key = format!("/metadata/{}", crate::dispatch_worker::KEY);
    if let Some(worker) = &worker {
        let step = building(before).context("--started found no building step")?;
        match step.pointer(&key) {
            Some(existing) if existing != worker => {
                bail!("this run's building claim already records another worker binding")
            }
            Some(_) => {}
            None => {
                let sid = step
                    .get("id")
                    .and_then(Value::as_str)
                    .context("the building step has no id")?;
                let absent = serde_json::Map::from_iter([(
                    crate::dispatch_worker::KEY.to_string(),
                    Value::Null,
                )]);
                crate::sor_client::api_at_signed_with(
                    http,
                    base,
                    reqwest::Method::PATCH,
                    &format!("/api/jobs/{run}/steps/{sid}/metadata"),
                    Some(json!({ crate::dispatch_worker::KEY: worker })),
                    crate::identity::Signature::As(actor.to_string()),
                    &[(
                        boss_jobs::step_metadata_write::EXPECT_HEADER,
                        crate::sor_client::expectation_value(&absent)?,
                    )],
                )
                .await?;
            }
        }
    }
    if !patch.is_empty() {
        api(reqwest::Method::PATCH, Some(Value::Object(patch))).await?;
    }
    let after = api(reqwest::Method::GET, None)
        .await?
        .context("receipt readback returned no body")?;
    // R1's rule at the binder's own read-back: the claim handed back
    // before the binding landed gives up the worker it was admitted.
    if worker.is_some() && !holds_building(&after, registered) {
        let refused = anyhow!(
            "the run reads back with building not held by {registered} after its worker \
             binding was recorded"
        );
        return Err(retire_unheld(workers, (http, base), run, actor, &worker, refused).await);
    }
    // A concurrent close or reassignment makes this a failed acknowledgment;
    // the observer validates current identity again, never inferring death.
    receipt(&after, run, registered, owned)?;
    let after = after.get("data").unwrap_or(&after);
    if after.get("metadata").and_then(|m| m.get(RECEIPT_KEY)) != Some(&expected) {
        bail!("worker receipt did not read back; no acknowledgment is claimed");
    }
    if let Some(worker) = &worker
        && (after.pointer(&key) != Some(worker)
            || building(after).and_then(|s| s.pointer(&key)) != Some(worker))
    {
        bail!("worker binding did not read back; controller retains this run's worker for retry");
    }
    println!("worker receipt recorded for {run} as {registered}, signed by {actor}");
    // A RESTART'S SETUP COST (backlog a99dd722). The receipt above is the
    // end of the station's setup on the record, so this is where a run
    // that restarts one handed to the gate says how long its station took
    // to set up again. A record, not an acknowledgment: losing it costs
    // the measurement and never the START, so it is said and not fatal.
    match record_station_setup(http, base, run, actor, after).await {
        Ok(Some(setup)) => println!(
            "station setup recorded on restarted run {run}: claim to worker bound {} s; \
             checkout, warm target and first work are not on the record",
            setup
                .get("claim_to_worker_bound_seconds")
                .map_or_else(|| "not measurable".to_string(), Value::to_string)
        ),
        Ok(None) => {}
        Err(e) => eprintln!(
            "boss dispatch --started: could not record this restarted run's station setup \
             ({e:#}); the worker receipt above stands"
        ),
    }
    Ok(())
}

/// The excludes a car admitted at build start carries until its builder
/// states them: the boundary is the builder's to write, and the run's
/// packet has no field for it.
pub(crate) const ADMITTED_EXCLUDES: &str = "Not yet stated: this car was admitted by boss \
     dispatch when its build started, before its builder wrote the boundary; the builder \
     states it at the gate (park excludes), and the green records it on the car.";

/// The branch a worktree's HEAD names, read from the files alone — no
/// git is spawned here, because the worktree is the builder's and this
/// runs in the credentialled session (design a68fc42a D2). `None` for a
/// detached HEAD or an unreadable checkout.
pub(crate) fn worktree_branch(root: &std::path::Path) -> Option<String> {
    let text = std::fs::read_to_string(worktree_gitdir(root)?.join("HEAD")).ok()?;
    text.trim()
        .strip_prefix("ref: refs/heads/")
        .map(str::to_string)
        .filter(|b| !b.is_empty())
}

/// The gitdir a worktree's `.git` names: the directory itself, or the
/// one a linked worktree's `.git` FILE points at.
fn worktree_gitdir(root: &std::path::Path) -> Option<std::path::PathBuf> {
    let dot = root.join(".git");
    if dot.is_dir() {
        return Some(dot);
    }
    let text = std::fs::read_to_string(&dot).ok()?;
    let gitdir = text
        .lines()
        .find_map(|l| l.strip_prefix("gitdir: "))?
        .trim();
    let gitdir = std::path::Path::new(gitdir);
    Some(if gitdir.is_relative() {
        root.join(gitdir)
    } else {
        gitdir.to_path_buf()
    })
}

/// The car branch a worktree's HEAD branch TRACKS on a remote — a
/// CANDIDATE for the car's key, never the key by itself. `None` when the
/// HEAD is on no branch or its branch tracks no car branch.
///
/// Backlog d5bebca7 (run e0a7200e, 2026-11-10): a car branch held by
/// another worktree cannot be checked out twice, so a builder takes it
/// as a differently named local branch tracking the forge branch. Keyed
/// on the LOCAL name, --started matched no car and admitted twin
/// ebddec00 beside 88645b17. An upstream that is not a car branch —
/// `origin/main`, which rule 1's `checkout -b <b> origin/main` sets on
/// every fresh car — or a local one (`remote = .`) says nothing about a
/// car. Read from files, like the HEAD, with no git spawned.
///
/// WHY ONLY A CANDIDATE (review dce2840b F1): git's default
/// `branch.autoSetupMerge` gives a branch cut from `origin/fix/parent`
/// the upstream fix/parent, so a STACKED child tracks its parent's car
/// branch exactly as a renamed repair tracks its own. The config cannot
/// tell the two apart; the record can, and [`admit_run_car`] asks it.
pub(crate) fn worktree_tracked_car_branch(root: &std::path::Path) -> Option<String> {
    let local = worktree_branch(root)?;
    let gitdir = worktree_gitdir(root)?;
    // A linked worktree's gitdir names the shared repository in
    // `commondir`; the branch config lives there.
    let common = match std::fs::read_to_string(gitdir.join("commondir")) {
        Ok(c) => {
            let c = std::path::Path::new(c.trim());
            if c.is_relative() {
                gitdir.join(c)
            } else {
                c.to_path_buf()
            }
        }
        Err(_) => gitdir,
    };
    std::fs::read_to_string(common.join("config"))
        .ok()
        .and_then(|config| tracked_car_branch(&config, &local))
}

/// PURE: the car branch `local` tracks on a remote, read out of a git
/// config's `[branch "<local>"]` section — `None` when it tracks none,
/// tracks a local branch, or tracks one that is not `feat/…` / `fix/…`.
pub(crate) fn tracked_car_branch(config: &str, local: &str) -> Option<String> {
    let header = format!("[branch \"{local}\"]");
    let (mut inside, mut remote, mut merge) = (false, None, None);
    for line in config.lines().map(str::trim) {
        if line.starts_with('[') {
            inside = line == header;
            continue;
        }
        if !inside {
            continue;
        }
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        let value = value.trim().trim_matches('"').to_string();
        match key.trim().to_ascii_lowercase().as_str() {
            "remote" => remote = Some(value),
            "merge" => merge = Some(value),
            _ => {}
        }
    }
    if remote.as_deref().is_none_or(|r| r.is_empty() || r == ".") {
        return None;
    }
    merge?
        .strip_prefix("refs/heads/")
        .filter(|b| b.starts_with("feat/") || b.starts_with("fix/"))
        .map(str::to_string)
}

/// PURE: the branch a car is admitted for, or why none is.
///
/// A car is admitted for a BUILDER run on a backlog-item's `build` step,
/// whose worktree is on a car branch — `feat/…` or `fix/…`, the builder
/// rules' own naming (rule 1). Anything else is a run whose branch is not
/// yet cut (the harness's `worktree-agent-…`), a detached head, or not a
/// change at all: admitting a car for it would put one on a branch nobody
/// gates, and the green would file a second for the real one.
pub(crate) fn admission_branch(
    run: &Value,
    item: &Value,
    head: Option<&str>,
) -> Result<String, String> {
    if run.pointer("/metadata/step").and_then(Value::as_str) != Some("build") {
        return Err("the run is not on a `build` step".into());
    }
    if item.get("kind").and_then(Value::as_str) != Some("backlog-item") {
        return Err("the run's packet is not a backlog-item".into());
    }
    let Some(branch) = head else {
        return Err("the worktree is on no branch (detached or unreadable)".into());
    };
    if !(branch.starts_with("feat/") || branch.starts_with("fix/")) {
        return Err(format!(
            "the worktree is on `{branch}`, not a car branch (feat/… or fix/…): cut the \
             branch before --started, or the green files the car"
        ));
    }
    Ok(branch.to_string())
}

/// PURE: the car a dock-red repair run was dispatched onto, read off the
/// run's own `repair` record (`dispatch::Repair::record`) — `None` for
/// every other run.
fn dock_red_car(run: &Value) -> Option<&str> {
    let repair = run.pointer("/metadata/repair")?;
    (repair.get("origin").and_then(Value::as_str)
        == Some(crate::dispatch::RepairOrigin::DockRed.as_str()))
    .then(|| repair.get("car").and_then(Value::as_str))
    .flatten()
    .map(str::trim)
    .filter(|c| !c.is_empty())
}

/// THE DOCK-RED REPAIR TAKES ITS CAR (backlog 5d0758dc): its `agent_run`
/// is set to this run, the key the conductor's dock reads to hold a car
/// its repair is building rather than replay and re-gate it under the
/// builder. Measured 2026-11-11: car 4b740c44's `agent_run` named a run
/// whose building had completed the evening before, for the whole window
/// in which repair run 2a9293b8 built it and the dock replayed it. A parked
/// car is not a building car, so the branch adoption below never reaches
/// it; the run's own `repair` record names it. Said, never fatal: a car
/// that could not be stamped is the dock's as before, and the line says so.
async fn take_dock_red_car(
    door: &dyn crate::car_retire::Door,
    run: &Value,
    run_id: &str,
) -> Option<String> {
    let car = dock_red_car(run)?;
    let short = &car[..8.min(car.len())];
    let body = json!({ boss_jobs::agent_runs::EDGE_KEY: run_id });
    Some(
        match door
            .send(
                reqwest::Method::PATCH,
                &format!("/api/jobs/{car}/metadata"),
                Some(body),
            )
            .await
        {
            Ok(_) => format!(
                "boss dispatch --started: garaged car {short} now names this repair run as its \
                 agent_run — the dock holds it, unreplayed and ungated, while this run builds \
                 (5d0758dc)"
            ),
            Err(e) => format!(
                "boss dispatch --started: garaged car {short}'s agent_run could not be set to this \
                 repair run ({e:#}) — the dock may replay and re-gate it while this run builds"
            ),
        },
    )
}

/// ADMIT THE DISPATCHED BUILD'S CAR through `boss car open`'s own door
/// ([`crate::car::admit`]) — or adopt the branch's live car, which a
/// repair on an existing car's branch finds. `Ok(None)` when the run is
/// not one a car is admitted for; the reason is said, not hidden.
pub(crate) async fn admit_run_car(
    http: &crate::machine_http::MachineClient,
    base: &str,
    actor: &str,
    run: &Value,
    root: &std::path::Path,
    now: chrono::DateTime<chrono::Utc>,
) -> Result<Option<String>> {
    if run.pointer("/metadata/step").and_then(Value::as_str) != Some("build") {
        return Ok(None);
    }
    let Some(packet) = run.pointer("/metadata/packet").and_then(Value::as_str) else {
        return Ok(None);
    };
    let run_id = run
        .get("id")
        .and_then(Value::as_str)
        .context("the run has no id")?;
    let item = crate::sor_client::api_at_signed(
        http,
        base,
        reqwest::Method::GET,
        &format!("/api/jobs/{packet}"),
        None,
        crate::identity::Signature::As(actor.to_string()),
    )
    .await?
    .context("the run's packet read back empty")?;
    let item = item.get("data").unwrap_or(&item);
    let item_id = item
        .get("id")
        .and_then(Value::as_str)
        .unwrap_or(packet)
        .to_string();
    let door = crate::steps::Wire::at(
        base.to_string(),
        Some(crate::identity::Caller {
            id: actor.to_string(),
            source: crate::identity::Source::Env,
        }),
    )?;
    if let Some(said) = take_dock_red_car(&door, run, run_id).await {
        println!("{said}");
    }
    // THE TRACKED FORGE BRANCH KEYS THE CAR ONLY WHEN THE RECORD SAYS ITS
    // CAR IS THIS CHANGE'S (review dce2840b F1): this run's, or this
    // item's. A renamed repair tracking its own car branch adopts that
    // car (backlog d5bebca7); a stacked child, which git's defaults make
    // track its PARENT's car branch, keeps its own name and its own car.
    let local = worktree_branch(root);
    let head = match worktree_tracked_car_branch(root) {
        Some(tracked) if Some(&tracked) != local.as_ref() => {
            if crate::car::live_car_is_this_change(&door, &tracked, run_id, &item_id).await? {
                Some(tracked)
            } else {
                let here = local.as_deref().unwrap_or("?");
                println!(
                    "boss dispatch --started: {here} tracks {tracked}, but no live car of this \
                     run or this item rides {tracked} — the car is keyed on {here}"
                );
                local
            }
        }
        _ => local,
    };
    let branch = match admission_branch(run, item, head.as_deref()) {
        Ok(b) => b,
        Err(why) => {
            return Ok(Some(format!(
                "boss dispatch --started: no car admitted — {why}"
            )));
        }
    };
    let title = item
        .get("title")
        .and_then(Value::as_str)
        .filter(|t| !t.trim().is_empty())
        .unwrap_or(&branch)
        .to_string();
    // BY THE RUN FIRST, THEN THE BRANCH (Codex review of car 0d04ee3e,
    // R1): a second --started after the builder renamed its branch finds
    // the run's car and re-points it, where a branch-first admission
    // filed a twin beside it.
    if let Some(said) = crate::car::resolve_run_car(&door, run_id, &item_id, &branch).await? {
        return Ok(Some(format!("boss dispatch --started: {said}")));
    }
    let answer = crate::car::ItemAnswer {
        backlog_item: Some(item_id),
        ..Default::default()
    };
    let extra = serde_json::Map::from_iter([
        ("agent_run".to_string(), json!(run_id)),
        (
            boss_jobs::car::ADMITTED_BY.to_string(),
            json!("boss dispatch --started"),
        ),
    ]);
    let owner = crate::owner::for_filing_at(base).await;
    let admitted = crate::car::admit(
        &door,
        &crate::car::Admission {
            branch: &branch,
            summary: &title,
            excludes: ADMITTED_EXCLUDES,
            item: &answer,
            actor,
            host: &crate::prove::host(),
            worktree: &root.display().to_string(),
            extra,
        },
        &owner,
        false,
        now,
    )
    .await?;
    Ok(Some(match admitted {
        crate::car::Admitted::Filed(id) => format!(
            "boss dispatch --started: car {id} admitted at build for {branch} — the gate \
             moves it to gate, the green leaves it at review"
        ),
        crate::car::Admitted::Already(id) => format!(
            "boss dispatch --started: {branch} already has its car {id} — adopted, no second \
             car filed"
        ),
        crate::car::Admitted::Dry => "boss dispatch --started: nothing admitted (dry)".into(),
    }))
}

/// On a run that RESTARTS a run handed to the gate, the station's setup
/// cost read off this run's own record and merged onto it as
/// [`boss_jobs::gate_handoff::SETUP_KEY`] — `None` for every other run,
/// and for one that already carries it (recorded once).
///
/// David, 2026-11-06 15:13Z: "If it turns out we are wasting a lot of
/// time setting back up the station, maybe we cache the station resources
/// for a bit." The trial releases the slot at the gate hand-off, so a red
/// gate's restart binds a worker and warms a target again; this makes
/// that cost a number the factory reads rather than a feeling. Every time
/// in it is a server stamp — the claim step, building's claim, the event
/// that bound the worker — and nothing here reads a client clock.
pub(crate) async fn record_station_setup(
    http: &crate::machine_http::MachineClient,
    base: &str,
    run: &str,
    actor: &str,
    after: &Value,
) -> Result<Option<Value>> {
    let Some(prior) = boss_jobs::gate_handoff::restarted_from(after) else {
        return Ok(None);
    };
    if after
        .pointer(&format!("/metadata/{}", boss_jobs::gate_handoff::SETUP_KEY))
        .is_some()
    {
        return Ok(None);
    }
    let call = |method: reqwest::Method, path: String, body: Option<Value>| async move {
        crate::sor_client::api_at_signed(
            http,
            base,
            method,
            &path,
            body,
            crate::identity::Signature::As(actor.to_string()),
        )
        .await
    };
    let prior_run = call(reqwest::Method::GET, format!("/api/jobs/{prior}"), None)
        .await?
        .context("the run this one restarts returned no body")?;
    let prior_run = prior_run.get("data").unwrap_or(&prior_run).clone();
    let events = call(
        reqwest::Method::GET,
        format!("/api/jobs/{run}/events?limit=1000"),
        None,
    )
    .await?
    .context("this run's event log returned no body")?;
    let events = events
        .get("data")
        .and_then(Value::as_array)
        .cloned()
        .context("this run's event log is not a list")?;
    let Some(setup) = boss_jobs::gate_handoff::setup_record(after, &events, &prior_run) else {
        return Ok(None);
    };
    call(
        reqwest::Method::PATCH,
        format!("/api/jobs/{run}/metadata"),
        Some(json!({ boss_jobs::gate_handoff::SETUP_KEY: setup })),
    )
    .await?;
    Ok(Some(setup))
}

pub(crate) async fn started(
    run: String,
    local_root: Option<std::path::PathBuf>,
    now: chrono::DateTime<chrono::Utc>,
) -> Result<()> {
    // This is deliberately not a prefix resolver: own-run identity is exact.
    let id = uuid::Uuid::parse_str(&run).context("--started requires the full native run UUID")?;
    if id.to_string() != run {
        bail!("--started requires the canonical native run UUID");
    }
    let actor = crate::identity::sign(&reqwest::Method::PATCH, "/api/jobs")?;
    let owned = std::env::var("BOSS_AGENT_RUN").ok();
    let http = crate::sor_client::machine_client()?;
    let base = crate::sor_client::resolve_jobs_base(None)?;
    started_with_worker_at(
        &http,
        &base,
        &run,
        &actor,
        owned.as_deref(),
        local_root.as_deref(),
        &crate::dispatch_worker::Published,
    )
    .await?;
    // THE CAR IS ADMITTED AT BUILD (backlog 68a8726e, design 689d1f7f
    // car 1). Here, and not when the run is opened, because this is the
    // first moment the BRANCH is known: the builder cuts it before binding
    // its worktree, and the run's packet never names it. After the
    // receipt, and never in its way: a car that cannot be admitted is
    // filed at green as before, so the failure is said and not fatal.
    if let Some(root) = local_root.as_deref() {
        let admitted = async {
            let read = crate::sor_client::api_at_signed(
                &http,
                &base,
                reqwest::Method::GET,
                &format!("/api/jobs/{run}"),
                None,
                crate::identity::Signature::As(actor.clone()),
            )
            .await?
            .context("own run read returned no body")?;
            let job = read.get("data").unwrap_or(&read);
            admit_run_car(&http, &base, &actor, job, root, now).await
        }
        .await;
        match admitted {
            Ok(Some(said)) => println!("{said}"),
            Ok(None) => {}
            Err(e) => eprintln!(
                "boss dispatch --started: no car admitted at build ({e:#}); the worker \
                 receipt above stands, and the green files the car as before"
            ),
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn active() -> serde_json::Value {
        json!({"id":"own-run","kind":"agent-run","status":"open",
            "metadata":{"agent":"codex@algedonic.dev","unrelated":"kept"},
            "steps":[{"spec_slug":"building","status":"active","assignee_id":"agent-codex",
                "metadata":{"worker_start_expectation":{"schema":1,"minutes":15}}}]})
    }

    #[test]
    fn a_worker_receipt_names_its_own_run_actor_and_schema_without_a_clock() {
        assert_eq!(
            receipt(&active(), "own-run", "agent-codex", Some("own-run")).unwrap(),
            json!({"schema":1,"run":"own-run","actor":"agent-codex"})
        );
    }

    #[test]
    fn coordinator_metadata_and_another_run_or_actor_cannot_record_worker_receipt() {
        assert!(receipt(&active(), "own-run", "agent-codex", None).is_err());
        assert!(receipt(&active(), "own-run", "agent-codex", Some("other-run")).is_err());
        assert!(receipt(&active(), "own-run", "emp-david", Some("own-run")).is_err());
        let mut job = active();
        job["steps"][0]["assignee_id"] = json!("other-actor");
        assert!(receipt(&job, "own-run", "agent-codex", Some("own-run")).is_err());
        job = active();
        job["steps"][0]["status"] = json!("ready");
        assert!(receipt(&job, "own-run", "agent-codex", Some("own-run")).is_err());
    }

    #[test]
    fn legacy_and_terminal_runs_refuse_receipt_without_a_repin_or_resurrection() {
        let mut job = active();
        job["status"] = json!("closed");
        assert!(receipt(&job, "own-run", "agent-codex", Some("own-run")).is_err());
        job = active();
        job["steps"][0]["metadata"] = json!({});
        assert!(receipt(&job, "own-run", "agent-codex", Some("own-run")).is_err());
    }

    /// A run restarting `prior-run`, whose released source step the claim
    /// door handed back to dispatch, with its claim and building stamps.
    fn restarted() -> Value {
        json!({"id":"own-run","kind":"agent-run","status":"open",
            "metadata":{"agent":"codex@algedonic.dev","claim_receipt":{"metadata":{
                "released":{"why":"red gate","from_run":"prior-run","from":"agent-codex"}}}},
            "steps":[
                {"spec_slug":"claimed","status":"completed",
                 "completed_at":"2026-11-06T18:00:00Z","metadata":{}},
                {"spec_slug":"building","status":"active","assignee_id":"agent-codex",
                 "metadata":{"records_before_claim_closed_at":"2026-11-06T18:00:02Z"}}]})
    }

    async fn setup_stub() -> (
        String,
        std::sync::Arc<std::sync::Mutex<(Vec<String>, Vec<Value>)>>,
    ) {
        use axum::{
            Json, Router,
            extract::OriginalUri,
            routing::{get, patch},
        };
        let seen = std::sync::Arc::new(std::sync::Mutex::new((Vec::new(), Vec::new())));
        let (reads, events, writes) = (seen.clone(), seen.clone(), seen.clone());
        let app = Router::new()
            .route(
                "/api/jobs/prior-run",
                get(move || {
                    let reads = reads.clone();
                    async move {
                        reads.lock().unwrap().0.push("prior-run".into());
                        Json(json!({"id":"prior-run","kind":"agent-run","status":"open",
                            "metadata":{"agent":"codex@algedonic.dev","handed_to_gate":{
                                "schema":1,"run":"prior-run",
                                "gate_run":"77777777-7777-4777-8777-777777777777",
                                "command":"99999999-9999-4999-8999-999999999999",
                                "accepted_at":"2026-11-06T17:00:00Z",
                                "accepted_by":"agent-codex",
                                "event_id":"55555555-5555-4555-8555-555555555555",
                                "receipt_event_id":"66666666-6666-4666-8666-666666666666"}}}))
                    }
                }),
            )
            .route(
                "/api/jobs/own-run/events",
                get(move |OriginalUri(uri): OriginalUri| {
                    let events = events.clone();
                    async move {
                        events.lock().unwrap().0.push(uri.to_string());
                        Json(json!({"data":[
                            {"kind":"jobs.job.created","at":"2026-11-06T18:00:00Z",
                             "payload":{"metadata":{}}},
                            {"kind":"jobs.job.updated","at":"2026-11-06T18:00:32Z",
                             "payload":{"metadata":{"worker_session":{"session":"dev-build-0"}}}}
                        ]}))
                    }
                }),
            )
            .route(
                "/api/jobs/own-run/metadata",
                patch(move |Json(body): Json<Value>| {
                    let writes = writes.clone();
                    async move {
                        writes.lock().unwrap().1.push(body);
                        axum::http::StatusCode::NO_CONTENT
                    }
                }),
            );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        (format!("http://{address}"), seen)
    }

    /// THE SETUP COST OF A RESTART IS RECORDED ONCE, FROM SERVER STAMPS
    /// (backlog a99dd722). A run restarting one handed to the gate gets
    /// the measured spans and the named missing parts; a run that carries
    /// it already, and a run that restarts nothing, are left alone and
    /// read nothing.
    #[tokio::test]
    async fn a_restarted_run_records_its_station_setup_once() {
        let (base, seen) = setup_stub().await;
        let client = crate::sor_client::machine_client().unwrap();
        let setup = record_station_setup(&client, &base, "own-run", "agent-codex", &restarted())
            .await
            .unwrap()
            .expect("a restart after a hand-off is measured");
        assert_eq!(setup["restart_of"]["run"], "prior-run");
        assert_eq!(setup["claim_to_worker_bound_seconds"], 32.0);
        assert_eq!(setup["parts"]["worker_bind"]["at"], "2026-11-06T18:00:32Z");
        {
            let seen = seen.lock().unwrap();
            assert_eq!(seen.1, vec![json!({ "station_setup": setup.clone() })]);
            assert!(
                seen.0.iter().any(|read| read.contains("limit=1000")),
                "{:?}",
                seen.0
            );
        }

        let mut recorded = restarted();
        recorded["metadata"]["station_setup"] = setup;
        let mut fresh = restarted();
        fresh["metadata"]["claim_receipt"] = json!({"metadata":{}});
        for already_or_fresh in [recorded, fresh] {
            let before = seen.lock().unwrap().clone();
            let none =
                record_station_setup(&client, &base, "own-run", "agent-codex", &already_or_fresh)
                    .await
                    .unwrap();
            assert_eq!(none, None);
            assert_eq!(*seen.lock().unwrap(), before, "no read, no write");
        }
    }

    /// A bounded START's run (design e72dc7c9) asks for the receipt that
    /// names its exact START and manifest; an incomplete expectation is
    /// refused rather than acknowledged as a whole-START receipt.
    #[test]
    fn a_bounded_run_receipt_names_its_start_and_manifest_digests() {
        let mut job = active();
        let expected =
            boss_jobs::execution_inputs::start_expectation("the START", &json!([{"n": 1}]), 15);
        job["steps"][0]["metadata"]["worker_start_expectation"] = expected.clone();
        let r = receipt(&job, "own-run", "agent-codex", Some("own-run")).unwrap();
        assert_eq!(r["schema"], 3);
        assert_eq!(r["start_sha256"], expected["start_sha256"]);
        assert_eq!(r["manifest_sha256"], expected["manifest_sha256"]);
        assert!(boss_jobs::execution_inputs::start_receipt_matches(
            &expected,
            &r,
            "own-run",
            "agent-codex"
        ));
        job["steps"][0]["metadata"]["worker_start_expectation"]["start_bytes"] = json!(0);
        assert!(receipt(&job, "own-run", "agent-codex", Some("own-run")).is_err());
    }

    async fn stub(
        drop_receipt: bool,
        close_after_write: bool,
    ) -> (
        String,
        std::sync::Arc<std::sync::Mutex<(Value, Vec<Value>)>>,
    ) {
        stub_registry(
            drop_receipt,
            close_after_write,
            json!({"data":[
            {"id":"agent-codex","aliases":["codex@algedonic.dev"]}
        ],"total":1}),
        )
        .await
    }

    async fn stub_registry(
        drop_receipt: bool,
        close_after_write: bool,
        registry: Value,
    ) -> (
        String,
        std::sync::Arc<std::sync::Mutex<(Value, Vec<Value>)>>,
    ) {
        use axum::{
            Json, Router,
            routing::{get, patch},
        };
        let mut run = active();
        run["steps"][0]["id"] = json!("s-building");
        let state = std::sync::Arc::new(std::sync::Mutex::new((run, Vec::new())));
        let read = state.clone();
        let write = state.clone();
        let step_write = state.clone();
        let app = Router::new()
            // The binding a bound start records on its claim's own step
            // (review e296014b R2). Not logged: these tests count the
            // run's receipt writes.
            .route(
                "/api/jobs/own-run/steps/s-building/metadata",
                patch(move |Json(body): Json<Value>| {
                    let step_write = step_write.clone();
                    async move {
                        let mut s = step_write.lock().unwrap();
                        for (key, value) in body.as_object().unwrap() {
                            s.0["steps"][0]["metadata"][key] = value.clone();
                        }
                        axum::http::StatusCode::NO_CONTENT
                    }
                }),
            )
            .route(
                "/api/agents",
                get(move || {
                    let registry = registry.clone();
                    async move { Json(registry) }
                }),
            )
            .route(
                "/api/jobs/own-run",
                get(move || {
                    let read = read.clone();
                    async move { Json(read.lock().unwrap().0.clone()) }
                }),
            )
            .route(
                "/api/jobs/own-run/metadata",
                patch(move |Json(body): Json<Value>| {
                    let write = write.clone();
                    async move {
                        let mut s = write.lock().unwrap();
                        s.1.push(body.clone());
                        if !drop_receipt {
                            for (key, value) in body.as_object().unwrap() {
                                s.0["metadata"][key] = value.clone();
                            }
                        }
                        if close_after_write {
                            s.0["status"] = json!("closed");
                        }
                        axum::http::StatusCode::NO_CONTENT
                    }
                }),
            );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        (format!("http://{address}"), state)
    }

    #[tokio::test]
    async fn the_supported_door_merges_only_its_receipt_and_retries_without_a_second_write() {
        let (base, state) = stub(false, false).await;
        let client = crate::sor_client::machine_client().unwrap();
        for _ in 0..2 {
            started_at(
                &client,
                &base,
                "own-run",
                "codex@algedonic.dev",
                Some("own-run"),
            )
            .await
            .unwrap();
        }
        let s = state.lock().unwrap();
        assert_eq!(s.1.len(), 1);
        assert_eq!(
            s.1[0],
            json!({"worker_started":{"schema":1,"run":"own-run","actor":"agent-codex"}})
        );
        assert_eq!(s.0["metadata"]["unrelated"], "kept");
        assert_eq!(s.0["steps"][0]["status"], "active");
    }

    struct Worker {
        refused: bool,
        calls: std::sync::Mutex<Vec<String>>,
    }

    #[async_trait::async_trait]
    impl crate::dispatch_worker::Workers for Worker {
        async fn ensure(&self, run: &str, root: &std::path::Path) -> Result<Option<Value>> {
            self.calls
                .lock()
                .unwrap()
                .push(format!("{run}:{}", root.display()));
            if self.refused {
                bail!(
                    "storage budget: full allocation refusal; retained worker dev-build-0123456789abcdef"
                );
            }
            Ok(Some(
                json!({"run":run,"session":"dev-build-0123456789abcdef","local_root":root}),
            ))
        }
        async fn stop(&self, _: &str) -> Result<Option<Value>> {
            Ok(None)
        }
    }

    #[tokio::test]
    async fn allocation_refusal_is_kept_whole_and_acknowledges_no_start() {
        let (base, state) = stub(false, false).await;
        let worker = Worker {
            refused: true,
            calls: Default::default(),
        };
        let error = started_with_worker_at(
            &crate::sor_client::machine_client().unwrap(),
            &base,
            "own-run",
            "agent-codex",
            Some("own-run"),
            Some(std::path::Path::new("/source")),
            &worker,
        )
        .await
        .expect_err("no worker, no acknowledgment");
        assert!(
            format!("{error:#}")
                .contains("full allocation refusal; retained worker dev-build-0123456789abcdef")
        );
        assert!(state.lock().unwrap().1.is_empty());
    }

    #[tokio::test]
    async fn a_bound_start_records_controller_identity_and_retries_one_receipt() {
        let (base, state) = stub(false, false).await;
        let worker = Worker {
            refused: false,
            calls: Default::default(),
        };
        for _ in 0..2 {
            started_with_worker_at(
                &crate::sor_client::machine_client().unwrap(),
                &base,
                "own-run",
                "agent-codex",
                Some("own-run"),
                Some(std::path::Path::new("/source")),
                &worker,
            )
            .await
            .unwrap();
        }
        let state = state.lock().unwrap();
        assert_eq!(state.1.len(), 1);
        assert_eq!(
            state.0["metadata"][crate::dispatch_worker::KEY]["session"],
            "dev-build-0123456789abcdef"
        );
        assert!(state.0["metadata"][RECEIPT_KEY].is_object());
    }

    #[tokio::test]
    async fn a_retirement_receipt_is_copied_not_recast_as_physical_gc() {
        for lost in [false, true] {
            let (base, state) = stub(lost, false).await;
            let receipt = json!({"run":"own-run","retirement":{"state":"local execution retired",
                "job_delete_acknowledged":true,"job_deletion_complete":false}});
            let result = crate::dispatch_worker::record_retirement_at(
                &crate::sor_client::machine_client().unwrap(),
                &base,
                "own-run",
                "agent-codex",
                receipt.clone(),
            )
            .await;
            if lost {
                assert!(result.is_err());
            } else {
                result.unwrap();
                assert_eq!(
                    state.lock().unwrap().0["metadata"][crate::dispatch_worker::RETIREMENT_KEY],
                    receipt
                );
            }
        }
    }

    #[tokio::test]
    async fn a_lost_receipt_or_concurrent_close_is_never_reported_as_acknowledged() {
        let client = crate::sor_client::machine_client().unwrap();
        for (drop, close) in [(true, false), (false, true)] {
            let (base, state) = stub(drop, close).await;
            assert!(
                started_at(&client, &base, "own-run", "agent-codex", Some("own-run"))
                    .await
                    .is_err()
            );
            assert_eq!(state.lock().unwrap().1.len(), 1);
        }
        let (base, state) = stub(false, false).await;
        assert!(
            started_at(&client, &base, "own-run", "agent-codex", Some("other-run"))
                .await
                .is_err()
        );
        assert!(state.lock().unwrap().1.is_empty());
    }

    #[test]
    fn started_is_a_separate_clap_mode_and_cannot_override_dispatch_settings() {
        use clap::Parser;
        assert!(crate::Cli::try_parse_from(["boss", "dispatch", "own-run", "--started"]).is_ok());
        assert!(
            crate::Cli::try_parse_from([
                "boss",
                "dispatch",
                "own-run",
                "--started",
                "--local-root",
                "/tree"
            ])
            .is_ok()
        );
        assert!(
            crate::Cli::try_parse_from(["boss", "dispatch", "own-run", "--local-root", "/tree"])
                .is_err()
        );
        for flag in ["--from-hook", "--next", "--force"] {
            assert!(
                crate::Cli::try_parse_from(["boss", "dispatch", "own-run", "--started", flag])
                    .is_err()
            );
        }
        assert!(
            crate::Cli::try_parse_from([
                "boss",
                "dispatch",
                "own-run",
                "--started",
                "--model",
                "invented"
            ])
            .is_err()
        );
    }

    #[tokio::test]
    async fn an_unknown_ambiguous_or_unavailable_principal_never_writes_a_receipt() {
        let client = crate::sor_client::machine_client().unwrap();
        for registry in [
            json!({"data":[],"total":0}),
            json!({"data":[{"aliases":["codex@algedonic.dev"]}],"total":1}),
            json!({"data":[{"id":"agent-codex","aliases":["codex@algedonic.dev"]},
                {"id":"other","aliases":["codex@algedonic.dev"]}],"total":2}),
            json!({"data":[{"id":"agent-codex","aliases":["codex@algedonic.dev"]}],"total":2}),
            json!({"error":"unavailable"}),
        ] {
            let (base, state) = stub_registry(false, false, registry).await;
            assert!(
                started_at(
                    &client,
                    &base,
                    "own-run",
                    "codex@algedonic.dev",
                    Some("own-run")
                )
                .await
                .is_err()
            );
            assert!(state.lock().unwrap().1.is_empty());
        }
        let (base, state) = stub(false, false).await;
        assert!(
            started_at(
                &client,
                &base,
                "own-run",
                "stranger@example.test",
                Some("own-run")
            )
            .await
            .is_err()
        );
        assert!(state.lock().unwrap().1.is_empty());
    }

    #[tokio::test]
    async fn a_malformed_registry_id_cannot_become_a_recorded_worker_actor() {
        let client = crate::sor_client::machine_client().unwrap();
        for id in [json!(""), json!("   "), json!(null), json!(17)] {
            let registry = json!({"data":[{"id":id,"aliases":["codex@algedonic.dev"]}],"total":1});
            let (base, state) = stub_registry(false, false, registry).await;
            // A malformed native assignment cannot make a malformed row an actor.
            state.lock().unwrap().0["steps"][0]["assignee_id"] = id;
            assert!(
                started_at(
                    &client,
                    &base,
                    "own-run",
                    "codex@algedonic.dev",
                    Some("own-run")
                )
                .await
                .is_err()
            );
            assert!(state.lock().unwrap().1.is_empty());
        }
    }

    /// The claim door as the jobs API answers it for a READY step: the
    /// caller's own nomination becomes ACTIVE, anyone else's is a 409
    /// naming the holder. The stub records who asked.
    async fn stub_with_claim(
        holder: &str,
    ) -> (
        String,
        std::sync::Arc<std::sync::Mutex<(Value, Vec<Value>)>>,
    ) {
        use axum::{Json, routing::post};
        let (base, state) = stub(false, false).await;
        {
            let mut s = state.lock().unwrap();
            s.0["steps"][0]["id"] = json!("s-building");
            s.0["steps"][0]["status"] = json!("ready");
            s.0["steps"][0]["assignee_id"] = json!(holder);
        }
        let claims = state.clone();
        let app = axum::Router::new()
            .route(
                "/api/jobs/own-run/steps/s-building/claim",
                post(move |headers: axum::http::HeaderMap| {
                    let claims = claims.clone();
                    async move {
                        let signer = headers
                            .get("x-boss-user")
                            .and_then(|v| v.to_str().ok())
                            .and_then(|v| serde_json::from_str::<Value>(v).ok())
                            .map(|v| v["id"].clone())
                            .unwrap_or(Value::Null);
                        let mut s = claims.lock().unwrap();
                        s.1.push(json!({"claim_by": signer}));
                        // The login door: an alias signs as its registered id.
                        if s.0["steps"][0]["assignee_id"] == "agent-codex"
                            && signer == "codex@algedonic.dev"
                        {
                            s.0["steps"][0]["status"] = json!("active");
                            (axum::http::StatusCode::OK, Json(json!({"status":"active"})))
                        } else {
                            (
                                axum::http::StatusCode::CONFLICT,
                                Json(
                                    json!({"error":"held","holder":s.0["steps"][0]["assignee_id"]}),
                                ),
                            )
                        }
                    }
                }),
            )
            .fallback(move |req: axum::extract::Request| {
                let base = base.clone();
                async move {
                    let client = reqwest::Client::new();
                    let url = format!("{base}{}", req.uri().path());
                    let method = req.method().clone();
                    let body = axum::body::to_bytes(req.into_body(), 1 << 20)
                        .await
                        .unwrap();
                    let answer = client
                        .request(method, url)
                        .header("content-type", "application/json")
                        .body(body)
                        .send()
                        .await
                        .unwrap();
                    let status =
                        axum::http::StatusCode::from_u16(answer.status().as_u16()).unwrap();
                    (status, answer.bytes().await.unwrap())
                }
            });
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        (format!("http://{address}"), state)
    }

    /// THE SHARED CAUSE (backlog 2f7b8c00, run f7c7891b, 2026-10-07).
    /// Every Claude subagent that day had `--started` refused "requires
    /// this actor's active assigned building step": its Building was
    /// READY and nominated to it, and nothing in the protocol claims a
    /// run's Building — Codex sessions did it by hand. The verb now takes
    /// its own nomination through the claim door first.
    #[tokio::test]
    async fn started_claims_its_own_ready_nomination_before_the_receipt() {
        let (base, state) = stub_with_claim("agent-codex").await;
        let client = crate::sor_client::machine_client().unwrap();
        started_at(
            &client,
            &base,
            "own-run",
            "codex@algedonic.dev",
            Some("own-run"),
        )
        .await
        .expect("a ready step nominated to the caller is claimed, then acknowledged");
        let s = state.lock().unwrap();
        assert_eq!(s.0["steps"][0]["status"], "active");
        assert_eq!(
            s.1,
            vec![
                json!({"claim_by":"codex@algedonic.dev"}),
                json!({"worker_started":{"schema":1,"run":"own-run","actor":"agent-codex"}}),
            ],
            "one claim signed by the caller, then the one receipt"
        );
    }

    /// NO WRITE BEFORE A REFUSAL (review f7f0b689, N5). A legacy run
    /// declares no worker receipt, so `--started` refuses it — and it
    /// used to claim the step first, leaving it ACTIVE behind a refusal.
    #[tokio::test]
    async fn a_run_that_declares_no_worker_receipt_is_refused_before_any_claim() {
        let (base, state) = stub_with_claim("agent-codex").await;
        state.lock().unwrap().0["steps"][0]["metadata"] = json!({});
        let client = crate::sor_client::machine_client().unwrap();
        let refused = started_at(
            &client,
            &base,
            "own-run",
            "codex@algedonic.dev",
            Some("own-run"),
        )
        .await
        .unwrap_err()
        .to_string();
        assert!(refused.contains("declares no worker receipt"), "{refused}");
        let s = state.lock().unwrap();
        assert!(s.1.is_empty(), "no claim and no receipt: {:?}", s.1);
        assert_eq!(s.0["steps"][0]["status"], "ready");
    }

    /// The claim is the caller's own or it is not made: a Building
    /// nominated to another actor, or to nobody, is reported and no claim
    /// and no receipt is sent. The door would refuse the first anyway;
    /// the second it would GRANT, and a worker that takes an unassigned
    /// run is the identity slip this verb exists to refuse.
    #[tokio::test]
    async fn started_never_claims_a_building_step_nominated_to_someone_else_or_to_nobody() {
        let client = crate::sor_client::machine_client().unwrap();
        for holder in [json!("agent-claude"), Value::Null] {
            let (base, state) = stub_with_claim("agent-claude").await;
            state.lock().unwrap().0["steps"][0]["assignee_id"] = holder;
            let refused = started_at(
                &client,
                &base,
                "own-run",
                "codex@algedonic.dev",
                Some("own-run"),
            )
            .await
            .unwrap_err()
            .to_string();
            assert!(
                refused.contains("active assigned building step"),
                "{refused}"
            );
            assert!(state.lock().unwrap().1.is_empty(), "no claim, no receipt");
        }
    }

    /// The floor one `--started` stands on (backlog 0c55e31d): the run,
    /// the claim door, the step's merge door and PUT, and the worker
    /// controller, all writing one ordered log, so a test reads WHAT
    /// happened and in which order.
    type Floor = std::sync::Arc<std::sync::Mutex<(Value, Vec<String>)>>;

    /// The claim door's answer: granted, or a pool exhausted (409).
    /// `GrantsThenUnheld` answers 200 while a concurrent hand-back puts the
    /// step back READY before the authoritative read (review e296014b R1);
    /// `GrantsThenDark` answers 200 and the run then cannot be read.
    #[derive(Clone, Copy)]
    enum Door {
        Grants,
        Exhausted,
        GrantsThenUnheld,
        GrantsThenDark,
    }

    /// The binding a CONCURRENT `--started` of the same run records, the
    /// way the binder records it: on its claim's own step first, then on
    /// the run (review e296014b R2).
    fn concurrent_binding(s: &mut (Value, Vec<String>)) {
        let other = json!({"run":"own-run","session":"dev-build-fedcba9876543210"});
        s.0["steps"][0]["metadata"][crate::dispatch_worker::KEY] = other.clone();
        s.0["metadata"][crate::dispatch_worker::KEY] = other;
    }

    /// Does the fixture's building step still read as the writer judged
    /// it, in [`boss_jobs::step_metadata_write::EXPECT_HEADER`]? The real
    /// doors judge it under the row's lock; the floor judges it under its
    /// one mutex.
    fn judged_still_reads(s: &(Value, Vec<String>), headers: &axum::http::HeaderMap) -> bool {
        headers
            .get(boss_jobs::step_metadata_write::EXPECT_HEADER)
            .is_none_or(|raw| {
                let judged =
                    boss_jobs::step_metadata_write::expectation_from_header(raw.as_bytes())
                        .unwrap();
                boss_jobs::step_metadata_write::metadata_still_reads(
                    &s.0["steps"][0]["metadata"],
                    &judged,
                )
            })
    }

    fn step_changed() -> axum::response::Response {
        use axum::response::IntoResponse;
        (
            axum::http::StatusCode::CONFLICT,
            axum::Json(json!({"code": boss_jobs::step_metadata_write::STEP_CHANGED_CODE})),
        )
            .into_response()
    }

    /// A fixture switch, set on the floor's run under `_fixture`.
    fn switch(s: &(Value, Vec<String>), name: &str) -> bool {
        s.0["_fixture"][name] == json!(true)
    }

    async fn floor(building: &str, door: Door) -> (String, Floor) {
        use axum::{
            Json, Router,
            routing::{get, patch, post},
        };
        let mut run = active();
        run["steps"][0]["id"] = json!("s-building");
        run["steps"][0]["status"] = json!(building);
        let floor: Floor = std::sync::Arc::new(std::sync::Mutex::new((run, Vec::new())));
        let f = |floor: &Floor| floor.clone();
        let (read, meta, claim, step_meta, put, version) = (
            f(&floor),
            f(&floor),
            f(&floor),
            f(&floor),
            f(&floor),
            f(&floor),
        );
        let app = Router::new()
            // The run's PINNED agent-run versions (backlog db88f6a9): v16
            // waives the analyst's vCPU, v15 is the bill before it (no
            // list), and any other version is not in the registry.
            .route(
                "/api/workflows/agent-run/versions/{v}",
                get(move |axum::extract::Path(v): axum::extract::Path<i64>| {
                    let version = version.clone();
                    async move {
                        use axum::response::IntoResponse;
                        version.lock().unwrap().1.push(format!("version {v} read"));
                        let bill = match v {
                            16 => json!({"model":"from-run","vcpu":"2.5",
                                "workerless_profiles":["analyst"]}),
                            15 => json!({"model":"from-run","vcpu":"2.5"}),
                            _ => return axum::http::StatusCode::NOT_FOUND.into_response(),
                        };
                        Json(json!({"kind":"agent-run","version":v,"steps":[
                            {"title":"briefed","kind":"task","ready_when":"true"},
                            {"title":"building","kind":"task","ready_when":"true",
                             "resources":bill}]}))
                        .into_response()
                    }
                }),
            )
            .route(
                "/api/agents",
                get(|| async {
                    Json(
                        json!({"data":[{"id":"agent-codex","aliases":["codex@algedonic.dev"]}],
                        "total":1}),
                    )
                }),
            )
            .route(
                "/api/jobs/own-run",
                get(move || {
                    let read = read.clone();
                    async move {
                        use axum::response::IntoResponse;
                        let s = read.lock().unwrap();
                        if switch(&s, "dark") {
                            return axum::http::StatusCode::INTERNAL_SERVER_ERROR.into_response();
                        }
                        Json(s.0.clone()).into_response()
                    }
                }),
            )
            .route(
                "/api/jobs/own-run/metadata",
                patch(move |Json(body): Json<Value>| {
                    let meta = meta.clone();
                    async move {
                        let mut s = meta.lock().unwrap();
                        let mut keys: Vec<_> = body.as_object().unwrap().keys().cloned().collect();
                        keys.sort();
                        s.1.push(format!("run metadata {}", keys.join(",")));
                        for (key, value) in body.as_object().unwrap() {
                            s.0["metadata"][key] = value.clone();
                        }
                        axum::http::StatusCode::NO_CONTENT
                    }
                }),
            )
            .route(
                "/api/jobs/own-run/steps/s-building/claim",
                post(move || {
                    let claim = claim.clone();
                    async move {
                        let mut s = claim.lock().unwrap();
                        s.1.push("claim".into());
                        match door {
                            Door::Grants => {
                                s.0["steps"][0]["status"] = json!("active");
                                (axum::http::StatusCode::OK, Json(json!({"status":"active"})))
                            }
                            Door::Exhausted => (
                                axum::http::StatusCode::CONFLICT,
                                Json(json!({"error":"capacity-exhausted",
                                    "queued":{"pool":"robots:opus","position":2,"waiting":3}})),
                            ),
                            Door::GrantsThenUnheld => {
                                s.0["steps"][0]["status"] = json!("ready");
                                (axum::http::StatusCode::OK, Json(json!({"status":"active"})))
                            }
                            Door::GrantsThenDark => {
                                s.0["steps"][0]["status"] = json!("active");
                                s.0["_fixture"]["dark"] = json!(true);
                                (axum::http::StatusCode::OK, Json(json!({"status":"active"})))
                            }
                        }
                    }
                }),
            )
            .route(
                "/api/jobs/own-run/steps/s-building/metadata",
                patch(
                    move |headers: axum::http::HeaderMap, Json(body): Json<Value>| {
                        let step_meta = step_meta.clone();
                        async move {
                            use axum::response::IntoResponse;
                            let mut s = step_meta.lock().unwrap();
                            if body.get(crate::dispatch_worker::KEY).is_some()
                                && switch(&s, "hand_back_before_mirror")
                            {
                                s.1.push("concurrent hand-back".into());
                                s.0["steps"][0]["status"] = json!("ready");
                            }
                            if !judged_still_reads(&s, &headers) {
                                s.1.push("building metadata refused".into());
                                return step_changed();
                            }
                            s.1.push("building metadata".into());
                            for (key, value) in body.as_object().unwrap() {
                                s.0["steps"][0]["metadata"][key] = value.clone();
                            }
                            if body.get(RETURNED_KEY).is_some() && switch(&s, "bind_after_returned")
                            {
                                s.1.push("concurrent binding".into());
                                concurrent_binding(&mut s);
                            }
                            axum::http::StatusCode::NO_CONTENT.into_response()
                        }
                    },
                ),
            )
            .route(
                "/api/jobs/own-run/steps/s-building",
                axum::routing::put(
                    move |headers: axum::http::HeaderMap, Json(body): Json<Value>| {
                        let put = put.clone();
                        async move {
                            use axum::response::IntoResponse;
                            let mut s = put.lock().unwrap();
                            if !judged_still_reads(&s, &headers) {
                                s.1.push("building put refused".into());
                                return step_changed();
                            }
                            s.1.push(format!("building put {body}"));
                            for (key, value) in body.as_object().unwrap() {
                                s.0["steps"][0][key] = value.clone();
                            }
                            Json(s.0["steps"][0].clone()).into_response()
                        }
                    },
                ),
            );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        (format!("http://{address}"), floor)
    }

    /// The worker controller on the same log: it admits or refuses, and
    /// it stops with a receipt.
    struct Controller {
        refuses: bool,
        floor: Floor,
    }

    #[async_trait::async_trait]
    impl crate::dispatch_worker::Workers for Controller {
        async fn ensure(&self, run: &str, root: &std::path::Path) -> Result<Option<Value>> {
            self.floor.lock().unwrap().1.push("ensure".into());
            if self.refuses {
                bail!(
                    "worker storage budget reached: 3 worker volume(s) held and Longhorn can \
                     place 3 of 100Gi: w-1/disk 371.6 GiB placeable; nothing created"
                );
            }
            Ok(Some(
                json!({"run":run,"session":"dev-build-0123456789abcdef","local_root":root}),
            ))
        }
        async fn stop(&self, run: &str) -> Result<Option<Value>> {
            self.floor.lock().unwrap().1.push("stop".into());
            Ok(Some(
                json!({"run":run,"session":"dev-build-0123456789abcdef","state":"retired"}),
            ))
        }
    }

    async fn start(base: &str, floor: &Floor, refuses: bool) -> Result<()> {
        started_with_worker_at(
            &crate::sor_client::machine_client().unwrap(),
            base,
            "own-run",
            "agent-codex",
            Some("own-run"),
            Some(std::path::Path::new("/source")),
            &Controller {
                refuses,
                floor: floor.clone(),
            },
        )
        .await
    }

    /// The floor's run as `boss dispatch` files it: the execution
    /// `profile` and the agent-run version the packet is pinned to.
    fn profiled(floor: &Floor, profile: &str, version: i64) {
        let mut s = floor.lock().unwrap();
        s.0["metadata"][boss_jobs::resources::RUN_PROFILE_KEY] = json!(profile);
        s.0["workflow_version"] = json!(version);
    }

    /// A PROFILE THE BILL WAIVES BINDS NO WORKER (backlog db88f6a9;
    /// review 71005282 B2). The run's pinned building bill lists
    /// `analyst` in `workerless_profiles`, so its claim draws no host;
    /// a worker admitted under it would be a 2.5-vCPU Pod no host pool
    /// counts — the 5608c1a2 defect again. So `--local-root` on such a
    /// run is refused at the door, off the same registry fact the claim
    /// reads, before the claim and before the worker door is asked:
    /// nothing is claimed, ensured, bound or receipted.
    #[tokio::test]
    async fn a_profile_its_bill_waives_is_refused_a_worker_before_any_claim() {
        let (base, floor) = floor("ready", Door::Grants).await;
        profiled(&floor, "analyst", 16);
        let refused = format!("{:#}", start(&base, &floor, false).await.unwrap_err());
        assert!(refused.contains("workerless_profiles"), "{refused}");
        assert!(refused.contains("analyst"), "{refused}");
        assert!(refused.contains("without --local-root"), "{refused}");
        let s = floor.lock().unwrap();
        assert_eq!(
            s.1,
            vec!["version 16 read"],
            "the pinned bill read, and no claim, no ensure, no write"
        );
        assert_eq!(s.0["steps"][0]["status"], "ready");
        assert!(s.0["metadata"].get(crate::dispatch_worker::KEY).is_none());
        assert!(s.0["metadata"].get(RECEIPT_KEY).is_none());
    }

    /// Only a profile the PINNED bill names is refused: a builder on the
    /// waiving version, and an analyst pinned to the version before it
    /// (whose claim still bills the 2.5 on a host), each take the claim
    /// and the worker as before. A pinned version that cannot be read
    /// is refused too — whether the claim draws a host is then unknown,
    /// and a missing fact never admits a worker no pool was asked about.
    #[tokio::test]
    async fn only_a_profile_its_pinned_bill_waives_is_refused_a_worker() {
        for (profile, version) in [("builder", 16), ("analyst", 15)] {
            let (base, floor) = floor("ready", Door::Grants).await;
            profiled(&floor, profile, version);
            start(&base, &floor, false)
                .await
                .unwrap_or_else(|e| panic!("{profile} on v{version}: {e:#}"));
            let s = floor.lock().unwrap();
            assert_eq!(
                s.1[..3],
                [
                    format!("version {version} read"),
                    "claim".into(),
                    "ensure".into()
                ],
                "{profile} on v{version}"
            );
        }
        let (base, floor) = floor("ready", Door::Grants).await;
        profiled(&floor, "builder", 99);
        let refused = format!("{:#}", start(&base, &floor, false).await.unwrap_err());
        assert!(refused.contains("v99"), "{refused}");
        assert_eq!(floor.lock().unwrap().1, vec!["version 99 read"]);
    }

    /// THE BUILDING CLAIM PRECEDES THE WORKER (backlog 5608c1a2, review
    /// 593436d2 R1): claim, then ensure, then the binding on the claim's
    /// own step, then the binding and receipt on the run — in that order.
    /// The claim draws the run's host (`building` bills vCPU placed on a
    /// host:<node> pool), and the worker door reads that draw off the run
    /// to require the worker onto that node; asked for first — the order
    /// 0c55e31d chose — the worker had no node to go to. The step's copy
    /// of the binding is what a hand-back's write is judged against
    /// (review e296014b R2).
    #[tokio::test]
    async fn the_building_claim_precedes_the_worker() {
        let (base, floor) = floor("ready", Door::Grants).await;
        start(&base, &floor, false)
            .await
            .expect("claimed, admitted");
        let s = floor.lock().unwrap();
        assert_eq!(
            s.1,
            vec![
                "claim",
                "ensure",
                "building metadata",
                "run metadata worker_session,worker_started"
            ],
            "the claim first, then the worker, then the binding and the receipt"
        );
        assert_eq!(s.0["steps"][0]["status"], "active");
        assert_eq!(
            s.0["steps"][0]["metadata"][crate::dispatch_worker::KEY],
            s.0["metadata"][crate::dispatch_worker::KEY],
            "the claim's step and the run record one binding"
        );
    }

    /// A WORKER REFUSAL LEAVES NO CLAIM. Measured on afa6db72e: the claim
    /// came first and a storage refusal left `building` ACTIVE with no
    /// worker under it, the half-held state 0c55e31d removed by asking
    /// for the worker first. The claim comes first again (5608c1a2), so
    /// the same refusal now HANDS THE CLAIM BACK: the refusal whole on the
    /// step, the step READY and still nominated to this actor, no receipt
    /// — the end state 0c55e31d bought, reached the way it is reached for
    /// a building `boss dispatch` claimed before it printed.
    #[tokio::test]
    async fn a_worker_refusal_hands_the_claim_it_followed_back() {
        let (base, floor) = floor("ready", Door::Grants).await;
        let refused = format!("{:#}", start(&base, &floor, true).await.unwrap_err());
        assert!(
            refused.contains("Longhorn can place 3 of 100Gi"),
            "{refused}"
        );
        assert!(refused.contains("HANDED BACK"), "{refused}");
        let s = floor.lock().unwrap();
        assert_eq!(
            s.1,
            vec![
                "claim",
                "ensure",
                "building metadata",
                r#"building put {"status":"ready"}"#
            ],
            "the claim, the refusal, the hand-back; no receipt"
        );
        let building = &s.0["steps"][0];
        assert_eq!(building["status"], "ready");
        assert_eq!(building["assignee_id"], "agent-codex");
        assert!(
            building["metadata"][RETURNED_KEY]["why"]
                .as_str()
                .unwrap()
                .contains("Longhorn can place 3 of 100Gi")
        );
        assert!(s.0["metadata"].get(crate::dispatch_worker::KEY).is_none());
        assert!(s.0["metadata"].get(RECEIPT_KEY).is_none());
    }

    /// A REFUSED CLAIM ASKS FOR NO WORKER: the pool is exhausted at the
    /// claim door, so nothing is admitted and nothing has to be retired —
    /// no binding, no START receipt, no retirement, and the step still
    /// waits nominated to this actor. Admission before placement was the
    /// defect (review 593436d2 R1): a worker created before the claim was
    /// scheduled before the pool had been asked, and its retirement was
    /// cleanup, not a refusal.
    #[tokio::test]
    async fn a_refused_claim_asks_for_no_worker() {
        let (base, floor) = floor("ready", Door::Exhausted).await;
        let refused = format!("{:#}", start(&base, &floor, false).await.unwrap_err());
        assert!(refused.contains("capacity-exhausted"), "{refused}");
        assert!(refused.contains("no worker was asked for"), "{refused}");
        let s = floor.lock().unwrap();
        assert_eq!(s.1, vec!["claim"], "the refusal, and nothing to clean up");
        assert_eq!(s.0["steps"][0]["status"], "ready");
        assert_eq!(s.0["steps"][0]["assignee_id"], "agent-codex");
        assert!(
            s.0["metadata"]
                .get(crate::dispatch_worker::RETIREMENT_KEY)
                .is_none()
        );
        assert!(s.0["metadata"].get(crate::dispatch_worker::KEY).is_none());
        assert!(s.0["metadata"].get(RECEIPT_KEY).is_none());
    }

    /// A building ALREADY claimed when the worker door refuses — by `boss
    /// dispatch` before it printed (allocation item 1), or by hand — is
    /// handed back: the refusal recorded on the step, the step READY and
    /// still nominated to this actor, read back. Neither side holds.
    #[tokio::test]
    async fn a_worker_refusal_hands_an_already_claimed_building_back_to_waiting() {
        let (base, floor) = floor("active", Door::Grants).await;
        let refused = format!("{:#}", start(&base, &floor, true).await.unwrap_err());
        assert!(refused.contains("HANDED BACK"), "{refused}");
        let s = floor.lock().unwrap();
        assert_eq!(
            s.1,
            vec![
                "ensure",
                "building metadata",
                r#"building put {"status":"ready"}"#
            ]
        );
        let building = &s.0["steps"][0];
        assert_eq!(building["status"], "ready");
        assert_eq!(building["assignee_id"], "agent-codex");
        assert!(
            building["metadata"][RETURNED_KEY]["why"]
                .as_str()
                .unwrap()
                .contains("Longhorn can place 3 of 100Gi")
        );
        assert!(s.0["metadata"].get(RECEIPT_KEY).is_none());
    }

    /// A run that already records a worker binding keeps its claim when a
    /// retry's worker request is refused: that claim is that worker's.
    #[tokio::test]
    async fn a_refused_retry_of_a_bound_run_changes_nothing() {
        let (base, floor) = floor("active", Door::Grants).await;
        floor.lock().unwrap().0["metadata"][crate::dispatch_worker::KEY] =
            json!({"run":"own-run","session":"dev-build-0123456789abcdef"});
        let refused = format!("{:#}", start(&base, &floor, true).await.unwrap_err());
        assert!(
            refused.contains("already records a worker binding"),
            "{refused}"
        );
        let s = floor.lock().unwrap();
        assert_eq!(s.1, vec!["ensure"]);
        assert_eq!(s.0["steps"][0]["status"], "active");
    }

    /// THE READ-BACK IS THE FACT, WHATEVER THE CLAIM ANSWERED (review
    /// e296014b R1, ported from its reproduction
    /// `reviewer_successful_claim_but_unheld_readback_retires_worker`). The
    /// claim door answers 200 and a concurrent hand-back puts the step back
    /// READY before the run is read: the claim is judged NOT held, so no
    /// worker is asked for — with the claim first (5608c1a2) there is no
    /// admitted worker to retire, and nothing is recorded.
    #[tokio::test]
    async fn a_granted_claim_that_reads_back_unheld_asks_for_no_worker() {
        let (base, floor) = floor("ready", Door::GrantsThenUnheld).await;
        let refused = format!("{:#}", start(&base, &floor, false).await.unwrap_err());
        assert!(refused.contains("not held"), "{refused}");
        assert!(refused.contains("no worker was asked for"), "{refused}");
        let s = floor.lock().unwrap();
        assert_eq!(s.1, vec!["claim"], "judged not held; nothing admitted");
        assert!(
            s.0["metadata"]
                .get(crate::dispatch_worker::RETIREMENT_KEY)
                .is_none()
        );
        assert!(s.0["metadata"].get(crate::dispatch_worker::KEY).is_none());
        assert!(s.0["metadata"].get(RECEIPT_KEY).is_none());
    }

    /// ...and a read-back that FAILS stays conservative: whether the claim
    /// holds is unknown, so no worker is asked for under it, and the retry
    /// reads it first.
    #[tokio::test]
    async fn a_granted_claim_whose_read_back_fails_asks_for_no_worker() {
        let (base, floor) = floor("ready", Door::GrantsThenDark).await;
        let refused = format!("{:#}", start(&base, &floor, false).await.unwrap_err());
        assert!(refused.contains("unknown"), "{refused}");
        let s = floor.lock().unwrap();
        assert_eq!(s.1, vec!["claim"], "nothing proved, nothing admitted");
    }

    /// The controller of review e296014b's R2 reproduction: while this
    /// invocation's worker is asked for, a concurrent `--started` of the
    /// same run binds its worker; then this invocation is refused.
    struct RefuseAfterBinding(Floor);

    #[async_trait::async_trait]
    impl crate::dispatch_worker::Workers for RefuseAfterBinding {
        async fn ensure(&self, _: &str, _: &std::path::Path) -> Result<Option<Value>> {
            let mut s = self.0.lock().unwrap();
            s.1.push("ensure concurrent binding".into());
            concurrent_binding(&mut s);
            bail!("fixture allocation retry refused after concurrent binding")
        }
        async fn stop(&self, _: &str) -> Result<Option<Value>> {
            panic!("must not stop the concurrently bound worker")
        }
    }

    /// A BINDING RECORDED WHILE THE WORKER WAS ASKED FOR KEEPS ITS CLAIM
    /// (review e296014b R2, ported from
    /// `reviewer_refusal_preserves_concurrently_bound_building`). The
    /// snapshot read before `ensure` shows no binding; the hand-back's
    /// first write is judged against the claim's step and refused, so
    /// nothing is handed back and nothing is written.
    #[tokio::test]
    async fn a_refusal_keeps_a_claim_bound_while_its_worker_was_asked_for() {
        let (base, floor) = floor("active", Door::Grants).await;
        let refused = started_with_worker_at(
            &crate::sor_client::machine_client().unwrap(),
            &base,
            "own-run",
            "agent-codex",
            Some("own-run"),
            Some(std::path::Path::new("/source")),
            &RefuseAfterBinding(floor.clone()),
        )
        .await
        .unwrap_err();
        let refused = format!("{refused:#}");
        assert!(refused.contains("stays with"), "{refused}");
        let s = floor.lock().unwrap();
        assert_eq!(
            s.0["steps"][0]["status"], "active",
            "the new binding keeps its building claim"
        );
        assert_eq!(
            s.0["metadata"][crate::dispatch_worker::KEY]["session"],
            "dev-build-fedcba9876543210"
        );
        assert!(
            !s.1.iter().any(|v| v.starts_with("building put")),
            "{:?}",
            s.1
        );
        assert!(s.0["steps"][0]["metadata"].get(RETURNED_KEY).is_none());
    }

    /// THE GUARD IS AT THE WRITE, NOT A READ (review e296014b R2): the
    /// binding lands BETWEEN the hand-back's two writes, after every read
    /// it made. The status PUT carries the same judgement and is refused,
    /// so the bound claim stays ACTIVE.
    #[tokio::test]
    async fn a_binding_recorded_between_the_hand_back_writes_keeps_the_claim() {
        let (base, floor) = floor("active", Door::Grants).await;
        floor.lock().unwrap().0["_fixture"]["bind_after_returned"] = json!(true);
        let refused = format!("{:#}", start(&base, &floor, true).await.unwrap_err());
        assert!(refused.contains("stays with"), "{refused}");
        let s = floor.lock().unwrap();
        assert_eq!(
            s.1,
            vec![
                "ensure",
                "building metadata",
                "concurrent binding",
                "building put refused"
            ]
        );
        assert_eq!(s.0["steps"][0]["status"], "active");
        assert_eq!(
            s.0["metadata"][crate::dispatch_worker::KEY]["session"],
            "dev-build-fedcba9876543210"
        );
    }

    /// The other half of that exclusion: a hand-back that landed BEFORE
    /// this binder's write leaves the claim READY, and the binder's own
    /// read-back proves it, so the worker it was admitted is retired
    /// rather than acknowledged — R1's rule at the binder's read-back.
    #[tokio::test]
    async fn a_binding_after_its_claim_was_handed_back_retires_its_worker() {
        let (base, floor) = floor("ready", Door::Grants).await;
        floor.lock().unwrap().0["_fixture"]["hand_back_before_mirror"] = json!(true);
        let refused = format!("{:#}", start(&base, &floor, false).await.unwrap_err());
        assert!(refused.contains("RETIRED"), "{refused}");
        let s = floor.lock().unwrap();
        assert_eq!(
            s.1,
            vec![
                "claim",
                "ensure",
                "concurrent hand-back",
                "building metadata",
                "run metadata worker_session,worker_started",
                "stop",
                "run metadata worker_retirement"
            ]
        );
        assert_eq!(s.0["steps"][0]["status"], "ready");
        assert_eq!(
            s.0["metadata"][crate::dispatch_worker::RETIREMENT_KEY]["state"],
            "retired"
        );
    }

    // -- the car is admitted at build (backlog 68a8726e) ---------------
    //
    // The real jobs router with the real platform bundle
    // (`car_unland::tests::serve`): what admission, a gate launch, a red
    // and a green leave is read back off the packets.

    const ADMIN: &str = "emp-bootstrap-admin";
    const CAR_BRANCH: &str = "feat/the-car-is-admitted-at-build";

    fn admin_wire(base: &str) -> crate::steps::Wire {
        crate::steps::Wire::at(
            base.to_string(),
            Some(crate::identity::Caller {
                id: ADMIN.into(),
                source: crate::identity::Source::Env,
            }),
        )
        .unwrap()
    }

    fn at(s: &str) -> chrono::DateTime<chrono::Utc> {
        chrono::DateTime::parse_from_rfc3339(s)
            .unwrap()
            .with_timezone(&chrono::Utc)
    }

    /// A worktree whose HEAD names `branch`, in the shape a linked
    /// worktree has: `.git` a FILE pointing at its gitdir.
    fn worktree_on(tag: &str, branch: &str) -> std::path::PathBuf {
        let root = boss_testing::scratch::scratch_dir(tag);
        let gitdir = root.join("gitdir");
        std::fs::create_dir_all(&gitdir).unwrap();
        std::fs::write(gitdir.join("HEAD"), format!("ref: refs/heads/{branch}\n")).unwrap();
        let tree = root.join("tree");
        std::fs::create_dir_all(&tree).unwrap();
        std::fs::write(tree.join(".git"), format!("gitdir: {}\n", gitdir.display())).unwrap();
        tree
    }

    async fn open_cars_for(door: &crate::steps::Wire, branch: &str) -> Vec<Value> {
        let page = door
            .call(
                reqwest::Method::GET,
                "/api/jobs?kind=ship-a-change&status=open&full=true&limit=100",
                None,
            )
            .await
            .unwrap()
            .unwrap();
        page["data"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|c| c["metadata"]["branch"] == branch)
            .cloned()
            .collect()
    }

    fn status_of(car: &Value, slug: &str) -> String {
        boss_jobs::car::find_step(car, slug, "")
            .and_then(|s| s["status"].as_str())
            .unwrap_or("?")
            .to_string()
    }

    async fn write_step(
        door: &crate::steps::Wire,
        car: &str,
        step: &str,
        merge: Value,
        status: Value,
    ) {
        door.call(
            reqwest::Method::PATCH,
            &format!("/api/jobs/{car}/steps/{step}/metadata"),
            Some(merge),
        )
        .await
        .unwrap();
        door.call(
            reqwest::Method::PUT,
            &format!("/api/jobs/{car}/steps/{step}"),
            Some(status),
        )
        .await
        .unwrap();
    }

    #[test]
    fn a_worktree_branch_is_read_from_its_head_without_git() {
        let tree = worktree_on("car-admit-head", CAR_BRANCH);
        assert_eq!(worktree_branch(&tree).as_deref(), Some(CAR_BRANCH));
        let plain = boss_testing::scratch::scratch_dir("car-admit-plain");
        std::fs::create_dir_all(plain.join(".git")).unwrap();
        std::fs::write(plain.join(".git/HEAD"), "0123abcd\n").unwrap();
        assert_eq!(
            worktree_branch(&plain),
            None,
            "a detached head names no branch"
        );
    }

    /// A linked worktree in git's own shape: `.git` a FILE naming
    /// `<repo>/worktrees/<tag>`, whose `commondir` names the repository
    /// holding `config`, where `local` tracks `upstream` on `remote`.
    fn tracking_worktree(
        tag: &str,
        local: &str,
        remote: &str,
        upstream: &str,
    ) -> std::path::PathBuf {
        let root = boss_testing::scratch::scratch_dir(tag);
        let repo = root.join("repo");
        let gitdir = repo.join("worktrees").join("wt");
        std::fs::create_dir_all(&gitdir).unwrap();
        std::fs::write(gitdir.join("HEAD"), format!("ref: refs/heads/{local}\n")).unwrap();
        std::fs::write(gitdir.join("commondir"), "../..\n").unwrap();
        std::fs::write(
            repo.join("config"),
            format!(
                "[core]\n\tbare = false\n[branch \"other\"]\n\tremote = origin\n\tmerge = \
                 refs/heads/fix/not-mine\n[branch \"{local}\"]\n\tremote = {remote}\n\tmerge = \
                 refs/heads/{upstream}\n"
            ),
        )
        .unwrap();
        let tree = root.join("tree");
        std::fs::create_dir_all(&tree).unwrap();
        std::fs::write(tree.join(".git"), format!("gitdir: {}\n", gitdir.display())).unwrap();
        tree
    }

    /// Backlog d5bebca7: the candidate key is the FORGE branch the local
    /// one tracks, when that is a car branch on a remote — else none, and
    /// the local name stands.
    #[test]
    fn a_tracking_branch_keys_its_car_on_the_forge_branch_it_tracks() {
        let held = tracking_worktree("car-key-held", "fix/held-2", "origin", "fix/held");
        assert_eq!(
            worktree_tracked_car_branch(&held).as_deref(),
            Some("fix/held")
        );
        // Rule 1's fresh car tracks origin/main: main is no car.
        let fresh = tracking_worktree("car-key-fresh", "fix/new", "origin", "main");
        assert_eq!(worktree_tracked_car_branch(&fresh), None);
        // A local upstream names no forge branch.
        let local = tracking_worktree("car-key-local", "fix/a", ".", "fix/b");
        assert_eq!(worktree_tracked_car_branch(&local), None);
        // No upstream at all.
        let bare = worktree_on("car-key-bare", CAR_BRANCH);
        assert_eq!(worktree_tracked_car_branch(&bare), None);
        assert_eq!(
            tracked_car_branch("[branch \"x\"]\n\tremote = origin\n", "x"),
            None,
            "a remote without a merge ref tracks nothing"
        );
    }

    /// Backlog d5bebca7, the measured case: run e0a7200e repaired a car
    /// whose branch another worktree held, on a differently named local
    /// branch tracking it, and --started admitted twin ebddec00 beside
    /// 88645b17. Keyed on the upstream, the repair ADOPTS the one car.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_repair_on_a_renamed_tracking_branch_adopts_the_car_not_a_twin() {
        let base = crate::car_unland::tests::serve().await;
        let door = admin_wire(&base);
        let item = file_item(&door, "Held by another worktree").await;
        let owner = "88147400-0000-4000-8000-000000000007";
        let repair = "88147400-0000-4000-8000-000000000008";
        let run_of = |id: &str| json!({"id": id, "metadata": {"step": "build", "packet": item}});
        let http = crate::sor_client::machine_client().unwrap();
        let now = at("2026-11-10T22:00:00Z");
        let first = worktree_on("car-twin-owner", "fix/held");
        admit_run_car(&http, &base, ADMIN, &run_of(owner), &first, now)
            .await
            .unwrap();
        let second = tracking_worktree("car-twin-repair", "fix/held-2", "origin", "fix/held");
        let said = admit_run_car(&http, &base, ADMIN, &run_of(repair), &second, now)
            .await
            .unwrap()
            .unwrap();
        assert!(said.contains("adopted"), "{said}");
        let cars = every_open_car(&door).await;
        assert_eq!(cars.len(), 1, "no twin: {cars:#?}");
        assert_eq!(cars[0]["metadata"]["branch"], "fix/held");
        assert_eq!(cars[0]["metadata"]["agent_run"], repair);
    }

    /// Review dce2840b F1: git's default `branch.autoSetupMerge` makes a
    /// branch cut from `origin/fix/parent` TRACK fix/parent, so a stacked
    /// child carries its parent's car branch as its upstream. The upstream
    /// is the car's key only when the record says that car is THIS
    /// change's; the parent's car belongs to another item, so the child
    /// admits its own car on its own branch and adopts nothing.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_stacked_child_tracking_another_items_car_admits_its_own_car() {
        let base = crate::car_unland::tests::serve().await;
        let door = admin_wire(&base);
        let parent_item = file_item(&door, "The parent change").await;
        let child_item = file_item(&door, "The stacked child").await;
        let parent_run = "88147400-0000-4000-8000-000000000009";
        let child_run = "88147400-0000-4000-8000-00000000000a";
        let run_of =
            |id: &str, item: &str| json!({"id": id, "metadata": {"step": "build", "packet": item}});
        let http = crate::sor_client::machine_client().unwrap();
        let now = at("2026-11-11T06:00:00Z");
        let parent = worktree_on("car-stack-parent", "fix/parent");
        admit_run_car(
            &http,
            &base,
            ADMIN,
            &run_of(parent_run, &parent_item),
            &parent,
            now,
        )
        .await
        .unwrap();
        let child = tracking_worktree("car-stack-child", "fix/child", "origin", "fix/parent");
        let said = admit_run_car(
            &http,
            &base,
            ADMIN,
            &run_of(child_run, &child_item),
            &child,
            now,
        )
        .await
        .unwrap()
        .unwrap();
        assert!(!said.contains("adopted"), "{said}");
        let cars = every_open_car(&door).await;
        assert_eq!(cars.len(), 2, "each change its own car: {cars:#?}");
        let on = |b: &str| {
            cars.iter()
                .find(|c| c["metadata"]["branch"] == b)
                .unwrap_or_else(|| panic!("no car on {b}: {cars:#?}"))
        };
        assert_eq!(on("fix/child")["metadata"]["agent_run"], child_run);
        assert_eq!(
            on("fix/child")["metadata"]["backlog_item"],
            child_item.as_str()
        );
        assert_eq!(on("fix/parent")["metadata"]["agent_run"], parent_run);
    }

    /// Only a builder run on a backlog-item `build` step, on a car
    /// branch, admits a car — never the harness's own worktree branch.
    #[test]
    fn a_car_is_admitted_only_for_a_build_on_a_car_branch() {
        let run = json!({"metadata": {"step": "build", "packet": "i"}});
        let item = json!({"kind": "backlog-item"});
        assert_eq!(
            admission_branch(&run, &item, Some(CAR_BRANCH)).unwrap(),
            CAR_BRANCH
        );
        assert!(admission_branch(&run, &item, Some("fix/x")).is_ok());
        let e = admission_branch(&run, &item, Some("worktree-agent-ad97")).unwrap_err();
        assert!(e.contains("not a car branch"), "{e}");
        assert!(admission_branch(&run, &item, None).is_err());
        let triage = json!({"metadata": {"step": "triage", "packet": "i"}});
        assert!(admission_branch(&triage, &item, Some(CAR_BRANCH)).is_err());
        let design = json!({"kind": "design-doc"});
        assert!(admission_branch(&run, &design, Some(CAR_BRANCH)).is_err());
    }

    /// THE ONE-CAR INVARIANT, ACROSS THE WHOLE PATH (backlog 68a8726e).
    /// A dispatched build leaves exactly one open ship-a-change for its
    /// branch, AT BUILD; a second `--started` adopts it; a gate launch
    /// puts it AT GATE; a red hands the gate back (the car waits at its
    /// gate, ready — `build` completed at launch and a completed step
    /// never moves backwards); a relaunch takes it again; and the green's
    /// own writes (`car::finish_writes`, the auto-park adopt path) leave
    /// that same car AT REVIEW. One car at every step.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_dispatched_build_is_one_car_from_build_through_gate_to_review() {
        let base = crate::car_unland::tests::serve().await;
        let door = admin_wire(&base);
        let item = door
            .call(
                reqwest::Method::POST,
                "/api/jobs",
                Some(json!({
                    "kind": "backlog-item",
                    "title": "The car is admitted at build",
                    "subject": {"subject_kind": "custom", "id": "station-flow"},
                    "owner_id": ADMIN,
                    "priority": "urgent",
                    "status": "open",
                    "tags": [],
                    "metadata": {},
                })),
            )
            .await
            .unwrap()
            .unwrap();
        let item_id = item["id"].as_str().unwrap().to_string();
        let run_id = "88147400-0000-4000-8000-000000000001";
        let run = json!({"id": run_id, "metadata": {"step": "build", "packet": item_id}});
        let tree = worktree_on("car-admit-e2e", CAR_BRANCH);
        let http = crate::sor_client::machine_client().unwrap();
        let now = at("2026-11-08T23:00:00Z");

        // ADMITTED AT BUILD.
        let said = admit_run_car(&http, &base, ADMIN, &run, &tree, now)
            .await
            .unwrap()
            .unwrap();
        assert!(said.contains("admitted at build"), "{said}");
        let cars = open_cars_for(&door, CAR_BRANCH).await;
        assert_eq!(cars.len(), 1, "exactly one car: {cars:#?}");
        let car = &cars[0];
        let car_id = car["id"].as_str().unwrap().to_string();
        assert_eq!(status_of(car, "scope"), "completed");
        assert_eq!(status_of(car, "build"), "active", "{car:#}");
        assert_eq!(status_of(car, "gate"), "pending");
        assert_eq!(car["metadata"]["agent_run"], run_id);
        assert_eq!(car["metadata"]["backlog_item"], item_id.as_str());
        assert_eq!(
            car["metadata"][boss_jobs::car::ADMITTED_BY],
            "boss dispatch --started"
        );
        assert_eq!(car["title"], "The car is admitted at build");

        // A SECOND START ADOPTS — no twin.
        let again = admit_run_car(&http, &base, ADMIN, &run, &tree, now)
            .await
            .unwrap()
            .unwrap();
        assert!(again.contains("adopted"), "{again}");
        assert_eq!(open_cars_for(&door, CAR_BRANCH).await.len(), 1);

        // A GATE LAUNCH PUTS IT AT GATE.
        let gate_run = "3c6b2f0e-0000-4000-8000-000000000001";
        let moved = crate::car::into_gate(
            &door,
            CAR_BRANCH,
            gate_run,
            "0123456789abcdef",
            "9 tests",
            Some(run_id),
            Some(item_id.as_str()),
            now,
        )
        .await
        .unwrap();
        assert_eq!(moved.as_deref(), Some(car_id.as_str()));
        let cars = open_cars_for(&door, CAR_BRANCH).await;
        assert_eq!(cars.len(), 1);
        assert_eq!(status_of(&cars[0], "build"), "completed");
        assert_eq!(status_of(&cars[0], "gate"), "active", "{:#}", cars[0]);
        let gate = boss_jobs::car::find_step(&cars[0], "gate", "").unwrap();
        assert_eq!(gate["metadata"][boss_jobs::car::GATE_RUN], gate_run);

        // A RED HANDS THE GATE BACK — the auto-park handler's writes.
        let back = boss_jobs::car::gate_return(&cars[0], gate_run, "failed");
        assert_eq!(back.len(), 2, "held by the gate-run");
        let step = boss_jobs::car::step_id_for(&cars[0], "gate", "").unwrap();
        write_step(
            &door,
            &car_id,
            &step,
            back[0].body.clone(),
            back[1].body.clone(),
        )
        .await;
        let cars = open_cars_for(&door, CAR_BRANCH).await;
        assert_eq!(cars.len(), 1);
        assert_eq!(status_of(&cars[0], "gate"), "ready", "{:#}", cars[0]);
        let gate = boss_jobs::car::find_step(&cars[0], "gate", "").unwrap();
        assert_eq!(
            gate["metadata"][boss_jobs::car::LAST_VERDICT]["verdict"],
            "failed"
        );
        // ...and a repair's start on the same branch adopts it.
        let repair = admit_run_car(&http, &base, ADMIN, &run, &tree, now)
            .await
            .unwrap()
            .unwrap();
        assert!(repair.contains("adopted"), "{repair}");

        // THE RELAUNCH TAKES THE GATE AGAIN.
        let second = "3c6b2f0e-0000-4000-8000-000000000002";
        crate::car::into_gate(
            &door,
            CAR_BRANCH,
            second,
            "fedcba",
            "9 tests",
            Some(run_id),
            Some(item_id.as_str()),
            now,
        )
        .await
        .unwrap();
        let cars = open_cars_for(&door, CAR_BRANCH).await;
        assert_eq!(status_of(&cars[0], "gate"), "active");

        // THE GREEN: the auto-park handler decides Adopt for this shape
        // (pinned in its own crate) and writes `car::finish_writes`.
        let full = door
            .call(reqwest::Method::GET, &format!("/api/jobs/{car_id}"), None)
            .await
            .unwrap()
            .unwrap();
        let receipt = boss_jobs::car::Receipt {
            raw: r#"{"verdict":"green","head":"fedcba","mode":"auto"}"#.into(),
            head: "fedcba".into(),
            mode: "auto".into(),
        };
        let writes =
            boss_jobs::car::finish_writes(&full, "s", "e", "9 tests", "seen", &receipt, now)
                .unwrap();
        assert_eq!(writes.len(), 1, "only the gate is left to finish");
        for w in writes {
            write_step(&door, &car_id, &w.step_id, w.metadata, w.status_body).await;
        }
        let cars = open_cars_for(&door, CAR_BRANCH).await;
        assert_eq!(cars.len(), 1, "one car at review, no twin");
        assert_eq!(status_of(&cars[0], "gate"), "completed");
        assert_eq!(status_of(&cars[0], "review"), "ready");
    }

    /// A RENAMED BRANCH TAKES ITS RUN'S CAR WITH IT (the twin 4547c8bc):
    /// the launch re-points the admitted car instead of leaving it at
    /// build for a green to file a second beside.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_launch_on_a_renamed_branch_moves_the_runs_own_car() {
        let base = crate::car_unland::tests::serve().await;
        let door = admin_wire(&base);
        let item = door
            .call(
                reqwest::Method::POST,
                "/api/jobs",
                Some(json!({
                    "kind": "backlog-item", "title": "Renamed", "owner_id": ADMIN,
                    "subject": {"subject_kind": "custom", "id": "station-flow"},
                    "priority": "standard", "status": "open", "tags": [], "metadata": {},
                })),
            )
            .await
            .unwrap()
            .unwrap();
        let run_id = "88147400-0000-4000-8000-000000000002";
        let run = json!({"id": run_id, "metadata": {"step": "build", "packet": item["id"]}});
        let tree = worktree_on("car-admit-rename", "feat/first-name");
        let http = crate::sor_client::machine_client().unwrap();
        let now = at("2026-11-08T23:00:00Z");
        admit_run_car(&http, &base, ADMIN, &run, &tree, now)
            .await
            .unwrap();
        crate::car::into_gate(
            &door,
            "feat/second-name",
            "gr",
            "abc",
            "t",
            Some(run_id),
            item["id"].as_str(),
            now,
        )
        .await
        .unwrap()
        .expect("the run's car is moved");
        assert!(open_cars_for(&door, "feat/first-name").await.is_empty());
        let cars = open_cars_for(&door, "feat/second-name").await;
        assert_eq!(cars.len(), 1);
        assert_eq!(
            cars[0]["metadata"]["branch_renamed_from"],
            "feat/first-name"
        );
        assert_eq!(status_of(&cars[0], "gate"), "active");
        // Another run's launch moves nothing.
        assert!(
            crate::car::into_gate(
                &door,
                "feat/third",
                "gr2",
                "abc",
                "t",
                Some("other"),
                None,
                now
            )
            .await
            .unwrap()
            .is_none()
        );
    }

    async fn file_item(door: &crate::steps::Wire, title: &str) -> String {
        let item = door
            .call(
                reqwest::Method::POST,
                "/api/jobs",
                Some(json!({
                    "kind": "backlog-item", "title": title, "owner_id": ADMIN,
                    "subject": {"subject_kind": "custom", "id": "station-flow"},
                    "priority": "standard", "status": "open", "tags": [], "metadata": {},
                })),
            )
            .await
            .unwrap()
            .unwrap();
        item["id"].as_str().unwrap().to_string()
    }

    async fn every_open_car(door: &crate::steps::Wire) -> Vec<Value> {
        let page = door
            .call(
                reqwest::Method::GET,
                "/api/jobs?kind=ship-a-change&status=open&full=true&limit=100",
                None,
            )
            .await
            .unwrap()
            .unwrap();
        page["data"].as_array().unwrap().clone()
    }

    /// R1 (Codex review of car 0d04ee3e): the SAME run starts on feat/a,
    /// its builder renames the branch to feat/b and runs --started again.
    /// The run's car is found by the run, re-pointed — no second car —
    /// and a gate on feat/b moves that one car to its gate.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_renamed_start_of_the_same_run_files_no_twin_and_its_gate_moves_the_one_car() {
        let base = crate::car_unland::tests::serve().await;
        let door = admin_wire(&base);
        let item_id = file_item(&door, "Renamed twice").await;
        let run_id = "88147400-0000-4000-8000-000000000003";
        let run = json!({"id": run_id, "metadata": {"step": "build", "packet": item_id}});
        let tree = worktree_on("car-admit-restart", "feat/a");
        let http = crate::sor_client::machine_client().unwrap();
        let now = at("2026-11-08T23:00:00Z");
        let first = admit_run_car(&http, &base, ADMIN, &run, &tree, now)
            .await
            .unwrap()
            .unwrap();
        assert!(first.contains("admitted at build"), "{first}");

        // The builder renames its branch; HEAD now names feat/b.
        let head = tree.parent().unwrap().join("gitdir/HEAD");
        std::fs::write(&head, "ref: refs/heads/feat/b\n").unwrap();
        let again = admit_run_car(&http, &base, ADMIN, &run, &tree, now)
            .await
            .unwrap()
            .unwrap();
        assert!(
            again.contains("re-pointed from feat/a to feat/b"),
            "{again}"
        );
        let cars = every_open_car(&door).await;
        assert_eq!(cars.len(), 1, "one car for one run, not a twin: {cars:#?}");
        assert_eq!(cars[0]["metadata"]["branch"], "feat/b");
        assert_eq!(cars[0]["metadata"]["agent_run"], run_id);

        let moved = crate::car::into_gate(
            &door,
            "feat/b",
            "3c6b2f0e-0000-4000-8000-000000000003",
            "abcdef0",
            "t",
            Some(run_id),
            Some(item_id.as_str()),
            now,
        )
        .await
        .unwrap();
        assert_eq!(moved.as_deref(), cars[0]["id"].as_str());
        let cars = every_open_car(&door).await;
        assert_eq!(cars.len(), 1);
        assert_eq!(status_of(&cars[0], "build"), "completed");
        assert_eq!(status_of(&cars[0], "gate"), "active");
    }

    /// R1: a building car on the branch that ANOTHER run of ANOTHER change
    /// admitted is refused by admission and by the gate launch — never
    /// adopted, never stamped. A repair (another run of the SAME change)
    /// adopts it and takes it as its own.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_building_car_another_change_owns_is_refused_not_stamped() {
        let base = crate::car_unland::tests::serve().await;
        let door = admin_wire(&base);
        let mine = file_item(&door, "Mine").await;
        let theirs = file_item(&door, "Theirs").await;
        let owner = "88147400-0000-4000-8000-000000000004";
        let stranger = "88147400-0000-4000-8000-000000000005";
        let repair = "88147400-0000-4000-8000-000000000006";
        let tree = worktree_on("car-admit-foreign", "feat/shared");
        let http = crate::sor_client::machine_client().unwrap();
        let now = at("2026-11-08T23:00:00Z");
        let run_of =
            |id: &str, item: &str| json!({"id": id, "metadata": {"step": "build", "packet": item}});
        admit_run_car(&http, &base, ADMIN, &run_of(owner, &mine), &tree, now)
            .await
            .unwrap();

        let refused = admit_run_car(&http, &base, ADMIN, &run_of(stranger, &theirs), &tree, now)
            .await
            .unwrap_err();
        assert!(
            format!("{refused:#}").contains("another run of another change"),
            "{refused:#}"
        );
        let gate = crate::car::into_gate(
            &door,
            "feat/shared",
            "3c6b2f0e-0000-4000-8000-000000000004",
            "abcdef0",
            "t",
            Some(stranger),
            Some(theirs.as_str()),
            now,
        )
        .await
        .unwrap_err();
        assert!(format!("{gate:#}").contains("was not moved"), "{gate:#}");
        let cars = every_open_car(&door).await;
        assert_eq!(cars.len(), 1, "{cars:#?}");
        assert_eq!(status_of(&cars[0], "build"), "active", "not stamped");
        assert_eq!(status_of(&cars[0], "gate"), "pending", "not stamped");
        assert_eq!(cars[0]["metadata"]["agent_run"], owner);

        let adopted = admit_run_car(&http, &base, ADMIN, &run_of(repair, &mine), &tree, now)
            .await
            .unwrap()
            .unwrap();
        assert!(adopted.contains("adopted"), "{adopted}");
        let cars = every_open_car(&door).await;
        assert_eq!(cars.len(), 1);
        assert_eq!(
            cars[0]["metadata"]["agent_run"], repair,
            "the repair's own now"
        );
    }

    /// THE DOCK-RED REPAIR TAKES ITS PARKED CAR (backlog 5d0758dc).
    /// Measured 2026-11-11: repair run 2a9293b8 was dispatched onto
    /// garaged car 4b740c44 (f76b7661), and for the whole window the dock
    /// replayed it, the car's `agent_run` named 504b4863, a run whose
    /// building had completed the evening before. A parked car is not a
    /// building car, so the branch adoption never reached it; the run's
    /// own `repair` record names it, and `--started` stamps it, so the
    /// dock can read that a builder has the car. A run of the same item
    /// with no such record takes nothing from a parked car, as before.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_dock_red_repair_takes_its_parked_car_at_started() {
        let base = crate::car_unland::tests::serve().await;
        let door = admin_wire(&base);
        let item = file_item(&door, "Garaged at the dock").await;
        let owner = "88147400-0000-4000-8000-000000000021";
        let other = "88147400-0000-4000-8000-000000000022";
        let repair = "88147400-0000-4000-8000-000000000023";
        let branch = "fix/garaged-at-the-dock";
        let tree = worktree_on("car-dock-red-repair", branch);
        let http = crate::sor_client::machine_client().unwrap();
        let now = at("2026-11-11T11:57:54Z");
        let run_of = |id: &str| json!({"id": id, "metadata": {"step": "build", "packet": item}});
        admit_run_car(&http, &base, ADMIN, &run_of(owner), &tree, now)
            .await
            .unwrap();
        // Gated green and parked at review: the auto-park handler's writes.
        let gate_run = "3c6b2f0e-0000-4000-8000-000000000021";
        crate::car::into_gate(
            &door,
            branch,
            gate_run,
            "fedcba",
            "t",
            Some(owner),
            Some(item.as_str()),
            now,
        )
        .await
        .unwrap();
        let car_id = open_cars_for(&door, branch).await[0]["id"]
            .as_str()
            .unwrap()
            .to_string();
        let full = door
            .call(reqwest::Method::GET, &format!("/api/jobs/{car_id}"), None)
            .await
            .unwrap()
            .unwrap();
        let receipt = boss_jobs::car::Receipt {
            raw: r#"{"verdict":"green","head":"fedcba","mode":"auto"}"#.into(),
            head: "fedcba".into(),
            mode: "auto".into(),
        };
        for w in boss_jobs::car::finish_writes(&full, "s", "e", "t", "seen", &receipt, now).unwrap()
        {
            write_step(&door, &car_id, &w.step_id, w.metadata, w.status_body).await;
        }
        let cars = open_cars_for(&door, branch).await;
        assert_eq!(status_of(&cars[0], "review"), "ready", "parked");

        // Control: another run of the item, no repair record — untouched.
        admit_run_car(&http, &base, ADMIN, &run_of(other), &tree, now)
            .await
            .unwrap();
        let cars = open_cars_for(&door, branch).await;
        assert_eq!(cars[0]["metadata"]["agent_run"], owner, "{:#}", cars[0]);

        // The dock-red repair, as `boss dispatch` records it on the run.
        let mut run = run_of(repair);
        run["metadata"]["repair"] = json!({"car": car_id, "branch": branch, "head": "fedcba",
                                          "origin": "dock-red", "gate_run": gate_run});
        admit_run_car(&http, &base, ADMIN, &run, &tree, now)
            .await
            .unwrap();
        let cars = open_cars_for(&door, branch).await;
        assert_eq!(cars.len(), 1, "no twin: {cars:#?}");
        assert_eq!(
            cars[0]["metadata"][boss_jobs::agent_runs::EDGE_KEY],
            repair,
            "the dock reads the repair on the car"
        );
        assert_eq!(status_of(&cars[0], "review"), "ready", "still parked");
    }

    // -- a car whose run or item ends before any gate (backlog 366c9fb1)
    //
    // The gate launch and the ending share ONE fence, the gate step's
    // `ended_before_gate` against its `gate_run` (Codex review of car
    // 0d04ee3e, R4b), and a claimed ending is finished by whoever finds
    // it (R4a). Both orders, through the real router.

    async fn send_judged(
        door: &crate::steps::Wire,
        w: &boss_jobs::car::JudgedWrite,
    ) -> crate::car_retire::Judged {
        let method = reqwest::Method::from_bytes(w.method.as_bytes()).unwrap();
        match door
            .call_expecting(method, &w.path, Some(w.body.clone()), &w.expect)
            .await
        {
            Ok(_) => crate::car_retire::Judged::Written,
            Err(e) if crate::dispatch::lost_the_step_race(&e) => crate::car_retire::Judged::Moved,
            Err(e) => panic!("{e:#}"),
        }
    }

    async fn car_by_id(door: &crate::steps::Wire, id: &str) -> Value {
        let c = door
            .call(reqwest::Method::GET, &format!("/api/jobs/{id}"), None)
            .await
            .unwrap()
            .unwrap();
        c.get("data").cloned().unwrap_or(c)
    }

    /// One admitted car at build for a fresh item and run, on `branch`.
    async fn admitted_on(
        base: &str,
        door: &crate::steps::Wire,
        run_id: &str,
        branch: &str,
        tag: &str,
    ) -> (String, Value) {
        let item_id = file_item(door, "Ends before any gate").await;
        let run = json!({"id": run_id, "metadata": {"step": "build", "packet": item_id}});
        let tree = worktree_on(tag, branch);
        let http = crate::sor_client::machine_client().unwrap();
        admit_run_car(&http, base, ADMIN, &run, &tree, at("2026-11-09T08:00:00Z"))
            .await
            .unwrap();
        let cars = open_cars_for(door, branch).await;
        assert_eq!(cars.len(), 1, "{cars:#?}");
        (item_id, cars[0].clone())
    }

    /// THE ENDING WINS. A launch read the car unlaunched; the item's
    /// decline then claimed it. The launch's first write — the stamp —
    /// is refused, so nothing of the launch lands: `build` is never
    /// handed in, the gate never stamped. The launch verb on the claimed
    /// car finishes the ending instead of moving it.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_launch_that_read_before_an_ending_claimed_the_car_moves_nothing() {
        let base = crate::car_unland::tests::serve().await;
        let door = admin_wire(&base);
        let run_id = "88147400-0000-4000-8000-000000000011";
        let branch = "fix/ending-wins";
        let (item_id, car) = admitted_on(&base, &door, run_id, branch, "car-end-first").await;
        let id = car["id"].as_str().unwrap().to_string();
        let now = at("2026-11-09T08:00:00Z");
        let gr = "3c6b2f0e-0000-4000-8000-000000000011";

        // The launch's read, then the ending's claim.
        let plan = boss_jobs::car::gate_launch(&car, gr, "abc1234", "t", now)
            .unwrap()
            .unwrap();
        let claim = boss_jobs::car::ending_claim(
            &car,
            boss_jobs::car::Ended::Item {
                item: &item_id,
                outcome: "declined",
            },
            now,
        )
        .unwrap();
        assert_eq!(
            send_judged(&door, &claim).await,
            crate::car_retire::Judged::Written
        );
        // The launch's stamp, from its stale read: refused, nothing written.
        let stamp = boss_jobs::car::JudgedWrite {
            method: "PATCH",
            path: format!("/api/jobs/{id}/steps/{}/metadata", plan.gate_step),
            body: plan.gate_merge.clone(),
            expect: plan.fence.clone(),
        };
        assert_eq!(
            send_judged(&door, &stamp).await,
            crate::car_retire::Judged::Moved
        );

        // The launch verb itself, on the claimed car.
        let refused = crate::car::into_gate(
            &door,
            branch,
            gr,
            "abc1234",
            "t",
            Some(run_id),
            Some(item_id.as_str()),
            now,
        )
        .await
        .unwrap_err();
        assert!(
            format!("{refused:#}").contains("was ended before this gate"),
            "{refused:#}"
        );
        assert!(open_cars_for(&door, branch).await.is_empty());
        let ended = car_by_id(&door, &id).await;
        assert_ne!(ended["status"], "open", "{ended:#}");
        assert_eq!(status_of(&ended, "abandoned"), "completed");
        // The close skips what was still open: the build was never handed in.
        assert_eq!(status_of(&ended, "build"), "skipped", "never handed in");
        let gate = boss_jobs::car::find_step(&ended, "gate", "").unwrap();
        assert!(gate["metadata"].get(boss_jobs::car::GATE_RUN).is_none());
        assert!(
            ended["metadata"][boss_jobs::car::ABANDONED_WHY]
                .as_str()
                .unwrap()
                .contains(&item_id)
        );
    }

    /// THE LAUNCH WINS. The run's ending read the car unlaunched; a gate
    /// launch then moved it. The ending's claim is refused and the car
    /// stands at its gate, untouched.
    #[tokio::test(flavor = "multi_thread")]
    async fn an_ending_that_read_before_a_launch_ends_nothing() {
        let base = crate::car_unland::tests::serve().await;
        let door = admin_wire(&base);
        let run_id = "88147400-0000-4000-8000-000000000012";
        let branch = "fix/launch-wins";
        let (item_id, car) = admitted_on(&base, &door, run_id, branch, "car-launch-first").await;
        let now = at("2026-11-09T08:00:00Z");
        let claim = boss_jobs::car::ending_claim(
            &car,
            boss_jobs::car::Ended::Run {
                run: run_id,
                outcome: "died",
            },
            now,
        )
        .unwrap();
        let moved = crate::car::into_gate(
            &door,
            branch,
            "3c6b2f0e-0000-4000-8000-000000000012",
            "abc1234",
            "t",
            Some(run_id),
            Some(item_id.as_str()),
            now,
        )
        .await
        .unwrap();
        assert_eq!(moved.as_deref(), car["id"].as_str());
        assert_eq!(
            send_judged(&door, &claim).await,
            crate::car_retire::Judged::Moved
        );
        let cars = open_cars_for(&door, branch).await;
        assert_eq!(cars.len(), 1);
        assert_eq!(status_of(&cars[0], "build"), "completed");
        assert_eq!(status_of(&cars[0], "gate"), "active");
        assert_eq!(status_of(&cars[0], "abandoned"), "pending");
        assert!(boss_jobs::car::ending_of(&cars[0]).is_none());
    }

    /// R4a, AT THE REPAIR'S DOOR. The run's ending claimed the car and
    /// wrote its evidence, then its writer died. A repair run of the same
    /// change does not find a car it can neither adopt nor replace: it
    /// finishes that ending, word for word, and admits its own car.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_repair_finishes_an_ending_left_part_written_and_admits_its_own_car() {
        let base = crate::car_unland::tests::serve().await;
        let door = admin_wire(&base);
        let first = "88147400-0000-4000-8000-000000000013";
        let repair = "88147400-0000-4000-8000-000000000014";
        let branch = "fix/repair-after-a-failed-ending";
        let (item_id, car) = admitted_on(&base, &door, first, branch, "car-end-repair").await;
        let id = car["id"].as_str().unwrap().to_string();
        let claim = boss_jobs::car::ending_claim(
            &car,
            boss_jobs::car::Ended::Run {
                run: first,
                outcome: "refused",
            },
            at("2026-11-09T08:00:00Z"),
        )
        .unwrap();
        assert_eq!(
            send_judged(&door, &claim).await,
            crate::car_retire::Judged::Written
        );
        let claimed = car_by_id(&door, &id).await;
        let owed = boss_jobs::car::ending_rest(&claimed).unwrap();
        assert_eq!(owed.len(), 3);
        door.call(
            reqwest::Method::PATCH,
            &owed[0].path,
            Some(owed[0].body.clone()),
        )
        .await
        .unwrap();
        // ...and the writer died. The car stands claimed, open, at build.
        assert_eq!(open_cars_for(&door, branch).await.len(), 1);

        let run = json!({"id": repair, "metadata": {"step": "build", "packet": item_id}});
        let tree = worktree_on("car-end-repair-2", branch);
        let http = crate::sor_client::machine_client().unwrap();
        let said = admit_run_car(&http, &base, ADMIN, &run, &tree, at("2026-11-09T09:00:00Z"))
            .await
            .unwrap()
            .unwrap();
        assert!(said.contains("admitted at build"), "{said}");
        let ended = car_by_id(&door, &id).await;
        assert_eq!(status_of(&ended, "abandoned"), "completed");
        assert!(
            ended["metadata"][boss_jobs::car::ABANDONED_WHY]
                .as_str()
                .unwrap()
                .contains(first),
            "the claim's words, not the repair's"
        );
        let cars = open_cars_for(&door, branch).await;
        assert_eq!(cars.len(), 1, "{cars:#?}");
        assert_ne!(cars[0]["id"], id.as_str());
        assert_eq!(cars[0]["metadata"]["agent_run"], repair);
    }

    // THE STRICT DOOR (review e042d952 R1, backlog ad486420). The floor
    // above stubs the jobs API, and its PUT accepted a bare exit, so the
    // hand-back that production refuses 409 read as handed back. These
    // drive the REAL step routes — claim, metadata merge, step PUT with
    // its resource exit rule — through the in-memory router, with a pool
    // the claim really draws from. Measured on 2026-11-08: three runs
    // (05513138, 64e5feea, cd5cba71) were left ACTIVE with a billed claim
    // and no worker until the conductor freed each by hand with its id.

    struct Strict {
        base: String,
        jobs: std::sync::Arc<boss_jobs::InMemoryJobs>,
        run: boss_core::job::JobId,
        building: boss_core::job::StepId,
        stamp: boss_core::publisher::EventStamp,
    }

    /// One `vcpu` unit, and a run whose `building` bills it, READY and
    /// nominated to `agent-codex`.
    async fn strict_door() -> Strict {
        use boss_core::job::{Job, Priority, Step, StepStatus, Subject};
        use boss_jobs::JobsRepository;
        use boss_jobs::WorkflowRegistry;
        use boss_jobs::pools::PoolRegistry;
        use boss_policy_client::{Action, FakePolicyClient, PolicyClient, Resource, Scope};
        use std::sync::Arc;
        let stamp = boss_core::publisher::EventStamp::new(
            "jobs",
            boss_core::actor::ActorId::automation("strict-door"),
        );
        let workflows = Arc::new(boss_jobs::InMemoryWorkflows::for_fixture());
        let spec: boss_jobs::registry::WorkflowSpec = serde_json::from_value(json!({
            "kind":"agent-run","label":"Run","category":"test","version":1,"status":"active",
            "subject_kinds":["custom"],"owning_team":"platform","created_at":stamp.timestamp,
            "steps":[
                {"title":"building","kind":"task","duration_hours":1,"ready_when":"true",
                 "resources":{"vcpu":"1"}},
                {"title":"done","kind":"outcome","ready_when":"steps.building.done",
                 "terminal":{"outcome":"done"}}]}))
        .unwrap();
        workflows
            .publish_authored(
                spec,
                boss_core::job::JobId::new(),
                stamp.actor(),
                stamp.timestamp,
            )
            .await
            .unwrap();
        let pools = Arc::new(boss_jobs::pools::InMemoryPools::default());
        pools
            .publish_declared(
                serde_json::from_value(json!({"name":"vcpu","version":1,"status":"active",
                    "unit":"vCPU","kind":"flow","capacity":{"state":"configured","amount":"1"},
                    "created_at":stamp.timestamp}))
                .unwrap(),
                stamp.actor(),
                stamp.timestamp,
            )
            .await
            .unwrap();
        let jobs = Arc::new(
            boss_jobs::InMemoryJobs::new()
                .with_workflows(workflows.clone())
                .with_resource_pools(pools),
        );
        let mut job = Job::new(
            "agent-run",
            Subject::new("custom", "strict-door"),
            "A run the worker door refuses",
            "emp-owner",
            Priority::Standard,
            stamp.timestamp.date_naive(),
        );
        job.status = boss_core::job::JobStatus::Open;
        jobs.create_job_at(
            &job,
            stamp.timestamp,
            &[stamp.event(boss_jobs::events::JOB_CREATED, json!(job))],
        )
        .await
        .unwrap();
        let mut step = Step::new(job.id, "task", "building", 1);
        step.spec_slug = Some("building".into());
        step.status = StepStatus::Ready;
        step.assignee_id = Some("agent-codex".into());
        step.metadata = json!({EXPECTATION_KEY: {"schema":1,"minutes":15}});
        jobs.add_step_at(
            &step,
            stamp.timestamp,
            &[stamp.event(boss_jobs::events::STEP_CREATED, json!(step))],
        )
        .await
        .unwrap();
        let mut policy = FakePolicyClient::builder();
        for (action, resource) in [
            (Action::Read, Resource::job()),
            (Action::Update, Resource::job()),
            (Action::Read, Resource::step()),
            (Action::Update, Resource::step()),
        ] {
            policy = policy.allow("platform-admin", action, resource, Scope::All);
        }
        let policy: Arc<dyn PolicyClient> = Arc::new(policy.build());
        let bus = boss_testing::RecordingEventBus::new();
        let bus_dyn: Arc<dyn boss_core::port::EventBus> = bus.clone();
        let app = boss_jobs::http::router(boss_jobs::http::JobsApiState {
            kind_registry: Some(workflows as Arc<dyn WorkflowRegistry>),
            ..boss_jobs::http::JobsApiState::minimal(
                jobs.clone(),
                bus,
                boss_core::publisher::DomainPublisher::new(bus_dyn, "jobs"),
                policy,
                Arc::new(boss_clock_client::WallClockClient),
            )
        })
        .route(
            "/api/agents",
            axum::routing::get(|| async {
                axum::Json(json!({"data":[{"id":"agent-codex","aliases":[]}],"total":1}))
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        Strict {
            base: format!("http://{address}"),
            jobs,
            run: job.id,
            building: step.id,
            stamp,
        }
    }

    /// The worker door's refusal, against the strict door.
    async fn refused_start(door: &Strict) -> String {
        let run = door.run.to_string();
        let floor: Floor = std::sync::Arc::new(std::sync::Mutex::new((json!({}), Vec::new())));
        let refused = started_with_worker_at(
            &crate::sor_client::machine_client().unwrap(),
            &door.base,
            &run,
            "agent-codex",
            Some(&run),
            Some(std::path::Path::new("/source")),
            &Controller {
                refuses: true,
                floor,
            },
        )
        .await
        .unwrap_err();
        format!("{refused:#}")
    }

    /// Is the one `vcpu` unit free? A second claim of the same bill is
    /// the measurement: it is granted only when nothing holds the unit.
    async fn the_unit_is_free(door: &Strict) -> bool {
        use boss_jobs::JobsRepository;
        let mut other = boss_core::job::Step::new(door.run, "task", "building", 2);
        other.spec_slug = Some("building".into());
        other.status = boss_core::job::StepStatus::Ready;
        door.jobs
            .add_step_at(
                &other,
                door.stamp.timestamp,
                &[door
                    .stamp
                    .event(boss_jobs::events::STEP_CREATED, json!(other))],
            )
            .await
            .unwrap();
        door.jobs
            .claim_step_displacing_at(&other.id, "emp-probe", &[], &door.stamp, &[])
            .await
            .is_ok()
    }

    /// A WORKER REFUSAL HANDS A BILLED CLAIM BACK THROUGH THE REAL EXIT
    /// RULE: the claim --started acquired is named on the hand-back PUT,
    /// so the step reads READY, still nominated, unclaimed, and the unit
    /// it drew is free again. RED at 49cc85d2: the PUT named no claim,
    /// the door answered 409, and the step stayed ACTIVE holding the unit.
    #[tokio::test]
    async fn a_worker_refusal_returns_the_billed_claim_through_the_strict_exit() {
        use boss_jobs::JobsRepository;
        let door = strict_door().await;
        let refused = refused_start(&door).await;
        assert!(refused.contains("HANDED BACK"), "{refused}");
        let step = door.jobs.get_step(&door.building).await.unwrap().unwrap();
        assert_eq!(step.status, boss_core::job::StepStatus::Ready);
        assert_eq!(step.assignee_id.as_deref(), Some("agent-codex"));
        assert!(
            step.metadata[RETURNED_KEY]["why"]
                .as_str()
                .is_some_and(|why| why.contains("Longhorn can place 3 of 100Gi")),
            "{}",
            step.metadata
        );
        assert!(
            the_unit_is_free(&door).await,
            "the pool's in-use is back where it was"
        );
    }

    /// THE HAND-BACK NAMES THE ACQUIRED CLAIM, NOT A FRESH READ: a
    /// building `boss dispatch` claimed before it printed, re-acquired by
    /// its holder before --started (the door hands the same generation
    /// back), is still handed back by the id the executor read.
    #[tokio::test]
    async fn a_strict_hand_back_after_a_reacquisition_still_names_the_acquired_claim() {
        use boss_jobs::JobsRepository;
        let door = strict_door().await;
        let first = door
            .jobs
            .claim_step_displacing_at(&door.building, "agent-codex", &[], &door.stamp, &[])
            .await
            .unwrap();
        let again = door
            .jobs
            .claim_step_displacing_at(&door.building, "agent-codex", &[], &door.stamp, &[])
            .await
            .unwrap();
        assert!(boss_jobs::flow_resources::held_claim_id(&first.metadata).is_some());
        assert_eq!(
            boss_jobs::flow_resources::held_claim_id(&first.metadata),
            boss_jobs::flow_resources::held_claim_id(&again.metadata),
        );
        assert!(!the_unit_is_free(&door).await, "the claim holds the unit");
        let refused = refused_start(&door).await;
        assert!(refused.contains("HANDED BACK"), "{refused}");
        let step = door.jobs.get_step(&door.building).await.unwrap().unwrap();
        assert_eq!(step.status, boss_core::job::StepStatus::Ready);
        assert!(the_unit_is_free(&door).await);
    }
}
