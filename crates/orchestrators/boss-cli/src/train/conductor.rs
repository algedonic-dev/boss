//! The conductor.

use super::*;
use std::collections::HashSet;

// ---------------------------------------------------------------------------
// The conductor
// ---------------------------------------------------------------------------

pub(super) struct Conductor {
    pub(super) cfg: Config,
    http: boss_core::machine_token::Client,
    forge: Box<dyn Forge>,
    /// Who the conductor's packets are filed to — the platform owner,
    /// read from the people registry through the port and cached for
    /// this process (backlog 3c23662d). Every alarm and the train's
    /// gate-run go through `owner_for_filing`; none names a person.
    owner: boss_people_client::ReqwestPlatformOwner,
    /// THE RULES THIS INVOCATION DECIDES BY — resolved once, from the
    /// registry, and threaded to every decision point below. Nothing in
    /// this file reaches for a policy constant any more; if a threshold
    /// appears in a decision here, it arrived on this field.
    policy: DeliveryPolicy,
    /// The cluster's gate Jobs and their pod logs, as a port: `kubectl`
    /// in production, a fake under test — a field rather than a call to
    /// `Kubectl` at the site, so the tests drive the SAME lines reconcile
    /// runs (`settle_gate_runs`) instead of a wiring that was dead code
    /// under `cfg!(test)` (review e0ecb2f6, F4/m22).
    cluster: Box<dyn GateCluster>,
}

/// The conductor as a car writer's door: `boss car unland`'s writer runs
/// under the merge-lost arm through the conductor's own blip-guarded
/// client, signed as the conductor (backlog f9256445).
#[async_trait]
impl crate::car_retire::Door for Conductor {
    async fn send(&self, method: Method, path: &str, body: Option<Value>) -> Result<Option<Value>> {
        self.api(method, path, body).await
    }
}

#[async_trait]
impl crate::review_verdict::ExecutorRecordReader for Conductor {
    async fn executor_read(&self, path: &str) -> Result<Option<Value>> {
        self.api(Method::GET, path, None).await
    }
    async fn executor_original(
        &self,
        job: uuid::Uuid,
        step: uuid::Uuid,
        key: &str,
    ) -> Result<Option<boss_jobs::first_record::FirstRecord>> {
        if job.is_nil()
            || step.is_nil()
            || key.is_empty()
            || !key
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
        {
            bail!("invalid immutable record resource");
        }
        let path = format!("/api/jobs/{job}/steps/{step}/records/{key}");
        let raw = retrying(
            &JOBS_API_RETRY,
            &Method::GET,
            &path,
            self.policy.blip_cause_budget,
            &|message| log(message),
            || async {
                match self.api_once(Method::GET, &path, None).await {
                    Err(failure) if failure.kind == Failure::Http(404) => Ok(None),
                    Ok(None) => Err(ApiFailure {
                        kind: Failure::Malformed,
                        cause: anyhow!("the immutable record response is empty"),
                    }),
                    other => other,
                }
            },
        )
        .await?;
        let Some(raw) = raw else {
            return Ok(None);
        };
        let record =
            serde_json::from_value(raw).context("the immutable record response is malformed")?;
        crate::review_verdict::immutable_record_resource(&record, job, step, key)?;
        Ok(Some(record))
    }
}

/// What one walk of the dock found (`Conductor::candidates`).
pub(super) struct DockPass {
    /// Cars that may board now, with their branch.
    pub(super) boardable: Vec<(Value, String)>,
    /// The left-behind record for every car held this pass.
    pub(super) left_behind: Vec<Value>,
    /// The ids of the cars the DOCK held this walk — a red or in-flight
    /// re-gate, a stale receipt, a missing branch; not a struck car, not
    /// a declared ordering edge — what a departure counts as left behind
    /// (backlog 2fccbfd6, `Conductor::count_left_behind`).
    pub(super) held: Vec<String>,
    /// The head the dock JUDGED each boardable car at, by job id — the
    /// sha assembly merges, so what rides is what was judged, not what
    /// the branch name resolves to later (backlog b7b02024, review F4).
    pub(super) judged: std::collections::HashMap<String, String>,
}

/// A car the dock holds this pass: why, and the `base_regate` stamp to
/// record when this pass launched or refused a re-gate. (It carried the
/// re-gate in flight too, for the round a departure waited on; the board
/// no longer waits on a re-gate — backlog 96f02540.)
#[derive(Debug, Clone, PartialEq)]
pub(super) struct DockHold {
    pub(super) reason: String,
    pub(super) stamp: Option<Value>,
    /// The main the dock is waiting for a free gate bay to re-gate this
    /// car on — its claim ahead of every builder place (design 42279fb2
    /// D3, `gate::DOCK_WAITING`). `None` when it is not waiting for one.
    pub(super) waiting: Option<String>,
}

impl DockHold {
    /// A hold that records nothing but its reason.
    fn plain(reason: String) -> Self {
        DockHold {
            reason,
            stamp: None,
            waiting: None,
        }
    }
}

/// A gate launch that failed, and the gate-run it had already filed when
/// it did (backlog 7919fdcc, item 11). `filed` is `None` when the failure
/// came before the POST — nothing exists to reference — and names the
/// run otherwise, which the launch has already settled `lost`, so a
/// caller can stamp it rather than file a twin on its next pass.
#[derive(Debug)]
pub(super) struct GateLaunchFailed {
    pub(super) filed: Option<String>,
    pub(super) cause: anyhow::Error,
}

impl GateLaunchFailed {
    fn unfiled(cause: anyhow::Error) -> Self {
        GateLaunchFailed { filed: None, cause }
    }
}

/// The cause verbatim, and the run it filed when it filed one. No
/// `source()`: the cause's chain is already in this text, and anyhow would
/// print it twice.
impl std::fmt::Display for GateLaunchFailed {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match &self.filed {
            Some(run) => write!(
                f,
                "{:#} (gate-run {} was filed first, and is settled lost)",
                self.cause,
                id8(run)
            ),
            None => write!(f, "{:#}", self.cause),
        }
    }
}

impl std::error::Error for GateLaunchFailed {}

/// PURE: the hold a red's retry launch leaves, given how many reds in a
/// row the car had BEFORE the red being retried. The retry of a garaged
/// car (`reds_before > 0`) claims no bay (the re-review of car 5eb1967e,
/// B): its red is likely its own, so it waits its turn like a builder's
/// gate. A first red's retry keeps its claim — its red may well be
/// main's.
pub(super) fn retry_hold(hold: DockHold, reds_before: u32) -> DockHold {
    if reds_before == 0 {
        return hold;
    }
    DockHold {
        waiting: None,
        ..hold
    }
}

/// The stamp to write for an EDGE-HELD car whose stamp carries a red its
/// receipt has since answered — the receipt vouches for the head the forge
/// carries for its branch (backlog 7919fdcc, item 9). `None` when there is
/// nothing to clear, the receipt vouches for nothing there, or the head
/// cannot be read — a red left a pass too long, never one cleared on a
/// guess.
fn edge_held_red_to_clear(car: &Value, clone: &str) -> Option<Value> {
    // No red, no read: most edge-held cars cost nothing here.
    dock_regate::cleared_stamp(car)?;
    let branch = car
        .pointer("/metadata/branch")
        .and_then(Value::as_str)
        .filter(|b| !b.is_empty())?;
    let out = sh_unchecked(&[
        "git",
        "-C",
        clone,
        "rev-parse",
        "--verify",
        "--quiet",
        &format!("origin/{branch}"),
    ])
    .ok()
    .filter(|o| o.status.success())?;
    let head = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if head.is_empty() || receipt_skip_reason(car, Some(&head)).is_some() {
        return None;
    }
    dock_regate::cleared_stamp(car)
}

/// Does the car already carry every one of these metadata values? A hold
/// that says what the car already says is not written again (the dock is
/// walked every two minutes, design 42279fb2); a new value, or a key the
/// car lacks, is.
pub(crate) fn metadata_already(car: &Value, kv: &[(&str, Value)]) -> bool {
    kv.iter().all(|(k, v)| {
        car.get("metadata")
            .and_then(|m| m.get(*k))
            .is_some_and(|have| have == v)
    })
}

/// PURE: the cars whose claim on a gate bay (`gate::DOCK_WAITING`) this
/// pass deletes — every car carrying one, in any shape, whose claim the
/// pass has not already decided (`settled`). Design 38f3a488 D3: a claim
/// is written once and counted while the refresh rule fires, so a claim
/// nobody withdraws stands ahead of every builder for as long as the
/// conductor runs. A held refresh settles nothing and withdraws all; a
/// walk of the dock withdraws the claims of the cars it held for anything
/// but a bay.
pub(crate) fn claims_to_withdraw(cars: &[Value], settled: &HashSet<String>) -> Vec<String> {
    cars.iter()
        .filter(|c| {
            c.pointer(&format!("/metadata/{}", crate::gate::DOCK_WAITING))
                .is_some_and(|v| !v.is_null())
        })
        .filter_map(|c| c.get("id").and_then(Value::as_str))
        .filter(|id| !settled.contains(*id))
        .map(str::to_string)
        .collect()
}

/// The refresh's one journal line: what the walk found, so a two-minute
/// verb is never silent about whether it did anything.
fn refresh_line(pass: &DockPass) -> String {
    format!(
        "refresh: {} car(s) boardable, {} held — nothing assembled, nothing departed",
        pass.boardable.len(),
        pass.left_behind.len(),
    )
}

impl Conductor {
    pub(super) fn new(cfg: Config, forge: Box<dyn Forge>) -> Result<Self> {
        // The machine token is stamped per request from the process's one
        // watched source, so a conductor that outlives a rotation sends
        // the new value without a restart; it was read once into default
        // headers until car 2's CLI slice (design 6805c764, review S1).
        // The machine client follows no redirect. The forge is reached
        // through `forge`, on its own plain client — never this one.
        let http = crate::gate::machine_client_with(
            reqwest::Client::builder().timeout(Duration::from_secs(30)),
        )?;
        // Built on the compiled fallback so the conductor can make the
        // very API call that resolves the real one; `with_policy`
        // replaces it before any decision is taken.
        let owner = crate::owner::resolver(&cfg.jobs);
        Ok(Conductor {
            cfg,
            http,
            forge,
            owner,
            policy: DeliveryPolicy::compiled(),
            cluster: Box::new(Kubectl::default()),
        })
    }

    /// The owner a packet this loop files carries: the port's answer,
    /// or nobody with the refusal in the journal — the filing goes
    /// ahead either way, because an alarm that fell silent for want of
    /// an owner would be the failure mode this loop exists to end.
    async fn owner_for_filing(&self) -> String {
        boss_core::platform_owner::owner_for_filing(&self.owner, |e| {
            log(format!(
                "{e}; filing with no owner named — the jobs API resolves the kind's owner_role, or refuses"
            ))
        })
        .await
    }

    pub(super) fn with_policy(mut self, policy: DeliveryPolicy) -> Self {
        self.policy = policy;
        self
    }

    /// Read the delivery policy in force. Never fails — an unreachable
    /// or unusable registry lands on the compiled fallback with one loud
    /// journal line (`delivery_policy::resolve_from`), because a policy
    /// registry must not become a new way to wedge every train.
    pub(super) async fn resolve_policy(&self) -> DeliveryPolicy {
        let fetched = self
            .api(
                Method::GET,
                &format!("/api/delivery/policy/{}", delivery_policy::POLICY_NAME),
                None,
            )
            .await
            .and_then(row_of_policy);
        let policy = delivery_policy::resolve_from(fetched, &|m| log(m));
        if policy.is_from_registry() {
            log(format!(
                "delivery policy v{} in force (hold {}, stall {}h)",
                policy.version, policy.max_red_trains, policy.stall_hours
            ));
        }
        policy
    }

    /// The policy a train in flight is judged by: the version it
    /// DEPARTED under, not the one in force now. A mid-flight registry
    /// edit changes the next train, never this one — the same promise a
    /// packet gets from its pinned workflow version.
    async fn policy_for(&self, train: &Value) -> DeliveryPolicy {
        let Some(version) = delivery_policy::version_to_fetch(train, &self.policy) else {
            return self.policy.clone();
        };
        let fetched = self
            .api(
                Method::GET,
                &format!(
                    "/api/delivery/policy/{}/versions/{version}",
                    delivery_policy::POLICY_NAME
                ),
                None,
            )
            .await
            .and_then(row_of_policy);
        match fetched.and_then(|row| {
            row.ok_or_else(|| anyhow!("policy v{version} is not in the registry"))
                .and_then(delivery_policy::parse)
        }) {
            Ok(pinned) => pinned,
            Err(e) => {
                // Loud, and then carry on under the active policy: a
                // train whose pin cannot be read still has to be
                // reconciled, and refusing would strand its consist.
                log(format!(
                    "delivery policy: train pinned v{version} but it could not be read ({e}) — \
                     judging it under v{} instead",
                    self.policy.version
                ));
                self.policy.clone()
            }
        }
    }

    /// Every jobs-API call the conductor makes, under the blip guard:
    /// a rolling system of record must not fail a whole verb.
    async fn api(
        &self,
        method: Method,
        path: &str,
        payload: Option<Value>,
    ) -> Result<Option<Value>> {
        retrying(
            &JOBS_API_RETRY,
            &method,
            path,
            self.policy.blip_cause_budget,
            &|m| log(m),
            || {
                let method = method.clone();
                let payload = payload.clone();
                async move { self.api_once(method, path, payload).await }
            },
        )
        .await
    }

    /// One attempt. Every way it can fail is classified on the way
    /// out, so the caller above decides retry-or-surface on evidence
    /// rather than on a string match over an error message.
    async fn api_once(
        &self,
        method: Method,
        path: &str,
        payload: Option<Value>,
    ) -> std::result::Result<Option<Value>, ApiFailure> {
        let mut req = self
            .http
            .request(method.clone(), format!("{}{path}", self.cfg.jobs))
            .header("content-type", "application/json")
            .header("x-boss-user", boss_user());
        if let Some(p) = &payload {
            req = req.json(p);
        }
        let resp = req
            .send()
            .await
            .map_err(|e| ApiFailure::transport(e, format!("{method} {path}")))?;
        let status = resp.status();
        let body = resp
            .text()
            .await
            .map_err(|e| ApiFailure::transport(e, format!("reading {method} {path} response")))?;
        if !status.is_success() {
            return Err(ApiFailure {
                kind: http_failure(status.as_u16(), &body),
                cause: anyhow!("{method} {path}: HTTP {status}: {}", body.trim()),
            });
        }
        if body.trim().is_empty() {
            return Ok(None);
        }
        serde_json::from_str(&body)
            .map(Some)
            .map_err(|e| ApiFailure {
                kind: Failure::Malformed,
                cause: anyhow::Error::new(e).context(format!("parsing {method} {path} response")),
            })
    }

    async fn get_job(&self, id: &str) -> Result<Value> {
        self.api(Method::GET, &format!("/api/jobs/{id}"), None)
            .await?
            .ok_or_else(|| anyhow!("job {id} came back empty"))
    }

    /// File the urgent packet a red train becomes — the estate-alarm
    /// idiom (kind backlog-item, priority urgent, on the pipeline
    /// subject), keyed by `train_alert` so the overdue/watchlist
    /// machinery can see it. Dedup is the train's `red_alert_filed`
    /// flag (see `announce_red_train`), not this key.
    async fn file_train_alert(&self, tid: &str, alert: &RedTrainAlert) -> Result<()> {
        let owner = self.owner_for_filing().await;
        self.api(
            Method::POST,
            "/api/jobs",
            Some(red_train_alert_body(tid, alert, &owner)),
        )
        .await?;
        Ok(())
    }

    /// Announce a red train unless it is already announced — best-effort
    /// caller in `reconcile`. Both the flag read and the POST are
    /// fallible; the caller treats ANY error here as non-fatal, because
    /// filing an alert is observability and must never abort the pass
    /// that boards, merges, and auto-cancels. See `reconcile`.
    ///
    /// Dedup is a per-train metadata FLAG (`red_alert_filed`), mirroring
    /// `deploy_alarm_filed` / `converge_alarm_filed` — not a scan of open
    /// backlog-items. The scan it replaces read `status=open&limit=200`
    /// and treated a truncated page as "no alert exists"; once open
    /// backlog-items passed 200 the existing alert sat beyond row 200,
    /// the dedup answered "not raised", and the alert re-filed every
    /// reconcile pass (a self-compounding notification flood). The flag
    /// is stamped only after a successful file, so a failed POST leaves
    /// the train unflagged and the next pass retries.
    async fn announce_red_train(&self, train: &Value, alert: &RedTrainAlert) -> Result<()> {
        if red_alert_filed(train) {
            return Ok(());
        }
        let tid = job_id(train)?;
        self.file_train_alert(tid, alert).await?;
        self.api(
            Method::PATCH,
            &format!("/api/jobs/{tid}/metadata"),
            Some(json!({"red_alert_filed": true})),
        )
        .await?;
        log(format!(
            "train {} red — filed alert: {}",
            id8(tid),
            alert.title
        ));
        Ok(())
    }

    /// Complete `step` on `job` with evidence fields (None values are
    /// dropped, matching the python kwargs filter): the fields through
    /// the step's merge door, then the status alone
    /// ([`step_completion_writes`] says why, backlog e39a9d2a). The
    /// step's stored keys are not read and not re-sent — the merge door
    /// keeps them, and a key another writer added since this pass read
    /// the train is kept too, rather than refused.
    async fn complete_step(
        &self,
        job: &Value,
        step: Option<&Value>,
        fields: &[(&str, Option<String>)],
    ) -> Result<()> {
        if step_done(step) {
            return Ok(());
        }
        let jid = job_id(job)?;
        let step = step.ok_or_else(|| anyhow!("step missing on job {}", id8(jid)))?;
        let mut md = Map::new();
        for (k, v) in fields {
            if let Some(v) = v {
                md.insert((*k).to_string(), json!(v));
            }
        }
        if self.cfg.dry {
            log(format!(
                "DRY: would complete {} on {} with {}",
                step_label(step),
                id8(jid),
                py_dict(fields)
            ));
            return Ok(());
        }
        let sid = step
            .get("id")
            .and_then(Value::as_str)
            .ok_or_else(|| anyhow!("step without an id on job {jid}"))?;
        // WHEN is evidence too: steps carry only a completion DATE,
        // so the conductor stamps the instant itself — the arrival
        // report's timings derive from these.
        md.insert(
            "completed_at".to_string(),
            json!(crate::gate::stamp(Utc::now())),
        );
        for (method, path, body) in step_completion_writes(jid, sid, md) {
            self.api(method, &path, Some(body)).await?;
        }
        log(completion_log_line(
            &step_label(step),
            id8(jid).as_str(),
            fields,
        ));
        Ok(())
    }

    /// Merge top-level metadata keys into a job through the server's
    /// merge door, `PATCH /api/jobs/{id}/metadata` (`job_metadata_patch`,
    /// pinned by tests): the keys named here are set, a `Value::Null`
    /// deletes one, and every other key stays as the row holds it.
    ///
    /// It was a client-side GET, overlay and full job PUT until design
    /// 38f3a488 D4 (backlog b15b0f4e): the PUT replaced `metadata`
    /// wholesale, so a key another writer set between the read and the
    /// write was erased. The server merges in one transaction against the
    /// row as it stands. It still records one event per write — what
    /// keeps the log to work is writing only what changed.
    async fn merge_job_metadata(&self, jid: &str, kv: Vec<(&str, Value)>) -> Result<()> {
        if self.cfg.dry {
            let keys: Vec<&str> = kv.iter().map(|(k, _)| *k).collect();
            log(format!(
                "DRY: would set {} on job {}",
                py_keys(&keys),
                id8(jid)
            ));
            return Ok(());
        }
        let (method, path, body) = job_metadata_patch(jid, kv);
        self.api(method, &path, Some(body)).await?;
        Ok(())
    }

    /// Close each boarded car of a just-merged train, BEST-EFFORT, and
    /// return how many failed to close this pass.
    ///
    /// Each car's close is its own fallible scope: a failure LOGS a line
    /// naming the car and the loop moves on, so one bad car cannot orphan
    /// the rest. The caller completes the train's `merged` step only when
    /// this returns 0, so a partial pass is retried on the next reconcile.
    /// All three writes are idempotent — `get_job` reads, `complete_step`
    /// early-returns on a done step, `merge_job_metadata` merges — so a
    /// retry re-closes only the car that did not close before.
    /// The squash commit's body for this train: `squash_message` over
    /// the consist its `boarded_jobs` name (backlog f252cb1c). BEST-
    /// EFFORT by construction — a car the API cannot hand back must
    /// not stop a green train from landing, so a failed read logs and
    /// the merge goes with an empty body, exactly what every train
    /// carried before 2026-09-19. The arrival report still files the
    /// same consist from the same ids on the sweep.
    async fn squash_message_for(&self, train: &Value) -> String {
        let boarded: Vec<&str> = train
            .get("metadata")
            .and_then(|m| m.get("boarded_jobs"))
            .and_then(Value::as_array)
            .map(|a| a.iter().filter_map(Value::as_str).collect())
            .unwrap_or_default();
        let mut cars = Vec::with_capacity(boarded.len());
        for cid in boarded {
            match self.get_job(cid).await {
                Ok(car) => cars.push(car),
                Err(e) => {
                    log(format!(
                        "train {}: car {} unreadable — merging without the consist body (non-fatal): {e}",
                        id8(job_id(train).unwrap_or("?")),
                        id8(cid)
                    ));
                    return String::new();
                }
            }
        }
        squash_message(&train_consist(&cars))
    }

    async fn close_boarded_cars(
        &self,
        tid: &str,
        boarded: &[String],
        merge_ref: &str,
        pr_url: &str,
    ) -> usize {
        let mut failures = 0usize;
        for cid in boarded {
            // The car's review closes HERE, not at boarding — the change
            // was open for review until it landed, and leaving the step
            // ready while the car rides is what lets a cancelled train
            // release it (see the boarding loop). Review first, because
            // the ship-a-change spec gates `merged` on `steps.review.done
            // AND job.metadata.merged`, and the marker below is what the
            // dispatcher watches to close the Job.
            let close: Result<()> = async {
                let car = self.get_job(cid).await?;
                let review = find_step(&car, "review", "Open for review");
                if !step_done(review) {
                    self.complete_step(
                        &car,
                        review,
                        &[
                            ("pr_url", Some(pr_url.to_string())),
                            ("note", Some(format!("landed on main as {merge_ref}"))),
                        ],
                    )
                    .await?;
                }
                // v3 ship-a-change gates `merged` on this marker; the
                // dispatcher closes the Job once it is set.
                self.merge_job_metadata(
                    cid,
                    vec![("merged", json!("true")), ("merge_ref", json!(merge_ref))],
                )
                .await?;
                Ok(())
            }
            .await;
            if let Err(e) = close {
                // BEST-EFFORT: one car's failed close must not orphan the
                // rest. Count it, name it, move on — the caller holds the
                // `merged` step pending so the next reconcile retries it.
                failures += 1;
                log(format!(
                    "train {} merged, but closing car {} failed (non-fatal, retries next \
                     pass): {e}",
                    id8(tid),
                    id8(cid)
                ));
            }
        }
        failures
    }

    // -----------------------------------------------------------------------
    // Phase 1 — reconcile open trains against reality
    // -----------------------------------------------------------------------

    /// Complete a merged train's `deployed` step. The conductor deploys
    /// nothing itself: the cluster converges on forge main by itself
    /// via the forge-host cluster-deploy-runner (deployment-as-network;
    /// the migration in docs/design/the-cluster-is-the-system.md), so
    /// this touches no repository and no tree — it records the one
    /// honest evidence (NO_PLAYGROUND_DEPLOY_EVIDENCE) and returns, and
    /// the convergence-verification step downstream is what proves the
    /// cluster took the merge.
    async fn deploy(&self, train: &Value, deployed_step: &Value) -> Result<()> {
        log("deploy skipped — no playground tree; the cluster converges on forge main");
        self.complete_step(
            train,
            Some(deployed_step),
            &[("deployed", Some(NO_PLAYGROUND_DEPLOY_EVIDENCE.to_string()))],
        )
        .await?;
        Ok(())
    }

    /// Verify the CLUSTER is serving this train's merge, and complete
    /// the `converged` step with the evidence — or file the loud
    /// packet when convergence has lagged past the threshold.
    ///
    /// The proof is self-report: the jobs API's health endpoint
    /// answers with the commit its binary was BUILT from
    /// (`Capabilities.commit`, baked in by the image build). That is
    /// stronger than reading the image tag off the Deployment — a tag
    /// proves a push was requested; a running binary reporting the
    /// commit proves the pod restarted onto it.
    async fn verify_convergence(&self, train: &Value, now: DateTime<Utc>) -> Result<()> {
        let tid = job_id(train)?.to_string();
        let converged_step = find_step(train, "converged", "Cluster converged")
            .ok_or_else(|| anyhow!("converged step missing on job {}", id8(&tid)))?;
        let merge_ref = find_step(train, "merged", "Merged into main")
            .and_then(|s| s.get("metadata"))
            .and_then(|m| m.get("merge_ref"))
            .and_then(Value::as_str)
            .ok_or_else(|| anyhow!("no merge_ref on job {} — nothing to verify", id8(&tid)))?
            .to_string();
        let health = self.api(Method::GET, "/api/jobs/health", None).await?;
        let cluster_commit = health
            .as_ref()
            .and_then(|h| h.get("capabilities"))
            .and_then(|c| c.get("commit"))
            .and_then(Value::as_str)
            .map(str::to_string);
        let merged_at = parse_stamp(step_stamp(train, "merged", "Merged into main"));
        let mins_since_merge = merged_at
            .map(|m| (now.fixed_offset() - m).num_minutes())
            .unwrap_or(0);
        // Equality misses "rolled past" (see convergence_verdict); ask
        // git the ancestry question only when equality already failed,
        // against the conductor clone — which reconcile keeps fetched.
        // Any git failure (no clone yet, commit unknown to the forge)
        // reads None: converges nothing, never guesses.
        let ancestor = match cluster_commit.as_deref() {
            Some(c) if !commits_match(&merge_ref, c) => {
                let clone = self.cfg.clone.clone();
                sh_unchecked(&[
                    "git",
                    "-C",
                    &clone,
                    "merge-base",
                    "--is-ancestor",
                    &merge_ref,
                    c,
                ])
                .ok()
                .map(|o| o.status.success())
            }
            _ => None,
        };
        let base = convergence_verdict(
            &merge_ref,
            cluster_commit.as_deref(),
            ancestor,
            mins_since_merge,
            self.cfg.converge_alarm_mins,
        );
        // THE MERGE-LOST ARM (backlog f9256445, design d812f1b7 D1). A
        // cluster that has not taken the merge may never take it: on
        // 2026-09-25 forge main moved back off train 20:04's merge within
        // 32 seconds, and this step waited for ever. So before the train
        // is left Waiting or Overdue, forge main itself is read — and a
        // reading git cannot make is a refusal to judge, never a verdict.
        let prior = first_lost_reading(train).cloned();
        let (main_read, reading) = if base == ConvergenceVerdict::Converged {
            (MainRead::Unread, None)
        } else {
            self.read_forge_main(train, &merge_ref, now).await
        };
        if main_read == MainRead::Carries && prior.is_some() && !self.cfg.dry {
            // Two NOT readings must be consecutive: main carrying the
            // merge again clears the first.
            self.api(
                Method::PATCH,
                &format!("/api/jobs/{tid}/metadata"),
                Some(json!({ MERGE_LOST_FIRST_READ: Value::Null })),
            )
            .await?;
        }
        match with_forge_main(base, main_read, prior.is_some()) {
            ConvergenceVerdict::MainLostOnce => {
                let Some(r) = reading else {
                    return Ok(());
                };
                log(format!(
                    "train {}: forge main {} does NOT carry the merge {} (read {}) — one \
                     reading; a second on a later pass ends the train on merge-lost",
                    id8(&tid),
                    id8(&r.main),
                    id8(&merge_ref),
                    r.read_at
                ));
                if self.cfg.dry {
                    return Ok(());
                }
                self.api(
                    Method::PATCH,
                    &format!("/api/jobs/{tid}/metadata"),
                    Some(json!({ MERGE_LOST_FIRST_READ: first_reading_record(&r) })),
                )
                .await?;
                Ok(())
            }
            ConvergenceVerdict::MergeLost => {
                let (Some(first), Some(r)) = (prior, reading) else {
                    return Ok(());
                };
                self.end_on_merge_lost(train, &merge_ref, &first, &r, now)
                    .await
            }
            ConvergenceVerdict::Converged => {
                let commit = cluster_commit.unwrap_or_default();
                self.complete_step(
                    train,
                    Some(converged_step),
                    &[
                        ("cluster_commit", Some(commit.clone())),
                        (
                            "verified",
                            Some(format!(
                                "the running cluster jobs API self-reports build commit \
                                 {} — matches merge {merge_ref}; verified {} min after merge",
                                id8(&commit),
                                mins_since_merge
                            )),
                        ),
                    ],
                )
                .await
            }
            ConvergenceVerdict::Waiting => {
                log(format!(
                    "train {}: cluster not yet on {} ({} min since merge, alarm at {})",
                    id8(&tid),
                    id8(&merge_ref),
                    mins_since_merge,
                    self.cfg.converge_alarm_mins
                ));
                Ok(())
            }
            ConvergenceVerdict::Overdue => {
                if truthy(
                    train
                        .get("metadata")
                        .and_then(|m| m.get("converge_alarm_filed")),
                ) {
                    return Ok(());
                }
                log(format!(
                    "train {}: cluster convergence OVERDUE ({} min since merge) — filing packet",
                    id8(&tid),
                    mins_since_merge
                ));
                if self.cfg.dry {
                    return Ok(());
                }
                let reported = cluster_commit.as_deref().unwrap_or("nothing");
                let owner = self.owner_for_filing().await;
                self.api(
                    Method::POST,
                    "/api/jobs",
                    Some(convergence_overdue_alarm_body(
                        &tid,
                        &merge_ref,
                        mins_since_merge,
                        reported,
                        self.cfg.converge_alarm_mins,
                        &owner,
                    )),
                )
                .await?;
                self.merge_job_metadata(&tid, vec![("converge_alarm_filed", json!(true))])
                    .await?;
                Ok(())
            }
        }
    }

    /// Read whether forge main carries this train's merge, from the
    /// conductor clone — `car_unland::read_main`, the reading `boss car
    /// unland` makes, so the arm and the writer cannot disagree about
    /// what "lost" means. The clone may never have fetched a merge main
    /// lost inside one pass, and `merge_ref` is the forge's 12-character
    /// answer, which cannot be fetched; so a first refusal asks the forge
    /// for the PR's full merge sha and reads again by it. Anything that
    /// still fails is `Unread`, logged.
    async fn read_forge_main(
        &self,
        train: &Value,
        merge_ref: &str,
        now: DateTime<Utc>,
    ) -> (MainRead, Option<crate::car_unland::MainReading>) {
        let clone = Path::new(&self.cfg.clone);
        let judged = |r: crate::car_unland::MainReading| {
            let read = if r.carries {
                MainRead::Carries
            } else {
                MainRead::NotCarried
            };
            (read, Some(r))
        };
        let refused = match crate::car_unland::read_main(clone, merge_ref, now) {
            Ok(r) => return judged(r),
            Err(e) => e,
        };
        let full = match self.pr_merge(train).await {
            Some((full, _)) if full != merge_ref => full,
            _ => {
                log(format!(
                    "train {}: forge main unread this pass ({refused}) — no verdict",
                    id8(job_id(train).unwrap_or("?"))
                ));
                return (MainRead::Unread, None);
            }
        };
        match crate::car_unland::read_main(clone, &full, now) {
            Ok(r) => judged(r),
            Err(e) => {
                log(format!(
                    "train {}: forge main unread this pass ({e}) — no verdict",
                    id8(job_id(train).unwrap_or("?"))
                ));
                (MainRead::Unread, None)
            }
        }
    }

    /// The PR's full merge sha and its merged_at, as the forge answers
    /// them now — `None` when the train names no PR or the forge cannot
    /// be read.
    async fn pr_merge(&self, train: &Value) -> Option<(String, Option<String>)> {
        let pr_url = find_step(train, "pr", "Open the batched PR")
            .and_then(|s| s.pointer("/metadata/pr_url"))
            .and_then(Value::as_str)?;
        let info = self.forge.pr_info(pr_url).await.ok()?;
        let oid = info
            .pointer("/mergeCommit/oid")
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())?
            .to_string();
        let at = info
            .get("mergedAt")
            .and_then(Value::as_str)
            .map(str::to_string);
        Some((oid, at))
    }

    /// END THE TRAIN ON `merge-lost` (design d812f1b7 D1), in the order
    /// the design fixes: the four fields and the marker first — the
    /// train closes, which releases the track — then the follow-up that
    /// unlands its cars and files the one item. The terminal does not
    /// wait on the follow-up: a follow-up that fails is left `owed` and
    /// the sweep retries it next pass (observability writes are not
    /// fatal to the loop).
    async fn end_on_merge_lost(
        &self,
        train: &Value,
        merge_ref: &str,
        first: &Value,
        r: &crate::car_unland::MainReading,
        now: DateTime<Utc>,
    ) -> Result<()> {
        let tid = job_id(train)?.to_string();
        let Some(step) = find_step(train, MERGE_LOST_SLUG, MERGE_LOST_TITLE) else {
            log(format!(
                "train {}: forge main has not carried its merge {merge_ref} on two readings, \
                 but the train is pinned to pr-train v{} with no `{MERGE_LOST_SLUG}` terminal \
                 — `boss job convert {}` moves it; waiting",
                id8(&tid),
                train
                    .get("workflow_version")
                    .map(Value::to_string)
                    .unwrap_or_else(|| "?".into()),
                id8(&tid)
            ));
            return Ok(());
        };
        let pr_url = find_step(train, "pr", "Open the batched PR")
            .and_then(|s| s.pointer("/metadata/pr_url"))
            .and_then(Value::as_str)
            .unwrap_or("(no PR recorded)")
            .to_string();
        let merged_at = self.pr_merge(train).await.and_then(|(_, at)| at);
        let fields = merge_lost_fields(merge_ref, first, r, &pr_url, merged_at.as_deref());
        log(format!(
            "train {}: forge main {} does NOT carry the merge {merge_ref} — the second \
             reading ({}); ending the train on merge-lost",
            id8(&tid),
            id8(&r.main),
            r.read_at
        ));
        if self.cfg.dry {
            return Ok(());
        }
        let sid = step
            .get("id")
            .and_then(Value::as_str)
            .ok_or_else(|| anyhow!("the merge-lost step has no id on train {}", id8(&tid)))?;
        // The evidence onto the step FIRST (it is pending, so the merge
        // door takes it), then the marker that readies it, then the
        // status — a terminal nobody reaches without the reading.
        let evidence: Map<String, Value> = fields
            .iter()
            .map(|(k, v)| ((*k).to_string(), json!(v)))
            .collect();
        self.api(
            Method::PATCH,
            &format!("/api/jobs/{tid}/steps/{sid}/metadata"),
            Some(Value::Object(evidence)),
        )
        .await?;
        self.api(
            Method::PATCH,
            &format!("/api/jobs/{tid}/metadata"),
            Some(json!({
                MERGE_LOST_MARKER: "true",
                MERGE_LOST_MERGE: r.merge,
                MERGE_LOST_FOLLOWUP: "owed",
            })),
        )
        .await?;
        let marked = self.get_job(&tid).await?;
        let step = find_step(&marked, MERGE_LOST_SLUG, MERGE_LOST_TITLE);
        self.complete_step(&marked, step, &[]).await?;
        let closed = self.get_job(&tid).await?;
        if let Err(e) = self.merge_lost_followup(&closed, now).await {
            log(format!(
                "train {}: ended merge-lost, but its follow-up failed this pass (left owed; \
                 the sweep retries it): {e}",
                id8(&tid)
            ));
        }
        Ok(())
    }

    /// Unland every car of a merge-lost train through `boss car
    /// unland`'s writer, then file the ONE item and mark the follow-up
    /// done. Each unlanding is idempotent, so a retry after a failed
    /// filing re-reads each car and writes nothing twice; a car that
    /// cannot be unlanded is named in the item with the command that
    /// finishes it, rather than holding the item back.
    async fn merge_lost_followup(&self, train: &Value, now: DateTime<Utc>) -> Result<()> {
        let tid = job_id(train)?.to_string();
        let md = |k: &str| {
            train
                .pointer(&format!("/metadata/{k}"))
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string()
        };
        let merge = md(MERGE_LOST_MERGE);
        let step = find_step(train, MERGE_LOST_SLUG, MERGE_LOST_TITLE);
        let field = |k: &str| {
            step.and_then(|s| s.pointer(&format!("/metadata/{k}")))
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string()
        };
        if merge.is_empty() {
            bail!(
                "train {} owes a merge-lost follow-up but records no merge",
                id8(&tid)
            );
        }
        let boarded: Vec<String> = train
            .pointer("/metadata/boarded_jobs")
            .and_then(Value::as_array)
            .map(|a| {
                a.iter()
                    .filter_map(Value::as_str)
                    .map(str::to_string)
                    .collect()
            })
            .unwrap_or_default();
        let clone = Path::new(&self.cfg.clone);
        let mut cars = Vec::new();
        for cid in &boarded {
            match crate::car_unland::unland_car(self, clone, cid, &merge, self.cfg.dry, now).await {
                Ok(u) => cars.push(format!(
                    "car {} unlanded; successor car {} rides again from `gate`",
                    id8(cid),
                    id8(&u.successor)
                )),
                Err(e) => cars.push(format!(
                    "car {} NOT unlanded: {e} — finish it with `boss car unland {} \
                     --merge-ref {merge}`",
                    id8(cid),
                    id8(cid)
                )),
            }
        }
        for line in &cars {
            log(format!("train {}: {line}", id8(&tid)));
        }
        if self.cfg.dry {
            return Ok(());
        }
        let first_read_at = train
            .pointer(&format!("/metadata/{MERGE_LOST_FIRST_READ}/read_at"))
            .and_then(Value::as_str)
            .unwrap_or("?")
            .to_string();
        let pr_url = find_step(train, "pr", "Open the batched PR")
            .and_then(|s| s.pointer("/metadata/pr_url"))
            .and_then(Value::as_str)
            .unwrap_or("(no PR recorded)")
            .to_string();
        let owner = self.owner_for_filing().await;
        let filed = self
            .api(
                Method::POST,
                "/api/jobs",
                Some(merge_lost_item_body(
                    &tid,
                    &pr_url,
                    &field("merge_ref"),
                    &field("main_at_read"),
                    &first_read_at,
                    &field("read_at"),
                    &cars,
                    &owner,
                )),
            )
            .await?
            .ok_or_else(|| anyhow!("filing the merge-lost item answered no body"))?;
        let item = job_id(&filed)?.to_string();
        self.api(
            Method::PATCH,
            &format!("/api/jobs/{tid}/metadata"),
            Some(json!({ MERGE_LOST_FOLLOWUP: "done", MERGE_LOST_ITEM: item })),
        )
        .await?;
        log(format!(
            "train {}: merge-lost follow-up done — item {} filed",
            id8(&tid),
            id8(&item)
        ));
        Ok(())
    }

    /// The sweep behind the follow-up: every CLOSED train still owing
    /// one (the terminal was written, the filing failed), read by the
    /// marker itself — a containment filter, not a page of trains that
    /// may or may not hold it.
    async fn follow_up_merge_lost(&self, now: DateTime<Utc>) -> Result<()> {
        let filter = json!({ MERGE_LOST_FOLLOWUP: "owed" }).to_string();
        let owed = rows(
            self.api(
                Method::GET,
                &format!(
                    "/api/jobs?kind=pr-train&status=closed&limit=20&full=true&metadata={}",
                    percent_encoding::utf8_percent_encode(&filter, crate::job::QUERY_VALUE)
                ),
                None,
            )
            .await?,
        )?;
        for t in owed {
            if let Err(e) = self.merge_lost_followup(&t, now).await {
                log(format!(
                    "reconcile: merge-lost follow-up for train {} failed (left owed): {e}",
                    id8(job_id(&t).unwrap_or("?"))
                ));
            }
        }
        Ok(())
    }

    // `record_abandon_reason` lived here: it wrote the machine's reason
    // onto the `cancelled` terminal of a train the board had opened only
    // to abandon. A board no longer opens a packet it is not departing
    // (see "A BOARD THAT DEPARTS NO TRAIN OPENS NO PACKET"), so there is
    // no self-cancelled train left to explain — the reason it used to
    // carry is now the journal's `no train departed` line and the cars'
    // own `skip_reason`. An operator's `boss train cancel` still fills
    // the same terminal with its `--reason`, on its own path.

    /// Log a refused board, and file ONE packet when the refusal is the
    /// kind that will not clear itself.
    ///
    /// WHY THIS EXISTS (backlog 6baabd43). From 04:27Z to 13:49Z on
    /// 2026-09-19 no train departed. The conductor never stopped and
    /// never failed: every minute it took its lock, ran preflight,
    /// evaluated all three parked cars and logged in full — naming the
    /// branches and the conflicting files. Nine and a half hours of
    /// perfect diagnosis with zero reach, while the yard rendered
    /// `3 cars parked — a train is due`, which is what it says two
    /// minutes after a healthy departure.
    ///
    /// NOT EVERY NON-DEPARTURE. `refusal_persists` is the split, and it
    /// is the whole design: an idle dock, a car waiting on a
    /// predecessor in flight, a host short of disk — all clear
    /// themselves, and alarming on them is how a check becomes noise
    /// and then becomes unread. Only the three that repeat identically
    /// until a person acts get a packet.
    ///
    /// NO TIMER, because none is needed: the conductor returns early on
    /// `BOARDING HELD — track occupied`, so a refusal reaching here has
    /// already proven the track is clear. A persistent refusal with an
    /// empty track is a stopped pipeline by construction.
    ///
    /// DEDUPLICATED BY THE PACKET ITSELF. While one is open no twin is
    /// filed, so the alarm does not become the thing it is warning
    /// about — and closing it is what re-arms it.
    async fn record_no_departure(&self, refusal: &NoDeparture) -> Result<()> {
        let line = no_departure_line(refusal);
        log(&line);
        // The decision first, before anything here can fail: it is what
        // the yard states in place of "nothing holds it" (96f02540).
        record_board_decision(&BoardDecision::from(refusal));
        if !crate::train::boarding::refusal_persists(refusal) || self.cfg.dry {
            return Ok(());
        }
        let open = rows(
            self.api(
                Method::GET,
                "/api/jobs?kind=user-feedback&status=open&limit=100",
                None,
            )
            .await?,
        )?;
        let already = open.iter().find(|j| {
            j.get("title")
                .and_then(Value::as_str)
                .is_some_and(|t| t.starts_with("Boarding stalled:"))
        });
        if let Some(alarm) = already {
            return self.escalate_boarding_stall(alarm, &line).await;
        }
        log("boarding stalled on a refusal that will not clear itself — filing a packet");
        let owner = self.owner_for_filing().await;
        self.api(
            Method::POST,
            "/api/jobs",
            Some(crate::train::stranded::no_departure_alarm_body(
                &line,
                &owner,
                Utc::now(),
            )),
        )
        .await?;
        Ok(())
    }

    /// Grow the number on an alarm nobody has read yet.
    ///
    /// WHY (backlog 94896e74). The dedup above is right and stays: while
    /// the packet is open no twin is filed, and closing it re-arms the
    /// alarm. But "already filed" was also "nothing further happens",
    /// and on 2026-09-22 that meant a packet filed at 07:01Z sat
    /// unchanged while the identical refusal fired ~420 more times until
    /// 14:21Z. The packet was already `priority: urgent` at filing, so
    /// there is no priority left to raise; what an operator is owed is
    /// the WAIT, on the packet, growing. `stall_escalation` is the
    /// decision — a finite three-rung ladder, so a stall of any length
    /// costs at most three writes and the escalation can never become
    /// the flood it warns about.
    ///
    /// An alarm from before this stamp existed carries no
    /// `stalled_since`; it is stamped here and judged from the next
    /// window, rather than having a duration invented for it from a
    /// date-level `opened_on`.
    async fn escalate_boarding_stall(&self, alarm: &Value, line: &str) -> Result<()> {
        let jid = job_id(alarm)?.to_string();
        let now = Utc::now();
        if metadata_map(alarm).get("stalled_since").is_none() {
            self.merge_job_metadata(&jid, vec![("stalled_since", json!(now.to_rfc3339()))])
                .await?;
            return Ok(());
        }
        let Some(escalation) = stall_escalation(alarm, now) else {
            return Ok(());
        };
        log(format!(
            "boarding stalled {} minutes and still refusing — escalating packet {} to {} of {}",
            escalation.minutes,
            id8(&jid),
            escalation.level,
            STALL_ESCALATION_MINS.len()
        ));
        self.merge_job_metadata(
            &jid,
            vec![
                ("escalation_level", json!(escalation.level)),
                ("stalled_minutes", json!(escalation.minutes)),
                ("escalated_at", json!(now.to_rfc3339())),
                (
                    "message",
                    json!(stall_escalation_message(&escalation, line)),
                ),
            ],
        )
        .await?;
        Ok(())
    }

    /// Settle gate-runs whose runner died without reporting: complete
    /// `record-verdict` as `lost`, the terminal the workflow already
    /// provides for exactly this. NOT green and NOT failed — the checks
    /// never finished, so the run says nothing about the branch, and a
    /// verdict nobody observed would be a lie the audit log carries
    /// forever. The decision itself is `dead_gate_run_hours`, pure and
    /// tested; this is the adapter that acts on it.
    async fn reap_dead_gate_runs(&self, now: DateTime<Utc>) -> Result<()> {
        let runs = rows(
            self.api(
                Method::GET,
                "/api/jobs?kind=gate-run&status=open&limit=100",
                None,
            )
            .await?,
        )?;
        for r0 in runs {
            // EACH RUN IS ITS OWN SCOPE (review of car 2ca8c7e9). A bare
            // `?` on this read ended the pass at the first row the API
            // would not return — closed or deleted since the list, a
            // refused scope — so every run after it waited on a pass that
            // stopped at the same row each time.
            let run = match job_id(&r0) {
                Ok(rid) => self.get_job(rid).await.map(|run| (rid.to_string(), run)),
                Err(e) => Err(e),
            };
            let (rid, run) = match run {
                Ok(read) => read,
                Err(e) => {
                    log(format!(
                        "reconcile: gate-run {} unreadable this pass (retries next): {e:#}",
                        r0.get("id")
                            .and_then(Value::as_str)
                            .map_or("(no id)".into(), id8)
                    ));
                    continue;
                }
            };
            let Some(hours) = dead_gate_run_hours(&run, now) else {
                // One refused settle must not cost the rest of the pass
                // (the clock settles below still owe their runs).
                if let Err(e) = self
                    .settle_orphaned_gate_run(&rid, &run, now, &crate::gate::gate_jobs_for_packet)
                    .await
                {
                    log(format!(
                        "reconcile: orphaned gate-run {} not settled this pass (retries next): {e:#}",
                        id8(&rid)
                    ));
                }
                continue;
            };
            let branch = metadata_map(&run)
                .get("branch")
                .and_then(Value::as_str)
                .unwrap_or("(no branch)")
                .to_string();
            log(format!(
                "reconcile: gate-run {} ({branch}) active {hours}h with no verdict — \
                 past the {GATE_DEADLINE_HOURS}h Job deadline, settling as lost",
                id8(&rid)
            ));
            let verdict_step = find_step(&run, "record-verdict", "Record the gate verdict");
            // Per run, like the read above: one refused write is logged
            // and the rest of the list is still settled.
            if let Err(e) = self
                .complete_step(
                    &run,
                    verdict_step,
                    &[
                        ("verdict", Some("lost".to_string())),
                        (
                            "receipt",
                            Some(format!(
                                "NO VERDICT WAS RECORDED. Active {hours}h with none, past the \
                                 gate Job's {GATE_DEADLINE_HOURS}h activeDeadlineSeconds, by \
                                 which the cluster has ended any Job this run had. What was \
                                 observed is only that: no verdict reached this packet in that \
                                 time. Whether the runner left none, or left one in a pod or a \
                                 log the conductor could not read, is in the conductor's \
                                 journal for this gate-run and — for a day after the Job \
                                 ended — in `kubectl logs job/<its gate Job> -c gate`. Settled \
                                 as LOST by the conductor's reconcile: this run says nothing \
                                 about {branch}, and an infrastructure death is not a consist \
                                 failure. Re-gate for a real verdict."
                            )),
                        ),
                    ],
                )
                .await
            {
                log(format!(
                    "reconcile: gate-run {} not settled this pass (retries next): {e:#}",
                    id8(&rid)
                ));
            }
        }
        Ok(())
    }

    /// Settle a gate-run NO JOB CARRIES and nothing has held for the
    /// orphan window — `lost`, with the evidence on the packet — instead
    /// of leaving it open to the three-hour clock (backlog 137c176d).
    ///
    /// WHY HERE. This is the one place gate-runs are settled by time, and
    /// since design 128b5496 (2026-09-12) the conductor holds the gates
    /// Role — it launches the train's gate — so it can now read the one
    /// fact the clock stood in for: whether any Job exists. The estate
    /// observer settles the OTHER dead class, a runner Job Kubernetes
    /// marked Failed, from that Job's condition; a packet with no Job at
    /// all was invisible to it. The two reads are disjoint by
    /// construction: any Job, in any state, makes this pass leave the
    /// packet alone.
    ///
    /// WHAT IT UNBLOCKS. The observer's dead-host pass will not end a
    /// builder run while a gate-run naming it is open, because a runner
    /// Job outlives the dev pod that launched it. Measured 2026-09-24: an
    /// eviction at 01:27Z killed six runs, five ended on the first pass,
    /// and the sixth waited until 04:45Z behind gate-run 91594262 — a
    /// queued run whose waiter died with the pod, so no Job ever existed.
    /// Closed here, that run ends on the observer's next pass.
    ///
    /// ORDER, FOR THE RACE THAT REMAINS. The cluster is read first and the
    /// packet re-read after it, then judged again: a waiter that launched
    /// in between either shows its Job or has refreshed its beat, which
    /// `queue_clear_patch` now keeps. A re-gate that reuses this packet
    /// and stamps `launching_at` after the re-read can still lose it to
    /// this write — no atomic claim on a gate-run exists (gate.rs,
    /// 76d41004). What catches it is on the LAUNCH side: `boss gate`
    /// re-reads the packet just before `kubectl create` and refuses one
    /// this settle has closed (`gate::launch_target_refusal`, b24e29cb).
    /// Without that, the runner's report met a completed step and was
    /// refused only as a line in the pod's log (run.sh), and the waiter
    /// read `lost` with a receipt saying no Job existed while one ran.
    ///
    /// The evidence is written AFTER the verdict step, dated at the
    /// cluster read — so no open packet carries an orphan finding for a
    /// settle that did not happen.
    ///
    /// FAILS CLOSED: a cluster that cannot be read is logged and the run
    /// is left to the clock, never settled on an absence nobody observed.
    /// The read is a parameter (`gate::gate_jobs_for_packet` in
    /// production) so every refusal above is driven by a test.
    async fn settle_orphaned_gate_run(
        &self,
        rid: &str,
        run: &Value,
        now: DateTime<Utc>,
        gate_jobs: &(dyn Fn(&str, &str) -> Result<Vec<String>> + Sync),
    ) -> Result<()> {
        if orphaned_gate_run(run, now).is_none() {
            return Ok(());
        }
        let ns = self.cfg.gate_namespace.clone();
        let read = gate_jobs(&ns, rid);
        // The instant the evidence reports is the READ, not the pass's
        // `now`: a pass walks up to a hundred runs, so the pass start can
        // be minutes older than the reading it would date (b24e29cb).
        let observed_at = Utc::now();
        match read {
            Ok(jobs) if jobs.is_empty() => {}
            Ok(_) => return Ok(()),
            Err(e) => {
                log(format!(
                    "reconcile: gate-run {} has no verdict and nothing has held it for \
                     {ORPHAN_GATE_RUN_MINUTES} min, but the gate Jobs in {ns} could not be \
                     read ({e:#}) — left for the {GATE_DEADLINE_HOURS}h clock",
                    id8(rid)
                ));
                return Ok(());
            }
        }
        let run = self.get_job(rid).await?;
        let Some(orphan) = orphaned_gate_run(&run, now) else {
            return Ok(());
        };
        let branch = metadata_map(&run)
            .get("branch")
            .and_then(Value::as_str)
            .unwrap_or("(no branch)")
            .to_string();
        let sha = metadata_map(&run)
            .get("sha")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        let verdict_step = find_step(&run, "record-verdict", "Record the gate verdict");
        // WITHDRAWN, NOT LOST, WHEN A CAR ALREADY VOUCHES FOR THE HEAD
        // (backlog b1c82a82; 8d7d0a2b gave the protocol the word). Gate-run
        // 8c2f644a was queued for a head car 59e1f436 already carried
        // green; its waiter died and this settle closed it `lost` — a
        // dead runner on the record for a run nobody needed, while orient
        // advised a re-gate the twin-car guard refuses. The question is
        // the guard's own (`gate::vouching_car`), so the three readers
        // cannot answer it differently, and nothing about the answer
        // needs a human. A car list that cannot be read settles NOTHING
        // this pass: "could not tell" is neither word.
        let cars = match list_all_pages(|offset| async move {
            self.api(
                Method::GET,
                &format!(
                    "/api/jobs?kind=ship-a-change&status=open&full=true&limit={PAGE_LIMIT}&offset={offset}"
                ),
                None,
            )
            .await
        })
        .await
        {
            Ok(cars) => cars,
            Err(e) => {
                log(format!(
                    "reconcile: gate-run {} ({branch}) is an orphan, but the open cars could not \
                     be read to ask whether one already carries {sha} ({e:#}) — settled next pass",
                    id8(rid)
                ));
                return Ok(());
            }
        };
        let vouch = crate::gate::vouching_car(&branch, &sha, &cars);
        if vouch.is_some() && !crate::gate::admits_withdrawn(&run) {
            log(format!(
                "reconcile: gate-run {} ({branch}) is already carried by a car, but its \
                 protocol version cannot say `withdrawn` — settling as lost",
                id8(rid)
            ));
        }
        if let Some(vouch) = vouch.filter(|_| crate::gate::admits_withdrawn(&run)) {
            let because = crate::gate::vouched_reason(&vouch);
            log(format!(
                "reconcile: gate-run {} ({branch}) has no gate Job in {ns} and nothing has held \
                 it for {} min — WITHDRAWN: {because}",
                id8(rid),
                orphan.idle_minutes,
            ));
            let receipt = crate::gate::withdrawal_receipt(&because, Some(&vouch));
            match self
                .complete_step(
                    &run,
                    verdict_step,
                    &[
                        ("verdict", Some(crate::gate::WITHDRAWN_VERDICT.to_string())),
                        ("receipt", Some(receipt.to_string())),
                    ],
                )
                .await
            {
                Ok(()) => return Ok(()),
                // A packet admitted under gate-run v2 materialized the
                // four-word verdict enum, so it cannot say `withdrawn` and
                // the completion is refused. Settle it as this pass always
                // did — `lost`, below, whose write replaces the merged
                // verdict — rather than leave the orphan to the three-hour
                // clock.
                Err(e) => log(format!(
                    "reconcile: gate-run {} could not record `withdrawn` ({e:#}) — its \
                     protocol version predates the word; settling as lost",
                    id8(rid)
                )),
            }
        }
        log(format!(
            "reconcile: gate-run {} ({branch}) has no gate Job in {ns} and nothing has held it \
             for {} min (last: {} {}) — settling as lost",
            id8(rid),
            orphan.idle_minutes,
            orphan.last_alive_key,
            orphan.last_alive,
        ));
        self.complete_step(
            &run,
            verdict_step,
            &[
                ("verdict", Some("lost".to_string())),
                ("receipt", Some(orphan_receipt(&orphan, &branch, &ns))),
            ],
        )
        .await?;
        // THE EVIDENCE FOLLOWS THE VERDICT (b24e29cb item 3). Written
        // first, a refused step write left `orphaned_gate_run {runner_jobs:
        // 0}` on a packet still open — and nothing cleared it when a Job
        // was later found. After the step, it only ever annotates a run
        // this pass settled. A failed write here costs the structured copy,
        // not the settle: the receipt names the same reading, and the
        // journal says the annotation is missing.
        if !self.cfg.dry
            && let Err(e) = self
                .merge_job_metadata(
                    rid,
                    vec![(
                        "orphaned_gate_run",
                        orphan_evidence(&orphan, rid, &ns, observed_at),
                    )],
                )
                .await
        {
            log(format!(
                "reconcile: gate-run {} settled lost, but its orphaned_gate_run evidence was not \
                 written ({e:#}) — the receipt carries the reading",
                id8(rid)
            ));
        }
        Ok(())
    }

    /// Record the verdicts gate runners LEFT in their pod logs (backlog
    /// 934ccad1; design bdc60b65, question `gate-verdict`). The runner of
    /// the new layout writes nothing to the system of record — it runs a
    /// car's branch and must never hold the estate token — so this, the
    /// holder of the gates Role that runs no branch code, records for it.
    /// The decisions are `carried_verdict`'s, pure and tested; this is
    /// the adapter that reads the cluster and writes the step.
    ///
    /// DRIVEN FROM THE JOBS, NOT FROM THE PACKETS. One cluster read lists
    /// every gate Job; a packet is asked about only when a Job labelled
    /// as a carrier is its newest. So a gate whose Job finished while this
    /// conductor or the jobs API was rolling is found on the next pass for
    /// as long as the Job exists (24 h), and settled — the lost-during-a-
    /// roll class (23188cc5) cannot recur by construction. A packet with
    /// no carrier Job costs nothing beyond that one read.
    ///
    /// ONLY FROM A CONTAINER THAT HAS ENDED. The verdict is the frame the
    /// gate container's termination message names (`carried_verdict`,
    /// module doc), so a running gate is one pod read a pass and no log
    /// read at all, and a line in a running pod's log decides nothing.
    ///
    /// IDEMPOTENT, AND NEVER OVER ANOTHER WRITER. A packet that is not
    /// open, or whose `record-verdict` step is done, is left exactly as it
    /// is: an old-layout runner's own report, the estate observer's
    /// settle, a withdrawal, and this pass's earlier recording all read
    /// the same here. The write is `complete_step` — the merge door, then
    /// the status alone — so every reader of a gate-run reads it as it
    /// read the runner's.
    ///
    /// NOTHING HERE IS FATAL TO THE PASS: an unreadable cluster, packet or
    /// log, or a refused write, is logged by name and tried again next
    /// pass, and the three-hour clock in `reap_dead_gate_runs` stays the
    /// ceiling over all of it.
    async fn record_carried_verdicts(&self) -> Result<()> {
        let ns = self.cfg.gate_namespace.clone();
        let jobs = self.cluster.gate_jobs(&ns).await?;
        let mut by_packet: std::collections::BTreeMap<&str, Vec<GateJob>> = Default::default();
        for j in &jobs {
            by_packet
                .entry(j.packet.as_str())
                .or_default()
                .push(j.clone());
        }
        for (packet, jobs) in by_packet {
            if !jobs.iter().any(|j| j.carrier) {
                continue;
            }
            let run = match self.get_job(packet).await {
                Ok(run) => run,
                Err(e) => {
                    log(format!(
                        "reconcile: gate-run {} has a verdict-carrying Job but could not be read \
                         this pass (retries next): {e:#}",
                        id8(packet)
                    ));
                    continue;
                }
            };
            // A verdict waiting on a read is said ONCE per state, not once
            // per pass (review 47319ee7, N1): `waiting` latches it by packet.
            let waiting_dir = Path::new(&self.cfg.home).join("gate-verdict-waiting");
            let waiting = |kind: &str, line: String| {
                if let Some(line) = waiting_line(&waiting_dir, packet, Some((kind, line))) {
                    log(line);
                }
            };
            // The common case for a day after every gate: already
            // recorded. Silent, or the journal is this line 40 times a pass.
            if verdict_owed(&run).is_err() {
                waiting_line(&waiting_dir, packet, None);
                continue;
            }
            let job = match carrier_subject(&jobs) {
                Ok(job) => job,
                Err(why) => {
                    waiting(
                        "no-subject",
                        format!(
                            "reconcile: gate-run {} owes a verdict and is not recorded from a \
                             pod log — {why}",
                            id8(packet)
                        ),
                    );
                    continue;
                }
            };
            // THE POD FIRST: whether the gate container has ended, and the
            // termination message that names its receipt. A read that
            // fails settles nothing — "could not see the pod" is not "the
            // pod is gone", which only a list that answered empty says.
            let pod = match self.cluster.gate_pod(&ns, &job.name).await {
                Ok(pod) => pod,
                Err(e) => {
                    waiting(
                        "pod-unreadable",
                        format!(
                            "reconcile: gate-run {} owes a verdict but the pod of its Job {} \
                             could not be read ({e:#}) — NOT settled; the \
                             {GATE_DEADLINE_HOURS}h clock is the ceiling",
                            id8(packet),
                            job.name
                        ),
                    );
                    continue;
                }
            };
            // The log only when the pod's end names a receipt in it.
            let text = if !needs_log(&pod) {
                String::new()
            } else {
                match self.cluster.gate_log(&ns, &job.name).await {
                    Ok(text) => text,
                    Err(e) => {
                        waiting(
                            "log-unreadable",
                            format!(
                                "reconcile: gate-run {} owes a verdict but the log of its Job \
                                 {} could not be read ({e:#}) — NOT settled; the \
                                 {GATE_DEADLINE_HOURS}h clock is the ceiling",
                                id8(packet),
                                job.name
                            ),
                        );
                        continue;
                    }
                }
            };
            let (verdict, receipt) = match judge(&run, job, &pod, &text, &ns, Utc::now()) {
                Carried::Wait(_) => continue,
                Carried::Record { verdict, receipt } => (verdict, receipt),
                Carried::Lost { receipt } => ("lost".to_string(), receipt),
            };
            log(format!(
                "reconcile: gate-run {} — recording `{verdict}` from the pod log of Job {} \
                 ({} bytes of receipt)",
                id8(packet),
                job.name,
                receipt.len()
            ));
            let step = find_step(&run, "record-verdict", "Record the gate verdict");
            if let Err(e) = self
                .complete_step(
                    &run,
                    step,
                    &[("verdict", Some(verdict)), ("receipt", Some(receipt))],
                )
                .await
            {
                log(format!(
                    "reconcile: gate-run {} — the verdict from Job {} was NOT recorded this pass \
                     (retries next; the log keeps it): {e:#}",
                    id8(packet),
                    job.name
                ));
            }
        }
        Ok(())
    }

    /// Say on the PACKET that its verdict is waiting on the recorder
    /// (backlog 06ae925a). The read of the gate Jobs failed on every pass
    /// for a day and the only record was one line in this pod's log; the
    /// people and verbs waiting on a gate read the gate-run. So while the
    /// read fails, each open gate-run that owes a verdict and has been
    /// quiet long enough gets one metadata note through the merge door,
    /// and on the pass the read works again each note still standing on
    /// an open run is removed. `carried_verdict::blocked_note` decides,
    /// pure and tested; this reads the packets and writes.
    ///
    /// NEVER FATAL, AND QUIET WHEN THERE IS NOTHING TO SAY: an unreadable
    /// run or a refused write is logged and the rest are still told, and
    /// a run already carrying this outage's note is not written again
    /// (the merge door records an event per write). It runs only on a
    /// failing pass and on the one that recovers, so a working cluster
    /// pays nothing for it.
    async fn note_recorder_blocked(
        &self,
        blocked: Option<&(String, String)>,
        now: DateTime<Utc>,
    ) -> Result<()> {
        let runs = rows(
            self.api(
                Method::GET,
                "/api/jobs?kind=gate-run&status=open&limit=100",
                None,
            )
            .await?,
        )?;
        for r0 in runs {
            let Ok(rid) = job_id(&r0) else { continue };
            let run = match self.get_job(rid).await {
                Ok(run) => run,
                Err(e) => {
                    log(format!(
                        "reconcile: gate-run {} unreadable, so not told its verdict is waiting \
                         on the recorder (retries next): {e:#}",
                        id8(rid)
                    ));
                    continue;
                }
            };
            let Some(note) = carried_verdict::blocked_note(
                &run,
                blocked.map(|(since, cause)| (since.as_str(), cause.as_str())),
                gate_run_quiet_minutes(&run, now),
                now,
            ) else {
                continue;
            };
            log(format!(
                "reconcile: gate-run {} — {} the note that its verdict is waiting on the \
                 recorder ({})",
                id8(rid),
                if note.is_null() {
                    "removing"
                } else {
                    "writing"
                },
                carried_verdict::RECORDER_BLOCKED_KEY
            ));
            if let Err(e) = self
                .merge_job_metadata(rid, vec![(carried_verdict::RECORDER_BLOCKED_KEY, note)])
                .await
            {
                log(format!(
                    "reconcile: gate-run {} — that note was NOT written this pass (retries \
                     next): {e:#}",
                    id8(rid)
                ));
            }
        }
        Ok(())
    }

    /// The gate-run half of reconcile: record what runners left, THEN
    /// bury the dead. Returns nothing, because nothing in it may stop
    /// the pass that boards and merges trains — each half's error is
    /// said and the other half still runs.
    ///
    /// THE ORDER IS THE POINT. A run with a receipt waiting in its pod
    /// log must be recorded with it before either clock in the reap can
    /// call it `lost`, which cannot be taken back.
    ///
    /// ITS OWN METHOD so a test drives these exact lines (review
    /// e0ecb2f6, F4/m22): the first version sat inline in `reconcile`
    /// behind `!cfg!(test)`, and nothing proved the pass was called,
    /// called first, or survived. `reconcile` itself has no test in this
    /// crate — it opens with a `git clone` of the upstream — so its one
    /// line calling this is held by a source pin beside the tests.
    ///
    /// A cluster that cannot be read is said when it starts, hourly while
    /// it lasts and when its cause changes — not once per pass, and not
    /// once per outage (`cluster_read_line`): this runs every two
    /// minutes. And it is said WHERE A READER IS: on each gate-run it
    /// keeps waiting (`note_recorder_blocked`), not only in this pod's log.
    pub(super) async fn settle_gate_runs(&self, now: DateTime<Utc>) {
        let read = self.record_carried_verdicts().await;
        let failure = read.as_ref().err().map(|e| format!("{e:#}"));
        let latch = Path::new(&self.cfg.home).join("gate-jobs-read.failing");
        let said = cluster_read_line(&latch, failure.as_deref(), Utc::now());
        let recovered = failure.is_none() && said.is_some();
        if let Some(line) = said {
            log(line);
        }
        // The outage as the latch holds it; a latch that could not be
        // written still names this pass's failure.
        let blocked = failure.as_deref().map(|why| {
            carried_verdict::outage(&latch)
                .unwrap_or_else(|| (crate::gate::stamp(now), carried_verdict::cause_of(why)))
        });
        if (blocked.is_some() || recovered)
            && let Err(e) = self.note_recorder_blocked(blocked.as_ref(), now).await
        {
            log(format!(
                "reconcile: the gate-runs kept waiting could not be told so this pass \
                 (non-fatal; retries next): {e:#}"
            ));
        }
        if let Err(e) = self.reap_dead_gate_runs(now).await {
            log(format!("reconcile: gate-run reap failed (non-fatal): {e}"));
        }
    }

    /// A change that landed buries its own verdicts. A closed gate-run
    /// whose verdict was `failed` or `lost` stays a red row on the yard's
    /// approach until a car names its branch, a later green answers it,
    /// or an operator annotates it `superseded` — and a change that went
    /// to main through the emergency lane has none of those, so its dead
    /// gate sat red on the yard for a day (fix/lean-ci-builds, lost
    /// 2026-09-04, buried by hand 2026-09-05). This is the machine's
    /// version of that annotation: if the run's sha is already an
    /// ancestor of main, the question it raised is answered by main
    /// itself. `verdict_to_bury` decides, pure and tested; this is the
    /// adapter that asks git and writes the annotation the yard reads.
    async fn bury_landed_verdicts(&self, now: DateTime<Utc>) -> Result<()> {
        let runs = rows(
            self.api(
                Method::GET,
                "/api/jobs?kind=gate-run&status=closed&limit=100&full=true",
                None,
            )
            .await?,
        )?;
        let clone = self.cfg.clone.clone();
        for run in runs {
            let Some((sha, verdict)) = verdict_to_bury(&run, now) else {
                continue;
            };
            let landed = sh_unchecked(&[
                "git",
                "-C",
                &clone,
                "merge-base",
                "--is-ancestor",
                &sha,
                "origin/main",
            ])?
            .status
            .success();
            if !landed {
                continue;
            }
            let main_sha = stdout_str(&sh_unchecked(&[
                "git",
                "-C",
                &clone,
                "rev-parse",
                "--short",
                "origin/main",
            ])?)
            .trim()
            .to_string();
            let rid = job_id(&run)?.to_string();
            let branch = metadata_map(&run)
                .get("branch")
                .and_then(Value::as_str)
                .unwrap_or("(no branch)")
                .to_string();
            log(format!(
                "reconcile: gate-run {} ({branch}) went {verdict}, but its sha {} is an ancestor of main ({main_sha}) — the change landed; burying the verdict",
                id8(&rid),
                &sha[..sha.len().min(7)]
            ));
            self.merge_job_metadata(
                &rid,
                vec![(
                    "superseded",
                    json!(format!(
                        "landed on main: {} is an ancestor of {main_sha} — the change went in without this gate (a re-gate or the emergency lane); buried by the conductor's reconcile",
                        &sha[..sha.len().min(7)]
                    )),
                )],
            )
            .await?;
        }
        Ok(())
    }

    /// The train gate as RECORDED, for a train whose ci step is already
    /// judged: the standing of the gate-run the train names, read and
    /// never filed or relaunched — the judgement is made; this keeps it
    /// in force (`train_gate::judged_verdict`). `None` when the train
    /// never had a gate-run; an unreadable run reads as pending, so a
    /// blip holds the train rather than merging it on CI alone.
    async fn recorded_train_gate(
        &self,
        t: &Value,
        tid: &str,
    ) -> (Option<crate::train_gate::Standing>, u32) {
        use crate::train_gate::{self as tg, Standing};
        let relaunches = t
            .pointer(&format!("/metadata/{}", tg::KEY_RELAUNCHES))
            .and_then(Value::as_u64)
            .unwrap_or(0) as u32;
        let Some(run_id) = t
            .pointer(&format!("/metadata/{}", tg::KEY_RUN))
            .and_then(Value::as_str)
        else {
            return (None, relaunches);
        };
        match self.get_job(run_id).await {
            Ok(run) => (Some(tg::standing(&run)), relaunches),
            Err(e) => {
                log(format!(
                    "train {}: could not re-read its judged gate-run {} this pass ({e}) — holding",
                    id8(tid),
                    id8(run_id)
                ));
                (Some(Standing::Pending), relaunches)
            }
        }
    }

    /// THE TRAIN GATE, read or filed (design 128b5496). Returns the
    /// gate's standing (None when no gate-run is on the train yet) and
    /// the relaunch count, for `train_gate::combined_verdict`. Every
    /// failure here is logged and read as "pending": a gate the
    /// conductor could not file or read this pass is filed or read the
    /// next, and the train waits — it never merges on CI alone. Two
    /// callers: `board`, once, right after the PR is recorded (so the
    /// gate starts in the pass that opens the PR, backlog 95c349a5),
    /// and the ci-step block of every `reconcile` pass, which is the
    /// retry when that first launch failed.
    async fn train_gate(
        &self,
        t: &mut Value,
        tid: &str,
        now: DateTime<Utc>,
    ) -> (Option<crate::train_gate::Standing>, u32) {
        use crate::train_gate::{self as tg, Standing};
        let relaunches = t
            .pointer(&format!("/metadata/{}", tg::KEY_RELAUNCHES))
            .and_then(Value::as_u64)
            .unwrap_or(0) as u32;
        let run_id = t
            .pointer(&format!("/metadata/{}", tg::KEY_RUN))
            .and_then(Value::as_str)
            .map(str::to_string);
        if let Some(run_id) = run_id {
            let run = match self.get_job(&run_id).await {
                Ok(r) => r,
                Err(e) => {
                    log(format!(
                        "train {}: could not read its gate-run {} this pass ({e}) — reading it as running",
                        id8(tid),
                        id8(&run_id)
                    ));
                    return (Some(Standing::Pending), relaunches);
                }
            };
            let standing = tg::standing(&run);
            if tg::wants_relaunch(&standing, relaunches) {
                let why = match &standing {
                    Standing::Refused(w) => w.clone(),
                    _ => "the Job ended without a verdict".to_string(),
                };
                log(format!(
                    "train {}: {}",
                    id8(tid),
                    tg::describe(Some(&standing), relaunches)
                ));
                if !self.cfg.dry {
                    let mut refusals = t
                        .pointer(&format!("/metadata/{}", tg::KEY_REFUSALS))
                        .and_then(Value::as_array)
                        .cloned()
                        .unwrap_or_default();
                    refusals.push(json!({
                        "gate_run": run_id,
                        "why": why,
                        "at": now.to_rfc3339(),
                    }));
                    if let Err(e) = self
                        .merge_job_metadata(
                            tid,
                            vec![
                                (tg::KEY_RUN, Value::Null),
                                (tg::KEY_RELAUNCHES, json!(relaunches + 1)),
                                (tg::KEY_REFUSALS, json!(refusals)),
                            ],
                        )
                        .await
                    {
                        log(format!(
                            "train {}: could not record the gate refusal ({e}) — retrying next pass",
                            id8(tid)
                        ));
                        return (Some(standing), relaunches);
                    }
                    if let Ok(fresh) = self.get_job(tid).await {
                        *t = fresh;
                    }
                }
                return (Some(standing), relaunches + 1);
            }
            return (Some(standing), relaunches);
        }
        // No gate-run yet: file one. Not in a dry run, and not past the
        // cluster's gate bound — the next pass tries again.
        if self.cfg.dry {
            log(format!("DRY: would file a train gate for {}", id8(tid)));
            return (None, relaunches);
        }
        match self.launch_train_gate(t, tid).await {
            Ok(run_id) => {
                log(format!(
                    "train {}: train gate filed — gate-run {} on {}",
                    id8(tid),
                    id8(&run_id),
                    self.cfg.gate_namespace
                ));
                if let Err(e) = self
                    .merge_job_metadata(
                        tid,
                        vec![
                            (tg::KEY_RUN, json!(run_id)),
                            (tg::KEY_LAUNCHED_AT, json!(now.to_rfc3339())),
                            // Filed: the train no longer waits for a reason.
                            (tg::KEY_WAIT_REASON, Value::Null),
                        ],
                    )
                    .await
                {
                    log(format!(
                        "train {}: gate-run {} launched but could not be recorded on the train ({e}) — \
                         the next pass would file a second gate; retried",
                        id8(tid),
                        id8(&run_id)
                    ));
                }
                if let Ok(fresh) = self.get_job(tid).await {
                    *t = fresh;
                }
                (Some(Standing::Pending), relaunches)
            }
            Err(e) => {
                let failures = t
                    .pointer(&format!("/metadata/{}", tg::KEY_LAUNCH_FAILURES))
                    .and_then(Value::as_u64)
                    .unwrap_or(0) as u32
                    + 1;
                let why = format!("{e:#}");
                let unavailable = failures >= tg::MAX_LAUNCH_FAILURES;
                log(format!(
                    "train {}: {}",
                    id8(tid),
                    tg::launch_failure_line(&why, failures, self.cfg.gate_required)
                ));
                // The count AND the reason: a troubled packet must look
                // troubled, and a count alone drew a healthy train at CI
                // for two hours (48f7aba1).
                let mut kv = vec![
                    (tg::KEY_LAUNCH_FAILURES, json!(failures)),
                    (tg::KEY_WAIT_REASON, json!(why)),
                ];
                if unavailable && !self.cfg.gate_required {
                    kv.push((
                        tg::KEY_FALLBACK,
                        json!(format!(
                            "the train gate could not be filed {} passes running ({why}); CI alone judged this train ({}=0)",
                            tg::MAX_LAUNCH_FAILURES,
                            tg::REQUIRED_ENV
                        )),
                    ));
                }
                if !self.cfg.dry {
                    if let Err(e2) = self.merge_job_metadata(tid, kv).await {
                        log(format!(
                            "train {}: could not record the launch failure ({e2})",
                            id8(tid)
                        ));
                    } else if let Ok(fresh) = self.get_job(tid).await {
                        *t = fresh;
                    }
                }
                if unavailable {
                    (Some(Standing::Unavailable(why)), relaunches)
                } else {
                    (None, relaunches)
                }
            }
        }
    }

    /// File the gate-run for the train branch and create its Job — the
    /// same packet body, manifest rendering and `kubectl create` that
    /// `boss gate` performs, without the operator-facing guards (the
    /// train branch is the conductor's own, freshly assembled on main).
    /// What the train's gate-run says failed (`train_gate::fails`) and
    /// why (`train_gate::fails_excerpt`), for the red-train alert, and
    /// the head it judged (`train_gate::judged_head`), for the question
    /// whether the red lies outside the consist — all off the one GET.
    /// Empty when the train has no gate-run or it cannot be read this
    /// pass — the alert then names what the forge names, as before, and
    /// the train keeps the stall rule; a missing name is never an error
    /// here.
    async fn train_gate_fails(
        &self,
        t: &Value,
    ) -> (Vec<String>, Vec<(String, String)>, Option<String>) {
        let Some(run_id) = t
            .pointer(&format!("/metadata/{}", crate::train_gate::KEY_RUN))
            .and_then(Value::as_str)
        else {
            return (Vec::new(), Vec::new(), None);
        };
        match self.get_job(run_id).await {
            Ok(run) => (
                crate::train_gate::fails(&run),
                crate::train_gate::fails_excerpt(&run),
                crate::train_gate::judged_head(&run),
            ),
            Err(_) => (Vec::new(), Vec::new(), None),
        }
    }

    /// The train's boarded cars as the jobs API holds them now, for the
    /// once-per-car bound on an outside release. ALL or NOTHING: one car
    /// that cannot be read might be the one carrying the stamp, so a
    /// partial read answers empty — no early release, the stall rule
    /// decides (backlog 5541d813).
    async fn boarded_cars(&self, t: &Value) -> Vec<Value> {
        let ids: Vec<&str> = t
            .get("metadata")
            .and_then(|m| m.get("boarded_jobs"))
            .and_then(Value::as_array)
            .map(|a| a.iter().filter_map(Value::as_str).collect())
            .unwrap_or_default();
        let mut cars = Vec::with_capacity(ids.len());
        for id in ids {
            match self.get_job(id).await {
                Ok(car) => cars.push(car),
                Err(_) => return Vec::new(),
            }
        }
        cars
    }

    /// The files one car changed, from the conductor's clone: `git diff
    /// --name-only origin/main...<boarded head>` — three dots, from the
    /// merge-base, so a car on an older base reports its own change and
    /// not main's since. Any git failure answers empty, which names the
    /// car for nothing: no hold, the release alone.
    fn car_changed_files(&self, head: &str) -> Vec<String> {
        let range = format!("origin/main...{head}");
        sh(&[
            "git",
            "-C",
            self.cfg.clone.as_str(),
            "diff",
            "--name-only",
            &range,
        ])
        .ok()
        .filter(|o| o.status.success())
        .map(|o| {
            stdout_str(&o)
                .lines()
                .map(str::trim)
                .filter(|l| !l.is_empty())
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default()
    }

    /// Brake the car(s) a JUDGED red names (a2d4d842): a file the
    /// verdict locates that exactly ONE car aboard changed puts that car
    /// on hold — `boss hold`'s marker on its open review step, so the
    /// dock will not board it and `boss release` takes it off. Without
    /// it the car goes back to the dock, re-boards the next train alone,
    /// goes red again on the same file, and only its second strike stops
    /// it — one more train spent learning what this one already said.
    ///
    /// BEST-EFFORT and silent-proof: every refusal logs a line naming
    /// the car and why, and nothing here can abort the cancel.
    /// Stamp `regate_owed` on each car aboard red train `tid` that the
    /// failure reaches (`dock_regate::owed_by`) — the dock's one remaining
    /// re-gate trigger (backlog 96f02540). `failing` is the red's located
    /// files; empty means it named none, and then every car is owed one,
    /// which was the dock's behaviour for every car on every landing.
    ///
    /// INFALLIBLE BY SIGNATURE, for `hold_named_cars`'s reason: this is on
    /// the auto-cancel path, and a marker that cannot be written costs one
    /// re-gate, never the cancel.
    async fn mark_regates_owed(&self, t: &Value, tid: &str, failing: &[String]) {
        let cars = self.boarded_cars(t).await;
        let now = Utc::now();
        for car in releasable_cars(&cars, tid) {
            let Some(cid) = car.get("id").and_then(Value::as_str) else {
                continue;
            };
            let files = boarded_head(car)
                .map(|head| self.car_changed_files(head))
                .unwrap_or_default();
            let Some(reached) = dock_regate::owed_by(&files, failing) else {
                log(format!(
                    "car {}: the red is in {} — none of its files, so no re-gate is owed",
                    id8(cid),
                    failing.join(", ")
                ));
                continue;
            };
            let stamp = dock_regate::owed_stamp(tid, failing, &reached, now);
            if self.cfg.dry {
                log(format!("DRY: would owe car {} a re-gate", id8(cid)));
                continue;
            }
            match self
                .merge_job_metadata(cid, vec![(dock_regate::REGATE_OWED, stamp)])
                .await
            {
                Ok(()) => log(format!(
                    "car {}: owed a re-gate by red train {} ({})",
                    id8(cid),
                    id8(tid),
                    if failing.is_empty() {
                        "the red named no path".to_string()
                    } else {
                        format!("its files meet {}", reached.join(", "))
                    }
                )),
                Err(e) => log(format!(
                    "car {}: its re-gate could not be owed (non-fatal; it boards as gated): {e}",
                    id8(cid)
                )),
            }
        }
    }

    async fn hold_named_cars(
        &self,
        t: &Value,
        tid: &str,
        named: &[String],
        gate_fails: &[String],
        gate_excerpt: &[(String, String)],
        rollup: Option<&Value>,
    ) {
        let located = verdict_located_files(gate_fails, gate_excerpt, rollup);
        if located.is_empty() {
            log(format!(
                "train {}: judged red names no file — releasing without a hold",
                id8(tid)
            ));
            return;
        }
        let cars = self.boarded_cars(t).await;
        let aboard = releasable_cars(&cars, tid);
        let car_files: Vec<(String, Vec<String>)> = aboard
            .iter()
            .filter_map(|c| {
                let id = c.get("id").and_then(Value::as_str)?;
                let head = boarded_head(c)?;
                Some((id.to_string(), self.car_changed_files(head)))
            })
            .collect();
        let to_hold = cars_to_hold(&located, &car_files);
        if to_hold.is_empty() {
            log(format!(
                "train {}: judged red in {} — no single car aboard changed it, so none is held",
                id8(tid),
                located.join(", ")
            ));
        }
        for (cid, files) in to_hold {
            let Some(car) = aboard
                .iter()
                .find(|c| c.get("id").and_then(Value::as_str) == Some(cid.as_str()))
            else {
                continue;
            };
            let review = match crate::steps::holdable(car) {
                Ok(r) => r,
                Err(why) => {
                    log(format!("car {}: not held — {why}", id8(&cid)));
                    continue;
                }
            };
            if let Some(already) = review
                .get("metadata")
                .and_then(boss_jobs::stranded::hold_reason)
            {
                log(format!("car {}: already held ({already})", id8(&cid)));
                continue;
            }
            let Some(sid) = review.get("id").and_then(Value::as_str) else {
                log(format!(
                    "car {}: not held — its review step has no id",
                    id8(&cid)
                ));
                continue;
            };
            let reason = judged_red_hold_reason(tid, named, &files);
            if self.cfg.dry {
                log(format!("DRY: would hold car {} ({reason})", id8(&cid)));
                continue;
            }
            match self
                .api(
                    Method::PATCH,
                    &format!("/api/jobs/{cid}/steps/{sid}/metadata"),
                    Some(crate::steps::hold_patch(&reason)),
                )
                .await
            {
                Ok(_) => log(format!("held car {}: {reason}", id8(&cid))),
                Err(e) => log(format!(
                    "car {}: hold not written (non-fatal; the cancel and its strike stand): {e}",
                    id8(&cid)
                )),
            }
        }
    }

    /// The evidence `red_outside_consist` judges, read from the
    /// conductor's own clone, where the train was assembled on main:
    /// the consist's changed files (`git diff --name-only
    /// origin/main...<head>` — three dots, from the merge-base, so a
    /// main that moved since assembly adds nothing) and which of the
    /// `failing` files exist in the tree the gate judged. Any git
    /// failure answers empty, which `red_outside_consist` reads as
    /// "not proven" — the train then keeps the stall rule and its
    /// strikes, exactly as before (backlog 5541d813).
    fn consist_evidence(&self, head: &str, failing: &[String]) -> (Vec<String>, Vec<String>) {
        if failing.is_empty() {
            return (Vec::new(), Vec::new());
        }
        let clone = self.cfg.clone.as_str();
        let lines = |args: &[&str]| -> Vec<String> {
            sh(args)
                .map(|o| {
                    stdout_str(&o)
                        .lines()
                        .map(str::trim)
                        .filter(|l| !l.is_empty())
                        .map(str::to_string)
                        .collect()
                })
                .unwrap_or_default()
        };
        let range = format!("origin/main...{head}");
        let consist = lines(&["git", "-C", clone, "diff", "--name-only", &range]);
        let mut ls = vec![
            "git",
            "-C",
            clone,
            "ls-tree",
            "-r",
            "--name-only",
            head,
            "--",
        ];
        ls.extend(failing.iter().map(String::as_str));
        (consist, lines(&ls))
    }

    async fn launch_train_gate(&self, t: &Value, tid: &str) -> Result<String> {
        let train_ref = train_ref_of(t)
            .ok_or_else(|| anyhow!("the train carries no train_ref on its assemble step"))?;
        let (branch, short) = train_ref
            .split_once('@')
            .ok_or_else(|| anyhow!("train_ref {train_ref:?} is not <branch>@<sha>"))?;
        // The full sha from the conductor's own clone, where the branch
        // was assembled; the short one from train_ref if the clone
        // cannot answer.
        let sha = sh(&[
            "git",
            "-C",
            &self.cfg.clone,
            "rev-parse",
            "--verify",
            branch,
        ])
        .ok()
        .filter(|o| o.status.success())
        .map(|o| stdout_str(&o).trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| short.to_string());
        let title = t.get("title").and_then(Value::as_str).unwrap_or("PR train");
        self.launch_gate(
            branch,
            &sha,
            crate::gate::Requester::Train,
            crate::train_gate::packet_marks(tid, title),
        )
        .await
        // The train counts its failed launches and names the cause
        // (48f7aba1); a run filed before the failure is settled lost by
        // the launch itself, and its id rides in this error's text.
        .map_err(anyhow::Error::from)
    }

    /// File a gate-run for `branch@sha`, stamp `marks` on it, and create
    /// its Job — the train's gate, and since backlog 969a1092 the dock's
    /// re-gate of a car main moved under. One launch path, so the two
    /// cannot differ in how a gate is admitted, filed or run.
    async fn launch_gate(
        &self,
        branch: &str,
        sha: &str,
        who: crate::gate::Requester,
        marks: Value,
    ) -> std::result::Result<String, GateLaunchFailed> {
        let admitted: Result<String> = async {
            let manifest_text =
                std::fs::read_to_string(&self.cfg.gate_manifest).with_context(|| {
                    format!(
                        "reading the gate runner manifest {} ({}=…)",
                        self.cfg.gate_manifest,
                        crate::train_gate::MANIFEST_ENV
                    )
                })?;
            let ns = self.cfg.gate_namespace.as_str();
            let max = crate::gate::max_concurrent(&self.http).await?;
            let live = crate::gate::running_gates(ns)?;
            // The train is admitted AT the bound (48f7aba1), a car's re-gate
            // below it: the one predicate `boss gate` also consults.
            if !crate::gate::admits(live.len(), max, who) {
                bail!(
                    "the cluster is at its gate bound ({} running of {max}: {})",
                    live.len(),
                    live.join(", ")
                );
            }
            Ok(manifest_text)
        }
        .await;
        let manifest_text = admitted.map_err(GateLaunchFailed::unfiled)?;
        let ns = self.cfg.gate_namespace.clone();
        // The instance's edit level, read here by the holder of the token
        // and handed to the Job (backlog 934ccad1): the pod that runs the
        // train's tree makes no request for it. Unreadable, none is
        // handed and the gate's own lint says so — never fatal to a launch.
        let edit_level = match self
            .api(Method::GET, crate::dispatch::EDIT_LEVEL_PATH, None)
            .await
        {
            Ok(answer) => answer,
            Err(e) => {
                log(format!(
                    "gate launch for {branch}: the instance's edit level could not be read to \
                     hand to the gate ({e:#}) — none is handed"
                ));
                None
            }
        };
        let start = |run_id: &str| -> Result<()> {
            let job = crate::gate::render_job(&manifest_text, branch, run_id, "--auto")?;
            let job = crate::gate::hand_edit_level(&job, edit_level.as_ref());
            let mut child = crate::gate::kubectl(&ns)
                .args(["create", "-f", "-"])
                .stdin(std::process::Stdio::piped())
                .stdout(std::process::Stdio::piped())
                .stderr(std::process::Stdio::piped())
                .spawn()
                .context("spawning kubectl create — is kubectl in the conductor's image?")?;
            {
                use std::io::Write;
                child
                    .stdin
                    .as_mut()
                    .context("kubectl stdin")?
                    .write_all(job.as_bytes())?;
            }
            let out = child.wait_with_output()?;
            if !out.status.success() {
                bail!(
                    "kubectl create failed for the gate of {}: {}",
                    branch,
                    String::from_utf8_lossy(&out.stderr).trim()
                );
            }
            Ok(())
        };
        self.file_and_start_gate(branch, sha, marks, &start).await
    }

    /// The half of a launch that FILES something: POST the gate-run, stamp
    /// `marks` on it, and `start` its Job (`kubectl create` in production;
    /// a parameter so the failure below is driven by a test).
    ///
    /// A GATE-RUN FILED IS A GATE-RUN ANSWERED (backlog 7919fdcc, item 11).
    /// Until 2026-09-28 a failure AFTER the POST — the marks refused, the
    /// manifest unrenderable, `kubectl create` refused — returned a bare
    /// error, so the caller never learned the packet existed: the dock
    /// stamped `gate_run ''` and its next pass, two minutes later, filed
    /// another, open and unreferenced, every pass until the fault cleared
    /// (the orphan reaper closed each only after its own window). Now the
    /// run is settled `lost` here, with the cause as its receipt, and the
    /// error NAMES it (`GateLaunchFailed::filed`), so the dock can stamp it
    /// and let the red rule bound the retry instead of the two-minute
    /// cadence. A settle that cannot be written is journalled: the run is
    /// then one the orphan reaper closes, as before.
    async fn file_and_start_gate(
        &self,
        branch: &str,
        sha: &str,
        marks: Value,
        start: &(dyn Fn(&str) -> Result<()> + Sync),
    ) -> std::result::Result<String, GateLaunchFailed> {
        let owner = self.owner_for_filing().await;
        let filed: Result<String> = async {
            let created = self
                .api(
                    Method::POST,
                    "/api/jobs",
                    Some(crate::gate::gate_run_body(
                        branch,
                        sha,
                        &self.cfg.gate_manifest,
                        None,
                        &owner,
                    )),
                )
                .await?;
            Ok(created
                .as_ref()
                .and_then(|c| c.get("data").unwrap_or(c).get("id"))
                .and_then(Value::as_str)
                .with_context(|| format!("the jobs API returned no id for {branch}'s gate-run"))?
                .to_string())
        }
        .await;
        let run_id = filed.map_err(GateLaunchFailed::unfiled)?;
        let started: Result<()> = async {
            self.api(
                Method::PATCH,
                &format!("/api/jobs/{run_id}/metadata"),
                Some(marks),
            )
            .await
            .with_context(|| format!("marking gate-run {} for {branch}", id8(&run_id)))?;
            start(&run_id)
        }
        .await;
        match started {
            Ok(()) => {
                // The run's Job exists: stamp it launched, as `boss gate`
                // does (backlog 4d088a7e), so every run carries the one
                // reading the yard dates a bay from. Never fatal — the
                // Job is running either way, and a run without it is
                // read from its filing, which for this path (filed and
                // started in one act, never queued) is seconds apart.
                if let Err(e) = self
                    .api(
                        Method::PATCH,
                        &format!("/api/jobs/{run_id}/metadata"),
                        Some(crate::gate::launched_patch(Utc::now())),
                    )
                    .await
                {
                    log(format!(
                        "gate-run {} for {branch}: could not stamp {} ({e:#}) — it is running",
                        id8(&run_id),
                        crate::gate::LAUNCHED_AT
                    ));
                }
                Ok(run_id)
            }
            Err(cause) => {
                self.settle_unstarted_gate_run(&run_id, branch, &cause)
                    .await;
                Err(GateLaunchFailed {
                    filed: Some(run_id),
                    cause,
                })
            }
        }
    }

    /// Settle a gate-run this conductor filed and could not start: `lost`,
    /// the cause as its receipt (`unstarted_receipt`). NEVER FATAL — the
    /// launch has already failed, and a settle that cannot be written
    /// leaves the run to the orphan reaper, which is said here once.
    async fn settle_unstarted_gate_run(&self, run_id: &str, branch: &str, cause: &anyhow::Error) {
        let settled: Result<()> = async {
            let run = self.get_job(run_id).await?;
            let verdict_step = find_step(&run, "record-verdict", "Record the gate verdict");
            self.complete_step(
                &run,
                verdict_step,
                &[
                    ("verdict", Some("lost".to_string())),
                    ("receipt", Some(unstarted_receipt(branch, cause))),
                ],
            )
            .await
        }
        .await;
        match settled {
            Ok(()) => log(format!(
                "gate-run {} for {branch} was filed but never started ({cause:#}) — settled lost",
                id8(run_id)
            )),
            Err(e) => log(format!(
                "gate-run {} for {branch} was filed but never started ({cause:#}), and could not \
                 be settled ({e:#}) — the orphan reaper closes it",
                id8(run_id)
            )),
        }
    }

    pub(super) async fn reconcile(&self, now: DateTime<Utc>) -> Result<()> {
        // Keep the clone fetched before the convergence check below asks git
        // "is the cluster's running commit a descendant of this train's
        // merge?" — a question answered AGAINST THIS CLONE. reconcile does
        // not board (only boarding called ensure_clone), so without this the
        // clone stays frozen at the last board and lacks the cluster's newer
        // commit; merge-base then exits non-zero (object unknown), is read as
        // "not an ancestor", and every train whose commit was superseded
        // between boards wedges at `converged` forever. On 2026-09-04 three
        // trains wedged exactly this way after the cutover boarded once and
        // then reconciled repeatedly against a stale clone. convergence_verdict's
        // comment claimed reconcile kept the clone fetched; it did not until
        // this line. A fetch failure is non-fatal — ancestry falls to None,
        // which converges nothing and retries next pass, the safe direction.
        // Log a failure rather than swallow it: a silent ensure_clone
        // error (the .ok() this replaces) is exactly how a broken clone
        // stayed invisible while trains wedged.
        if let Err(e) = self.ensure_clone() {
            log(format!(
                "reconcile: ensure_clone failed — ancestry-based convergence \
                 reads None this pass (converges nothing, retries): {e}"
            ));
        }
        // Bury the yard's dead before reading it. A gate pod that dies
        // without recording a verdict leaves its packet at
        // `record-verdict` forever: it holds one of the three gate slots,
        // renders as a live gate, and nothing ever clears it. On
        // 2026-09-04 two such ghosts sat there 17 hours with their
        // branches long landed, and a third silently ate a car — the
        // change was never gated and nobody noticed until a census.
        //
        // gate-runner.yaml already states the intent ("a runner that dies
        // anyway leaves an overdue packet — the alarm the protocol
        // already provides"); the alarm just had nobody listening. This
        // is the listener, and it belongs in reconcile because reconcile
        // IS the verb that makes the record match reality.
        //
        // The ceiling needs no cluster access, only a clock: past the
        // gate Job's own activeDeadlineSeconds, Kubernetes has already
        // killed the Job, so a packet still claiming to gate cannot be.
        // Under it, a run NO Job carries and nothing has held for
        // ORPHAN_GATE_RUN_MINUTES is settled from a read of the gates
        // Role this conductor already holds (backlog 137c176d).
        self.settle_gate_runs(now).await;
        if let Err(e) = self.bury_landed_verdicts(now).await {
            log(format!(
                "reconcile: burying landed verdicts failed this pass (retries next): {e}"
            ));
        }
        if let Err(e) = self.follow_up_merge_lost(now).await {
            log(format!(
                "reconcile: the merge-lost follow-up sweep failed this pass (retries next): {e}"
            ));
        }
        let trains = rows(
            self.api(
                Method::GET,
                "/api/jobs?kind=pr-train&status=open&limit=50",
                None,
            )
            .await?,
        )?;
        let trains_len = trains.len();
        let mut isolated_failures = 0usize;
        for t0 in trains {
            // PER-TRAIN ISOLATION (2026-09-06). One train's failure — a
            // failed observability write, a forge blip, a merge conflict —
            // must never abort the pass and wedge every train behind it.
            // That is what froze all landings for ~8h: a red-train alert
            // POST returned 422 and, filed with `?`, aborted reconcile
            // every pass. Each iteration now runs in its own fallible
            // scope, so a sick train costs itself one pass, not the fleet.
            // (`continue` inside the loop body therefore becomes
            // `return Ok(())` — the same "skip the rest of this train".)
            let outcome: Result<()> = async {
                let tid = job_id(&t0)?.to_string();
                let mut t = self.get_job(&tid).await?;
            // The rules THIS train departed under, which may not be the
            // ones in force now.
            let policy = self.policy_for(&t).await;
            // The stall sentinel first — a train stuck BEFORE its PR
            // (assembly died, push hung) would slip past the
            // pr-step early-continues below and stall invisibly.
            self.note_stall(&t, now, &policy).await?;
            let pr_step = find_step(&t, "pr", "Open the batched PR");
            if !step_done(pr_step) {
                return Ok(()); // this window's board phase, or a stalled assembly
            }
            let pr_url = pr_step
                .and_then(|s| s.get("metadata"))
                .and_then(|m| m.get("pr_url"))
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string();
            if pr_url.is_empty() {
                return Ok(());
            }
            let mut info = self.forge.pr_info(&pr_url).await?;

            // THE TRAIN GATE (design 128b5496): the train's Rust checks
            // are a gate-run of the train branch on the cluster, filed
            // here while the PR is open and unjudged, and the verdict is
            // CI's and the gate's read together (`train_gate`).
            // Once judged, the gate-run is RE-READ (never relaunched)
            // and still combined: the judged arm used to recompute
            // `forge_verdict` alone, and train #361 merged with its gate
            // RED on the tick after its ci step recorded `failing`
            // (2026-09-14, 6f18390b).
            let ci_judged = step_done(find_step(&t, "ci", "CI verdict"));
            let (gate, relaunches) = if ci_judged {
                self.recorded_train_gate(&t, &tid).await
            } else {
                self.train_gate(&mut t, &tid, now).await
            };
            let ci_step = find_step(&t, "ci", "CI verdict");
            let forge_verdict = ci_verdict(info.get("statusCheckRollup"));
            let verdict = if step_done(ci_step) {
                crate::train_gate::judged_verdict(
                    forge_verdict,
                    gate.as_ref(),
                    relaunches,
                    self.cfg.gate_required,
                )
            } else {
                crate::train_gate::combined_verdict(
                    forge_verdict,
                    gate.as_ref(),
                    relaunches,
                    self.cfg.gate_required,
                )
            };
            if !step_done(ci_step) && verdict != "pending" {
                let checks = ci_check_summary(info.get("statusCheckRollup"));
                // WHY, not just WHICH: the tail of each failing job's log,
                // resolved through /actions/runs/{run}/jobs (empty on green
                // or when no log could be fetched).
                let check_logs = format_check_logs(
                    &failing_check_logs(info.get("statusCheckRollup")),
                    CI_STEP_LOG_BYTES,
                );
                let gate_line = crate::train_gate::describe(gate.as_ref(), relaunches);
                let gate_run = t
                    .pointer(&format!("/metadata/{}", crate::train_gate::KEY_RUN))
                    .and_then(Value::as_str)
                    .map(str::to_string);
                self.complete_step(
                    &t,
                    ci_step,
                    &[
                        ("result", Some(verdict.to_string())),
                        // Both halves of the verdict, so a reader sees
                        // which one spoke.
                        ("forge_result", Some(forge_verdict.to_string())),
                        ("train_gate", Some(gate_line)),
                        ("train_gate_run", gate_run),
                        // WHICH check, not just that one failed.
                        ("checks", (!checks.is_empty()).then_some(checks)),
                        // The failing job's log tail — a verdict names WHY.
                        ("check_logs", (!check_logs.is_empty()).then_some(check_logs)),
                    ],
                )
                .await?;
            } else if step_done(ci_step) {
                // The step has already recorded its verdict and cannot
                // record another — terminal rows are frozen. Compare
                // against the last verdict we NOTICED (the job stamp,
                // falling back to the step's original) so this fires on
                // each change rather than on every ten-minute tick.
                let md = t.get("metadata");
                let noticed = md
                    .and_then(|m| m.get("ci_verdict_latest"))
                    .and_then(Value::as_str)
                    .or_else(|| {
                        ci_step
                            .and_then(|s| s.get("metadata"))
                            .and_then(|m| m.get("result"))
                            .and_then(Value::as_str)
                    });
                if let Some(note) = verdict_drift(noticed, verdict) {
                    log(format!("train {}: {note}", id8(&tid)));
                    if !self.cfg.dry {
                        self.merge_job_metadata(
                            &tid,
                            vec![
                                ("ci_verdict_latest", json!(verdict)),
                                ("ci_verdict_changed_at", json!(now.to_rfc3339())),
                            ],
                        )
                        .await?;
                        t = self.get_job(&tid).await?;
                    }
                }
            }

            // Asked and unanswered. Stamped once, like the stall
            // sentinel, so a hung runner produces one line rather than
            // one every ten minutes for as long as it hangs.
            if !truthy(t.get("metadata").and_then(|m| m.get("ci_overdue_since")))
                && let Some(note) = ci_overdue(&t, now, self.cfg.ci_hours)
            {
                log(format!("train {}: {note}", id8(&tid)));
                if !self.cfg.dry {
                    self.merge_job_metadata(
                        &tid,
                        vec![("ci_overdue_since", json!(now.to_rfc3339()))],
                    )
                    .await?;
                    t = self.get_job(&tid).await?;
                }
            }

            // The overnight rule, before the merge check: a train that
            // is red — or whose run was killed without judging anything
            // — AND has stopped moving releases its consist so the next
            // window can board without it. Decided on the LIVE verdict
            // just read, never on the `ci` step's first stamp. Whether
            // the release counts against the cars is a separate
            // question, and only a returned failing verdict answers it
            // yes (`verdict_strikes_cars`).
            // A red train announces ITSELF, immediately — not only when it
            // stalls out into auto-cancel below, and not only when a human
            // asks. One urgent packet naming the failing check, deduped, so
            // a red train is never a surprise (d69c4274).
            //
            // BEST-EFFORT, and that is load-bearing: filing this alert is
            // observability, and observability must NEVER abort the pass
            // that boards, merges, and auto-cancels. The first cut filed it
            // with `?`, so a malformed body (HTTP 422) aborted reconcile at
            // rc=1 every pass — one red train froze all landings for ~8h
            // (2026-09-06). Any error here now logs and the pass continues,
            // so a broken alert is at worst a missing alert, never a wedge.
            // The gate's failing checks are read only on a red pass —
            // one extra GET when there is something to name.
            let (gate_fails, gate_excerpt, gate_head) = if verdict == "failing" {
                self.train_gate_fails(&t).await
            } else {
                (Vec::new(), Vec::new(), None)
            };
            if info.get("state").and_then(Value::as_str) == Some("OPEN")
                && let Some(alert) = red_train_alert(
                    &t,
                    verdict,
                    info.get("statusCheckRollup"),
                    &gate_fails,
                    &gate_excerpt,
                )
            {
                if self.cfg.dry {
                    log(format!(
                        "DRY: would alert on red train {} ({})",
                        id8(&tid),
                        alert.title
                    ));
                } else if let Err(e) = self.announce_red_train(&t, &alert).await {
                    log(format!(
                        "train {} red — alert filing failed (non-fatal, reconcile continues): {e}",
                        id8(&tid)
                    ));
                }
            }

            // The yard's cancel button (7a24caf3): an operator's
            // `cancel_requested` stamp is honoured before the automatic
            // rule and never strikes the cars. Non-fatal by construction
            // — the method returns a bool, so nothing here can abort the
            // pass — and a request claims the train's pass whether the
            // cancel succeeded, is dry, or is being retried.
            if self
                .honour_cancel_request(&t, &tid, info.get("state").and_then(Value::as_str))
                .await
            {
                return Ok(());
            }

            // A red in files no car aboard changed releases the consist
            // NOW and UNSTRUCK (backlog 5541d813): trains 36692142 and
            // 578a0ee9 each held the one track on a failure no car
            // touched until an operator cancelled by hand, because the
            // only other way out was six hours of stall and a strike on
            // every car. Proven from the record or not at all — the
            // receipt's located failures, the judged head's diff — and a
            // red in a file a car DID change keeps the stall rule and its
            // strikes. It releases; it never merges.
            let outside = gate_head.as_deref().and_then(|head| {
                let failing: Vec<String> = gate_fails
                    .iter()
                    .filter_map(|e| fails_entry_path(e))
                    .collect();
                let (consist, present) = self.consist_evidence(head, &failing);
                red_outside_consist(
                    &gate_fails,
                    info.get("statusCheckRollup"),
                    &consist,
                    &present,
                )
            });
            // ONCE PER CAR: the cars aboard are read only when the red is
            // proven outside, and a car already stamped by an earlier
            // outside release sends the train to today's path — the
            // livelock bound for a pair red only when assembled.
            let decision = match outside.as_deref() {
                Some(files) => {
                    let cars = self.boarded_cars(&t).await;
                    outside_consist_cancel_reason(&t, verdict, files, &cars)
                }
                None => None,
            };
            let spent_note = match &decision {
                Some(OutsideRelease::Spent(note)) => {
                    log(format!(
                        "train {}: {note} — holding for the stall rule",
                        id8(&tid)
                    ));
                    Some(note.clone())
                }
                _ => None,
            };
            // A JUDGED red — CI and the train gate both finished, one of
            // them red, the red naming its check — does not wait out the
            // stall rule (a2d4d842): trains f7bd1e9d and 02801b05 held
            // the one track 40 and 27 minutes on 2026-09-24, each red on
            // a named `CI / web` with its gate red on the same
            // svelte-check, until an operator cancelled by hand. The
            // six-hour rule stays the backstop for every red this does
            // not judge.
            let judged = judged_red_checks(
                &t,
                forge_verdict,
                gate.as_ref(),
                info.get("statusCheckRollup"),
                &gate_fails,
            );
            // (reason, strike, outside release granted)
            let cancel = match decision {
                Some(OutsideRelease::Release(reason)) => Some((reason, false, true)),
                _ => judged
                    .as_deref()
                    .map(|named| judged_red_cancel_reason(named, policy.stall_hours))
                    .or_else(|| auto_cancel_reason(&t, verdict, now, policy.stall_hours))
                    .map(|reason| {
                        // Anything but a granted release is today's path,
                        // strikes and all — an unread consist or a spent
                        // release proves nothing for the cars.
                        let reason = match &spent_note {
                            Some(note) => format!("{reason} — {note}"),
                            None => reason,
                        };
                        let strike = verdict_strikes_cars(verdict, info.get("statusCheckRollup"));
                        (reason, strike, false)
                    }),
            };
            if self.cfg.auto_cancel
                && info.get("state").and_then(Value::as_str) == Some("OPEN")
                && let Some((reason, strike, outside_release)) = cancel
            {
                log(format!("train {} auto-cancelling: {reason}", id8(&tid)));
                // The car the failure names is braked BEFORE the release
                // clears its train marker, so it never reads boardable
                // in between. Best-effort: a hold that cannot be written
                // leaves the cancel and its strike standing.
                if !outside_release && let Some(named) = judged.as_deref() {
                    self.hold_named_cars(
                        &t,
                        &tid,
                        named,
                        &gate_fails,
                        &gate_excerpt,
                        info.get("statusCheckRollup"),
                    )
                    .await;
                }
                // THE ONE RE-GATE LEFT (backlog 96f02540): a red that
                // judged the cars owes a re-gate to the cars it released
                // whose files intersect the failure — or, when the red
                // names no path, to every car it released. Before the
                // release, like the hold above, and best-effort like it.
                if strike && !outside_release {
                    self.mark_regates_owed(
                        &t,
                        &tid,
                        &verdict_located_files(
                            &gate_fails,
                            &gate_excerpt,
                            info.get("statusCheckRollup"),
                        ),
                    )
                    .await;
                }
                if self.cfg.dry {
                    log(format!("DRY: would cancel {} ({reason})", id8(&tid)));
                } else {
                    self.cancel_train(&tid, &reason, strike, outside_release)
                        .await?;
                }
                return Ok(());
            }

            let pr_state = info.get("state").and_then(Value::as_str);
            // Second lock on the same door: the ci step's RECORDED
            // verdict. The live reading above is what merges; this is
            // the frozen judgement, and a train judged failing or
            // aborted never merges whatever the live reading says —
            // the merge observed against the record, never assumed.
            let judged_red = find_step(&t, "ci", "CI verdict")
                .and_then(|s| s.get("metadata"))
                .and_then(|m| m.get("result"))
                .and_then(Value::as_str)
                .is_some_and(|r| matches!(r, "failing" | "aborted"));
            if judged_red && verdict == "green" && pr_state == Some("OPEN") {
                log(format!(
                    "train {}: live verdict green but the ci step is judged red — NOT merging (6f18390b)",
                    id8(&tid)
                ));
            }
            if self.cfg.auto_merge && verdict == "green" && !judged_red && pr_state == Some("OPEN") {
                log(format!(
                    "CI green — merging {pr_url} (train protocol 27ab7680)"
                ));
                if !self.cfg.dry {
                    let message = self.squash_message_for(&t).await;
                    self.forge.merge(&pr_url, &message).await?;
                    info = self.forge.pr_info(&pr_url).await?;
                }
            } else if let Some(why) = merge_declined_reason(self.cfg.auto_merge, verdict, pr_state)
            {
                // A decline says so. Silence here cost 2026-09-04 hours —
                // see `merge_declined_reason`. Not stamped-once like the
                // overdue sentinel: green-and-unmerged is a train stopped
                // one step from landing, and it should read as stopped on
                // every pass until the switch is on or the operator merges.
                log(format!("CI green on {pr_url} but NOT merging — {why}"));
            }

            let merged_step = find_step(&t, "merged", "Merged into main");
            if info.get("state").and_then(Value::as_str) == Some("MERGED")
                && !step_done(merged_step)
            {
                let merge_ref: String = info
                    .get("mergeCommit")
                    .and_then(|m| m.get("oid"))
                    .and_then(Value::as_str)
                    .unwrap_or("unknown")
                    .chars()
                    .take(12)
                    .collect();
                let boarded: Vec<String> = t
                    .get("metadata")
                    .and_then(|m| m.get("boarded_jobs"))
                    .and_then(Value::as_array)
                    .map(|a| {
                        a.iter()
                            .filter_map(|v| v.as_str().map(str::to_string))
                            .collect()
                    })
                    .unwrap_or_default();
                // Close every boarded car BEST-EFFORT, then complete the
                // train's `merged` step — and only when nothing failed.
                //
                // ORDER IS LOAD-BEARING. The first cut completed `merged`
                // FIRST and looped the cars with `?`: one car whose close
                // write errored aborted the (isolated) per-train scope, but
                // `merged` was already `completed`, so the retry guard above
                // (`state==MERGED && !step_done(merged)`) was false forever
                // after — the OTHER landed cars never got their close
                // markers and their car Jobs stayed open as residue,
                // inflating the open-car count and starving boarding. Now a
                // bad car costs only itself, and a partial pass leaves
                // `merged` pending so the next reconcile retries the
                // stragglers. Every close write is idempotent, so the retry
                // re-closes only the car that did not close before.
                let failures = self
                    .close_boarded_cars(&tid, &boarded, &merge_ref, &pr_url)
                    .await;
                if failures == 0 {
                    self.complete_step(&t, merged_step, &[("merge_ref", Some(merge_ref.clone()))])
                        .await?;
                } else {
                    log(format!(
                        "train {} merged, but {failures} car(s) failed to close this pass — \
                         holding the `merged` step pending so the next reconcile retries them",
                        id8(&tid)
                    ));
                }
                t = self.get_job(&tid).await?;
            }

            let merged_step = find_step(&t, "merged", "Merged into main");
            let deployed_step = find_step(&t, "deployed", "Deployed to the playground");
            if step_done(merged_step) && !step_done(deployed_step) {
                let deployed_step = deployed_step
                    .ok_or_else(|| anyhow!("deployed step missing on job {}", id8(&tid)))?;
                self.deploy(&t, deployed_step).await?;
                t = self.get_job(&tid).await?;
            }
            // Installation is not the finish line either — the cluster
            // must be SERVING the merge before the train can claim
            // arrival (fdff316c / 7e5ee013, decided 2026-08-19).
            // Trains admitted under the pre-converged spec have no
            // such step and skip this whole pass — version pinning
            // working as designed, nothing stranded.
            let converged_step = find_step(&t, "converged", "Cluster converged");
            if step_done(find_step(&t, "deployed", "Deployed to the playground"))
                && converged_step.is_some()
                && !step_done(converged_step)
                && let Err(e) = self.verify_convergence(&t, now).await
            {
                // Convergence checking must not fail the run whose
                // deploys succeeded — the next pass looks again, and
                // the overdue alarm bounds the silence.
                log(format!("convergence check failed (run stands): {e}"));
            }
                Ok(())
            }
            .await;
            if let Err(e) = outcome {
                isolated_failures += 1;
                let tid = t0
                    .get("id")
                    .and_then(Value::as_str)
                    .map(id8)
                    .unwrap_or_else(|| "?".to_string());
                log(format!(
                    "reconcile: train {tid} failed this pass — isolated, other trains continue: {e}"
                ));
            }
        }
        // Isolation must not become a blind spot. If EVERY train failed
        // this pass, that is almost never N independent per-train faults —
        // it is a systemic outage (forge / API / auth) that per-train
        // logging would scatter into noise indistinguishable from an
        // all-green pass. Say so, loudly and once, so a total downstream
        // failure surfaces rather than hiding behind the very isolation
        // that protects against the single-bad-train case.
        if trains_len > 0 && isolated_failures == trains_len {
            log(format!(
                "reconcile: ALL {trains_len} train(s) failed this pass — likely a SYSTEMIC \
                 outage (forge/API/auth), not per-train faults; investigate"
            ));
        }
        // Housekeeping must not fail a run whose real work succeeded.
        // The sweep runs last, after merges, deploys and evidence are
        // recorded; on 2026-08-13 a single un-deletable branch made
        // every reconcile report rc=1 and re-file its arrival report,
        // which reads as "the conductor is broken" when the trains had
        // in fact landed. Journal the failure, keep the verb green.
        if let Err(e) = self.sweep_landed_branches().await {
            log(format!(
                "branch sweep failed (housekeeping, run stands): {e}"
            ));
        }
        // The dock's merge preview rides the same tick (12a25f3e):
        // best-effort like the sweep — a failed preview journals and
        // the reconcile stands, because a projection that sometimes
        // lags is stale-not-wrong by design.
        if let Err(e) = self.preview_dock(now).await {
            log(format!("dock preview failed (projection, run stands): {e}"));
        }
        // A stranded green — gated, never parked — rots silently until a
        // human runs orient and reads it. This makes that detection
        // active: a green past the threshold with no car files a
        // backlog-item so the overdue/watchlist machinery can see it.
        // BEST-EFFORT, like the sweep and preview above: this froze
        // delivery for 8h once (a fatal write in reconcile stops ALL
        // landings — boss-conductor-loop-writes-must-not-be-fatal), so
        // any failure — a bad read, the POST filing the packet — journals
        // one visible line and the reconcile stands. Never .ok()-swallow:
        // a silent failure here is exactly how a strand stays unfiled.
        if let Err(e) = self.alarm_stranded_greens(now).await {
            log(format!(
                "stranded-green alarm failed (housekeeping, run stands): {e}"
            ));
        }
        Ok(())
    }

    /// File a best-effort backlog-item for each stranded green past its
    /// window that no open alarm already names, REFRESH the alarm of a
    /// strand that persists, and CLOSE the alarm of a strand that ended.
    /// Detection is `census::stranded_gate_runs` (which reads
    /// `boss_jobs::stranded`, the one definition the yard uses, §9a);
    /// the pure selection + dedup is `stranded_greens_to_alarm`; this
    /// method is only the I/O around them. Reads the same closed
    /// gate-runs orient reads (a gate-run CLOSES on its verdict, so a
    /// `status=open` query would miss every green). Returns `Err` on a
    /// read/write failure so the caller can journal it — the caller
    /// wraps this non-fatally.
    ///
    /// THE CLEAR IS NOT OPTIONAL. Raising and never revisiting the
    /// claim left four false STRANDED GREEN packets on the operator's
    /// queue on 2026-09-09, closed by hand (e60398dc): each branch
    /// parked, landed or was held within minutes of the alarm.
    async fn alarm_stranded_greens(&self, now: DateTime<Utc>) -> Result<()> {
        let gate_runs = rows(
            self.api(
                Method::GET,
                "/api/jobs?kind=gate-run&limit=60&full=true",
                None,
            )
            .await?,
        )?;
        // EVERY car, not one page: at 832 ship-a-change packets on
        // 2026-09-09 the old `limit=800` was already truncated, and the
        // rows it dropped were the OLDEST — so a landed branch whose car
        // had aged off the page read as "no car" and alarmed
        // (a-limit-is-not-a-filter). Open AND closed: a landing closes
        // the car, and a closed car still answers "this branch has one".
        let cars = list_all_pages(|offset| async move {
            self.api(
                Method::GET,
                &format!("/api/jobs?kind=ship-a-change&limit={PAGE_LIMIT}&offset={offset}"),
                None,
            )
            .await
        })
        .await?;
        let car_branches: BTreeSet<String> = cars
            .iter()
            .filter_map(|c| {
                c.get("metadata")
                    .and_then(|m| m.get("branch"))
                    .and_then(Value::as_str)
                    .map(str::to_string)
            })
            .filter(|b| !b.is_empty())
            .collect();
        // Dedup set: the branches an OPEN backlog-item alarm already
        // names in `metadata.stranded_branch`. A persisting strand is one
        // packet, not one every ten minutes. (A closed-then-still-
        // stranded green re-files — a recurrence after a human answered
        // is a new fact, the same call estate.alarm makes.)
        //
        // Read PAST page one. A bare `limit=200` treated a truncated
        // page as the whole set, so once open backlog-items passed 200
        // the existing alarm sat beyond the page, `already_alarmed`
        // missed it, and the strand re-filed every reconcile pass — a
        // self-compounding flood. `list_all_pages` pages on `total`
        // until every matching row is read (same paginator boarding and
        // the dock preview use), so the dedup set is complete.
        let open_alarms = list_all_pages(|offset| async move {
            self.api(
                Method::GET,
                &format!(
                    "/api/jobs?kind=backlog-item&status=open&limit={PAGE_LIMIT}&offset={offset}"
                ),
                None,
            )
            .await
        })
        .await?;
        let already_alarmed: BTreeSet<String> = open_alarms
            .iter()
            .filter_map(|j| {
                j.get("metadata")
                    .and_then(|m| m.get("stranded_branch"))
                    .and_then(Value::as_str)
                    .map(str::to_string)
            })
            .collect();
        let windows = StrandWindows {
            never_parked_mins: self.cfg.stranded_alarm_mins,
            auto_park_grace_mins: self.cfg.auto_park_grace_mins,
        };
        let to_alarm =
            stranded_greens_to_alarm(&gate_runs, &car_branches, &already_alarmed, now, windows);
        for a in &to_alarm {
            log(format!(
                "reconcile: stranded green {} — gated green {}min, no car, no open alarm; filing backlog-item",
                a.branch, a.age_mins
            ));
            if self.cfg.dry {
                continue;
            }
            let owner = self.owner_for_filing().await;
            self.api(
                Method::POST,
                "/api/jobs",
                Some(stranded_alarm_body(a, windows, now, &owner)),
            )
            .await?;
        }
        // A STANDING alarm is refreshed, never twinned: the strand is
        // still true, and the packet should carry today's measurement
        // rather than the age it was filed with (the silence sweep's
        // idiom). `already_alarmed` kept these out of `to_alarm`.
        let stranded_now: BTreeSet<String> =
            crate::census::stranded_gate_runs(&gate_runs, &car_branches)
                .into_iter()
                .collect();
        for j in &open_alarms {
            let Some(branch) = j
                .get("metadata")
                .and_then(|m| m.get("stranded_branch"))
                .and_then(Value::as_str)
            else {
                continue;
            };
            if !stranded_now.contains(branch) {
                continue;
            }
            let (Some(id), Some((gate_run_id, age_mins, park_intent))) = (
                j.get("id").and_then(Value::as_str),
                freshest_green(&gate_runs, branch, now),
            ) else {
                continue;
            };
            if self.cfg.dry {
                continue;
            }
            let measured = StrandedGreen {
                branch: branch.to_string(),
                gate_run_id,
                age_mins,
                cause: StrandCause::from_intent(park_intent),
            };
            self.api(
                Method::PATCH,
                &format!("/api/jobs/{id}/metadata"),
                Some(stranded_refresh_patch(&measured, now)),
            )
            .await?;
        }
        // AND THE ALARM CLOSES ITSELF when the claim stops holding.
        for (id, branch) in stranded_alarms_to_clear(&open_alarms, &stranded_now) {
            let why = stranded_clear_reason(&gate_runs, &car_branches, &branch);
            log(format!(
                "reconcile: stranded-green alarm {} for {branch} no longer holds ({why}); closing it",
                id8(&id)
            ));
            if self.cfg.dry {
                continue;
            }
            self.close_alarm_stale(&id, "stranded-green", stranded_clear_writes(&branch, &why))
                .await?;
        }
        // THE LEFT-BEHIND ALARM CLOSES ITSELF TOO (backlog 7919fdcc, item
        // 6), off the two reads this pass has already made: every open
        // backlog-item and every car.
        self.clear_left_behind_alarms(&open_alarms, &cars).await;
        Ok(())
    }

    /// Close each open left-behind alarm whose car's streak has ended
    /// (`left_behind_alarms_to_clear`) on its `stale` terminal. NEVER
    /// FATAL: the stranded half of this pass has already run, and an
    /// alarm that could not be closed is closed by the next pass, which
    /// finds the same car in the same state.
    async fn clear_left_behind_alarms(&self, open_alarms: &[Value], cars: &[Value]) {
        for (id, branch, why) in left_behind_alarms_to_clear(open_alarms, cars) {
            log(format!(
                "reconcile: left-behind alarm {} for {branch} no longer holds ({why}); closing it",
                id8(&id)
            ));
            if self.cfg.dry {
                continue;
            }
            if let Err(e) = self
                .close_alarm_stale(&id, "left-behind", left_behind_clear_writes(&branch, &why))
                .await
            {
                log(format!(
                    "reconcile: left-behind alarm {} could not be closed ({e:#}) — the next \
                     pass tries again",
                    id8(&id)
                ));
            }
        }
    }

    /// Close an alarm backlog-item the conductor filed, with `writes`
    /// (its `stale` disposition, evidence and `cleared_by`), at the step
    /// the packet is WAITING ON — the one close both self-clearing alarms
    /// use.
    ///
    /// NOT ALWAYS `triage` (backlog e61093a1). This completed the triage
    /// step only, so once a person had routed an alarm to `build` the
    /// merge door refused the write 409 as a write to a terminal step:
    /// measured 2026-09-28 07:10Z on LEFT BEHIND alarms 11ea6b8b and
    /// 80b961bf, each urgent and unclosable, every pass. Where it closes
    /// now is the dispatcher's own judgement,
    /// [`boss_dispatcher_handlers::handlers::common::retraction`], which
    /// cured the same defect in the sensor and estate closers (a2d8bad3):
    /// an open `triage` (what `boss triage <id> stale` does), else a
    /// READY `measure`/`build` completed `stale` — both reach the row's
    /// `stale` terminal — else the packet is told once, in metadata,
    /// that its claim ended: an ACTIVE step belongs to its executor, and
    /// completing a dispatched run's step would land the run as delivered.
    async fn close_alarm_stale(
        &self,
        id: &str,
        what: &str,
        writes: Map<String, Value>,
    ) -> Result<()> {
        use boss_dispatcher_handlers::handlers::common::{
            RECOVERED_AT, Retraction, recovery_note, retraction,
        };
        let Some(job) = self
            .api(Method::GET, &format!("/api/jobs/{id}"), None)
            .await?
        else {
            return Ok(());
        };
        match retraction(&job) {
            None => {
                // A packet with no triage step cannot close itself. Say
                // so rather than silently leaving a false alarm.
                log(format!(
                    "reconcile: {what} alarm {} has no triage step; left open",
                    id8(id)
                ));
            }
            Some(Retraction::Complete { slug, step_id }) => {
                for (method, path, body) in step_completion_writes(id, &step_id, writes) {
                    self.api(method, &path, Some(body)).await?;
                }
                log(format!(
                    "reconcile: {what} alarm {} closed stale at `{slug}`",
                    id8(id)
                ));
            }
            // Told once: a packet already carrying the note is not
            // rewritten every pass.
            Some(Retraction::Annotate { .. })
                if job
                    .pointer(&format!("/metadata/{RECOVERED_AT}"))
                    .is_some_and(|v| !v.is_null()) => {}
            Some(Retraction::Annotate { why_open }) => {
                let text = |k: &str| writes.get(k).and_then(Value::as_str).unwrap_or_default();
                let note = recovery_note(
                    text("evidence"),
                    text("cleared_by"),
                    &Utc::now().to_rfc3339(),
                    &why_open,
                );
                self.api(
                    Method::PATCH,
                    &format!("/api/jobs/{id}/metadata"),
                    Some(Value::Object(note)),
                )
                .await?;
                log(format!(
                    "reconcile: {what} alarm {} has ended but stays open ({why_open}); told \
                     the packet",
                    id8(id)
                ));
            }
        }
        Ok(())
    }

    /// The dock's SHA-anchored merge preview (12a25f3e): every
    /// parked-ready car gets `metadata.merge_preview` — clean-or-
    /// conflicted vs current main, pairwise conflicts across the
    /// parked set, anchored to main@sha + a parked-set hash so a moved
    /// input reads STALE rather than wrong. Written only on CHANGE
    /// (`dock_preview::changed`): the 10-minute tick is a heartbeat,
    /// not an event source.
    async fn preview_dock(&self, now: DateTime<Utc>) -> Result<()> {
        use crate::dock_preview as dp;
        let clone = &self.cfg.clone;
        // Every parked car needs its merge preview, so read past page
        // one: a tail car left off would silently get no preview, and
        // the pairwise-conflict set would be computed over an incomplete
        // dock — a "clean" preview that hides a real conflict.
        let listed = list_all_pages(|offset| async move {
            self.api(
                Method::GET,
                &format!(
                    "/api/jobs?kind=ship-a-change&status=open&limit={PAGE_LIMIT}&full=true&offset={offset}"
                ),
                None,
            )
            .await
        })
        .await?;
        let mut cars: Vec<(String, Value, String)> = Vec::new(); // (id, job, branch)
        for j0 in listed {
            let jid = job_id(&j0)?.to_string();
            // A car off the dock keeps no preview (20d0d717): nothing
            // here rewrites it, so it would name the dock it last saw
            // forever. Cleared once, on the tick after it leaves — and
            // best-effort, so one failed write cannot cost the parked
            // cars their preview this tick; it is retried on the next.
            if left_the_dock_with_a_preview(&j0) && !self.cfg.dry {
                match self
                    .merge_job_metadata(&jid, vec![("merge_preview", Value::Null)])
                    .await
                {
                    Ok(_) => log(format!(
                        "{}: merge preview cleared (left the dock)",
                        id8(&jid)
                    )),
                    Err(e) => log(format!("{}: merge preview clear failed: {e}", id8(&jid))),
                }
            }
            if !parked_ready(&j0) {
                continue;
            }
            let Some(branch) = j0
                .pointer("/metadata/branch")
                .and_then(Value::as_str)
                .map(str::to_string)
            else {
                continue;
            };
            cars.push((jid, j0, branch));
        }
        if cars.is_empty() {
            return Ok(());
        }
        // Bring main + every parked branch into temp refs the trial
        // merges can address; refs/preview/* is cleaned each tick so a
        // deleted branch does not linger as a phantom.
        //
        // BEST-EFFORT PER BRANCH (2026-09-06). One car whose branch has
        // vanished — rerailed, deleted, or held with its branch removed —
        // must not abort the whole preview: a single combined fetch with
        // a missing refspec exits rc=128 and blanked the entire dock
        // projection every pass. `main` is required (the baseline); each
        // car branch is fetched on its own, and a car whose ref does not
        // resolve is dropped from the preview (it is not boardable anyway).
        let dir = Some(Path::new(clone.as_str()));
        sh_in(
            dir,
            true,
            &[
                "git",
                "fetch",
                "--quiet",
                "origin",
                "+refs/heads/main:refs/preview/main",
            ],
        )?;
        for (_, _, b) in &cars {
            let refspec = format!("+refs/heads/{b}:refs/preview/{b}");
            // check=false: a vanished branch is expected here and handled
            // by the resolve-and-drop below, not an error.
            let _ = sh_in(dir, false, &["git", "fetch", "--quiet", "origin", &refspec]);
        }
        let rev = |r: &str| -> Option<String> {
            let out = sh_in(dir, false, &["git", "rev-parse", "--verify", "--quiet", r]).ok()?;
            if !out.status.success() {
                return None;
            }
            let s = String::from_utf8_lossy(&out.stdout).trim().to_string();
            (!s.is_empty()).then_some(s)
        };
        let main_sha =
            rev("refs/preview/main").ok_or_else(|| anyhow!("preview: main ref did not resolve"))?;
        let mut pairs: Vec<(String, String)> = Vec::new();
        cars.retain(|(_, _, b)| match rev(&format!("refs/preview/{b}")) {
            Some(sha) => {
                pairs.push((b.clone(), sha));
                true
            }
            None => {
                log(format!(
                    "preview: branch {b} did not resolve (vanished?) — dropped from the dock preview"
                ));
                false
            }
        });
        if cars.is_empty() {
            return Ok(());
        }
        let set = dp::set_hash(clone, &pairs)?;
        let stamp = boss_jobs::car::stamp(now);

        // vs main, then pairwise. n is dock-sized (<=24 by WIP limit);
        // n^2 in-memory merges is cheap next to one real boarding.
        let mut vs_main: Vec<dp::Verdict> = Vec::new();
        for (_, _, b) in &cars {
            vs_main.push(dp::trial_merge(
                clone,
                "refs/preview/main",
                &format!("refs/preview/{b}"),
            )?);
        }
        for (i, (jid, job, b)) in cars.iter().enumerate() {
            let mut co: Vec<(String, Vec<String>)> = Vec::new();
            for (k, (_, _, other)) in cars.iter().enumerate() {
                if i == k {
                    continue;
                }
                if let dp::Verdict::Conflicts(files) = dp::trial_merge(
                    clone,
                    &format!("refs/preview/{b}"),
                    &format!("refs/preview/{other}"),
                )? {
                    co.push((other.clone(), files));
                }
            }
            // `pairs` was pushed in step with `cars.retain`, so index i
            // is this car's measured head.
            let head = pairs.get(i).map(|(_, h)| h.as_str()).unwrap_or_default();
            let fresh = dp::preview_payload(&vs_main[i], &co, &main_sha, head, &set, &stamp);
            let stored = job.pointer("/metadata/merge_preview");
            if dp::changed(stored, &fresh) && !self.cfg.dry {
                self.merge_job_metadata(jid, vec![("merge_preview", fresh)])
                    .await?;
                log(format!(
                    "{}: merge preview updated (vs-main {}, {} co-boarder conflict(s))",
                    id8(jid),
                    if matches!(vs_main[i], dp::Verdict::Clean) {
                        "clean"
                    } else {
                        "CONFLICT"
                    },
                    co.len(),
                ));
            }
        }
        Ok(())
    }

    /// The stall sentinel: stamp `stalled_since` (once) when an open
    /// train's newest step completion ages past the threshold; clear
    /// the stamp when the train advances. Raising is protocol,
    /// cancelling is judgment — nothing here auto-cancels; the
    /// operator's verb for that is `boss train cancel`.
    async fn note_stall(
        &self,
        t: &Value,
        now: DateTime<Utc>,
        policy: &DeliveryPolicy,
    ) -> Result<()> {
        let tid = job_id(t)?;
        let stamped = truthy(t.get("metadata").and_then(|m| m.get("stalled_since")));
        match stall_age_hours(t, now, policy.stall_hours) {
            Some(age) if !stamped => {
                log(format!(
                    "train {} stalled ({age}h, threshold {}h)",
                    id8(tid),
                    policy.stall_hours
                ));
                // Since WHEN: the newest completion — the moment
                // progress provably stopped, not the moment the
                // sentinel happened to look.
                let since = newest_completion(t).unwrap_or_default().to_string();
                self.merge_job_metadata(tid, vec![("stalled_since", json!(since))])
                    .await?;
            }
            None if stamped => {
                log(format!("train {} advanced — stall stamp cleared", id8(tid)));
                self.merge_job_metadata(tid, vec![("stalled_since", Value::Null)])
                    .await?;
            }
            _ => {}
        }
        Ok(())
    }

    /// Reconcile's arrival sweep: delete landed cars' branches from
    /// the forge (protocol decision, David). The repo auto-deletes
    /// merged `train/*` PR heads, but each CAR branch survives its
    /// squash-merged content landing — and ancestry cannot prove the
    /// landing, so nothing git-side can ever say "safe to sweep".
    /// The job record can: once a train has closed (arrived) and a
    /// boarded car closed with the merged outcome, that branch's
    /// work is on main and the conductor deletes it. A 404 is a fine
    /// answer — something got there first, and the sweep says nothing
    /// about it (see `sweep_note`). A train with nothing left that
    /// could become deletable is stamped `branches_swept`
    /// (`sweep_complete`), so the steady state costs the list read and
    /// no per-car fetches.
    ///
    /// Cost: one jobs-list PAGE per hundred closed trains, plus per
    /// UNSWEPT train one fetch per boarded car, one `branch_head` per
    /// deletable branch, and one delete of the train's own branch (a
    /// silent 404 once it is gone). The `branches_swept` stamp bounds
    /// the per-train work, not the read — the read pages the whole
    /// closed set, because the stamp cannot bound what it has not
    /// seen.
    ///
    /// THE LEAK THIS PAGING FIXES (measured 2026-09-10, packet
    /// 02069932). This read was `limit=50` under a comment claiming
    /// coverage was never capped. It was capped at 50, and the window
    /// turns over fast: a consist check that refuses opens and closes
    /// a cancelled train about once a minute, so ~50 minutes of
    /// refusals push every arrived train off page one. A train is
    /// stamped only once all its cars are terminal, so a car still
    /// open at `proven` — the residue we spend sessions draining —
    /// leaves its train unstamped, and once the window has turned
    /// over that train is never read again and its landed branches
    /// stay on the forge for good. Self-aggravating: proof delay
    /// causes the leak, and proof delay is what we drain.
    ///
    /// Measured: 971 closed trains, eight branches of merged+closed
    /// cars still on the forge, 529 consist-refused trains closed in
    /// the preceding nine hours. Two of those eight belong to train
    /// 82a643b4, whose cars closed 46 seconds AFTER the only reconcile
    /// that pass — it was still inside the window then, so the cap is
    /// not what held those two that hour; every later reconcile exited
    /// on `another conductor run holds the lock` (a separate defect,
    /// backlog), and by the time the sweep runs again the window has
    /// turned over 10 times and the cap is what keeps them leaked.
    async fn sweep_landed_branches(&self) -> Result<()> {
        // Every closed train, not the newest page of them: the one
        // paginator this file shares with candidates,
        // open_car_branches, preview_dock and probe_dock_depth.
        let arrived = list_all_pages(|offset| async move {
            self.api(
                Method::GET,
                &format!(
                    "/api/jobs?kind=pr-train&status=closed&limit={PAGE_LIMIT}&offset={offset}"
                ),
                None,
            )
            .await
        })
        .await?;
        let pending = sweep_pending(&arrived);
        if pending.is_empty() {
            return Ok(());
        }
        // Branches still-open cars name, fetched once per pass: a
        // live car's claim beats any landed car's deletion.
        let open_branches = self.open_car_branches().await?;
        for t in pending {
            // PER-TRAIN ISOLATION (mirrors reconcile's, 2026-09-06).
            // The sweep is housekeeping and its CALLER already keeps the
            // reconcile green — but the sweep had no per-item isolation
            // of its own, so one persistent failure (a boarded car Job
            // deleted → 404 at get_job, a malformed arrival-report
            // PATCH, a forge blip on branch_head/delete_branch) aborted
            // the WHOLE sweep on a `?` every pass. Every LATER pending
            // train then went unswept and its landed `train/*` and car
            // branches accumulated on the forge — the recurring
            // forge-disk fill. Each train now runs in its own fallible
            // scope: a sick train costs itself one pass, not the fleet.
            let t_id = t.get("id").and_then(Value::as_str).unwrap_or("?");
            let outcome: Result<()> = async {
                let tid = job_id(t)?;
                let boarded: Vec<String> = t
                    .get("metadata")
                    .and_then(|m| m.get("boarded_jobs"))
                    .and_then(Value::as_array)
                    .map(|a| {
                        a.iter()
                            .filter_map(|v| v.as_str().map(str::to_string))
                            .collect()
                    })
                    .unwrap_or_default();
                let mut cars = Vec::with_capacity(boarded.len());
                for cid in &boarded {
                    cars.push(self.get_job(cid).await?);
                }
                // The full record, once per unswept train: the arrival
                // report and the branch cleanup both read its steps,
                // which the list rows do not carry.
                let train = self.get_job(tid).await?;
                self.file_arrival_report(&train, &cars).await?;
                self.clean_arrived_train_branch(&train).await;
                // PER-BRANCH ISOLATION. One un-sweepable branch must not
                // strand the train's OTHER landed branches on the forge:
                // a `?` here would abort this train and leave its clean
                // siblings undeleted (disk debt) until the failing one
                // healed. A branch that failed also leaves the train
                // UNSTAMPED below, so it is revisited next pass rather
                // than marked swept with the branch leaked.
                let mut branch_failures = 0usize;
                // THE RECORD OF THIS PASS, one row per branch decided,
                // written on the train beside the stamp so the stamp
                // carries its evidence (1096b1a4: six leaked branches
                // under stamped trains, and the only trace of what the
                // forge had answered was a journal outside the cluster).
                let mut report: Vec<Value> = Vec::new();
                for b in deletable_branches(&cars, &open_branches) {
                    let branch_outcome: Result<()> = async {
                        // The job record proved the CONTENT landed; the head
                        // guard proves the branch still holds only that
                        // content. Both, or the branch stays (car 23923b40).
                        // For a rerail original the recorded head is the one
                        // the rerail read off it, so a commit pushed after the
                        // rerail keeps the original exactly as a commit pushed
                        // after boarding keeps a car's own branch.
                        let current = self.forge.branch_head(&b.branch).await?;
                        let guard = sweep_guard(b.head.as_deref(), current.as_deref());
                        // Verdicts that keep a branch narrate themselves, and
                        // a branch already off the forge narrates nothing.
                        if let Some(note) = sweep_note(&guard, &b) {
                            log(note);
                        }
                        match &guard {
                            SweepGuard::Gone => report.push(sweep_report_row(&b, "gone before this pass")),
                            SweepGuard::NoRecord => report.push(sweep_report_row(&b, "kept: no boarded head on record")),
                            SweepGuard::Moved { recorded, current } => report.push(sweep_report_row(
                                &b,
                                &format!("kept: moved since boarding ({} -> {})", &recorded[..recorded.len().min(8)], &current[..current.len().min(8)]),
                            )),
                            SweepGuard::Delete => {}
                        }
                        if guard == SweepGuard::Delete {
                            let what = sweep_subject(&b);
                            if self.cfg.dry {
                                log(format!(
                                    "DRY: would delete {what} (car {} landed)",
                                    id8(&b.car)
                                ));
                                return Ok(());
                            }
                            // THE DELETE IS OBSERVED, NEVER ASSUMED. The forge's
                            // answer is a claim; the branch read back afterwards
                            // is the fact. A branch still present after an
                            // answered delete is a failure of THIS branch: the
                            // train stays pending, the row says what the forge
                            // said, and the line is loud.
                            let claimed = self.forge.delete_branch(&b.branch).await?;
                            let after = self.forge.branch_head(&b.branch).await?;
                            match sweep_delete_verdict(claimed, after.as_deref()) {
                                SweepDelete::Deleted => {
                                    log(format!("deleted {what} (car {} landed)", id8(&b.car)));
                                    report.push(sweep_report_row(&b, "deleted"));
                                }
                                SweepDelete::AlreadyGone => {
                                    // It existed a moment ago — something else
                                    // swept it between the two calls. Rare, and
                                    // worth saying so it is not read as our doing.
                                    log(format!("{what} already gone (car {} landed)", id8(&b.car)));
                                    report.push(sweep_report_row(&b, "already gone"));
                                }
                                SweepDelete::StillPresent { forge_said } => {
                                    let head = after.unwrap_or_default();
                                    log(sweep_still_present_line(&b, forge_said, &head));
                                    report.push(sweep_report_row(
                                        &b,
                                        &format!("still present after DELETE answered \"{forge_said}\""),
                                    ));
                                    bail!(
                                        "{what} still on the forge after DELETE answered \"{forge_said}\""
                                    );
                                }
                            }
                        }
                        Ok(())
                    }
                    .await;
                    if let Err(e) = branch_outcome {
                        branch_failures += 1;
                        log(sweep_branch_failed_line(&b.branch, &b.car, &e));
                    }
                }
                // A branch withheld for a still-open car's claim is not
                // swept — it is deferred, and it becomes deletable the
                // moment that car closes. Named here so the train's
                // pending state has a stated reason, and counted so the
                // stamp below cannot close over it.
                let deferred = claim_deferred_branches(&cars, &open_branches);
                for b in &deferred {
                    log(claim_deferred_line(b));
                    report.push(sweep_report_row(b, "kept: a still-open car claims it"));
                }
                // A boarded car that is not terminal is the other reason a
                // train stays pending; name its branch and the step that
                // holds it, so the report is complete, not only correct.
                report.extend(unsettled_rows(&cars));
                // Stamp swept only when EVERY branch was handled: a
                // branch we could not sweep this pass must be revisited,
                // and the stamp is what drops the train off the pending
                // list. Stamping over an un-swept branch leaks it onto
                // the forge forever — the very debt this isolation
                // exists to stop. The report is written EITHER WAY, in
                // the same PUT as the stamp when there is one: an
                // unstamped train says on its own record why it is
                // still pending, and a stamped one says what each
                // branch's delete was observed to do.
                let mut kv: Vec<(&str, Value)> = vec![("sweep_report", json!(report))];
                if sweep_complete(branch_failures, deferred.len(), &cars) {
                    kv.push(("branches_swept", json!("true")));
                }
                self.merge_job_metadata(tid, kv).await?;
                Ok(())
            }
            .await;
            if let Err(e) = outcome {
                log(sweep_train_failed_line(t_id, &e));
            }
        }
        Ok(())
    }

    /// File the arrival report — the landing's final structured entry
    /// — on an arrived train's `arrived` step, once. The sweep is the
    /// conductor's visit to every arrived train, so the report is
    /// composed here from the full job record plus the boarded cars
    /// the sweep already fetched. It lands on the job through the
    /// metadata merge (`job_metadata_patch`), so the job's other keys
    /// survive the filing.
    async fn file_arrival_report(&self, train: &Value, cars: &[Value]) -> Result<()> {
        let tid = job_id(train)?;
        let Some(step) = find_step(train, "arrived", "Train arrived") else {
            return Ok(());
        };
        let filed = arrival_already_filed(train);
        // Strictly `completed` — never `skipped`: a cancelled train
        // closes with its arrived step SKIPPED, and a landing report
        // on a train that never landed would be fiction.
        let arrived = step.get("status").and_then(Value::as_str) == Some("completed");
        if !arrived || filed {
            return Ok(());
        }
        let report = arrival_report(train, cars);
        let summary = arrival_summary(&report);
        let n = report
            .get("consist")
            .and_then(Value::as_array)
            .map_or(0, Vec::len);
        let total = report
            .get("timings")
            .and_then(|t| t.get("total_s"))
            .and_then(Value::as_i64)
            .map_or_else(|| "?".to_string(), |s| s.to_string());
        if self.cfg.dry {
            log(format!(
                "DRY: would file the arrival report on {}",
                id8(tid)
            ));
            return Ok(());
        }
        // THE REPORT LANDS ON THE JOB, NOT THE STEP (defect f402a681).
        //
        // It used to PUT onto the `arrived` step's metadata — and the
        // guard above requires that step to be `completed`, so the only
        // write this function could ever attempt was a write to a
        // TERMINAL step. Once terminal steps became immutable, every
        // attempt returned 409 "step is terminal — these fields are
        // immutable", and because this returns Err, it took
        // `sweep_landed_branches` down with it on the `?` at the call
        // site: no arrival report AND no branch cleanup, every ten
        // minutes, for weeks.
        //
        // The 409's own hint names the fix: "To correct or annotate it,
        // write to the parent job's metadata (PATCH /api/jobs/{id}/
        // metadata) instead." That endpoint MERGES top-level keys, so
        // no overlay is needed here — the merge is the server's job.
        //
        // The report is a fact ABOUT the train, not a field of the
        // transition that recorded arrival, so the job is where it
        // belonged anyway. `summary` is written as `arrival_summary`
        // because a bare `summary` on job metadata is a name anything
        // could want.
        // THE TRAIN'S TIERS (ba429e7f, design 01c3cc3f), beside the
        // report: the union of its cars' `software_tiers` and the cars
        // per tier — one row per train for the series `boss channels`
        // and the production view read, written at arrival because
        // that is when the consist is final. A car parked before the
        // stamp existed counts as unclassified rather than as a tier.
        let (tiers, counts) = crate::channels::train_tiers(cars.iter());
        self.api(
            Method::PATCH,
            &format!("/api/jobs/{tid}/metadata"),
            Some(json!({
                "arrival_report": report,
                "arrival_summary": summary,
                boss_jobs::car::SOFTWARE_TIERS: tiers,
                "software_tier_counts": counts,
            })),
        )
        .await?;
        log(format!(
            "arrival report on {} ({n} cars, total {total}s)",
            id8(tid)
        ));
        Ok(())
    }

    /// Housekeeping at arrival: the train's OWN branch comes off the
    /// forge once the landing is on the record — the same forge
    /// delete the cancel path has always used, now owned by the happy
    /// path too (`arrival_branch_to_delete` says when and which).
    /// Infallible by signature: a delete that fails is a journal line
    /// and the arrival stands — a leftover branch is debt, a failed
    /// arrival is an outage.
    async fn clean_arrived_train_branch(&self, train: &Value) {
        let Some(branch) = arrival_branch_to_delete(train, &self.cfg.forge_kind) else {
            return;
        };
        if self.cfg.dry {
            log(format!("DRY: would delete branch {branch} (train arrived)"));
            return;
        }
        let outcome = self.forge.delete_branch(&branch).await;
        if let Some(note) = arrival_cleanup_note(&branch, outcome) {
            log(note);
        }
    }

    /// The branches named by still-open ship-a-change cars — never
    /// deletable, whoever landed on them. Read off the list rows
    /// (the jobs list returns full metadata); an open car with no
    /// branch yet contributes nothing.
    async fn open_car_branches(&self) -> Result<BTreeSet<String>> {
        // Every open car's branch, past page one: an older open car
        // sorts to the tail, and a capped read that misses it would let
        // the sweep delete a branch a still-open car names.
        let listed = list_all_pages(|offset| async move {
            self.api(
                Method::GET,
                &format!(
                    "/api/jobs?kind=ship-a-change&status=open&limit={PAGE_LIMIT}&offset={offset}"
                ),
                None,
            )
            .await
        })
        .await?;
        Ok(listed
            .iter()
            .filter_map(|j| {
                j.get("metadata")
                    .and_then(|m| m.get("branch"))
                    .and_then(Value::as_str)
                    .filter(|b| !b.is_empty())
                    .map(str::to_string)
            })
            .collect())
    }

    // -----------------------------------------------------------------------
    // Phase 2 — board this window's train
    // -----------------------------------------------------------------------

    fn ensure_clone(&self) -> Result<()> {
        let clone = &self.cfg.clone;
        if !Path::new(clone).join(".git").is_dir() {
            // A dir left from a partial/interrupted clone — present but
            // with no .git — makes `git clone` refuse ("destination
            // exists and is not empty"). Swallowed by a caller's .ok(),
            // that leaves reconcile with no clone and every superseded
            // train wedged at `converged` (2026-09-04: three trains, and
            // the reconcile ran in 0s because the clone fast-failed).
            // Clear the stale dir so the clone can proceed; a valid clone
            // has .git and never reaches here.
            if Path::new(clone).exists() {
                let _ = fs::remove_dir_all(clone);
            }
            fs::create_dir_all(&self.cfg.home)?;
            sh(&["git", "clone", &self.cfg.upstream_url, clone])?;
            sh(&[
                "git",
                "-C",
                clone,
                "remote",
                "add",
                "fork",
                &self.cfg.fork_url,
            ])?;
            // The merge commits the assembly makes need an author, and the
            // honest one is the machine that made them (a fresh clone has
            // no identity — the first real run failed exactly here).
            sh(&[
                "git",
                "-C",
                clone,
                "config",
                "user.name",
                "BOSS train conductor",
            ])?;
            sh(&[
                "git",
                "-C",
                clone,
                "config",
                "user.email",
                "train-conductor@boss.invalid",
            ])?;
        }
        sh(&["git", "-C", clone, "fetch", "origin", "--prune"])?;
        sh(&["git", "-C", clone, "fetch", "fork", "--prune"])?;
        Ok(())
    }

    /// The parked-ready cars whose branch is actually on the fork,
    /// plus the left-behind record for the ones whose branch is not
    /// — each of those gets its `skip_reason` stamped (the yard's
    /// "LEFT BEHIND" chip) and an entry for the train's own books — and
    /// the dock's re-gates in flight, which a departure may wait for.
    ///
    /// ONE DEFINITION OF THE DOCK'S JUDGEMENT, for two callers: `board`
    /// departs what it returns, and `refresh` (design 42279fb2) walks the
    /// dock between departures for the re-gates it launches and nothing
    /// else.
    async fn candidates(&self) -> Result<DockPass> {
        let mut out = Vec::new();
        let mut left_behind = Vec::new();
        let mut held = Vec::new();
        let mut judged = std::collections::HashMap::new();
        // EVERY open car, not just page one. A car opened days ago but
        // parked today sorts to the tail (`ORDER BY opened_on DESC`), so
        // a bare `limit=` boards nothing from the tail once the backlog
        // passes a page — the silent starvation this fix exists for.
        let listed = list_all_pages(|offset| async move {
            self.api(
                Method::GET,
                &format!(
                    "/api/jobs?kind=ship-a-change&status=open&limit={PAGE_LIMIT}&offset={offset}"
                ),
                None,
            )
            .await
        })
        .await?;
        // The cars whose bay claim this walk decides as it goes; every
        // other claim at the dock is withdrawn after it (design 38f3a488).
        let mut settled = HashSet::new();
        for j0 in &listed {
            let jid = job_id(j0)?.to_string();
            let j = self.get_job(&jid).await?;
            if !parked_ready(&j) {
                continue;
            }
            // The two-strike hold. Without it the auto-cancel above is
            // a loop: the same consist re-boards, goes red, cancels,
            // and burns the night landing nothing.
            if let Some(reason) = car_hold_reason(&j, self.policy.max_red_trains) {
                log(format!("{}: {reason} — leaving behind", id8(&jid)));
                left_behind.push(json!({"car_id_short": id8(&jid), "reason": reason.as_str()}));
                // NOT counted into the departure's left-behind streak: a
                // struck car is the strike rule's hold, already waiting on
                // a person, not the dock's (review of car 5eb1967e, M3).
                // Written only when it CHANGES, like every hold on this
                // walk (see the bay write below): the refresh walks the
                // dock every two minutes, and a reason the car already
                // carries is an audit event for nothing (df93994b).
                let kv = vec![("skip_reason", json!(reason))];
                if !self.cfg.dry && !metadata_already(&j, &kv) {
                    self.merge_job_metadata(&jid, kv).await?;
                }
                continue;
            }
            // THE DECLARED ORDERING EDGE (d3320278). Judged here, beside
            // the two-strike hold, because both answer "this car is green
            // and still must not ride yet" — a question about the car's
            // WORLD, not its content — and both are cheaper than the git
            // work below. A car with no edge costs nothing: no read is
            // made at all, which is what keeps the regression surface of
            // this change to the cars that opt in.
            if let Some(hold) = self.edge_hold(&j, &jid).await {
                log(format!("{}: {} — leaving behind", id8(&jid), hold.reason));
                left_behind.push(json!({
                    "car_id_short": id8(&jid),
                    "reason": hold.reason.as_str(),
                    EDGE_HOLD: hold.kind,
                }));
                let kv = vec![("skip_reason", json!(hold.reason))];
                if !self.cfg.dry && !metadata_already(&j, &kv) {
                    self.merge_job_metadata(&jid, kv).await?;
                }
                // A RED THE RECEIPT HAS ANSWERED COMES OFF HERE TOO (backlog
                // 7919fdcc, item 9). The clear below sits past this
                // `continue`, so an edge-held car whose builder repaired
                // and re-gated it kept the old red — drawn "not boardable"
                // by the yard and orient — until its predecessor released
                // it. Judged against the FORGE's head, not the fork's: this
                // path publishes nothing, the forge is where both the
                // builder's repair and the dock's replay are pushed, and a
                // receipt vouches only for a sha that went green, which a
                // red replay never did.
                if !self.cfg.dry
                    && let Some(stamp) = edge_held_red_to_clear(&j, &self.cfg.clone)
                {
                    self.clear_red(&jid, stamp).await;
                }
                continue;
            }
            let branch = j
                .get("metadata")
                .and_then(|m| m.get("branch"))
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string();
            let ok = sh_unchecked(&[
                "git",
                "-C",
                &self.cfg.clone,
                "rev-parse",
                "--verify",
                "--quiet",
                &format!("fork/{branch}"),
            ])?;
            // RECOVER RATHER THAN SKIP. A car is parked at review by its
            // author pushing the branch; the natural place to push is
            // the upstream the author cloned, and the fork is an
            // implementation detail of how this conductor assembles a
            // train. On 2026-08-14 that gap silently held NINE cars for
            // a whole session: the dock reported 12 parked while the
            // boardable count was 0, because `parked_ready` asks
            // "branch declared, review ready" and this asks "branch on
            // the fork" — two predicates for one question, and only the
            // first is on any dashboard.
            //
            // So if the branch exists upstream, put it on the fork and
            // board the car. Copying a ref the author already published
            // is not a judgement call; refusing to, and reporting a
            // dock depth that cannot board, is the surprising
            // behaviour. A branch that exists in NEITHER place is still
            // a real skip — that car was never pushed at all.
            // ABSENT **OR STALE**. Existence is not the question: a
            // branch already on the forge is never refreshed, so a car
            // fixed after a red train boards the commit that failed.
            let fork_sha = if ok.status.success() {
                Some(String::from_utf8_lossy(&ok.stdout).trim().to_string())
            } else {
                None
            };
            let want = car_head(&self.cfg.clone, &branch)?;
            let stale = matches!((&fork_sha, &want), (Some(f), Some(w)) if f != w);
            let mut ok = ok;
            if (!ok.status.success() || stale)
                && !self.cfg.dry
                && publish_car_branch(&self.cfg.clone, &branch)?
            {
                log(format!(
                    "{}: branch {branch} was not on the fork — published it",
                    id8(&jid)
                ));
                ok = sh_unchecked(&[
                    "git",
                    "-C",
                    &self.cfg.clone,
                    "rev-parse",
                    "--verify",
                    "--quiet",
                    &format!("fork/{branch}"),
                ])?;
            }
            if !ok.status.success() {
                let reason = skip_reason_branch_missing(&branch);
                log(format!("{}: {reason} — leaving behind", id8(&jid)));
                left_behind.push(json!({"car_id_short": id8(&jid), "reason": reason.as_str()}));
                held.push(jid.clone());
                // Loud on the Job, not just in the journal: the author
                // parked this at review believing it would board. Once is
                // loud; the same reason again is not a new fact.
                let kv = vec![("skip_reason", json!(reason))];
                if !self.cfg.dry && !metadata_already(&j, &kv) {
                    self.merge_job_metadata(&jid, kv).await?;
                }
                continue;
            }
            // The receipt spot-check (742d1faa): the head this car will
            // actually board must be the head its gate receipt vouches
            // for, and the receipt must be green on a clean tree.
            //
            // THE HEAD THAT BOARDS IS THE ONE ON THE FORK. The consist is
            // assembled from `fork/{branch}` (`rerail_onto_consist`) and
            // the boarded head is stamped from it, so that ref — not the
            // conductor clone's own `refs/heads` — is what the receipt
            // has to match. `car_head` prefers the LOCAL branch, which is
            // the right question for "is there anything newer to publish"
            // and the wrong answer to "what will ride". The two come
            // apart when a car is rebased and re-pushed, which is the
            // normal repair: the clone keeps the pre-rebase commit, the
            // push cannot fast-forward past it, the fork rightly keeps
            // the gated commit, and comparing the receipt against the
            // local head leaves a correctly-gated car behind for "gated,
            // then changed". Read on 2026-08-29 from a live dock — car
            // c6531868 was held out with its receipt (56b817eb) matching
            // the fork exactly, against a local ref eight hours older.
            //
            // Read AFTER the publish attempt above, so a branch that was
            // just published is judged on what actually landed there
            // rather than on what was offered.
            let boards = fork_head(&self.cfg.clone, &branch)?;
            // THE REVIEW HOLD, RE-JUDGED HERE (backlog b7b02024 car 1).
            // Every road onto the dock ends at this walk, including the
            // four that skip the CLI's judge; the diff of the head that
            // will ride is judged before the receipt check can spend a
            // bay re-gating a car that waits on a person. Not counted
            // into `held`: like a struck car, it waits on a review, not
            // on the dock.
            if let Some(reason) = self.review_hold(&j, &jid, boards.as_deref()).await {
                left_behind.push(json!({"car_id_short": id8(&jid), "reason": reason.as_str()}));
                continue;
            }
            // THE DOCK'S RE-GATE (backlog 969a1092) owns two of the holds
            // below: a receipt the DOCK outran — it replayed the branch
            // onto main, and the re-gate's green has not refreshed the
            // car yet — and a car whose receipt still vouches for its
            // head but whose files main has since moved into. Every other
            // car passes through both untouched.
            let vouches = receipt_skip_reason(&j, boards.as_deref());
            // A receipt that vouches for the head again — a green copied,
            // or its builder's repair — ends any red the dock recorded
            // for an earlier head (backlog 2fccbfd6): the yard and orient
            // read that red as "not boardable", so it comes off this pass.
            let cleared = vouches
                .is_none()
                .then(|| dock_regate::cleared_stamp(&j))
                .flatten();
            let hold = match vouches {
                Some(reason) => Some(
                    match boards.as_deref().and_then(|b| dock_regate::pending(&j, b)) {
                        Some(stamp) => self.regate_in_flight(&j, &jid, &branch, stamp).await,
                        None => DockHold::plain(reason),
                    },
                ),
                None => match boards.as_deref() {
                    Some(head) => self.base_hold(&j, &jid, &branch, head).await,
                    None => None,
                },
            };
            if let Some(DockHold {
                reason,
                stamp,
                waiting,
            }) = hold
            {
                log(format!("{}: {reason} — leaving behind", id8(&jid)));
                // A GARAGED car is marked on the record: the dock stops
                // re-gating it at this head, so a window of nothing else
                // needs a person (`empty_dock_refusal`, review M2).
                // Read off what THIS pass writes: its stamp, or the stamp its
                // red is cleared to — never the pre-clear snapshot (L1 of the
                // round-3 review of car 5eb1967e, backlog f8383a38).
                let garaged = dock_regate::hold_garaged(stamp.as_ref(), cleared.as_ref(), &j);
                left_behind.push(json!({
                    "car_id_short": id8(&jid),
                    "reason": reason.as_str(),
                    GARAGED: garaged,
                }));
                held.push(jid.clone());
                settled.insert(jid.clone());
                if !self.cfg.dry {
                    let mut kv = vec![("skip_reason", json!(reason))];
                    // A re-gate a red train owed this car is paid the pass
                    // a gate is FILED for it (backlog 96f02540): one
                    // re-gate per red, never one per landing after it.
                    if let Some(w) = stamp.as_ref().and_then(|s| dock_regate::owed_paid(&j, s)) {
                        kv.push((dock_regate::REGATE_OWED, w));
                    }
                    match (stamp, cleared) {
                        (Some(stamp), _) => kv.push((dock_regate::BASE_REGATE, stamp)),
                        (None, Some(stamp)) => self.clear_red(&jid, stamp).await,
                        (None, None) => {}
                    }
                    // The dock's claim on the next free bay: written the
                    // pass it starts waiting or its main moves, deleted
                    // the pass it stops — and otherwise not at all, so a
                    // car still waiting leaves this write unchanged
                    // (design 38f3a488 D1).
                    if let Some(w) = dock_regate::waiting_write(&j, waiting.as_deref(), Utc::now())
                    {
                        kv.push((crate::gate::DOCK_WAITING, w));
                    }
                    // Only a CHANGED hold is written: the dock is walked
                    // every two minutes by the refresh as well as on every
                    // board (design 42279fb2), and a reason the car already
                    // carries is not a new fact — it would be an audit
                    // event for nothing.
                    if !metadata_already(&j, &kv) {
                        self.merge_job_metadata(&jid, kv).await?;
                    }
                }
                continue;
            }
            // Boardable, so it waits for no bay: a claim it still carries
            // would hold builders back for a car about to depart.
            settled.insert(jid.clone());
            if let Some(w) = dock_regate::waiting_write(&j, None, Utc::now())
                && !self.cfg.dry
            {
                self.merge_job_metadata(&jid, vec![(crate::gate::DOCK_WAITING, w)])
                    .await?;
            }
            if let Some(stamp) = cleared
                && !self.cfg.dry
            {
                self.clear_red(&jid, stamp).await;
            }
            // Boardable, so no re-gate is owed it any more: the red train
            // that owed one found it current, untouched, or re-gated green.
            // Best-effort, like `clear_red`: a marker left a pass too long
            // re-asks the base of one car, never refuses the cars behind it.
            if dock_regate::owes_regate(&j)
                && !self.cfg.dry
                && let Err(e) = self
                    .merge_job_metadata(&jid, vec![(dock_regate::REGATE_OWED, Value::Null)])
                    .await
            {
                log(format!(
                    "{}: boardable, but its owed re-gate marker could not be cleared ({e:#}) — \
                     the next pass tries again",
                    id8(&jid)
                ));
            }
            if let Some(h) = boards.clone() {
                judged.insert(jid.clone(), h);
            }
            out.push((j, branch));
        }
        // Every car this walk held for anything but a bay — two strikes,
        // an ordering edge, a missing branch, a review hold — is waiting
        // for no bay either. A claim it still carries is withdrawn here,
        // because nothing else would: a claim counts for as long as the
        // refresh fires, not until it ages (design 38f3a488).
        self.withdraw_claims(&listed, &settled).await?;
        Ok(DockPass {
            boardable: out,
            left_behind,
            held,
            judged,
        })
    }

    /// Does the car's own diff, at the head the fork carries, wait for
    /// its adversarial review? `None` = board it; `Some` = the reason it
    /// stays off this train, which is also the left-behind entry.
    ///
    /// THE ONE ROAD EVERY CAR TRAVELS (backlog b7b02024 finding 8,
    /// design 7cedfa29 D1). `mutating_verb::judge` ran only at the CLI's
    /// doors, so a car parked by an older binary, a hand-PATCHed park
    /// intent, the dock's own re-gate or the dispatcher's rerail-a-car
    /// rule boarded unheld: `parked_ready` reads only the marker. It is
    /// judged HERE, in the collector that holds the clone, and not in
    /// `parked_ready`, which stays a pure predicate because the cadence
    /// loop shares it for dock depth.
    ///
    /// HELD, NOT SKIPPED. The hold is written on the review step —
    /// the marker the yard, orient, the loading-dock row and the cadence
    /// count all read — with the head it judged beside it (`hold_sha`).
    ///
    /// A RELEASE IS A VERDICT AT A HEAD (design 7cedfa29 D6). A car that
    /// records a release boards only when that release is at the head the
    /// fork carries AND the review run it names vouches for the car
    /// there — read in one GET, by `review_verdict::vouches`, the same
    /// check `boss release --review` made. A release at another head, or
    /// a review hold cleared without one, is held again. None of it asks
    /// who wrote a key; the shared token makes actor ids forgeable.
    ///
    /// FAILS CLOSED, NEVER FATAL. An unreadable diff is a hold. A review
    /// run that cannot be READ keeps the car off this train without a
    /// hold — a blip is not a finding, and a hold would cost the release
    /// that was valid. A hold that cannot be written still keeps the car
    /// off THIS train — the write is the record, the `Some` is the brake
    /// — and the failure rides in the reason, so it reaches the train's
    /// books; the next pass judges and writes again. Nothing here returns
    /// an error to the walk (CLAUDE.md: a fallible write in reconcile
    /// froze ALL landings).
    async fn review_hold(&self, car: &Value, jid: &str, head: Option<&str>) -> Option<String> {
        use crate::mutating_verb::Dock;
        let head = head.unwrap_or_default();
        let clone = Path::new(&self.cfg.clone);
        let executor_guard: Result<()> = async {
            let requirement = crate::review_verdict::pinned_executor_requirement(self, car).await?;
            if requirement
                == boss_jobs::executor_attestation::ExecutorProvenanceRequirement::Verified
            {
                let release = crate::review_verdict::release_on(car)
                    .context("the pinned FORMAL requirement has no reviewer release")?;
                let run = self
                    .api(Method::GET, &format!("/api/jobs/{}", release.review), None)
                    .await?
                    .context("the reviewer run is unavailable")?;
                let executor =
                    crate::review_verdict::read_executor_eligibility(self, &run, car, head).await?;
                crate::review_verdict::vouches_with_executor(&run, car, head, &executor)
                    .map_err(|why| anyhow!("{why}"))?;
                if release.record.get("executor_binding") != executor.record_binding().as_ref() {
                    bail!("the release does not retain the original executor receipt");
                }
            }
            Ok(())
        }
        .await;
        let dock = match executor_guard {
            Err(reason) => Dock::Hold {
                reason: format!("pinned executor eligibility refused: {reason:#}"),
                bind: true,
            },
            Ok(()) => crate::mutating_verb::dock(
                car,
                head,
                || crate::mutating_verb::judge_on_main(clone, "origin/main", head),
                |from, to| crate::mutating_verb::same_change(clone, "origin/main", from, to),
            ),
        };
        let (reason, bind) = match dock {
            Dock::Board => return None,
            Dock::Hold { reason, bind } => (reason, bind),
            Dock::Check {
                review,
                reviewed,
                carry,
            } => {
                let named = |why: String| {
                    format!(
                        "conductor re-judge at {}: its release names review run {}, which does \
                         not vouch for this head ({why}); boss review, then boss release --review",
                        &head[..12.min(head.len())],
                        id8(&review)
                    )
                };
                match self
                    .api(Method::GET, &format!("/api/jobs/{review}"), None)
                    .await
                {
                    Ok(Some(run)) => match crate::review_verdict::read_executor_eligibility(
                        self, &run, car, &reviewed,
                    )
                    .await
                    .map_err(|why| format!("{why:#}"))
                    .and_then(|executor| {
                        crate::review_verdict::vouches_with_executor(
                            &run, car, &reviewed, &executor,
                        )
                    }) {
                        Ok(()) => {
                            if let Some(patch_id) = carry {
                                self.carry_release(car, jid, head, &patch_id).await;
                            }
                            return None;
                        }
                        Err(why) => (named(why), true),
                    },
                    Ok(None) => (
                        named("the system of record serves nothing for it".into()),
                        true,
                    ),
                    Err(e) if is_no_such_job(&e) => (named("no such packet".into()), true),
                    Err(e) => {
                        let why = short_cause(&e, self.policy.blip_cause_budget);
                        log(format!(
                            "{}: its release's review run {} could not be read ({why}) — kept \
                             off this train, not held",
                            id8(jid),
                            id8(&review)
                        ));
                        return Some(format!(
                            "its release's review run {} could not be read ({why}): kept off \
                             this train; the next pass reads it again",
                            id8(&review)
                        ));
                    }
                }
            }
        };
        log(format!("{}: {reason} — holding at the dock", id8(jid)));
        if self.cfg.dry {
            log(format!("DRY: would hold car {} ({reason})", id8(jid)));
            return Some(reason);
        }
        let written =
            match crate::mutating_verb::review_hold_write(car, &reason, bind.then_some(head)) {
                Ok(Some((path, body))) => self
                    .api(Method::PATCH, &path, Some(body))
                    .await
                    .map(|_| ())
                    .map_err(|e| format!("{e:#}")),
                // Held already between the read and this write: its reason stands.
                Ok(None) => Ok(()),
                Err(e) => Err(e),
            };
        match written {
            Ok(()) => Some(reason),
            Err(e) => {
                log(format!(
                    "{}: the review hold could not be written ({e}) — kept off this train anyway; \
                     the next pass writes it again",
                    id8(jid)
                ));
                Some(format!(
                    "{reason} [the hold could not be written: {e}; kept off this train]"
                ))
            }
        }
    }

    /// Record a release the dock's replay carried to `head` (review F3):
    /// the release moves to the replayed head and keeps the head its
    /// review read. BEST-EFFORT — the car was already proved boardable
    /// this pass; a write that fails leaves the stamp that proved it, and
    /// the next pass carries it again.
    async fn carry_release(&self, car: &Value, jid: &str, head: &str, patch_id: &str) {
        let Some(release) = crate::review_verdict::release_on(car) else {
            return;
        };
        let record = crate::review_verdict::carried_forward(&release, head, patch_id);
        log(format!(
            "{}: its release at {} carried to the dock's replay {} — the car's own diff is \
             unchanged (patch-id {})",
            id8(jid),
            id8(release.sha),
            id8(head),
            id8(patch_id)
        ));
        if self.cfg.dry {
            return;
        }
        let path = match crate::steps::holdable(car) {
            Ok(step) => format!(
                "/api/jobs/{jid}/steps/{}/metadata",
                step.get("id").and_then(Value::as_str).unwrap_or_default()
            ),
            Err(e) => {
                log(format!(
                    "{}: the carried release was not recorded ({e})",
                    id8(jid)
                ));
                return;
            }
        };
        if let Err(e) = self
            .api(
                Method::PATCH,
                &path,
                Some(json!({ boss_jobs::car::RELEASE: record })),
            )
            .await
        {
            log(format!(
                "{}: the carried release was not recorded ({e:#}) — the next pass carries it \
                 again",
                id8(jid)
            ));
        }
    }

    /// Does this car's DECLARED ORDERING EDGE hold it back?
    /// `None` = board it (no edge, a satisfied edge, or an edge this pass
    /// could not judge).
    ///
    /// INFALLIBLE BY SIGNATURE, deliberately, and that is the whole of
    /// the safety argument. This runs inside the loop that boards every
    /// train; a `?` here would let one unreadable packet refuse the
    /// entire window, and the gate never runs the conductor, so nothing
    /// before production would have caught it (CLAUDE.md: a fallible
    /// write in reconcile froze ALL landings). So every failure becomes
    /// `Predecessor::Unreadable`, which boards the car and journals WHY
    /// — loud, per CLAUDE.md §Diagnosis, because a swallowed read is the
    /// next diagnosis paid for in advance.
    ///
    /// The 404 is separated from the blips on purpose: "there is no such
    /// Job" is an ANSWER and a hold a person must fix, while "I could not
    /// ask" is neither. `api` has already exhausted its retry budget by
    /// the time either reaches here.
    async fn edge_hold(&self, car: &Value, jid: &str) -> Option<EdgeHold> {
        let declared = declared_predecessor(car)?;
        let pred = match self
            .api(Method::GET, &format!("/api/jobs/{declared}"), None)
            .await
        {
            Ok(Some(p)) => Predecessor::Found(p),
            // A success with no body is the same fact as a 404 for this
            // question: the system of record served nothing for that id.
            Ok(None) => Predecessor::Absent,
            Err(e) if is_no_such_job(&e) => Predecessor::Absent,
            Err(e) => Predecessor::Unreadable(short_cause(&e, self.policy.blip_cause_budget)),
        };
        match boards_after_outcome(&declared, &pred) {
            EdgeOutcome::Board => None,
            EdgeOutcome::BoardUnjudged(note) => {
                log(format!("{}: {note}", id8(jid)));
                None
            }
            EdgeOutcome::Hold(h) => Some(h),
        }
    }

    /// Does main's movement since this car's gate hold it back? `None` =
    /// board it; otherwise the skip reason and, when this pass launched or
    /// refused a re-gate, the `base_regate` stamp to record (backlog
    /// 969a1092 — the rule and the bound are `dock_regate`'s).
    ///
    /// ONLY A CAR A RED TRAIN OWES A RE-GATE IS ASKED (backlog 96f02540).
    /// The re-gate ON LANDING is off: every landing used to re-judge every
    /// parked car against the new main, and on 2026-09-28 five of seven
    /// cars queued 40-60 minutes behind saturated bays for a test the TRAIN
    /// gate runs anyway on the assembled consist (design 128b5496) — a
    /// cost that grew as (parked cars) x (landings). A car carries
    /// `regate_owed` only when a red train released it and its files
    /// intersect the failure (or the red named nothing), and for that car
    /// the judgement below is today's, unchanged. Every other car whose
    /// receipt vouches for its head boards as gated.
    ///
    /// INFALLIBLE BY SIGNATURE, for `edge_hold`'s reason: this runs inside
    /// the loop that boards every train, and a base git cannot read is not
    /// a finding about the car. It boards, and the journal says why.
    async fn base_hold(
        &self,
        car: &Value,
        jid: &str,
        branch: &str,
        head: &str,
    ) -> Option<DockHold> {
        if !dock_regate::owes_regate(car) {
            return None;
        }
        let reading = match dock_regate::read_base(&self.cfg.clone, head) {
            Ok(r) => r,
            Err(e) => {
                log(format!(
                    "{}: could not read its base against main ({e:#}) — boarding as gated; the \
                     train gate still judges the assembled tree",
                    id8(jid)
                ));
                return None;
            }
        };
        let stamp = dock_regate::RegateStamp::of(car);
        match dock_regate::judge(reading.as_ref(), stamp.as_ref()) {
            dock_regate::DockBase::Current => None,
            dock_regate::DockBase::Untouched { main_changed } => {
                log(format!(
                    "{}: behind main, but none of the {main_changed} path(s) main changed since \
                     its gate touch it — boards as gated",
                    id8(jid)
                ));
                None
            }
            // Already re-gated for this main: the bound. The only way here
            // is a refused replay — a launched one moved the branch and is
            // read by `regate_in_flight` instead.
            dock_regate::DockBase::Touched {
                launch: false,
                touched,
            } => {
                let stamp = stamp.unwrap_or_default();
                let reason = if stamp.refused.is_empty() {
                    format!(
                        "already re-gated once for main {} (gate-run {}) and still behind it with \
                         {} path(s) touched — the dock re-gates it again when main moves",
                        &stamp.main[..8.min(stamp.main.len())],
                        id8(&stamp.gate_run),
                        touched.len()
                    )
                } else {
                    dock_regate::refused_reason(jid, &stamp)
                };
                Some(DockHold::plain(reason))
            }
            dock_regate::DockBase::Touched {
                launch: true,
                touched,
            } => {
                let reading = reading?;
                // A launch the receipt asked for starts a new red streak.
                self.launch_base_regate(car, jid, branch, &reading, &touched, 0)
                    .await
            }
        }
    }

    /// Replay the car onto current main and file its re-gate — the
    /// `boss gate --rebase --park-*` a builder would run, run by the dock.
    /// `None` = the MEANS failed and the car boards as gated. `reds` is
    /// how many dock re-gates of it in a row went red before this one —
    /// 0 unless this launch is a red's retry (backlog 2fccbfd6).
    async fn launch_base_regate(
        &self,
        car: &Value,
        jid: &str,
        branch: &str,
        reading: &dock_regate::BaseReading,
        touched: &[String],
        reds: u32,
    ) -> Option<DockHold> {
        let stamp = dock_regate::RegateStamp {
            base: reading.base.clone(),
            reds,
            ..dock_regate::RegateStamp::for_main(&reading.main, touched)
        };
        if self.cfg.dry {
            log(format!(
                "DRY: {}: main moved into {} of its path(s) since its gate — would replay it onto \
                 main and re-gate it",
                id8(jid),
                touched.len()
            ));
            return Some(DockHold::plain(dock_regate::busy_reason(
                "dry run", reading, touched,
            )));
        }
        // A SLOT, AND THE MEANS TO USE IT, BEFORE THE BRANCH MOVES. A
        // replayed branch no longer matches its receipt, so a car moved
        // and then not gated is a car that cannot board — check first.
        let ns = self.cfg.gate_namespace.as_str();
        let slot: Result<(usize, usize)> = async {
            std::fs::metadata(&self.cfg.gate_manifest).with_context(|| {
                format!("no gate runner manifest at {}", self.cfg.gate_manifest)
            })?;
            let max = crate::gate::max_concurrent(&self.http).await?;
            Ok((crate::gate::running_gates(ns)?.len(), max))
        }
        .await;
        match slot {
            Ok((live, max)) if crate::gate::admits(live, max, crate::gate::Requester::Car) => {}
            // No bay: claim the next one, ahead of every builder place
            // (design 42279fb2 D3). The refresh re-asks within two minutes.
            Ok((live, max)) => {
                let why = format!("{live} gate(s) running of {max}");
                return Some(DockHold {
                    waiting: Some(reading.main.clone()),
                    ..DockHold::plain(dock_regate::busy_reason(&why, reading, touched))
                });
            }
            Err(e) => {
                log(format!(
                    "{}: main moved into its files, but no gate can be launched ({e:#}) — \
                     boarding as gated; the train gate still judges the assembled tree",
                    id8(jid)
                ));
                return None;
            }
        }
        let rebased = match crate::freshness::rebase_onto_main(Path::new(&self.cfg.clone), branch) {
            Ok(r) => r,
            Err(e) if dock_regate::replay_refused(&e) => {
                let refused = format!("{e:#}");
                let stamp = dock_regate::RegateStamp {
                    refused: refused.lines().next().unwrap_or_default().to_string(),
                    ..stamp
                };
                return Some(DockHold {
                    reason: dock_regate::refused_reason(jid, &stamp),
                    stamp: Some(stamp.to_value(Utc::now())),
                    waiting: None,
                });
            }
            Err(e) => {
                log(format!(
                    "{}: main moved into its files, but replaying it failed ({e:#}) — boarding \
                     as gated; the train gate still judges the assembled tree",
                    id8(jid)
                ));
                return None;
            }
        };
        let stamp = dock_regate::RegateStamp {
            head: rebased.new_head.clone(),
            from: rebased.old_head.clone(),
            ..stamp
        };
        log(format!(
            "{}: main moved into {} of its path(s) since its gate — {} {} -> {} onto main {}",
            id8(jid),
            touched.len(),
            if rebased.merged {
                "merged forward"
            } else {
                "replayed"
            },
            &rebased.old_head[..8.min(rebased.old_head.len())],
            &rebased.new_head[..8.min(rebased.new_head.len())],
            &reading.main[..8.min(reading.main.len())],
        ));
        Some(self.file_regate(car, jid, branch, stamp).await)
    }

    /// File the gate-run for a replayed car and say what happened — the
    /// half of a launch that is retried when it fails, because the branch
    /// has already moved.
    async fn file_regate(
        &self,
        car: &Value,
        jid: &str,
        branch: &str,
        stamp: dock_regate::RegateStamp,
    ) -> DockHold {
        let marks = dock_regate::marks(car, jid, &stamp);
        let at = Utc::now();
        match self
            .launch_gate(branch, &stamp.head, crate::gate::Requester::Car, marks)
            .await
        {
            Ok(run) => {
                let stamp = dock_regate::RegateStamp {
                    gate_run: run,
                    ..stamp
                };
                DockHold {
                    reason: dock_regate::launched_reason(&stamp),
                    stamp: Some(stamp.to_value(at)),
                    waiting: None,
                }
            }
            // No gate is running for it: the next pass files it.
            Err(GateLaunchFailed { filed: None, cause }) => DockHold {
                reason: dock_regate::unfiled_reason(&stamp, &format!("{cause:#}")),
                stamp: Some(stamp.to_value(at)),
                waiting: None,
            },
            // Filed, then never started (backlog 7919fdcc, item 11): the
            // run is named on the stamp, so the next pass reads its `lost`
            // verdict and the red rule (`dock_regate::after_red`) bounds
            // the retry — never a fresh packet every two minutes.
            Err(GateLaunchFailed {
                filed: Some(run),
                cause,
            }) => {
                let stamp = dock_regate::RegateStamp {
                    gate_run: run,
                    ..stamp
                };
                DockHold {
                    reason: dock_regate::unstarted_reason(&stamp, &format!("{cause:#}")),
                    stamp: Some(stamp.to_value(at)),
                    waiting: None,
                }
            }
        }
    }

    /// Delete the bay claim (`gate::DOCK_WAITING`) from every car in
    /// `cars` that carries one and is not `settled` (`claims_to_withdraw`),
    /// and say how many.
    async fn withdraw_claims(&self, cars: &[Value], settled: &HashSet<String>) -> Result<usize> {
        let ids = claims_to_withdraw(cars, settled);
        for id in &ids {
            log(format!(
                "{}: waiting for no gate bay — withdrawing its claim",
                id8(id)
            ));
            self.merge_job_metadata(id, vec![(crate::gate::DOCK_WAITING, Value::Null)])
                .await?;
        }
        Ok(ids.len())
    }

    /// Is the track held? The name of the pre-merge train holding it, or
    /// `None` when it is clear — one read for `board` and `refresh`, so
    /// the dock and a departure can never disagree about the track.
    ///
    /// Every page: the list rows carry `steps` (http/jobs.rs enriches
    /// each row), which is what the predicate reads, and the one
    /// pre-merge train that matters may sit behind merged ones waiting
    /// to converge — a limit is not a filter.
    async fn track_occupant(&self) -> Result<Option<String>> {
        let on_track = list_all_pages(|offset| async move {
            self.api(
                Method::GET,
                &format!("/api/jobs?kind=pr-train&status=open&limit={PAGE_LIMIT}&offset={offset}"),
                None,
            )
            .await
        })
        .await?;
        Ok(track_occupied_by(&on_track))
    }

    /// The dock between departures (D1 of design 42279fb2, backlog
    /// 4890165b): the same per-car judgement boarding runs — and so the
    /// same re-gates launched, stamped and bounded — with nothing
    /// assembled and nothing departed. Fired every two minutes by the
    /// `train-dock-refresh` cadence rule.
    ///
    /// ONLY WHILE THE TRACK IS CLEAR. Main moves only when a train
    /// merges; a re-gate launched while one holds the track is launched
    /// against a main about to be replaced — the measured waste this
    /// verb exists to remove (19:26 on 2026-09-25: a car replayed onto
    /// 22c1a876 in the pass that departed train #686, whose merge changed
    /// four of its files). Since backlog 96f02540 the dock re-gates only a
    /// car a red train owes one, and `board` never waits for it.
    ///
    /// Before this, re-gates launched only inside `board`, which fires at
    /// most once per cooldown after a departure: 42 of 58 left-behind rows
    /// over ten trains read "waiting for a gate slot".
    ///
    /// A HELD REFRESH WITHDRAWS THE DOCK'S CLAIMS (design 38f3a488 D3). A
    /// claim on a bay counts while this rule fires, and a held pass still
    /// fires with rc 0 — so without this, every claim would stand ahead
    /// of every builder for the whole transit, for a re-gate that cannot
    /// launch until the merge replaces its main. The next clear pass
    /// writes the claim again on the new main: at most one delete and one
    /// set per waiting car per departure.
    pub(super) async fn refresh(&self) -> Result<()> {
        if let Some(occupant) = self.track_occupant().await? {
            let cars = list_all_pages(|offset| async move {
                self.api(
                    Method::GET,
                    &format!(
                        "/api/jobs?kind=ship-a-change&status=open&limit={PAGE_LIMIT}&offset={offset}"
                    ),
                    None,
                )
                .await
            })
            .await?;
            let withdrawn = self.withdraw_claims(&cars, &HashSet::new()).await?;
            log(format!(
                "REFRESH HELD — track occupied by {occupant}; a re-gate launched now would \
                 test a main that train is about to replace ({withdrawn} dock claim(s) on a \
                 bay withdrawn)"
            ));
            return Ok(());
        }
        self.ensure_clone()?;
        let pass = self.candidates().await?;
        log(refresh_line(&pass));
        Ok(())
    }

    /// A train has DEPARTED leaving every car the dock held behind: count
    /// one more on each car's own dock streak (`LEFT_BEHIND_TRAINS`, apart
    /// from assembly's `skips`; cleared on boarding), and alarm on a car
    /// the streak has carried to `LEFT_BEHIND_ALARM_TRAINS` (backlog
    /// 2fccbfd6; review of car 5eb1967e, M3).
    ///
    /// INFALLIBLE BY SIGNATURE: the train has left, and a count or an
    /// alarm that cannot be written is journalled with the car it concerns
    /// rather than failing the departure (a fallible write in the boarding
    /// loop froze every landing once).
    async fn count_left_behind(&self, held: &[String]) {
        for id in held {
            let car = match self.get_job(id).await {
                Ok(car) => car,
                Err(e) => {
                    log(format!(
                        "{}: left behind, but the car could not be read to count it ({e:#})",
                        id8(id)
                    ));
                    continue;
                }
            };
            let trains = next_left_behind_count(&car);
            if let Err(e) = self
                .merge_job_metadata(id, vec![(LEFT_BEHIND_TRAINS, json!(trains))])
                .await
            {
                log(format!(
                    "{}: left behind, but the count could not be written ({e:#})",
                    id8(id)
                ));
                continue;
            }
            self.alarm_left_behind(id, &car, trains).await;
        }
    }

    /// File the left-behind alarm for `car` when its streak of `trains`
    /// consecutive departures has earned one and none is filed yet, and
    /// name it on the car so the streak files no twin.
    ///
    /// AN ALARM ALREADY OPEN FOR THE CAR IS ADOPTED, NEVER TWINNED (LOW 7
    /// of the review of car 5eb1967e): the car's stamp is the cheap
    /// answer, but a stamp that failed to land, or a hand edit, would
    /// otherwise file a second alarm on the next departure. So before a
    /// POST, the open backlog-items are read for one naming this car —
    /// every page, a limit is not a filter — and its id is stamped instead.
    async fn alarm_left_behind(&self, id: &str, car: &Value, trains: u64) {
        if !left_behind_alarm_due(car, trains) {
            return;
        }
        let open = list_all_pages(|offset| async move {
            self.api(
                Method::GET,
                &format!(
                    "/api/jobs?kind=backlog-item&status=open&limit={PAGE_LIMIT}&offset={offset}"
                ),
                None,
            )
            .await
        })
        .await;
        let adopted = match open {
            Ok(rows) => rows.into_iter().find_map(|j| {
                (j.pointer("/metadata/left_behind_car")
                    .and_then(Value::as_str)
                    == Some(id))
                .then(|| j.get("id").and_then(Value::as_str).map(str::to_string))
                .flatten()
            }),
            Err(e) => {
                // Unread is not "none open": filing blind would twin the
                // very alarm this read exists to find.
                log(format!(
                    "{}: left behind by {trains} consecutive trains, but the open alarms could \
                     not be read to dedupe its own ({e:#}) — the next departure asks again",
                    id8(id)
                ));
                return;
            }
        };
        let alarm = match adopted {
            Some(existing) => {
                log(format!(
                    "{}: left behind by {trains} consecutive trains — alarm {} already open, \
                     adopted",
                    id8(id),
                    id8(&existing)
                ));
                existing
            }
            None => {
                let owner = self.owner_for_filing().await;
                let body = left_behind_alarm_body(car, trains, Utc::now(), &owner);
                match self.api(Method::POST, "/api/jobs", Some(body)).await {
                    Ok(created) => {
                        let filed = created
                            .as_ref()
                            .and_then(|v| v.get("id"))
                            .and_then(Value::as_str)
                            .unwrap_or("filed")
                            .to_string();
                        log(format!(
                            "{}: left behind by {trains} consecutive trains — alarm {} filed",
                            id8(id),
                            id8(&filed)
                        ));
                        filed
                    }
                    Err(e) => {
                        log(format!(
                            "{}: left behind by {trains} consecutive trains, but its alarm could \
                             not be filed ({e:#})",
                            id8(id)
                        ));
                        return;
                    }
                }
            }
        };
        if let Err(e) = self
            .merge_job_metadata(id, vec![(LEFT_BEHIND_ALARM, json!(alarm))])
            .await
        {
            log(format!(
                "{}: alarm {} stands, but the car could not be stamped with it ({e:#}) — the \
                 next departure finds it open and adopts it",
                id8(id),
                id8(&alarm)
            ));
        }
    }

    /// Write a car's re-gate stamp with its red taken off (`cleared_stamp`).
    /// NEVER FATAL to the walk (LOW 11 of the review of car 5eb1967e): a
    /// red that could not be cleared is a surface that says "not boardable"
    /// a pass too long, and the next pass writes it again — while a `?`
    /// here would refuse every car behind this one.
    async fn clear_red(&self, jid: &str, stamp: Value) {
        if let Err(e) = self
            .merge_job_metadata(jid, vec![(dock_regate::BASE_REGATE, stamp)])
            .await
        {
            log(format!(
                "{}: its receipt vouches for its head again, but the red on its re-gate stamp \
                 could not be cleared ({e:#}) — the next pass tries again",
                id8(jid)
            ));
        }
    }

    /// A car the dock replayed, whose receipt has not caught up: where its
    /// re-gate stands, and the gate-run filed if the launch never got that
    /// far. Always a hold — the car's receipt does not vouch for its head.
    async fn regate_in_flight(
        &self,
        car: &Value,
        jid: &str,
        branch: &str,
        stamp: dock_regate::RegateStamp,
    ) -> DockHold {
        if stamp.gate_run.is_empty() {
            if self.cfg.dry {
                let r =
                    dock_regate::in_flight_reason(jid, &stamp, &dock_regate::InFlight::FileGate);
                return DockHold::plain(r);
            }
            return self.file_regate(car, jid, branch, stamp).await;
        }
        let run = match self.get_job(&stamp.gate_run).await {
            Ok(run) => Some(run),
            Err(e) => {
                log(format!(
                    "{}: could not read its re-gate {} ({e:#}) — reading it as still running",
                    id8(jid),
                    id8(&stamp.gate_run)
                ));
                None
            }
        };
        let verdict = run
            .as_ref()
            .and_then(boss_jobs::flake::verdict)
            .map(str::to_string);
        let standing = dock_regate::in_flight(&stamp, verdict.as_deref());
        // A red is not the end of it (backlog 2fccbfd6): retried once,
        // then garaged — `dock_regate::after_red`.
        if let dock_regate::InFlight::Failed(verdict) = &standing {
            return self
                .red_regate(car, jid, branch, stamp, verdict, run.as_ref())
                .await;
        }
        // Still running (or green and not yet copied): the car stays on
        // the dock, and no departure waits for it (backlog 96f02540).
        DockHold {
            reason: dock_regate::in_flight_reason(jid, &stamp, &standing),
            stamp: None,
            waiting: None,
        }
    }

    /// A dock re-gate came back red (backlog 2fccbfd6; the rule is
    /// `dock_regate::after_red`). Retry it once — on a fresh base when
    /// main has moved, at the same head once the bound passes — and hold
    /// the car ON the red until then, with the red on its stamp so the
    /// yard and `boss orient` draw it as not boardable. A retry that goes
    /// red too garages it: held, the failure named, nothing more launched.
    ///
    /// INFALLIBLE BY SIGNATURE, for `edge_hold`'s reason. An unreadable
    /// main is no move, an unreadable registry no bound, and a retry
    /// whose means fail holds on the red and is re-asked next pass —
    /// never a board: the receipt does not vouch for this head.
    async fn red_regate(
        &self,
        car: &Value,
        jid: &str,
        branch: &str,
        stamp: dock_regate::RegateStamp,
        verdict: &str,
        run: Option<&Value>,
    ) -> DockHold {
        // The receipt's word before the packet's (LOW 5 of the review of
        // car 5eb1967e): a refusal recorded as `failed` judged nothing.
        let verdict = dock_regate::judged_verdict(verdict, run);
        let verdict = verdict.as_str();
        let red = dock_regate::red_of(&stamp, verdict, run);
        let main_now = match self.origin_main() {
            Ok(m) => Some(m),
            Err(e) => {
                log(format!(
                    "{}: its re-gate is red, and main could not be read ({e:#}) — no retry on \
                     a main move this pass",
                    id8(jid)
                ));
                None
            }
        };
        let red_at = run
            .and_then(dock_regate::red_closed_at)
            .or_else(|| dock_regate::launched_at(car));
        let held_on = |red: boss_jobs::dock_red::RegateRed, reason: String| DockHold {
            reason,
            stamp: dock_regate::red_stamp(car, &red),
            waiting: None,
        };
        let fresh_base = match dock_regate::after_red(
            &stamp,
            verdict,
            main_now.as_deref(),
            red_at,
            Utc::now(),
        ) {
            dock_regate::AfterRed::Garage => {
                let red = boss_jobs::dock_red::RegateRed {
                    garaged: true,
                    ..red
                };
                log(format!("{}: {}", id8(jid), red.reason()));
                let reason = red.reason();
                return held_on(red, reason);
            }
            dock_regate::AfterRed::Wait { more_minutes } => {
                log(format!(
                    "{}: its re-gate {} is {verdict} on main {} — retried when main moves{}",
                    id8(jid),
                    id8(&stamp.gate_run),
                    id8(&stamp.main),
                    more_minutes.map_or(String::new(), |m| format!(", or in {m} min"))
                ));
                let reason = red.reason();
                return held_on(red, reason);
            }
            dock_regate::AfterRed::Retry { fresh_base } => fresh_base,
        };
        // A judged red counts toward the garage; an unjudged one does not.
        let reds = stamp.reds + u32::from(verdict == "failed");
        let main = main_now.unwrap_or_default();
        // A car already garaged stays garaged while its retry is owed:
        // every path below that holds it on the red writes the red it
        // stood on, not a fresh one (round-2 re-review of car 5eb1967e,
        // N1) — or a retry that could not launch would read as a red the
        // dock still retries, and the board would call the window idle.
        // "Already garaged" is the car's own recorded red, not `reds > 0`:
        // a first retry of a LOST red carries the one judged red before
        // it, and a relaunch that failed marked it garaged without its
        // earning it (L2 of the round-3 review, backlog f8383a38).
        let red = boss_jobs::dock_red::RegateRed {
            garaged: dock_regate::car_garaged(car),
            ..red
        };
        if self.cfg.dry {
            let reason = dock_regate::retry_unlaunched_reason(&red, &main, "dry run");
            return held_on(red, reason);
        }
        log(format!(
            "{}: its re-gate {} went {verdict} on main {} — re-gating it {}",
            id8(jid),
            id8(&stamp.gate_run),
            id8(&stamp.main),
            if fresh_base {
                format!("on main {}, which has moved since", id8(&main))
            } else {
                "at the same head, its bound passed with main unmoved".to_string()
            }
        ));
        if fresh_base {
            let reading = match dock_regate::read_base(&self.cfg.clone, &stamp.head) {
                Ok(Some(r)) => r,
                Ok(None) => {
                    let reason = dock_regate::retry_unlaunched_reason(
                        &red,
                        &main,
                        "main is already an ancestor of the re-gated head",
                    );
                    return held_on(red, reason);
                }
                Err(e) => {
                    let reason =
                        dock_regate::retry_unlaunched_reason(&red, &main, &format!("{e:#}"));
                    return held_on(red, reason);
                }
            };
            let touched = dock_regate::touched_by_main(&reading.car_files, &reading.main_files);
            return match self
                .launch_base_regate(car, jid, branch, &reading, &touched, reds)
                .await
            {
                Some(hold) => retry_hold(hold, stamp.reds),
                None => {
                    let reason = dock_regate::retry_unlaunched_reason(
                        &red,
                        &main,
                        "no gate could be launched or the replay failed — the journal names which",
                    );
                    held_on(red, reason)
                }
            };
        }
        // The same head again: a new launch owed, the red cleared off it,
        // and the red it re-gates named so a green is tallied as a flake.
        let relaunch = dock_regate::RegateStamp {
            regate_of: stamp.gate_run.clone(),
            prior_failed: red.checks.clone(),
            gate_run: String::new(),
            reds,
            ..stamp
        };
        self.file_regate(car, jid, branch, relaunch).await
    }

    /// `origin/main` in the conductor's clone.
    fn origin_main(&self) -> Result<String> {
        let out = sh(&[
            "git",
            "-C",
            &self.cfg.clone,
            "rev-parse",
            "--verify",
            "origin/main",
        ])?;
        Ok(stdout_str(&out).trim().to_string())
    }

    async fn open_train_job(&self, train_branch: &str, window: &str) -> Result<Option<Value>> {
        // THE PIN. The train records the policy version it is departing
        // under, so an edit made while it is in flight cannot rewrite
        // the rules it left on — the same promise a packet gets from the
        // workflow version it was admitted under. Nothing is stamped
        // when the conductor fell back to compiled values: there is no
        // version, and a record that claimed one would be lying.
        let mut metadata = Map::new();
        metadata.insert("actor".to_string(), json!(ACTOR));
        for (k, v) in delivery_policy::pin_stamps(&self.policy) {
            metadata.insert(k.to_string(), v);
        }
        let payload = json!({
            "kind": "pr-train",
            "subject": {"subject_kind": "custom", "id": train_branch},
            "title": format!("PR train {window}"),
            // The conductor is a machine and says so. `resolve_owner`
            // reads any colon-bearing id as automation and places the
            // Job on an active holder of the kind's `owner_role`
            // (`platform-admin` for pr-train) — so the responsible
            // human is whoever actually holds the role today.
            //
            // This used to name `emp-bootstrap-admin` outright, which
            // survived only because that row happened to be the
            // deployment's admin. Once the bootstrap identity is
            // retired in favour of a named person, a hardcoded owner
            // is a dead id that resolution has to quietly override —
            // right by accident rather than by construction.
            "owner_id": ACTOR,
            "status": "open",
            "priority": "standard",
            "metadata": metadata,
            "tags": ["train"],
        });
        if self.cfg.dry {
            log(format!("DRY: would open train Job for {train_branch}"));
            return Ok(None);
        }
        let created = self.api(Method::POST, "/api/jobs", Some(payload)).await?;
        let jid = created
            .as_ref()
            .and_then(|c| c.get("id"))
            .and_then(Value::as_str)
            .map(str::to_string);
        let jid = match jid {
            Some(id) => id,
            None => {
                // some create paths return the row wrapped
                let listed = rows(
                    self.api(
                        Method::GET,
                        "/api/jobs?kind=pr-train&status=open&limit=5",
                        None,
                    )
                    .await?,
                )?;
                job_id(
                    listed
                        .first()
                        .ok_or_else(|| anyhow!("no open pr-train Job found after create"))?,
                )?
                .to_string()
            }
        };
        Ok(Some(self.get_job(&jid).await?))
    }

    /// The CI host's boarding verdict, from the estate's host-scope
    /// observation series. `BOSS_TRAIN_CI_HOST` names the estate node
    /// id of the box CI runs on; a deployment that has not configured
    /// it gets exactly the old behaviour, minus silence — one journal
    /// line says the check did not run.
    async fn ci_host_readiness(&self, now: DateTime<Utc>) -> host_readiness::Readiness {
        use crate::host_readiness::Readiness;
        let Some(host) = self.cfg.ci_host.as_deref() else {
            log("ci host check skipped — BOSS_TRAIN_CI_HOST unset");
            return Readiness::Proceed;
        };
        // `scope=host` so the page is not spent by the faster cluster
        // series (the reader's own lesson, 2026-09-02); `limit=50` is
        // its hard cap, depth enough to find this host among the other
        // host-scope observers.
        let fetched = self
            .api(
                Method::GET,
                "/api/estate/observations?scope=host&limit=50",
                None,
            )
            .await;
        match fetched {
            Ok(Some(body)) => host_readiness::host_readiness(
                &body,
                host,
                self.policy.ci_host_floor_gb,
                host_readiness::max_observation_age(),
                now,
            ),
            Ok(None) => Readiness::Unverifiable {
                reason: "the observations reader answered nothing".to_string(),
            },
            Err(e) => Readiness::Unverifiable {
                reason: format!("the observations reader is unreachable ({e})"),
            },
        }
    }

    pub(super) async fn board(&self, now: DateTime<Utc>) -> Result<()> {
        // Minute precision, not an AM/PM half-day. Boardings fire on
        // dock depth (min 4, 120m cooldown), not a twice-daily clock, so
        // the old "{date} AM/PM" label both COLLIDED — two trains carried
        // an identical "PM" the night of 2026-08-31 — and implied a
        // schedule the system does not run (21d4f433). Mirrors the
        // train_branch stamp on the next line.
        let window = now.format("%Y-%m-%d %H:%M").to_string();
        let train_branch = format!("train/{}", now.format("%Y%m%d-%H%M"));

        // THE TRACK — one train MERGING at a time. The cadence loop
        // holds a departure before it claims a window (cadence::decide),
        // so this is the backstop for a hand-run `boss train board` and
        // for two conductors racing: an open PRE-MERGE pr-train packet
        // means the previous consist has not landed on main, and a
        // second consist assembled now would merge onto a main the
        // first is about to change (a8c6773b). A MERGED train waiting
        // to deploy or converge does not hold it (`holds_the_track`):
        // the next consist merges on top of its content and converges
        // it by ancestry — holding for it deadlocked delivery twice on
        // 2026-09-07 (f3796323). No packet is opened for a hold — it is
        // not a refusal, the yard is not empty, and the next tick after
        // the track clears departs. It is still a DECISION, and recorded
        // like every other (backlog 96f02540): a hand-run board says why.
        if let Some(occupant) = self.track_occupant().await? {
            self.record_no_departure(&NoDeparture::TrackOccupied { occupant })
                .await?;
            return Ok(());
        }

        // THE HOST CHECK — before anything is assembled. On 2026-09-03
        // the conductor boarded two consists onto a CI host whose disk
        // was full, and each burned a full CI cycle discovering it; the
        // locomotive's run-start floor had even PASSED at 01:26,
        // because a start-of-run check cannot see a consist's
        // mid-flight consumption. So the question is asked here, from
        // the estate's observed series, before the first merge is
        // attempted (David, 2026-09-03: "protocol should actually
        // verify before anyone bothers to even start").
        //
        // Only a POSITIVE "the host is short" refuses. Unverifiable —
        // an absent, stale, or unreadable series — proceeds with one
        // loud line, deliberately FAIL-OPEN: the host-scope observer
        // (infra/estate/observe-host.sh) is not yet installed anywhere,
        // and landing this check must not stop all boarding on the day
        // the series does not exist yet. Once the series is live,
        // tightening stale-to-refuse is a policy question, not a
        // rebuild.
        match self.ci_host_readiness(now).await {
            host_readiness::Readiness::Refuse { reason } => {
                // The refusal is the journal's, not a packet's (see "A
                // BOARD THAT DEPARTS NO TRAIN OPENS NO PACKET"). The
                // condition itself — a host short of disk — is already
                // a packet: the estate observer files and refreshes one
                // for the host, and it does not arrive once a minute.
                self.record_no_departure(&NoDeparture::HostShort { reason })
                    .await?;
                return Ok(());
            }
            host_readiness::Readiness::Unverifiable { reason } => {
                log(format!(
                    "ci host unverifiable — {reason} — boarding anyway (fail-open until \
                     the host observation series exists)"
                ));
            }
            host_readiness::Readiness::Proceed => {}
        }

        self.ensure_clone()?;
        let DockPass {
            boardable: cands,
            mut left_behind,
            held,
            judged,
        } = self.candidates().await?;
        if self.cfg.dry {
            log(format!("DRY: candidates: {}", py_pairs(&cands)));
            log(format!(
                "DRY: would assemble {train_branch} and, if the consist check passes, open its \
                 train Job for {window}"
            ));
            return Ok(());
        }

        if cands.is_empty() {
            // NOT NECESSARILY AN IDLE WINDOW. `NothingParked` says "an
            // idle window, not a failure", which is a lie when the dock is
            // full of cars the ordering filter held — and whether this
            // window is self-clearing or waiting on a person is precisely
            // what an operator reads the line to learn.
            self.record_no_departure(&empty_dock_refusal(&left_behind))
                .await?;
            return Ok(());
        }

        // THE BOARD NEVER WAITS ON A RE-GATE (backlog 96f02540). It held
        // here for the dock's re-gate round (D2 of design 42279fb2) until
        // 2026-09-28, when a 15-minute hold anchored to the oldest re-gate
        // IN FLIGHT became a moving target under saturated bays: each
        // finished re-gate launched the next in the same pass and moved the
        // anchor, and the board refused every tick from 02:37Z while cars
        // that were current sat beside it. A car mid-re-gate stays on the
        // dock (its receipt does not vouch for its head); every car whose
        // receipt does, boards now.

        let clone = &self.cfg.clone;
        sh(&[
            "git",
            "-C",
            clone,
            "checkout",
            "-B",
            &train_branch,
            "origin/main",
        ])?;
        // (car, branch, boarded head) — the head is WHAT boarded, and
        // the sweep's licence to delete the branch later depends on it
        // (car 23923b40). Read from the fetched `fork/<branch>` ref,
        // which is precisely the commit the merge below carries.
        let mut boarded: Vec<(Value, String, String)> = Vec::new();
        let mut skipped: Vec<(Value, String)> = Vec::new();
        for (j, branch) in cands {
            let jid = job_id(&j)?.to_string();
            let head = match judged_head(clone, &judged, &jid, &branch)? {
                Ok(head) => head,
                Err(reason) => {
                    log(format!("{branch}: {reason}"));
                    left_behind.push(json!({"car_id_short": id8(&jid), "reason": reason.as_str()}));
                    continue;
                }
            };
            let r = sh_unchecked(&[
                "git",
                "-C",
                clone,
                "merge",
                "--no-ff",
                "-m",
                &format!("train: merge {branch}"),
                &head,
            ])?;
            if r.status.success() {
                boarded.push((j, branch, head));
            } else {
                let diff =
                    sh_unchecked(&["git", "-C", clone, "diff", "--name-only", "--diff-filter=U"])?;
                let conflicted: Vec<String> = stdout_str(&diff)
                    .split_whitespace()
                    .map(str::to_string)
                    .collect();
                sh_unchecked(&["git", "-C", clone, "merge", "--abort"])?;

                // Before abandoning it, try re-railing.
                //
                // The commonest conflict here is not a real one. The
                // repo squash-merges, so a car cut before the last
                // train — or stacked on a car that has since landed —
                // carries commits whose CHANGES are already in main but
                // whose SHAS are not ancestors of it. Merging re-applies
                // landed hunks on top of themselves and collides.
                //
                // `git rebase` is the tool that knows the difference: it
                // drops a patch already present upstream. So replay the
                // car's own commits onto the consist as it stands and
                // merge that instead. A car with a GENUINE conflict
                // fails the rebase too and is skipped exactly as before.
                //
                // Measured cost of not doing this: four cars re-railed
                // by hand in one evening (2026-08-15), each one a fresh
                // branch name, a repointed `metadata.branch` and a wait
                // for the next window — and the same by hand on 08-12
                // and 08-14. The conductor already knows everything it
                // needs; it just gave up one step early.
                if let Some(rerailed) = rerail_onto_consist(clone, &train_branch, &branch)? {
                    let retry = sh_unchecked(&[
                        "git",
                        "-C",
                        clone,
                        "merge",
                        "--no-ff",
                        "-m",
                        &format!("train: merge {branch} (re-railed)"),
                        &rerailed,
                    ])?;
                    if retry.status.success() {
                        log(format!(
                            "{branch}: re-railed onto the consist — its base was no longer an \
                             ancestor of main"
                        ));
                        // The ORIGINAL head is still what boarded: the
                        // sweep's licence to delete the branch compares
                        // against the ref the car names, and re-railing
                        // changed the shas we merged, not the car.
                        boarded.push((j, branch, head));
                        continue;
                    }
                    sh_unchecked(&["git", "-C", clone, "merge", "--abort"])?;
                }
                // ONE reason string, journal and Job alike — the chip
                // the yard renders and the line the operator greps
                // must never tell different stories.
                let reason = skip_reason_conflict(&conflicted, self.policy.skip_reason_file_budget);
                let skips = next_skip_count(&j);
                log(format!(
                    "{branch}: {reason} — left for the next train (refused {skips}x in a row)"
                ));
                left_behind.push(json!({
                    "car_id_short": id8(job_id(&j)?),
                    "reason": reason.as_str(),
                }));
                // The COUNT rides with the reason (backlog 94896e74). One
                // skip is routine — 39% of trains carry one and depart —
                // so the reason alone says nothing about whether this car
                // is having a bad window or has been refused all morning.
                // Cleared with `skip_reason` on boarding, so it counts
                // CONSECUTIVE skips.
                self.merge_job_metadata(job_id(&j)?, conflict_skip_write(&reason, skips))
                    .await?;
                skipped.push((j, branch));
            }
        }

        let skipped_names = skipped
            .iter()
            .map(|(_, b)| b.as_str())
            .collect::<Vec<_>>()
            .join(", ");

        if boarded.is_empty() {
            self.record_no_departure(&NoDeparture::AllConflicted {
                branches: skipped_names.clone(),
            })
            .await?;
            return Ok(());
        }

        // THE CONSIST CHECK — the assembled tree answers the cheap
        // questions before the train spends anything on the expensive
        // ones. See the section comment above `consist_check` for the
        // arrival-rate numbers that bought it; the short version is
        // that a per-branch gate cannot see a failure that exists only
        // in the combination, and every failure of the last two days
        // was one of those.
        //
        // Placed BEFORE the push, not merely before the PR: a refused
        // consist should leave nothing behind on the forge to clean up
        // later (the 62 stale `train/*` branches of ab3fa473 are what
        // that debt looks like when nobody owns it).
        //
        // Freshen the trunk ref FIRST. The cheap lints resolve their
        // baseline as `merge-base(origin/main, HEAD)` in this clone,
        // and a train that landed since this board's `ensure_clone`
        // leaves that ref lagging behind the assembled tree — which
        // reads already-landed changes as this consist's own and
        // refuses it (2026-09-06). Best-effort: a failed fetch logs
        // and the lints use the ref as it stands, exactly as before.
        freshen_trunk(clone);
        let verdict =
            super::consist_job::isolated_consist_check(clone, &self.cfg.fork_url, &self.policy)
                .await;
        for w in verdict.warnings() {
            log(format!(
                "consist check: {w} — skipping it, a broken check must not hold a train"
            ));
        }
        if let ConsistVerdict::Refuse { failed, ran, .. } = &verdict {
            let reason = consist_refusal_reason(failed, self.policy.skip_reason_file_budget);
            log(format!(
                "consist check: {} of {ran} checks disagree with the assembled tree",
                failed.len()
            ));
            // The output goes in the journal in full, not just the
            // name: what cost 90 minutes was learning ONE bit per
            // attempt, and the bit is in what the check SAID.
            for f in failed {
                log(format!("consist check: {} said —", f.name));
                for line in f.output.lines() {
                    log(format!("consist check:   {line}"));
                }
            }
            // NOBODY'S CAR IS AT FAULT. Each one was green on its own
            // branch; the tree only broke once they were merged
            // together. So: no train packet, no PR, no push, no CI spent
            // — and every car keeps `metadata.train` unset (never
            // boarded, so still `parked_ready`) and `red_trains`
            // untouched. Striking cars for a combination failure is the
            // bug we already know about.
            //
            // THE EVIDENCE RIDES THE CARS, not a cancelled train. It
            // used to live in `consist_check` on a pr-train Job that
            // existed only to be cancelled (4860aff8); the car whose
            // boarding it blocks is both the honest owner of the fact
            // and where an operator is already looking. `skip_reason`
            // names the check and the files; `consist_refusal` carries
            // what each check SAID, in full, because what cost 90
            // minutes on 2026-09-04 was learning one bit per attempt.
            // Both are cleared in the same write that stamps a later
            // boarding, so neither outlives the refusal.
            let refusal = json!({
                "verdict": "refused",
                "checks_run": ran,
                "failed": failed
                    .iter()
                    .map(|f| json!({
                        "lint": f.name,
                        "files": f.files,
                        "output": f.output,
                    }))
                    .collect::<Vec<_>>(),
            });
            for (j, _branch, _head) in &boarded {
                let cid = job_id(j)?;
                self.merge_job_metadata(
                    cid,
                    vec![
                        ("skip_reason", json!(reason)),
                        ("consist_refusal", refusal.clone()),
                    ],
                )
                .await?;
            }
            self.record_no_departure(&NoDeparture::ConsistRefused {
                reason: format!("consist check refused — {reason}"),
                cars: boarded.len(),
            })
            .await?;
            return Ok(());
        }
        // "0 cheap lint(s) clean" was the line a could-not-list
        // warning left here, and it read as green (699145ac). A
        // consist that ran nothing is UNCHECKED — the line says so, and
        // `consist_step_fields` puts the same words on the assemble
        // step below so the yard cannot read it as clean either.
        log(consist_departure_line(&verdict));

        sh(&["git", "-C", clone, "push", "fork", &train_branch])?;
        let train_ref_out = sh(&["git", "-C", clone, "rev-parse", "--short", "HEAD"])?;
        let train_ref = stdout_str(&train_ref_out).trim().to_string();

        // THE PACKET OPENS HERE — one call site, after the consist check
        // passed and after the branch is on the forge, so a pr-train Job
        // exists only for a train that is actually departing (4860aff8).
        // AFTER the push on purpose: a push that fails leaves one stale
        // `train/*` branch, while a packet opened for a train that never
        // pushed HOLDS THE TRACK until a human cancels it.
        let Some(train) = self.open_train_job(&train_branch, &window).await? else {
            // `None` is the dry-run answer and a dry run returned long
            // before the clone was touched. Say it, rather than running
            // on with no packet to record anything against.
            log("no train departed — the train Job was not opened; nothing further attempted");
            return Ok(());
        };
        let train_id = job_id(&train)?.to_string();

        let mut lines: Vec<String> = boarded
            .iter()
            .map(|(j, b, _)| {
                format!(
                    "- `{b}` — {} (Job `{}`)",
                    j.get("title").and_then(Value::as_str).unwrap_or_default(),
                    id8(j.get("id").and_then(Value::as_str).unwrap_or("?"))
                )
            })
            .collect();
        if !skipped.is_empty() {
            lines.push(String::new());
            lines.push(format!(
                "Left behind on merge conflicts (next train): {skipped_names}"
            ));
        }
        let body = format!(
            "The {window} train: {} change(s) batched by the conductor.\n\n{}\n\n\
             🤖 opened by `boss train` (pr-train Workflow)",
            boarded.len(),
            lines.join("\n")
        );
        let pr_url = self
            .forge
            .pr_create(
                &self.cfg.gh_repo,
                &train_branch,
                &format!("train: {window} ({} changes)", boarded.len()),
                &body,
            )
            .await?;

        let boarded_ids: Vec<String> = boarded
            .iter()
            .map(|(j, _, _)| job_id(j).map(str::to_string))
            .collect::<Result<_>>()?;
        let skipped_branches: Vec<String> = skipped.iter().map(|(_, b)| b.clone()).collect();
        // THE TRAIN'S CHANNEL — the heaviest of its cars' (data < config
        // < software < infra, the order `channels.rs` resolves a mixed
        // car on), stamped here beside `boarded_jobs` so a reader can
        // tell a config-only train from a software one without opening
        // every car, and the yard can name it ('data train · 1 car').
        // A car with no stamp reads as software, its own default
        // (cffef553, 2026-09-15).
        let train_channel = crate::channels::train_channel(boarded.iter().map(|(j, _, _)| j));
        self.merge_job_metadata(
            &train_id,
            vec![
                ("boarded_jobs", json!(boarded_ids)),
                ("delivery_channel", json!(train_channel)),
                ("skipped_branches", json!(skipped_branches)),
                // The train's own record of who it left behind and
                // why — the arrival report reads THIS, because a
                // car's skip_reason clears the moment a later train
                // boards it.
                ("left_behind", json!(left_behind)),
            ],
        )
        .await?;
        // Every car the DOCK held is left behind by THIS train: one more
        // on its dock streak, and an alarm at the threshold (2fccbfd6).
        // Assembly's conflicts are not counted here — they count `skips`,
        // and a routine conflict is no alarm (review of car 5eb1967e, M3).
        self.count_left_behind(&held).await;
        let train = self.get_job(&train_id).await?;
        let boarded_note = boarded
            .iter()
            .map(|(j, b, _)| {
                format!(
                    "{b} ({})",
                    id8(j.get("id").and_then(Value::as_str).unwrap_or("?"))
                )
            })
            .collect::<Vec<_>>()
            .join(", ");
        // THE SHA EACH CAR CONTRIBUTED, recorded because twice on
        // 2026-08-17 a consist carried a commit nobody intended and
        // nothing said so: `feat/dev-shared-target` was 3370b42
        // locally and 96109f7 on the forge, and the train assembled
        // the stale one silently. The head is already resolved to
        // board the car, so writing it down costs nothing and turns
        // "which commit did this train actually carry" from a hand
        // diff into a field.
        let heads_note = boarded
            .iter()
            .map(|(j, b, _)| {
                // `fork/<branch>` on purpose, not the local ref: this
                // records what the train ASSEMBLED FROM, which is the
                // thing a reader needs when a consist misbehaves.
                let sha = sh_unchecked(&[
                    "git",
                    "-C",
                    &self.cfg.clone,
                    "rev-parse",
                    "--short",
                    "--verify",
                    "--quiet",
                    &format!("fork/{b}"),
                ])
                .ok()
                .filter(|o| o.status.success())
                .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
                .filter(|s| !s.is_empty())
                .unwrap_or_else(|| "unknown".to_string());
                format!(
                    "{} {b}@{sha}",
                    id8(j.get("id").and_then(Value::as_str).unwrap_or("?"))
                )
            })
            .collect::<Vec<_>>()
            .join(", ");
        self.complete_step(
            &train,
            find_step(&train, "collect", "Collect what is ready to board"),
            &[("boarded", Some(boarded_note))],
        )
        .await?;
        // The consist verdict rides the assemble step — the step that
        // names the assembled branch, which is what the check tested.
        // Until 699145ac a green consist left nothing on the packet at
        // all, so the yard could not tell a checked tree from one the
        // conductor never managed to look at.
        let mut assemble_fields: Vec<(&str, Option<String>)> = vec![
            ("train_ref", Some(format!("{train_branch}@{train_ref}"))),
            ("car_heads", (!heads_note.is_empty()).then_some(heads_note)),
            (
                "skipped",
                Some(if skipped_names.is_empty() {
                    "none".to_string()
                } else {
                    skipped_names.clone()
                }),
            ),
        ];
        assemble_fields.extend(consist_step_fields(&verdict));
        self.complete_step(
            &train,
            find_step(&train, "assemble", "Assemble the train branch"),
            &assemble_fields,
        )
        .await?;
        self.complete_step(
            &train,
            find_step(&train, "pr", "Open the batched PR"),
            &[("pr_url", Some(pr_url.clone()))],
        )
        .await?;

        for (j, _branch, head) in &boarded {
            // BOARDING DOES NOT COMPLETE `review` — the merge does.
            //
            // It used to complete it here, and that quietly made
            // cancelling a loaded train impossible. A released car has
            // to become `parked_ready` again, which requires its review
            // step to be ready or active; but a completed step is FROZEN
            // at the row (`update_step_at` pins status, completed_on and
            // metadata on terminal rows, deliberately, so a racing
            // read-modify-write cannot demote it). So the cancel path's
            // reopen was a no-op that returned 204, and every "released
            // car back to the dock" line it logged was false — the car
            // had `train` cleared but stayed unboardable forever. The
            // only reason nobody hit it is that every cancel until now
            // carried zero cars.
            //
            // Boarded-ness does not need the step at all: it is
            // `metadata.train`, which is what `parked_ready` already
            // reads, and which a cancel can clear because metadata is
            // not frozen. So the step keeps meaning what it says —
            // this change is open for review until it lands — and
            // release becomes a metadata write with nothing to reverse.
            // (Requires no workflow edit: the spec still gates the
            // `merged` outcome on `steps.review.done`, and the merge
            // block below is what satisfies it.)
            //
            // skip_reason cleared on boarding, in the same update that
            // stamps the train: an earlier window's skip note must not
            // outlive the skip — the key is REMOVED (Null), not left
            // behind as "". `consist_refusal` — the lint output a
            // refused consist leaves on the car it blocked — comes off
            // in the same write, for the same reason. So does `skips`,
            // and that is what makes the count CONSECUTIVE: a car that
            // rides a train carries no history of refusals into its
            // next window (94896e74).
            //
            // `boarded_head` rides here too, and lives on the CAR
            // rather than in a second list on the train: the sweep
            // already fetches every boarded car, so the fact stays in
            // one place (guideline 9a) and costs no extra call. It is
            // rewritten on every boarding, so a car that rides a later
            // train carries that train's head, not the first one's.
            self.merge_job_metadata(
                job_id(j)?,
                vec![
                    ("train", json!(train_id.as_str())),
                    ("boarded_head", json!(head.as_str())),
                    ("skip_reason", Value::Null),
                    (boss_jobs::car::SKIPS, Value::Null),
                    // The dock's streak and its alarm go with it: the next
                    // streak is a new fact and may file its own (2fccbfd6).
                    (LEFT_BEHIND_TRAINS, Value::Null),
                    (LEFT_BEHIND_ALARM, Value::Null),
                    ("consist_refusal", Value::Null),
                ],
            )
            .await?;
        }
        log(format!(
            "train {} boarded {}, PR {pr_url}",
            id8(&train_id),
            boarded.len()
        ));
        record_board_decision(&BoardDecision::Boarded {
            cars: boarded.len(),
        });

        // THE TRAIN GATE, FILED IN THIS PASS (backlog 95c349a5). Until
        // 2026-09-14 the gate was filed only from the ci-step block in
        // `reconcile`, so every train waited for the next tick before its
        // Rust checks started — 8m30s, 4m29s, 6m30s, 2m30s on the last
        // four, on the critical path of every landing. `train_gate` is
        // the same launch path the ci block uses and is idempotent on
        // KEY_RUN, so that block stays the retry for a launch that fails
        // here (the gate bound, kubectl, the API). BEST-EFFORT, after
        // the cars are stamped: nothing in boarding may abort on it — a
        // train that boarded is a train, gate or no gate. The instant is
        // `Utc::now()`, not the pass's `now`: the boarding pass can run
        // minutes, and `train_gate_launched_at` before the pr step's own
        // `completed_at` would be a record nobody could read.
        match self.get_job(&train_id).await {
            Ok(mut fresh) if gate_due_at_boarding(&fresh) => {
                self.train_gate(&mut fresh, &train_id, Utc::now()).await;
            }
            Ok(_) => log(format!(
                "train {}: not filing its gate at boarding — it already carries one, or its record is incomplete; the reconcile pass files it",
                id8(&train_id)
            )),
            Err(e) => log(format!(
                "train {}: could not re-read the train to file its gate at boarding ({e}) — the reconcile pass files it",
                id8(&train_id)
            )),
        }
        Ok(())
    }

    // -----------------------------------------------------------------------
    // Cancel — the operator's judgment on a train that will not arrive
    // -----------------------------------------------------------------------

    /// Cancel an open train (David's ask: trains that don't arrive
    /// were cleaned up by hand, and cancellation orphaned the cars).
    /// Car-release comes FIRST, so a crash mid-cancel leaves cars
    /// free rather than orphaned:
    ///   1. release every still-open boarded car back to the dock —
    ///      review step back to `ready` (the dock predicate requires
    ///      it; clearing metadata alone re-boards nothing),
    ///      `metadata.train` removed, `skip_reason` saying why;
    ///   2. close the PR unmerged;
    ///   3. complete the `cancelled` terminal with the reason —
    ///      jobs-api then closes the Job with outcome=cancelled and
    ///      skips the remaining steps;
    ///   4. delete the train's OWN `train/*` branch — never a car's:
    ///      the cars keep their branches (train_branch_to_delete is
    ///      the pin, and it is tested).
    /// The operator's verb. Never counts a red against the cars — an
    /// operator cancels for reasons of their own (a bad consist, a
    /// withdrawn change), and only the automatic red-stall path below
    /// has evidence that the CARS were implicated.
    pub(super) async fn cancel(&self, handle: &str, reason: &str) -> Result<()> {
        self.cancel_train(handle, reason, false, false).await
    }

    /// Honour an operator's `cancel_requested` stamp — the yard's cancel
    /// button, read by `reconcile`. Returns whether the request has
    /// claimed this train's pass: when it has, the caller skips the rest
    /// of the pass for this train, because a train under a cancel
    /// request must not go on to merge — whether the cancel succeeded,
    /// is dry, or is being retried.
    ///
    /// NON-FATAL BY CONSTRUCTION: this returns `bool`, not `Result`, so
    /// the reconcile loop cannot `?` it. A cancel is forge writes first
    /// (`cancel_train` closes the PR before releasing a car) and the
    /// forge is the flaky half; a refusal there LOGS and the train stays
    /// intact — cars aboard, stamp in place — so the next pass retries.
    /// A fatal write in this loop once froze all landings for ~8h
    /// (boss-conductor-loop-writes-must-not-be-fatal).
    async fn honour_cancel_request(
        &self,
        train: &Value,
        tid: &str,
        pr_state: Option<&str>,
    ) -> bool {
        if let Some(refusal) = operator_cancel_refusal(train) {
            log(format!(
                "train {}: cancel requested but {refusal} — refusing, cars stay landed",
                id8(tid)
            ));
            if let Err(e) = self
                .merge_job_metadata(tid, vec![("cancel_refused", json!(refusal))])
                .await
            {
                log(format!(
                    "train {}: cancel_refused stamp failed (non-fatal, retries next pass): {e}",
                    id8(tid)
                ));
            }
            return false;
        }
        let Some(reason) = operator_cancel_reason(train) else {
            return false;
        };
        if pr_state != Some("OPEN") {
            // Neither merged nor open — closed on the forge by hand, or
            // mid-merge. The same gate the automatic rule keeps: the
            // operator verb (`boss train cancel`) takes it from here.
            log(format!(
                "train {}: cancel requested but its PR is {} — leaving it to `boss train cancel`",
                id8(tid),
                pr_state.unwrap_or("unknown")
            ));
            return false;
        }
        log(format!("train {} cancelling: {reason}", id8(tid)));
        if self.cfg.dry {
            log(format!("DRY: would cancel {} ({reason})", id8(tid)));
        } else if let Err(e) = self.cancel_train(tid, &reason, false, false).await {
            log(format!(
                "train {}: cancel failed (non-fatal, train intact, retries next pass): {e}",
                id8(tid)
            ));
        }
        true
    }

    /// `outside_release`: this cancel is a red proven outside the consist,
    /// so every released car is stamped with the train's id as its one
    /// such release (`KEY_OUTSIDE_RELEASE`, backlog 5541d813).
    async fn cancel_train(
        &self,
        handle: &str,
        reason: &str,
        count_red: bool,
        outside_release: bool,
    ) -> Result<()> {
        let listed = rows(
            self.api(
                Method::GET,
                "/api/jobs?kind=pr-train&status=open&limit=50",
                None,
            )
            .await?,
        )?;
        let mut trains = Vec::with_capacity(listed.len());
        for t0 in &listed {
            trains.push(self.get_job(job_id(t0)?).await?);
        }
        let train = resolve_train(&trains, handle)?;
        let tid = job_id(train)?;
        // Refuse a train that has no terminal to complete BEFORE any
        // write, and say what was found. On 2026-09-16 pr-train
        // 06e5610f was admitted with nine of its ten steps — a slow
        // database, a client timeout, the `cancelled` terminal never
        // written (backlog f2ba226e; admission is one transaction
        // since) — and this verb refused "step missing on job" only
        // AFTER closing the PR and releasing the cars. The registry's
        // re-evaluation logs the same divergence as "pairing by slug
        // ... unpaired" (registry.rs); this is the operator-facing
        // half of that warning, at the one verb that needed the row.
        if find_step(train, "cancelled", "Cancelled — nothing to board").is_none() {
            let version = train
                .get("workflow_version")
                .and_then(Value::as_i64)
                .unwrap_or_default();
            let present: Vec<String> = train
                .get("steps")
                .and_then(Value::as_array)
                .map(|a| a.iter().map(step_label).collect())
                .unwrap_or_default();
            bail!(
                "refusing to cancel train {}: its steps diverged from its workflow spec — \
                 the `cancelled` terminal has no row on this job (pinned pr-train v{version}, \
                 {} step row(s): {}), so it can never be completed. This is the residue of a \
                 partial admission (backlog f2ba226e). Repair door: re-materialise the \
                 unpaired steps of the pinned version onto the packet (a follow-up verb; until \
                 it exists the only door is a status close through the jobs API). Nothing was \
                 written: the PR is still open and the cars are still aboard.",
                id8(tid),
                present.len(),
                present.join(", ")
            );
        }
        // NOR A TERMINAL THE STEP API WILL REFUSE (backlog 5186c5e1).
        // `cancelled` waits on the `empty` marker, which a train that
        // boarded cars never carries, and this verb completes it without
        // one: the step API takes that only because the row is an abort
        // (`outcome_kind = aborted` completes from any open state,
        // 570e72bd). A version whose row were not would answer 409 at the
        // foot of this function — after the PR was closed and the cars
        // released. The forge writes stay first (10bb1e1a: a forge
        // failure must leave the train intact); this is the one jobs-API
        // refusal that can be read before them, so it is read here.
        if let Some(step) = find_step(train, "cancelled", "Cancelled — nothing to board")
            && !completes_by_hand(step)
        {
            let version = train
                .get("workflow_version")
                .and_then(Value::as_i64)
                .unwrap_or_default();
            bail!(
                "refusing to cancel train {}: its `cancelled` terminal is {} and its row is not \
                 an abort (no outcome_kind = aborted on pinned pr-train v{version}), so the step \
                 API would refuse its completion without the `empty` marker — after the PR was \
                 closed and the cars released. Move the packet to a version whose `cancelled` \
                 is an abort (`boss job convert`). Nothing was written: the PR is still open \
                 and the cars are still aboard.",
                id8(tid),
                step.get("status")
                    .and_then(Value::as_str)
                    .unwrap_or("of unknown status"),
            );
        }

        let boarded: Vec<String> = train
            .get("metadata")
            .and_then(|m| m.get("boarded_jobs"))
            .and_then(Value::as_array)
            .map(|a| {
                a.iter()
                    .filter_map(|v| v.as_str().map(str::to_string))
                    .collect()
            })
            .unwrap_or_default();
        let mut cars = Vec::with_capacity(boarded.len());
        for cid in &boarded {
            cars.push(self.get_job(cid).await?);
        }
        // Say what is NOT being released, and why. A car that moved on
        // is the interesting case: silently skipping it would leave the
        // operator with a cancel that released fewer cars than the train
        // claims to carry, and no way to tell whether that was correct.
        for car in &cars {
            if car.get("status").and_then(Value::as_str) != Some("open") {
                continue;
            }
            let owner = car
                .get("metadata")
                .and_then(|m| m.get("train"))
                .and_then(Value::as_str);
            match owner {
                Some(t) if t == tid => {}
                Some(other) => log(format!(
                    "car {} now rides {} — not releasing it",
                    id8(job_id(car)?),
                    id8(other)
                )),
                None => log(format!(
                    "car {} was already released — leaving its record alone",
                    id8(job_id(car)?)
                )),
            }
        }

        let pr_url = find_step(train, "pr", "Open the batched PR")
            .and_then(|s| s.get("metadata"))
            .and_then(|m| m.get("pr_url"))
            .and_then(Value::as_str)
            .unwrap_or_default();
        // Cancel the CI BEFORE closing the PR. Closing first leaves a
        // window where the run is still burning the single-concurrency
        // runner for a PR that is already gone, which is the state
        // 89b27e60 measured 27 minutes into.
        let train_head = train_ref_of(train)
            .and_then(|r| r.rsplit('@').next())
            .unwrap_or_default()
            .to_string();
        if !pr_url.is_empty() || !train_head.is_empty() {
            // Last path segment of the PR url is its number on both
            // forges; kept inline rather than reaching for a
            // Forgejo-specific helper from forge-blind code.
            let idx = pr_url
                .trim_end_matches('/')
                .rsplit('/')
                .next()
                .unwrap_or_default()
                .to_string();
            if self.cfg.dry {
                log(format!(
                    "DRY: would cancel CI runs for PR #{idx} / {train_head}"
                ));
            } else {
                match self.forge.cancel_ci_runs(&idx, &train_head).await {
                    Ok(0) => log("cancel: no CI runs were still active".to_string()),
                    Ok(n) => log(format!("cancel: cancelled {n} in-flight CI run(s)")),
                    Err(e) => log(format!("cancel: CI cancellation failed, continuing: {e}")),
                }
            }
        }

        if !pr_url.is_empty() {
            if self.cfg.dry {
                log(format!("DRY: would close {pr_url} unmerged"));
            } else {
                self.forge.close_pr(pr_url).await?;
                log(format!("closed {pr_url} unmerged"));
            }
        }

        // RELEASE THE CARS ONLY AFTER THE FORGE WRITES SUCCEED. Cancel
        // does two kinds of write: releasing a car is a jobs-API metadata
        // write (reliable, local), closing the PR is a forge write (the
        // flaky one — an unreachable forge, a read-only token). Releasing
        // FIRST left "half-cancelled" trains: the cars back on the dock
        // but the PR still open, because close_pr's `?` returned Err with
        // the release already done (10bb1e1a; the comment at the top of
        // this file's cancel path names the two it stranded). Doing the
        // flaky writes first means a forge failure aborts here with the
        // train fully intact — cars still aboard, PR still open — so a
        // re-run is clean, and the car release only happens once the PR is
        // actually closed.
        for car in releasable_cars(&cars, tid) {
            let cid = job_id(car)?;
            // NOTHING TO REOPEN. Releasing a car is a metadata write and
            // only a metadata write, because boarding no longer completes
            // its `review` step — see the boarding loop. This used to PUT
            // the step back to `ready`, which the row silently refused
            // (terminal steps are frozen in `update_step_at`) and which
            // now 409s out loud, taking the whole cancel with it. A car
            // that predates this change still carries a completed review
            // and cannot be released; those were translated into fresh
            // packets by hand on 2026-08-15 rather than reversed.
            self.merge_job_metadata(
                cid,
                release_stamps(car, reason, count_red, outside_release.then_some(tid)),
            )
            .await?;
            log(format!("released car {} back to the dock", id8(cid)));
        }

        // The cancelled terminal is gated (blocked_by) on collect; a
        // train that died mid-assembly never completed it. Close that
        // gate honestly first — nothing boarded on the record.
        let collect = find_step(train, "collect", "Collect what is ready to board");
        if !step_done(collect) {
            self.complete_step(
                train,
                collect,
                &[(
                    "boarded",
                    Some("nothing — train cancelled before boarding completed".to_string()),
                )],
            )
            .await?;
        }
        self.complete_step(
            train,
            find_step(train, "cancelled", "Cancelled — nothing to board"),
            &[("reason", Some(reason.to_string()))],
        )
        .await?;

        if let Some(branch) = train_branch_to_delete(train) {
            if self.cfg.dry {
                log(format!(
                    "DRY: would delete branch {branch} (train cancelled)"
                ));
            } else if self.forge.delete_branch(&branch).await? {
                log(format!("deleted branch {branch} (train cancelled)"));
            } else {
                log(format!("branch {branch} already gone (train cancelled)"));
            }
        }
        log(format!("train {} cancelled: {reason}", id8(tid)));
        Ok(())
    }
}

/// Whether the step API takes a hand completion of `step` with its
/// predicate unread: the engine has opened it (or it is already done,
/// which `complete_step` skips), or its materialised row is an abort,
/// which completes from any open state (boss-jobs `update_step`).
fn completes_by_hand(step: &Value) -> bool {
    let opened = matches!(
        step.get("status").and_then(Value::as_str),
        Some("ready" | "active" | "completed" | "skipped")
    );
    let abort = step
        .get("metadata")
        .and_then(|m| m.get("outcome_kind"))
        .and_then(Value::as_str)
        == Some("aborted");
    opened || abort
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::train::test_support::*;

    // -- a held car is written when its hold CHANGES -----------------------

    /// D1 of design 42279fb2 walks the dock every two minutes as well as
    /// on every board, and each held car's reason is a full job PUT and
    /// an event. A hold that says the same thing it said last pass is
    /// not a new fact, so it is not written again; a new reason, or any
    /// stamp (a launch is always new), is.
    #[test]
    fn a_hold_that_has_not_changed_is_not_written_again() {
        let car = json!({"metadata": {
            "skip_reason": "waiting for a gate slot (3 gate(s) running of 3)",
            "branch": "feat/x",
        }});
        assert!(metadata_already(
            &car,
            &[(
                "skip_reason",
                json!("waiting for a gate slot (3 gate(s) running of 3)")
            )]
        ));
        assert!(!metadata_already(
            &car,
            &[(
                "skip_reason",
                json!("waiting for a gate slot (2 gate(s) running of 3)")
            )]
        ));
        assert!(
            !metadata_already(
                &car,
                &[
                    (
                        "skip_reason",
                        json!("waiting for a gate slot (3 gate(s) running of 3)")
                    ),
                    (dock_regate::BASE_REGATE, json!({"main": "m"})),
                ]
            ),
            "a stamp the car does not carry is written"
        );
        assert!(!metadata_already(
            &json!({}),
            &[("skip_reason", json!("anything"))]
        ));
    }

    // -- every other dock hold is written when it CHANGES (df93994b) -------
    //
    // The bay path above was guarded in #690; the two-strike, ordering-edge
    // and missing-branch holds still wrote their reason on every walk of
    // the dock — measured, 13 no-op writes on the two-strike path in 50h,
    // before the two-minute refresh drove the walk at all. Each test below
    // walks a real dock (`candidates`, an in-process jobs API, a real
    // clone) holding two cars on one path: one already saying the reason,
    // which must not be written, and a control carrying none or an older
    // one, which must — so a path the walk never reached cannot pass.

    /// A car parked at review, ready to be judged by the dock.
    fn parked_car(id: &str, md: Value) -> Value {
        json!({
            "id": id, "kind": "ship-a-change", "status": "open", "metadata": md,
            "steps": [{"id": format!("{id}-rev"), "spec_slug": "review",
                       "title": "Open for review", "status": "ready", "metadata": {}}]
        })
    }

    /// Walk the dock once over `cars` (and `others`, readable by id but
    /// not listed), and return every job-metadata merge it sent.
    async fn walk_dock(cars: Vec<Value>, others: Vec<Value>) -> Vec<(String, Value)> {
        let (_g, clone) = clone_fixture("dock-hold-unchanged");
        walk_dock_in(&clone, cars, others).await
    }

    /// `walk_dock` over a clone the caller has already given branches.
    async fn walk_dock_in(
        clone: &std::path::Path,
        cars: Vec<Value>,
        others: Vec<Value>,
    ) -> Vec<(String, Value)> {
        use axum::extract::Path;
        use axum::routing::{get, patch};
        use axum::{Json, Router};
        use std::sync::{Arc, Mutex};

        let merges: Arc<Mutex<Vec<(String, Value)>>> = Arc::default();
        let rec = merges.clone();
        let listed = cars.clone();
        let every: Vec<Value> = cars.into_iter().chain(others).collect();
        let app = Router::new()
            .route(
                "/api/jobs",
                get(move || {
                    let listed = listed.clone();
                    async move { Json(json!({"data": listed, "total": listed.len()})) }
                }),
            )
            .route(
                "/api/jobs/{id}",
                get(move |Path(id): Path<String>| {
                    let every = every.clone();
                    async move {
                        match every.into_iter().find(|c| c["id"] == json!(id)) {
                            Some(c) => Ok(Json(c)),
                            None => Err(axum::http::StatusCode::NOT_FOUND),
                        }
                    }
                }),
            )
            .route(
                "/api/jobs/{id}/metadata",
                patch(move |Path(id): Path<String>, Json(b): Json<Value>| {
                    let rec = rec.clone();
                    async move {
                        rec.lock().unwrap().push((id, b));
                        axum::http::StatusCode::NO_CONTENT
                    }
                }),
            );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let mut c = dock_conductor(clone);
        c.cfg.jobs = format!("http://{addr}");
        let pass = c.candidates().await.expect("the walk completes");
        assert!(pass.boardable.is_empty(), "every car here is held");
        merges.lock().unwrap().clone()
    }

    /// The ids written, each with the `skip_reason` it was written with.
    fn written(merges: &[(String, Value)]) -> Vec<(String, Value)> {
        merges
            .iter()
            .map(|(id, b)| (id.clone(), b["skip_reason"].clone()))
            .collect()
    }

    #[tokio::test]
    async fn a_two_strike_hold_that_has_not_changed_is_not_written_again() {
        let reason = car_hold_reason(&json!({"metadata": {"red_trains": 2}}), 2)
            .expect("two reds hold a car");
        let cars = vec![
            parked_car(
                "same",
                json!({"branch": "feat/same", "red_trains": 2, "skip_reason": reason}),
            ),
            parked_car("new", json!({"branch": "feat/new", "red_trains": 2})),
        ];
        assert_eq!(
            written(&walk_dock(cars, vec![]).await),
            vec![("new".to_string(), json!(reason))],
            "the car already saying it is not written; the control is"
        );
    }

    #[tokio::test]
    async fn an_ordering_edge_hold_that_has_not_changed_is_not_written_again() {
        let pred = json!({"id": "pred", "status": "open", "metadata": {"branch": "feat/pred"}});
        let EdgeOutcome::Hold(hold) =
            boards_after_outcome("pred", &Predecessor::Found(pred.clone()))
        else {
            panic!("an open predecessor holds the car");
        };
        let edge = |id: &str, skip: Value| {
            parked_car(
                id,
                json!({"branch": format!("feat/{id}"), boss_jobs::car::BOARDS_AFTER: "pred", "skip_reason": skip}),
            )
        };
        let cars = vec![
            edge("same", json!(hold.reason)),
            edge("new", json!("branch feat/new not on fork")),
        ];
        assert_eq!(
            written(&walk_dock(cars, vec![pred]).await),
            vec![("new".to_string(), json!(hold.reason))],
            "the car already saying it is not written; the control is"
        );
    }

    /// ITEM 9 OF BACKLOG 7919fdcc. An edge-held car whose builder repaired
    /// it — the receipt now vouches for the head the forge carries — has
    /// its old dock re-gate red taken off while it waits on its edge, not
    /// only once the edge releases it: the yard and orient read that red as
    /// "not boardable" for as long as it stands. The control carries the
    /// same red with a receipt for another sha, and keeps it.
    #[tokio::test]
    async fn an_edge_held_car_whose_receipt_vouches_again_has_its_red_cleared() {
        let (_g, clone) = clone_fixture("dock-edge-red");
        let repaired = park_car(&clone, "feat/repaired", "a/repaired.rs");
        park_car(&clone, "feat/still-red", "a/still-red.rs");
        let main = rev(&clone, "origin/main");
        let pred = json!({"id": "pred", "status": "open", "metadata": {"branch": "feat/pred"}});
        let EdgeOutcome::Hold(hold) =
            boards_after_outcome("pred", &Predecessor::Found(pred.clone()))
        else {
            panic!("an open predecessor holds the car");
        };
        let red_car = |id: &str, vouches_for: &str| {
            let mut stamp = dock_regate::RegateStamp {
                head: "0dead0replay0".into(),
                gate_run: "gr-red".into(),
                reds: 1,
                ..dock_regate::RegateStamp::for_main(&main, &[])
            }
            .to_value(Utc::now());
            stamp[boss_jobs::dock_red::RED] = boss_jobs::dock_red::RegateRed {
                gate_run: "gr-red".into(),
                head: "0dead0replay0".into(),
                verdict: "failed".into(),
                ..Default::default()
            }
            .to_value();
            parked_car(
                id,
                json!({
                    "branch": format!("feat/{id}"),
                    boss_jobs::car::BOARDS_AFTER: "pred",
                    "skip_reason": hold.reason,
                    "regate_receipt": json!({"verdict": "green", "dirty": false,
                                             "head": vouches_for}).to_string(),
                    dock_regate::BASE_REGATE: stamp,
                }),
            )
        };
        let cars = vec![red_car("repaired", &repaired), red_car("still-red", &main)];
        let want = dock_regate::cleared_stamp(&cars[0]).expect("a red to clear");
        let merges = walk_dock_in(&clone, cars, vec![pred]).await;
        let cleared: Vec<(String, Value)> = merges
            .iter()
            .filter_map(|(id, b)| {
                b.get(dock_regate::BASE_REGATE)
                    .map(|s| (id.clone(), s.clone()))
            })
            .collect();
        assert_eq!(
            cleared,
            vec![("repaired".to_string(), want)],
            "only the car whose receipt vouches for the forge's head loses its red"
        );
    }

    #[tokio::test]
    async fn a_missing_branch_hold_that_has_not_changed_is_not_written_again() {
        let reason = skip_reason_branch_missing("feat/same");
        let cars = vec![
            parked_car(
                "same",
                json!({"branch": "feat/same", "skip_reason": reason}),
            ),
            parked_car("new", json!({"branch": "feat/new"})),
        ];
        assert_eq!(
            written(&walk_dock(cars, vec![]).await),
            vec![(
                "new".to_string(),
                json!(skip_reason_branch_missing("feat/new"))
            )],
            "the car already saying it is not written; the control is"
        );
    }

    // -- the dock's claim on a bay is withdrawn, not aged out --------------

    fn dock_car(id: &str, claim: Option<Value>) -> Value {
        let mut md = json!({"branch": format!("feat/{id}")});
        if let Some(c) = claim {
            md[crate::gate::DOCK_WAITING] = c;
        }
        json!({"id": id, "metadata": md})
    }

    /// Design 38f3a488 D3 (backlog b15b0f4e). A claim is written once and
    /// read alive off the refresh rule's last firing — and a refresh HELD
    /// by a train on the track still fires with rc 0, so a claim left in
    /// place would stand ahead of every builder through the whole transit,
    /// for a re-gate that cannot launch until the merge replaces its main.
    /// So the held pass withdraws every claim, and writes nothing to a car
    /// that carries none. The same sweep closes a walk of the dock: a car
    /// the walk held for any reason but a bay (two strikes, an edge, a
    /// missing branch, a review hold) is not waiting for one either.
    #[test]
    fn a_held_refresh_withdraws_every_claim_and_touches_no_other_car() {
        let now = Utc::now();
        let cars = vec![
            dock_car("waiting", Some(crate::gate::dock_waiting_stamp("m", now))),
            dock_car("bare", None),
            dock_car("cleared", Some(Value::Null)),
            dock_car(
                "heartbeat",
                Some(json!({"main": "m", "at": "2026-09-25T22:28:00Z"})),
            ),
        ];
        assert_eq!(
            claims_to_withdraw(&cars, &HashSet::new()),
            vec!["waiting".to_string(), "heartbeat".to_string()],
            "held: every claim the dock carries goes, in any shape"
        );
        let settled: HashSet<String> = ["waiting".to_string()].into();
        assert_eq!(
            claims_to_withdraw(&cars, &settled),
            vec!["heartbeat".to_string()],
            "a car whose claim this walk already decided is not written twice"
        );
    }

    // -- the arrival branch cleanup ----------------------------------------
    //
    // Cancel has deleted its train's branch since the verb existed;
    // nothing owned the branch after a HAPPY landing, and 62 stale
    // train/* branches accumulated on the forge between 08-13 and
    // 08-20 — squash merges mean ancestry can never classify them
    // after the fact (ab3fa473). The arrival record is the proof, and
    // the cleanup reads it at exactly the right moment.

    /// The forge as a call recorder: `delete_branch` notes the branch
    /// it was asked for and answers as told; every other verb is
    /// unreachable in these tests. The seam the Forge trait exists
    /// for, pointed at the cleanup.
    struct FakeForge {
        deleted: std::sync::Arc<std::sync::Mutex<Vec<String>>>,
        fail_deletes: bool,
    }

    #[async_trait]
    impl Forge for FakeForge {
        async fn pr_info(&self, _url: &str) -> Result<Value> {
            bail!("not exercised")
        }
        async fn pr_create(
            &self,
            _repo: &str,
            _head_branch: &str,
            _title: &str,
            _body: &str,
        ) -> Result<String> {
            bail!("not exercised")
        }
        async fn merge(&self, _url: &str, _message: &str) -> Result<()> {
            bail!("not exercised")
        }
        async fn close_pr(&self, _url: &str) -> Result<()> {
            bail!("not exercised")
        }
        async fn delete_branch(&self, branch: &str) -> Result<bool> {
            self.deleted.lock().unwrap().push(branch.to_string());
            if self.fail_deletes {
                bail!("HTTP 500: forge down");
            }
            Ok(true)
        }
        async fn branch_head(&self, _branch: &str) -> Result<Option<String>> {
            bail!("not exercised")
        }
        async fn cancel_ci_runs(&self, _pr_index: &str, _head_sha: &str) -> Result<usize> {
            bail!("not exercised")
        }
    }

    /// The tree root the config fixtures name.
    ///
    /// `scratch_path` rather than a fixed `/tmp/boss-train-test`, and it
    /// creates nothing — no test here touches the filesystem. The name
    /// still carries the uid and the pid, because a fixed name under the
    /// 1777 shared temp root is the shape that has bitten this repo
    /// fourteen times, and the next test that DOES touch this path would
    /// inherit the collision silently.
    fn train_test_home() -> std::path::PathBuf {
        boss_testing::scratch::scratch_path("boss-train-test")
    }

    /// A conductor whose config is fixtures and whose forge is the
    /// recorder — the cleanup touches neither the jobs API nor the
    /// tree, so nothing else needs to exist.
    fn cleanup_conductor(forge_kind: &str, forge: Box<dyn Forge>) -> Conductor {
        let home = train_test_home();
        Conductor {
            cfg: Config {
                jobs: "http://jobs.invalid".into(),
                gh_repo: "example/boss".into(),
                head_owner: "example".into(),
                fork_url: "https://github.com/example/boss-fork.git".into(),
                upstream_url: "https://github.com/example/boss.git".into(),
                home: home.display().to_string(),
                clone: home.join("repo").display().to_string(),
                forge_kind: forge_kind.into(),
                auto_merge: false,
                allow_local_jobs: true,
                ci_hours: 2,
                converge_alarm_mins: 30,
                stranded_alarm_mins: 45,
                auto_park_grace_mins: 10,
                auto_cancel: false,
                ci_host: None,
                gate_manifest: "/nonexistent/gate-runner.yaml".to_string(),
                gate_namespace: "boss-dev".to_string(),
                gate_required: false,
                dry: false,
            },
            http: crate::gate::machine_client().unwrap(),
            forge,
            owner: crate::owner::resolver("http://jobs.invalid"),
            policy: policy(),
            cluster: Box::new(FakeCluster::new(Vec::new(), "")),
        }
    }

    #[tokio::test]
    async fn a_happy_arrival_requests_deletion_of_the_trains_own_branch() {
        let deleted = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let forge = Box::new(FakeForge {
            deleted: std::sync::Arc::clone(&deleted),
            fail_deletes: false,
        });
        let c = cleanup_conductor("forgejo", forge);
        c.clean_arrived_train_branch(&arrived_train_with_branch())
            .await;
        assert_eq!(
            *deleted.lock().unwrap(),
            vec!["train/20260820-0600".to_string()]
        );
    }

    #[tokio::test]
    async fn a_failed_delete_does_not_fail_the_arrival() {
        let deleted = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let forge = Box::new(FakeForge {
            deleted: std::sync::Arc::clone(&deleted),
            fail_deletes: true,
        });
        let c = cleanup_conductor("forgejo", forge);
        // Returns () — there is no Result to fail: the forge blowing
        // up costs a journal line and nothing else. A leftover branch
        // is debt; a failed arrival is an outage.
        c.clean_arrived_train_branch(&arrived_train_with_branch())
            .await;
        // And the delete WAS attempted — the line narrates a real event.
        assert_eq!(
            *deleted.lock().unwrap(),
            vec!["train/20260820-0600".to_string()]
        );
    }

    #[tokio::test]
    async fn only_a_forgejo_happy_arrival_cleans_its_branch() {
        let deleted = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        // Under the github adapter the repo auto-deletes merged
        // train/* PR heads — nothing to own, nothing requested.
        let c = cleanup_conductor(
            "github",
            Box::new(FakeForge {
                deleted: std::sync::Arc::clone(&deleted),
                fail_deletes: false,
            }),
        );
        c.clean_arrived_train_branch(&arrived_train_with_branch())
            .await;
        // A cancelled train closes with `arrived` SKIPPED — its
        // branch was the cancel verb's, deleted at cancel time, and
        // the arrival cleanup asks for nothing.
        let mut cancelled = arrived_train_with_branch();
        cancelled["steps"][3]["status"] = json!("skipped");
        let c2 = cleanup_conductor(
            "forgejo",
            Box::new(FakeForge {
                deleted: std::sync::Arc::clone(&deleted),
                fail_deletes: false,
            }),
        );
        c2.clean_arrived_train_branch(&cancelled).await;
        assert!(deleted.lock().unwrap().is_empty());
        // Cancel's own pin is untouched by the arrival filter: the
        // cancelled train's branch is still exactly the one the
        // cancel path deletes.
        assert_eq!(
            train_branch_to_delete(&cancelled),
            Some("train/20260820-0600".to_string())
        );
    }

    // -- cancel releases cars only after the forge write succeeds -----
    struct CancelForge {
        close_called: std::sync::Arc<std::sync::Mutex<bool>>,
    }
    #[async_trait::async_trait]
    impl Forge for CancelForge {
        async fn pr_info(&self, _url: &str) -> Result<Value> {
            bail!("not exercised")
        }
        async fn pr_create(
            &self,
            _repo: &str,
            _head_branch: &str,
            _title: &str,
            _body: &str,
        ) -> Result<String> {
            bail!("not exercised")
        }
        async fn merge(&self, _url: &str, _message: &str) -> Result<()> {
            bail!("not exercised")
        }
        async fn close_pr(&self, _url: &str) -> Result<()> {
            *self.close_called.lock().unwrap() = true;
            bail!("HTTP 403: forge write refused")
        }
        async fn delete_branch(&self, _branch: &str) -> Result<bool> {
            bail!("not exercised")
        }
        async fn branch_head(&self, _branch: &str) -> Result<Option<String>> {
            bail!("not exercised")
        }
        async fn cancel_ci_runs(&self, _pr_index: &str, _head_sha: &str) -> Result<usize> {
            Ok(0)
        }
    }

    /// 10bb1e1a: releasing a car is a jobs-API metadata write; closing
    /// the PR is the flaky forge write. Releasing FIRST left
    /// "half-cancelled" trains — cars back on the dock, PR still open —
    /// when close_pr's `?` returned Err with the release already done.
    /// This drives cancel_train against a real in-process jobs server
    /// with a forge whose close_pr FAILS, and asserts NO car was
    /// released: the release now happens only after the PR is closed.
    #[tokio::test]
    async fn cancel_does_not_release_cars_when_close_pr_fails() {
        use axum::extract::Path;
        use axum::routing::get;
        use axum::{Json, Router};
        use std::sync::{Arc, Mutex};

        let train = json!({
            "id": "t1", "kind": "pr-train", "status": "open",
            "metadata": { "boarded_jobs": ["c1"], "train_ref": "train/x@abcdef1" },
            "steps": [
                {"id":"s-pr","spec_slug":"pr","title":"Open the batched PR","status":"completed","metadata":{"pr_url":"https://forge.example/david/boss/pulls/9"}},
                {"id":"s-collect","spec_slug":"collect","title":"Collect what is ready to board","status":"completed","metadata":{}},
                {"id":"s-cancelled","spec_slug":"cancelled","title":"Cancelled — nothing to board","status":"ready","metadata":{}}
            ]
        });
        let car = json!({
            "id": "c1", "kind": "ship-a-change", "status": "open",
            "metadata": { "train": "t1", "branch": "fix/x" },
            "steps": [{"id":"c-rev","spec_slug":"review","title":"Open for review","status":"ready","metadata":{}}]
        });

        // Every job write the conductor makes, through either door; a
        // release is a metadata merge on /api/jobs/{car}/metadata.
        let puts: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));

        let train_list = train.clone();
        let train_one = train.clone();
        let car_one = car.clone();
        let (puts_route, merges_route) = (puts.clone(), puts.clone());
        let app = Router::new()
            .route(
                "/api/jobs",
                get(move || {
                    let train = train_list.clone();
                    async move { Json(json!({ "data": [train] })) }
                }),
            )
            .route(
                "/api/jobs/{id}",
                get(move |Path(id): Path<String>| {
                    let (t, c) = (train_one.clone(), car_one.clone());
                    async move { Json(if id == "t1" { t } else { c }) }
                })
                .put(move |Path(id): Path<String>, _b: Json<Value>| {
                    let puts = puts_route.clone();
                    async move {
                        puts.lock().unwrap().push(id);
                        Json(json!({}))
                    }
                }),
            )
            .route(
                "/api/jobs/{id}/metadata",
                axum::routing::patch(move |Path(id): Path<String>, _b: Json<Value>| {
                    let merges = merges_route.clone();
                    async move {
                        merges.lock().unwrap().push(id);
                        axum::http::StatusCode::NO_CONTENT
                    }
                }),
            );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });

        let close_called = Arc::new(Mutex::new(false));
        let mut c = cleanup_conductor(
            "forgejo",
            Box::new(CancelForge {
                close_called: close_called.clone(),
            }),
        );
        c.cfg.jobs = format!("http://{addr}");

        let res = c
            .cancel_train("t1", "forge unreachable", false, false)
            .await;
        assert!(res.is_err(), "cancel must surface the close_pr failure");
        assert!(
            *close_called.lock().unwrap(),
            "close_pr must have been attempted"
        );
        assert!(
            !puts.lock().unwrap().contains(&"c1".to_string()),
            "the car was released despite close_pr failing — a half-cancelled train"
        );
    }

    /// 48f7aba1: a train whose gate could not be filed carried only
    /// `train_gate_launch_failures=5` — a count with no reason — and
    /// the yard drew a healthy train at CI for two hours. This drives
    /// `train_gate` against an in-process jobs server with a launch
    /// that fails before any cluster call (the runner manifest is
    /// unreadable) and reads the train's metadata merge: the count AND
    /// the reason land on the packet together.
    #[tokio::test]
    async fn a_gate_that_cannot_be_filed_records_why_on_the_train() {
        use axum::extract::Path;
        use axum::routing::{get, patch};
        use axum::{Json, Router};
        use std::sync::{Arc, Mutex};

        let train = json!({
            "id": "t1", "kind": "pr-train", "status": "open",
            "metadata": { "boarded_jobs": ["c1"], "train_ref": "train/x@abcdef1" },
            "steps": [
                {"id":"s-assemble","spec_slug":"assemble","title":"Assemble the train branch","status":"completed","metadata":{"train_ref":"train/x@abcdef1"}},
                {"id":"s-pr","spec_slug":"pr","title":"Open the batched PR","status":"completed","metadata":{"pr_url":"https://forge.example/david/boss/pulls/9"}},
                {"id":"s-ci","spec_slug":"ci","title":"CI verdict","status":"ready","metadata":{}}
            ]
        });
        // Every metadata merge the conductor sends for the train.
        let patches: Arc<Mutex<Vec<Value>>> = Arc::new(Mutex::new(Vec::new()));
        let train_one = train.clone();
        let patches_route = patches.clone();
        let app = Router::new()
            .route(
                "/api/jobs/{id}",
                get(move |Path(_id): Path<String>| {
                    let t = train_one.clone();
                    async move { Json(t) }
                }),
            )
            .route(
                "/api/jobs/{id}/metadata",
                patch(move |Path(_id): Path<String>, Json(b): Json<Value>| {
                    let patches = patches_route.clone();
                    async move {
                        patches.lock().unwrap().push(b);
                        axum::http::StatusCode::NO_CONTENT
                    }
                }),
            );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });

        let mut c = cleanup_conductor(
            "forgejo",
            Box::new(FakeForge {
                deleted: Arc::new(Mutex::new(Vec::new())),
                fail_deletes: false,
            }),
        );
        c.cfg.jobs = format!("http://{addr}");
        c.cfg.gate_required = true;

        let mut t = train.clone();
        let (standing, _) = c.train_gate(&mut t, "t1", Utc::now()).await;
        assert_eq!(
            standing, None,
            "no gate-run was filed, so the train has no standing yet"
        );
        let patches = patches.lock().unwrap();
        let recorded = patches
            .iter()
            .find(|b| b.get("train_gate_launch_failures").is_some())
            .expect("the failed launch is recorded on the train");
        assert_eq!(recorded["train_gate_launch_failures"], json!(1));
        let why = recorded[crate::train_gate::KEY_WAIT_REASON]
            .as_str()
            .expect("the reason rides with the count");
        assert!(
            why.contains("gate runner manifest"),
            "the reason is the launch error, verbatim: {why}"
        );
        assert!(
            recorded
                .get("train_gate_fallback")
                .is_none_or(Value::is_null),
            "a REQUIRED gate never falls back to CI alone"
        );
    }

    /// ONE UNREADABLE RUN DOES NOT COST THE PASS (review of car 2ca8c7e9,
    /// 2026-09-25). The reap reads every open gate-run in turn; a bare
    /// `?` on that read made the first row the API would not return —
    /// a packet deleted or closed between the list and the read, a 404,
    /// a refused policy scope — end the whole pass, so every run after
    /// it in the list waited for a pass that would stop at the same row.
    /// Driven against an in-process jobs server: the first row answers
    /// 404, the second is four hours past its deadline, and the second is
    /// still settled.
    ///
    /// THE STUB MODELS THE STEP WRITES THE LIVE API TAKES, NOT ONE
    /// WRITER'S SHAPE (train 07:48, gate 8dd901ac). It served only the
    /// step PUT, so once car 6a647c88 moved `complete_step` onto the
    /// merge door followed by a status-only PUT (backlog e39a9d2a), the
    /// PATCH answered 404, the settle was refused, and the assembled
    /// tree went red on this test with nothing wrong in either car. It
    /// now holds the step's state and serves both doors the way the jobs
    /// API does: the merge door merges keys (a `null` deletes one), and
    /// the PUT overlays its body but refuses 409 a `metadata` that OMITS
    /// a stored key (stage 1, on main). The assertion is on the step's
    /// resulting state, so it holds under either writer.
    #[tokio::test]
    async fn one_unreadable_gate_run_does_not_stop_the_reap_pass() {
        use axum::extract::Path;
        use axum::http::StatusCode;
        use axum::response::IntoResponse;
        use axum::routing::{get, patch, put};
        use axum::{Json, Router};
        use std::sync::{Arc, Mutex};

        let now = Utc::now();
        let step = Arc::new(Mutex::new(json!({
            "id": "s-v", "spec_slug": "record-verdict", "title": "Record the gate verdict",
            "status": "ready", "metadata": {}
        })));
        let dead = json!({
            "id": "dead", "kind": "gate-run", "status": "open",
            "metadata": { "branch": "fix/x",
                          "opened_at": (now - chrono::Duration::hours(4)).to_rfc3339() },
        });
        let listed = json!({ "data": [{"id": "gone"}, {"id": "dead"}] });
        let (step_get, step_merge, step_put) = (step.clone(), step.clone(), step.clone());
        let app = Router::new()
            .route(
                "/api/jobs",
                get(move || {
                    let l = listed.clone();
                    async move { Json(l) }
                }),
            )
            .route(
                "/api/jobs/{id}",
                get(move |Path(id): Path<String>| {
                    let mut d = dead.clone();
                    d["steps"] = json!([step_get.lock().unwrap().clone()]);
                    async move {
                        if id == "dead" {
                            Json(d).into_response()
                        } else {
                            (StatusCode::NOT_FOUND, "job not found").into_response()
                        }
                    }
                }),
            )
            .route(
                "/api/jobs/{id}/steps/{sid}/metadata",
                patch(
                    move |Path((id, sid)): Path<(String, String)>, Json(b): Json<Value>| {
                        let step = step_merge.clone();
                        async move {
                            if id != "dead" || sid != "s-v" {
                                return (StatusCode::NOT_FOUND, "step not found").into_response();
                            }
                            let mut s = step.lock().unwrap();
                            let md = s["metadata"].as_object_mut().unwrap();
                            for (k, v) in b.as_object().cloned().unwrap_or_default() {
                                if v.is_null() {
                                    md.remove(&k);
                                } else {
                                    md.insert(k, v);
                                }
                            }
                            Json(s.clone()).into_response()
                        }
                    },
                ),
            )
            .route(
                "/api/jobs/{id}/steps/{sid}",
                put(
                    move |Path((id, sid)): Path<(String, String)>, Json(b): Json<Value>| {
                        let step = step_put.clone();
                        async move {
                            if id != "dead" || sid != "s-v" {
                                return (StatusCode::NOT_FOUND, "step not found").into_response();
                            }
                            let mut s = step.lock().unwrap();
                            if let Some(md) = b.get("metadata") {
                                let omitted: Vec<String> = s["metadata"]
                                    .as_object()
                                    .map(|m| {
                                        m.keys().filter(|k| md.get(*k).is_none()).cloned().collect()
                                    })
                                    .unwrap_or_default();
                                if !omitted.is_empty() {
                                    return (
                                        StatusCode::CONFLICT,
                                        Json(json!({"error": "metadata omits stored keys",
                                                    "keys": omitted})),
                                    )
                                        .into_response();
                                }
                            }
                            for (k, v) in b.as_object().cloned().unwrap_or_default() {
                                s[k.as_str()] = v;
                            }
                            Json(s.clone()).into_response()
                        }
                    },
                ),
            );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });

        let mut c = cleanup_conductor(
            "forgejo",
            Box::new(FakeForge {
                deleted: Arc::new(Mutex::new(Vec::new())),
                fail_deletes: false,
            }),
        );
        c.cfg.jobs = format!("http://{addr}");

        c.reap_dead_gate_runs(now)
            .await
            .expect("a row that cannot be read is logged, not fatal");
        let settled = step.lock().unwrap().clone();
        assert_eq!(
            (&settled["status"], &settled["metadata"]["verdict"]),
            (&json!("completed"), &json!("lost")),
            "the run after the unreadable one is still settled: {settled}"
        );
    }

    // -- the orphan settle, driven through its cluster port ----------------
    //
    // Review of car dcdc6c64 (backlog b24e29cb item 2): the settle called
    // real kubectl, so its three refusals — a Job exists, the cluster
    // cannot be read, the re-read shows a fresh sign of life — were
    // unpinned, and a fall-through on `Ok(_)` would have passed every
    // test. The cluster read is now a parameter, and each branch is
    // driven below against an in-process jobs API that records every
    // write in the order it arrived.

    /// A gate-run whose last sign of life (its filing) was `idle_min`
    /// minutes before `now`, with its verdict step open.
    fn idle_gate_run(now: DateTime<Utc>, idle_min: i64) -> Value {
        json!({
            "id": "orph", "kind": "gate-run", "status": "open",
            "metadata": { "branch": "fix/orphan",
                          "opened_at": crate::gate::stamp(now - chrono::Duration::minutes(idle_min)) },
            "steps": [{"id": "s-v", "spec_slug": "record-verdict",
                       "title": "Record the gate verdict", "status": "ready", "metadata": {}}]
        })
    }

    /// Serve `reread` on the job GET and record every write, in order, as
    /// (method, path, body). `refuse_step_put` answers the status PUT 409,
    /// an answer the blip guard does not retry.
    async fn settle_api(
        reread: Value,
        refuse_step_put: bool,
    ) -> (
        String,
        std::sync::Arc<std::sync::Mutex<Vec<(String, String, Value)>>>,
    ) {
        settle_api_with_cars(reread, refuse_step_put, Some(Vec::new()), false).await
    }

    /// [`settle_api`], also serving the open-car list the orphan settle
    /// reads before it chooses `withdrawn` over `lost` (backlog
    /// 8d7d0a2b). `None` answers that list 403 — an answer the blip
    /// guard does not retry.
    async fn settle_api_with_cars(
        reread: Value,
        refuse_step_put: bool,
        cars: Option<Vec<Value>>,
        refuse_withdrawn: bool,
    ) -> (
        String,
        std::sync::Arc<std::sync::Mutex<Vec<(String, String, Value)>>>,
    ) {
        use axum::extract::Path;
        use axum::http::StatusCode;
        use axum::response::IntoResponse;
        use axum::routing::{get, patch, put};
        use axum::{Json, Router};
        use std::sync::{Arc, Mutex};

        let writes: Arc<Mutex<Vec<(String, String, Value)>>> = Arc::default();
        let (w1, w2, w3, w4) = (
            writes.clone(),
            writes.clone(),
            writes.clone(),
            writes.clone(),
        );
        // A create answers with the id of the run it serves, so a launch
        // that files a gate-run files THIS one (backlog 7919fdcc).
        let created = reread["id"].clone();
        let app = Router::new()
            .route(
                "/api/jobs",
                get(move || {
                    let cars = cars.clone();
                    async move {
                        match cars {
                            Some(c) => {
                                let total = c.len();
                                Json(json!({ "data": c, "total": total })).into_response()
                            }
                            None => (StatusCode::FORBIDDEN, "denied").into_response(),
                        }
                    }
                })
                .post(move |Json(b): Json<Value>| {
                    let w = w4.clone();
                    let id = created.clone();
                    async move {
                        w.lock()
                            .unwrap()
                            .push(("POST".into(), "/api/jobs".into(), b));
                        (StatusCode::CREATED, Json(json!({"id": id})))
                    }
                }),
            )
            .route(
                "/api/jobs/{id}",
                get(move || {
                    let r = reread.clone();
                    async move { Json(r) }
                }),
            )
            .route(
                "/api/jobs/{id}/metadata",
                patch(move |Path(id): Path<String>, Json(b): Json<Value>| {
                    let w = w1.clone();
                    async move {
                        w.lock().unwrap().push((
                            "PATCH".into(),
                            format!("/api/jobs/{id}/metadata"),
                            b,
                        ));
                        StatusCode::NO_CONTENT
                    }
                }),
            )
            .route(
                "/api/jobs/{id}/steps/{sid}/metadata",
                patch(
                    move |Path((id, sid)): Path<(String, String)>, Json(b): Json<Value>| {
                        let w = w2.clone();
                        async move {
                            w.lock().unwrap().push((
                                "PATCH".into(),
                                format!("/api/jobs/{id}/steps/{sid}/metadata"),
                                b,
                            ));
                            StatusCode::NO_CONTENT
                        }
                    },
                ),
            )
            .route(
                "/api/jobs/{id}/steps/{sid}",
                put(
                    move |Path((id, sid)): Path<(String, String)>, Json(b): Json<Value>| {
                        let w = w3.clone();
                        async move {
                            // An in-flight packet admitted under gate-run
                            // v2 declares no `withdrawn`: the completion
                            // after that metadata write is refused 422.
                            let last_says_withdrawn = w
                                .lock()
                                .unwrap()
                                .last()
                                .is_some_and(|(_, _, b)| b["verdict"] == "withdrawn");
                            if refuse_step_put || (refuse_withdrawn && last_says_withdrawn) {
                                return (StatusCode::CONFLICT, "refused").into_response();
                            }
                            w.lock().unwrap().push((
                                "PUT".into(),
                                format!("/api/jobs/{id}/steps/{sid}"),
                                b,
                            ));
                            StatusCode::NO_CONTENT.into_response()
                        }
                    },
                ),
            );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        (format!("http://{addr}"), writes)
    }

    fn settle_conductor(jobs: String) -> Conductor {
        let mut c = cleanup_conductor(
            "forgejo",
            Box::new(FakeForge {
                deleted: std::sync::Arc::new(std::sync::Mutex::new(Vec::new())),
                fail_deletes: false,
            }),
        );
        c.cfg.jobs = jobs;
        c
    }

    #[tokio::test]
    async fn a_gate_run_a_job_carries_is_not_settled() {
        let now = Utc::now();
        let run = idle_gate_run(now, 40);
        let (jobs, writes) = settle_api(run.clone(), false).await;
        let asked = std::sync::Mutex::new(Vec::new());
        let cluster = |ns: &str, packet: &str| -> Result<Vec<String>> {
            asked.lock().unwrap().push(format!("{ns}/{packet}"));
            Ok(vec!["gate-abc12".to_string()])
        };
        settle_conductor(jobs)
            .settle_orphaned_gate_run("orph", &run, now, &cluster)
            .await
            .expect("a carried run is left alone, not refused");
        assert_eq!(
            *asked.lock().unwrap(),
            vec!["boss-dev/orph".to_string()],
            "the cluster was asked, in the gate namespace, for this packet"
        );
        assert!(
            writes.lock().unwrap().is_empty(),
            "a run a Job carries gets no write at all: {:?}",
            writes.lock().unwrap()
        );
    }

    #[tokio::test]
    async fn a_cluster_that_cannot_be_read_settles_nothing() {
        let now = Utc::now();
        let run = idle_gate_run(now, 40);
        let (jobs, writes) = settle_api(run.clone(), false).await;
        let cluster =
            |_: &str, _: &str| -> Result<Vec<String>> { Err(anyhow!("kubectl: forbidden")) };
        settle_conductor(jobs)
            .settle_orphaned_gate_run("orph", &run, now, &cluster)
            .await
            .expect("an unreadable cluster is logged and left to the clock");
        assert!(
            writes.lock().unwrap().is_empty(),
            "an absence nobody observed settles nothing: {:?}",
            writes.lock().unwrap()
        );
    }

    #[tokio::test]
    async fn a_run_that_came_alive_during_the_read_is_not_settled() {
        let now = Utc::now();
        let run = idle_gate_run(now, 40);
        // Between the pass's read and the cluster read, a re-gate stamped
        // the packet alive at launch.
        let mut reread = run.clone();
        reread["metadata"][crate::gate::LAUNCHING_AT] =
            json!(crate::gate::stamp(now - chrono::Duration::minutes(1)));
        let (jobs, writes) = settle_api(reread, false).await;
        let cluster = |_: &str, _: &str| -> Result<Vec<String>> { Ok(vec![]) };
        settle_conductor(jobs)
            .settle_orphaned_gate_run("orph", &run, now, &cluster)
            .await
            .expect("a fresh stamp on the re-read is left alone");
        assert!(
            writes.lock().unwrap().is_empty(),
            "the re-read's fresh stamp wins over the pass's stale read: {:?}",
            writes.lock().unwrap()
        );
    }

    /// Item 3 of b24e29cb. The evidence is dated at the cluster read it
    /// reports, not at the start of the pass; and it is written only AFTER
    /// the verdict step completed, so no open packet ever carries an
    /// `orphaned_gate_run {runner_jobs: 0}` for a run that was not settled.
    #[tokio::test]
    async fn an_orphan_is_settled_lost_and_its_evidence_follows_the_step() {
        let now = Utc::now() - chrono::Duration::minutes(3);
        let run = idle_gate_run(now, 40);
        let (jobs, writes) = settle_api(run.clone(), false).await;
        let before = Utc::now();
        let cluster = |_: &str, _: &str| -> Result<Vec<String>> { Ok(vec![]) };
        settle_conductor(jobs)
            .settle_orphaned_gate_run("orph", &run, now, &cluster)
            .await
            .expect("an orphan is settled");
        let after = Utc::now();
        let w = writes.lock().unwrap().clone();
        let order: Vec<(String, String)> =
            w.iter().map(|(m, p, _)| (m.clone(), p.clone())).collect();
        assert_eq!(
            order,
            vec![
                ("PATCH".into(), "/api/jobs/orph/steps/s-v/metadata".into()),
                ("PUT".into(), "/api/jobs/orph/steps/s-v".into()),
                ("PATCH".into(), "/api/jobs/orph/metadata".into()),
            ],
            "the verdict first, the evidence after it"
        );
        assert_eq!(w[0].2["verdict"], "lost");
        let ev = &w[2].2["orphaned_gate_run"];
        assert_eq!(ev["runner_jobs"], 0, "{ev}");
        let observed = ev["observed_at"]
            .as_str()
            .and_then(|s| DateTime::parse_from_rfc3339(s).ok())
            .map(|t| t.with_timezone(&Utc))
            .expect("observed_at is an instant");
        assert!(
            observed >= before - chrono::Duration::seconds(1) && observed <= after,
            "observed_at {observed} is the cluster read, not the pass start {now}"
        );
    }

    // -- verdicts recorded from the runner's pod log (934ccad1) ------------

    /// The cluster as the recorder's port sees it: the Jobs it lists, one
    /// pod and one log for all of them, and the reads it was asked for
    /// (shared, so a test still holds them once the conductor owns the
    /// fake).
    #[derive(Clone)]
    struct FakeCluster {
        jobs: Result<Vec<GateJob>, String>,
        pod: Result<GatePod, String>,
        log: Result<String, String>,
        pod_reads: std::sync::Arc<std::sync::Mutex<Vec<String>>>,
        log_reads: std::sync::Arc<std::sync::Mutex<Vec<String>>>,
    }

    impl FakeCluster {
        /// The pod ENDED as an honest runner of `log` ends: its
        /// termination message is the trailer the log holds.
        fn new(jobs: Vec<GateJob>, log: &str) -> Self {
            Self {
                jobs: Ok(jobs),
                pod: Ok(carried_verdict::fixtures::ended(log)),
                log: Ok(log.to_string()),
                pod_reads: Default::default(),
                log_reads: Default::default(),
            }
        }
    }

    #[async_trait]
    impl GateCluster for FakeCluster {
        async fn gate_jobs(&self, _: &str) -> Result<Vec<GateJob>> {
            self.jobs.clone().map_err(|e| anyhow!(e))
        }
        async fn gate_pod(&self, ns: &str, job: &str) -> Result<GatePod> {
            self.pod_reads.lock().unwrap().push(format!("{ns}/{job}"));
            self.pod.clone().map_err(|e| anyhow!(e))
        }
        async fn gate_log(&self, ns: &str, job: &str) -> Result<String> {
            self.log_reads.lock().unwrap().push(format!("{ns}/{job}"));
            self.log.clone().map_err(|e| anyhow!(e))
        }
    }

    /// A conductor against `jobs` whose cluster is `cluster`.
    fn carried_conductor(jobs: String, cluster: &FakeCluster) -> Conductor {
        let mut c = settle_conductor(jobs);
        c.cluster = Box::new(cluster.clone());
        c
    }

    const CARRIED_HEAD: &str = "26b7e3081a52923879dd07cda85c2f3f74e7abdf";

    fn carrier_job(packet: &str, carrier: bool, failed: Option<bool>) -> GateJob {
        GateJob {
            name: "gate-fix-x-abc12".into(),
            uid: "uid-1".into(),
            packet: packet.into(),
            created: "2026-10-06T19:00:00Z".into(),
            carrier,
            finished: failed.map(|failed| Finished {
                failed,
                reason: "BackoffLimitExceeded".into(),
                at: "2026-10-06T19:40:00Z".into(),
            }),
        }
    }

    /// A gate-run that owes its verdict, as the jobs API returns it.
    fn owing_gate_run() -> Value {
        json!({
            "id": "orph", "kind": "gate-run", "status": "open",
            "metadata": { "branch": "fix/x", "sha": CARRIED_HEAD },
            "steps": [{"id": "s-v", "spec_slug": "record-verdict",
                       "title": "Record the gate verdict", "status": "ready", "metadata": {}}]
        })
    }

    /// run.sh's two lines, for `payload` printed by Job `gate-fix-x-abc12`.
    fn carried_log(verdict: &str, payload: &str) -> String {
        use sha2::{Digest, Sha256};
        format!(
            "gate-runner: receipt {payload}\ngate-runner: receipt-end v1 job=gate-fix-x-abc12 \
             verdict={verdict} bytes={} sha256={}\n",
            payload.len(),
            hex::encode(Sha256::digest(payload.as_bytes()))
        )
    }

    /// The new-layout path end to end: the runner wrote nothing, its gate
    /// container has ended on a termination message naming its receipt
    /// (the Job's own condition may lag the container by a moment), and
    /// the conductor records what the log carries — on the packet the JOB
    /// names, whatever the receipt says about itself.
    #[tokio::test]
    async fn a_verdict_left_in_the_pod_log_is_recorded_on_the_jobs_packet() {
        let (jobs, writes) = settle_api(owing_gate_run(), false).await;
        let payload = json!({"verdict": "failed", "head": CARRIED_HEAD, "packet": "another",
                             "fails": ["test: a_thing - FAILED"]})
        .to_string();
        let cluster = FakeCluster::new(
            vec![carrier_job("orph", true, None)],
            &carried_log("failed", &payload),
        );
        carried_conductor(jobs, &cluster)
            .record_carried_verdicts()
            .await
            .expect("recorded");
        let w = writes.lock().unwrap().clone();
        let order: Vec<(String, String)> =
            w.iter().map(|(m, p, _)| (m.clone(), p.clone())).collect();
        assert_eq!(
            order,
            vec![
                ("PATCH".into(), "/api/jobs/orph/steps/s-v/metadata".into()),
                ("PUT".into(), "/api/jobs/orph/steps/s-v".into()),
            ],
            "the verdict through the merge door, then the status alone — as run.sh wrote it"
        );
        assert_eq!(w[0].2["verdict"], "failed");
        assert_eq!(w[1].2, json!({"status": "completed"}));
        let receipt: Value = serde_json::from_str(w[0].2["receipt"].as_str().unwrap()).unwrap();
        assert_eq!(receipt["fails"], json!(["test: a_thing - FAILED"]));
        assert_eq!(receipt["recorded_by"]["job"], "gate-fix-x-abc12");
        assert_eq!(receipt["recorded_by"]["head_matches_launch"], true);
        assert_eq!(
            *cluster.log_reads.lock().unwrap(),
            vec!["boss-dev/gate-fix-x-abc12".to_string()]
        );
    }

    /// Rollout: a Job without the carrier label is an old-layout runner
    /// that reports for itself. Its packet is not even read.
    #[tokio::test]
    async fn an_old_layout_gate_is_left_to_its_own_runner() {
        let (jobs, writes) = settle_api(owing_gate_run(), false).await;
        let payload = json!({"verdict": "green", "head": CARRIED_HEAD}).to_string();
        let cluster = FakeCluster::new(
            vec![carrier_job("orph", false, Some(false))],
            &carried_log("green", &payload),
        );
        carried_conductor(jobs, &cluster)
            .record_carried_verdicts()
            .await
            .expect("nothing to do is not an error");
        assert!(writes.lock().unwrap().is_empty());
        assert!(cluster.log_reads.lock().unwrap().is_empty());
    }

    /// A receipt arriving twice, or after the packet was settled another
    /// way (an old-layout script's own report, the observer, a
    /// withdrawal): the step is done, so nothing is read and nothing
    /// written — never a second verdict over the first.
    #[tokio::test]
    async fn a_packet_that_already_carries_a_verdict_is_never_written_again() {
        let payload = json!({"verdict": "green", "head": CARRIED_HEAD}).to_string();
        for (status, verdict) in [
            ("open", "green"),
            ("closed", "withdrawn"),
            ("closed", "lost"),
        ] {
            let mut run = owing_gate_run();
            run["status"] = json!(status);
            run["steps"][0]["status"] = json!("completed");
            run["steps"][0]["metadata"] = json!({"verdict": verdict});
            let (jobs, writes) = settle_api(run, false).await;
            let cluster = FakeCluster::new(
                vec![carrier_job("orph", true, Some(false))],
                &carried_log("green", &payload),
            );
            carried_conductor(jobs, &cluster)
                .record_carried_verdicts()
                .await
                .expect("left alone");
            assert!(
                writes.lock().unwrap().is_empty(),
                "a {status} packet carrying `{verdict}` was written again"
            );
            assert!(cluster.log_reads.lock().unwrap().is_empty());
        }
    }

    /// AN INFRASTRUCTURE REFUSAL IS NOT A CONSIST FAILURE. The Job ended
    /// and its log holds no complete receipt (an eviction, a node reset,
    /// a script that died): `lost` at once, naming the Job — never
    /// `failed`, and never three silent hours.
    #[tokio::test]
    async fn a_finished_job_with_no_receipt_is_settled_lost_and_named() {
        let (jobs, writes) = settle_api(owing_gate_run(), false).await;
        let cluster = FakeCluster::new(
            vec![carrier_job("orph", true, Some(true))],
            "gate-runner: swept 0 dead gate workspace(s) from /gate-runs\n",
        );
        carried_conductor(jobs, &cluster)
            .record_carried_verdicts()
            .await
            .expect("settled");
        let w = writes.lock().unwrap().clone();
        assert_eq!(w[0].2["verdict"], "lost", "{w:?}");
        let receipt = w[0].2["receipt"].as_str().unwrap();
        assert!(
            receipt.contains("gate-fix-x-abc12") && receipt.contains("BackoffLimitExceeded"),
            "{receipt}"
        );
        assert_eq!(w[1].0, "PUT");
    }

    /// A running gate — WHATEVER ITS LOG HOLDS — an unreadable pod, an
    /// unreadable log, and a refused write each cost one pass and nothing
    /// else.
    #[tokio::test]
    async fn nothing_is_settled_on_what_could_not_be_read_and_no_refusal_is_fatal() {
        let (jobs, writes) = settle_api(owing_gate_run(), false).await;
        // A whole green frame in the log of a pod whose gate container
        // is still running: not a verdict, and the log is not even read.
        let green = json!({"verdict": "green", "head": CARRIED_HEAD}).to_string();
        let mut running = FakeCluster::new(
            vec![carrier_job("orph", true, None)],
            &carried_log("green", &green),
        );
        running.pod = Ok(carried_verdict::fixtures::running());
        carried_conductor(jobs.clone(), &running)
            .record_carried_verdicts()
            .await
            .expect("waits");
        assert_eq!(running.pod_reads.lock().unwrap().len(), 1);
        assert!(
            running.log_reads.lock().unwrap().is_empty(),
            "a running gate's log was read"
        );
        // "Could not see the pod" is not "the pod is gone".
        let mut dark_pod = FakeCluster::new(
            vec![carrier_job("orph", true, Some(true))],
            &carried_log("green", &green),
        );
        dark_pod.pod = Err("kubectl get pods did not answer within 30s and was killed".into());
        carried_conductor(jobs.clone(), &dark_pod)
            .record_carried_verdicts()
            .await
            .expect("an unreadable pod is logged, not fatal, and settles nothing");
        assert!(dark_pod.log_reads.lock().unwrap().is_empty());
        let mut dark_log = FakeCluster::new(
            vec![carrier_job("orph", true, Some(true))],
            &carried_log("green", &green),
        );
        dark_log.log = Err("pods \"x\" not found".into());
        carried_conductor(jobs.clone(), &dark_log)
            .record_carried_verdicts()
            .await
            .expect("an unreadable log is logged, not fatal, and settles nothing");
        assert!(
            writes.lock().unwrap().is_empty(),
            "{:?}",
            writes.lock().unwrap()
        );
        let mut dark = FakeCluster::new(vec![], "");
        dark.jobs = Err("kubectl: forbidden".into());
        assert!(
            carried_conductor(jobs, &dark)
                .record_carried_verdicts()
                .await
                .is_err(),
            "an unreadable cluster is the caller's line to log, never an empty list"
        );
        // The jobs API refuses the flip (the step moved under the pass):
        // logged, the pass goes on.
        let (jobs, _) = settle_api(owing_gate_run(), true).await;
        let payload = json!({"verdict": "green", "head": CARRIED_HEAD}).to_string();
        let cluster = FakeCluster::new(
            vec![carrier_job("orph", true, Some(false))],
            &carried_log("green", &payload),
        );
        carried_conductor(jobs, &cluster)
            .record_carried_verdicts()
            .await
            .expect("a refused write is not fatal to the pass");
    }

    /// THE FORGED FRAME, END TO END (review 7e5356f7's first probe: "a
    /// forged green before the real failed frame records green"). The
    /// log holds a green frame first and the runner's own `failed` after
    /// it; the gate container ended on the runner's trailer. The packet
    /// gets `failed`, with the runner's receipt.
    #[tokio::test]
    async fn a_green_frame_printed_before_the_runners_own_does_not_decide_the_verdict() {
        let (jobs, writes) = settle_api(owing_gate_run(), false).await;
        let forged = json!({"verdict": "green", "head": CARRIED_HEAD}).to_string();
        let real = json!({"verdict": "failed", "head": CARRIED_HEAD,
                          "fails": ["test: a_thing - FAILED"]})
        .to_string();
        let log = format!(
            "{}{}",
            carried_log("green", &forged),
            carried_log("failed", &real)
        );
        let mut cluster = FakeCluster::new(vec![carrier_job("orph", true, Some(true))], &log);
        cluster.pod = Ok(carried_verdict::fixtures::ended_on(
            carried_log("failed", &real).lines().nth(1).unwrap(),
            1,
            "Error",
        ));
        carried_conductor(jobs, &cluster)
            .record_carried_verdicts()
            .await
            .expect("recorded");
        let w = writes.lock().unwrap().clone();
        assert_eq!(w[0].2["verdict"], "failed", "{w:?}");
        let receipt: Value = serde_json::from_str(w[0].2["receipt"].as_str().unwrap()).unwrap();
        assert_eq!(receipt["fails"], json!(["test: a_thing - FAILED"]));
        assert_eq!(receipt["recorded_by"]["exit_code"], 1);
    }

    /// NO EVIDENCE IS NOT A PASS, END TO END: the gate container was
    /// killed (no termination message) with a whole green frame in its
    /// log. `lost`, which strikes no car — never green, never `failed`.
    #[tokio::test]
    async fn a_killed_gate_with_a_green_frame_in_its_log_is_lost_not_green() {
        let (jobs, writes) = settle_api(owing_gate_run(), false).await;
        let green = json!({"verdict": "green", "head": CARRIED_HEAD}).to_string();
        let mut cluster = FakeCluster::new(
            vec![carrier_job("orph", true, Some(true))],
            &carried_log("green", &green),
        );
        cluster.pod = Ok(carried_verdict::fixtures::ended_on("", 137, "OOMKilled"));
        carried_conductor(jobs, &cluster)
            .record_carried_verdicts()
            .await
            .expect("settled");
        let w = writes.lock().unwrap().clone();
        assert_eq!(w[0].2["verdict"], "lost", "{w:?}");
        assert!(
            w[0].2["receipt"].as_str().unwrap().contains("OOMKilled"),
            "{w:?}"
        );
        assert!(
            cluster.log_reads.lock().unwrap().is_empty(),
            "no message names a receipt, so the log is not read"
        );
        // And what `lost` means to the reader that strikes cars.
        let mut run = owing_gate_run();
        run["steps"][0]["status"] = json!("completed");
        run["steps"][0]["metadata"] = w[0].2.clone();
        assert_eq!(
            crate::train_gate::standing(&run),
            crate::train_gate::Standing::Lost
        );
    }

    /// EXACTLY ONCE, ACROSS PASSES AND ACROSS A CONDUCTOR RESTART. Every
    /// reconcile is its own process, so "already recorded" lives only on
    /// the packet: a second pass over the same finished Job — a new
    /// conductor, the same cluster — reads the step done and writes
    /// nothing, and the verdict the first pass left is the one that stands.
    #[tokio::test]
    async fn a_second_pass_over_a_recorded_gate_writes_nothing() {
        let (jobs, held) = stateful_runs_api(vec![owing_gate_run()]).await;
        let payload = json!({"verdict": "failed", "head": CARRIED_HEAD}).to_string();
        let cluster = FakeCluster::new(
            vec![carrier_job("orph", true, Some(true))],
            &carried_log("failed", &payload),
        );
        carried_conductor(jobs.clone(), &cluster)
            .record_carried_verdicts()
            .await
            .expect("recorded");
        let first = held.lock().unwrap()["orph"].clone();
        assert_eq!(first["steps"][0]["metadata"]["verdict"], "failed");
        // The second conductor meets a cluster that now tells another
        // story — the pass must not even ask it.
        let other = json!({"verdict": "green", "head": CARRIED_HEAD}).to_string();
        let later = FakeCluster::new(
            vec![carrier_job("orph", true, Some(true))],
            &carried_log("green", &other),
        );
        carried_conductor(jobs, &later)
            .record_carried_verdicts()
            .await
            .expect("left alone");
        assert_eq!(
            held.lock().unwrap()["orph"],
            first,
            "the second pass changed a recorded verdict"
        );
        assert!(later.pod_reads.lock().unwrap().is_empty());
        assert!(later.log_reads.lock().unwrap().is_empty());
    }

    /// r11 of review 7e5356f7: a packet no carrier Job is on is not READ
    /// — not its packet, not its pod, not its log. The old-layout test
    /// above could not see the packet read; this one counts it.
    #[tokio::test]
    async fn a_packet_with_no_carrier_job_is_never_read() {
        use axum::routing::get;
        let gets = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let g = gets.clone();
        let app = axum::Router::new().fallback(get(move || {
            let g = g.clone();
            async move {
                g.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                axum::Json(owing_gate_run())
            }
        }));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let payload = json!({"verdict": "green", "head": CARRIED_HEAD}).to_string();
        let cluster = FakeCluster::new(
            vec![carrier_job("orph", false, Some(false))],
            &carried_log("green", &payload),
        );
        carried_conductor(format!("http://{addr}"), &cluster)
            .record_carried_verdicts()
            .await
            .expect("nothing to do");
        assert_eq!(
            gets.load(std::sync::atomic::Ordering::SeqCst),
            0,
            "the packet of an old-layout gate was read"
        );
        assert!(cluster.pod_reads.lock().unwrap().is_empty());
    }

    /// A jobs API holding `runs` by id and KEEPING what is written: the
    /// step merge door merges into the first step's metadata, the step
    /// PUT overlays it, the list answers every run it holds, and an id
    /// it does not hold is 404 — so a pass can be read back, and a second
    /// writer in the same pass meets what the first one left.
    async fn stateful_runs_api(
        runs: Vec<Value>,
    ) -> (
        String,
        std::sync::Arc<std::sync::Mutex<std::collections::BTreeMap<String, Value>>>,
    ) {
        use axum::extract::Path;
        use axum::http::StatusCode;
        use axum::response::IntoResponse;
        use axum::routing::{get, patch, put};
        use axum::{Json, Router};
        use std::sync::{Arc, Mutex};

        let held: Arc<Mutex<std::collections::BTreeMap<String, Value>>> = Arc::new(Mutex::new(
            runs.into_iter()
                .map(|r| (r["id"].as_str().unwrap().to_string(), r))
                .collect(),
        ));
        let (h1, h2, h3, h4) = (held.clone(), held.clone(), held.clone(), held.clone());
        let h5 = held.clone();
        let app = Router::new()
            .route(
                "/api/jobs",
                get(move || {
                    let h = h1.clone();
                    async move {
                        let rows: Vec<Value> = h.lock().unwrap().values().cloned().collect();
                        let total = rows.len();
                        Json(json!({ "data": rows, "total": total }))
                    }
                }),
            )
            .route(
                "/api/jobs/{id}",
                get(move |Path(id): Path<String>| {
                    let h = h2.clone();
                    async move {
                        match h.lock().unwrap().get(&id) {
                            Some(r) => Json(r.clone()).into_response(),
                            None => (StatusCode::NOT_FOUND, "job not found").into_response(),
                        }
                    }
                }),
            )
            .route(
                "/api/jobs/{id}/metadata",
                // The merge door: a key is set, a null key is deleted.
                patch(move |Path(id): Path<String>, Json(b): Json<Value>| {
                    let h = h5.clone();
                    async move {
                        let mut held = h.lock().unwrap();
                        let Some(run) = held.get_mut(&id) else {
                            return (StatusCode::NOT_FOUND, "job not found").into_response();
                        };
                        let md = run["metadata"].as_object_mut().unwrap();
                        for (k, v) in b.as_object().cloned().unwrap_or_default() {
                            if v.is_null() {
                                md.remove(&k);
                            } else {
                                md.insert(k, v);
                            }
                        }
                        StatusCode::NO_CONTENT.into_response()
                    }
                }),
            )
            .route(
                "/api/jobs/{id}/steps/{sid}/metadata",
                patch(
                    move |Path((id, _sid)): Path<(String, String)>, Json(b): Json<Value>| {
                        let h = h3.clone();
                        async move {
                            let mut held = h.lock().unwrap();
                            let Some(run) = held.get_mut(&id) else {
                                return (StatusCode::NOT_FOUND, "job not found").into_response();
                            };
                            let step = &mut run["steps"][0];
                            if step["status"] == "completed" {
                                return (StatusCode::CONFLICT, "step is completed").into_response();
                            }
                            for (k, v) in b.as_object().cloned().unwrap_or_default() {
                                step["metadata"][k.as_str()] = v;
                            }
                            StatusCode::NO_CONTENT.into_response()
                        }
                    },
                ),
            )
            .route(
                "/api/jobs/{id}/steps/{sid}",
                put(
                    move |Path((id, _sid)): Path<(String, String)>, Json(b): Json<Value>| {
                        let h = h4.clone();
                        async move {
                            let mut held = h.lock().unwrap();
                            let Some(run) = held.get_mut(&id) else {
                                return (StatusCode::NOT_FOUND, "job not found").into_response();
                            };
                            for (k, v) in b.as_object().cloned().unwrap_or_default() {
                                run["steps"][0][k.as_str()] = v;
                            }
                            StatusCode::NO_CONTENT.into_response()
                        }
                    },
                ),
            );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        (format!("http://{addr}"), held)
    }

    /// The verdict a held run's `record-verdict` step ended with.
    fn held_verdict(
        held: &std::sync::Mutex<std::collections::BTreeMap<String, Value>>,
        id: &str,
    ) -> (Value, Value) {
        let run = held.lock().unwrap()[id].clone();
        (
            run["steps"][0]["status"].clone(),
            run["steps"][0]["metadata"]["verdict"].clone(),
        )
    }

    /// m16 of review e0ecb2f6. One packet the jobs API will not return —
    /// closed and reaped, a refused scope — must not cost every packet
    /// after it its verdict: the walk is per packet.
    #[tokio::test]
    async fn one_unreadable_packet_does_not_stop_the_carried_pass() {
        let mut run = owing_gate_run();
        run["id"] = json!("b-run");
        let (jobs, held) = stateful_runs_api(vec![run]).await;
        let payload = json!({"verdict": "green", "head": CARRIED_HEAD}).to_string();
        let cluster = FakeCluster::new(
            vec![
                carrier_job("a-gone", true, Some(false)),
                carrier_job("b-run", true, Some(false)),
            ],
            &carried_log("green", &payload),
        );
        carried_conductor(jobs, &cluster)
            .record_carried_verdicts()
            .await
            .expect("an unreadable packet is said, not fatal");
        assert_eq!(
            held_verdict(&held, "b-run"),
            (json!("completed"), json!("green")),
            "the packet after the unreadable one was not recorded"
        );
    }

    /// A gate-run old enough for the three-hour clock to call it dead.
    fn stale_owing_run(now: DateTime<Utc>) -> Value {
        let mut run = owing_gate_run();
        run["metadata"]["opened_at"] = json!(crate::gate::stamp(now - chrono::Duration::hours(4)));
        run
    }

    /// m22 of review e0ecb2f6: the lines reconcile runs. The carried pass
    /// goes FIRST — a run the clock would call lost, with a receipt
    /// waiting in its pod log, ends with the receipt's verdict.
    #[tokio::test]
    async fn a_receipt_waiting_in_a_pod_log_is_recorded_before_the_clock_calls_the_run_lost() {
        let now = Utc::now();
        let (jobs, held) = stateful_runs_api(vec![stale_owing_run(now)]).await;
        let payload = json!({"verdict": "green", "head": CARRIED_HEAD}).to_string();
        let cluster = FakeCluster::new(
            vec![carrier_job("orph", true, Some(false))],
            &carried_log("green", &payload),
        );
        carried_conductor(jobs, &cluster)
            .settle_gate_runs(now)
            .await;
        assert_eq!(
            held_verdict(&held, "orph"),
            (json!("completed"), json!("green")),
            "the reap ran before the carried pass, or the pass did not run"
        );
        assert_eq!(cluster.log_reads.lock().unwrap().len(), 1);
    }

    /// m22, the other half: a cluster that cannot be read costs the
    /// carried pass and NOTHING ELSE. The reap behind it still settles
    /// its dead run, and the outage is said on the latch, not thrown.
    #[tokio::test]
    async fn a_dark_cluster_costs_the_carried_pass_and_never_the_reap_behind_it() {
        let now = Utc::now();
        let (jobs, held) = stateful_runs_api(vec![stale_owing_run(now)]).await;
        let mut dark = FakeCluster::new(vec![], "");
        dark.jobs = Err("kubectl get jobs did not answer within 30s and was killed".into());
        let mut c = carried_conductor(jobs, &dark);
        // Its own home: the latch is a file, and the other tests of this
        // pass share the default one.
        c.cfg.home = boss_testing::scratch_dir("carried-verdict-dark-cluster")
            .display()
            .to_string();
        let latch = Path::new(&c.cfg.home).join("gate-jobs-read.failing");
        let _ = fs::remove_file(&latch);
        c.settle_gate_runs(now).await;
        assert_eq!(
            held_verdict(&held, "orph"),
            (json!("completed"), json!("lost")),
            "the carried pass's error stopped the reap"
        );
        let said = fs::read_to_string(&latch).expect("the outage is latched in the home");
        assert!(said.contains("did not answer within 30s"), "{said}");
        let _ = fs::remove_file(&latch);
    }

    /// Backlog 06ae925a: the outage reaches a READER. A cluster that
    /// cannot be read is written on the gate-run it keeps waiting, by the
    /// lines reconcile runs, BEFORE the reap — so even the run the clock
    /// then calls lost says why nobody recorded it.
    #[tokio::test]
    async fn a_dark_cluster_is_said_on_the_gate_run_it_keeps_waiting() {
        let now = Utc::now();
        let (jobs, held) = stateful_runs_api(vec![stale_owing_run(now)]).await;
        let mut dark = FakeCluster::new(vec![], "");
        dark.jobs = Err("kubectl get jobs failed: E1007 08:56:24.1 69 refused".into());
        let mut c = carried_conductor(jobs, &dark);
        c.cfg.home = boss_testing::scratch_dir("carried-verdict-dark-note")
            .display()
            .to_string();
        let latch = Path::new(&c.cfg.home).join("gate-jobs-read.failing");
        let _ = fs::remove_file(&latch);
        c.settle_gate_runs(now).await;
        let note = held.lock().unwrap()["orph"]["metadata"][RECORDER_BLOCKED_KEY].clone();
        assert_eq!(
            note["cause"], "kubectl get jobs failed: E# #:#:#.# # refused",
            "{note}"
        );
        assert!(
            DateTime::parse_from_rfc3339(note["since"].as_str().unwrap()).is_ok(),
            "{note}"
        );
        assert!(
            recorder_blocked_line(&held.lock().unwrap()["orph"])
                .is_some_and(|l| l.starts_with("verdict waiting: the recorder cannot read")),
            "a reader of the packet has no line for it"
        );
        let _ = fs::remove_file(&latch);
    }

    /// The same note, pass after pass: written ONCE per outage and cause
    /// (each write is an event), never on a run just launched, and
    /// removed from an open run on the pass the read works again.
    #[tokio::test]
    async fn the_waiting_note_is_written_once_and_removed_when_the_read_works() {
        let now = Utc::now();
        let aged = |id: &str, minutes: i64| {
            let mut run = owing_gate_run();
            run["id"] = json!(id);
            run["metadata"]["opened_at"] =
                json!(crate::gate::stamp(now - chrono::Duration::minutes(minutes)));
            run
        };
        let (jobs, held) = stateful_runs_api(vec![aged("quiet", 40), aged("fresh", 3)]).await;
        let c = carried_conductor(jobs, &FakeCluster::new(vec![], ""));
        let noted = |id: &str| held.lock().unwrap()[id]["metadata"][RECORDER_BLOCKED_KEY].clone();
        let outage = ("2026-10-07T08:56:24Z".to_string(), "refused".to_string());

        c.note_recorder_blocked(Some(&outage), now).await.unwrap();
        assert_eq!(noted("quiet")["since"], "2026-10-07T08:56:24Z");
        assert_eq!(
            noted("fresh"),
            Value::Null,
            "a run three minutes old was told"
        );

        // A later pass of the same outage: the note is the one first written.
        let first = noted("quiet");
        let later = now + chrono::Duration::minutes(2);
        c.note_recorder_blocked(Some(&outage), later).await.unwrap();
        assert_eq!(noted("quiet"), first, "the same outage was written twice");

        // A changed cause is a new fact for the reader.
        let forbidden = (outage.0.clone(), "Forbidden".to_string());
        c.note_recorder_blocked(Some(&forbidden), later)
            .await
            .unwrap();
        assert_eq!(noted("quiet")["cause"], "Forbidden");

        // The read works: gone, and the other keys untouched.
        c.note_recorder_blocked(None, later).await.unwrap();
        let run = held.lock().unwrap()["quiet"].clone();
        assert!(run["metadata"].get(RECORDER_BLOCKED_KEY).is_none(), "{run}");
        assert_eq!(run["metadata"]["branch"], "fix/x");
    }

    /// `reconcile` has no test of its own (it opens with a `git clone`),
    /// so the one line that reaches the gate-run half is pinned in the
    /// source: called once, before the open trains are read, and neither
    /// half called from `reconcile` any other way.
    #[test]
    fn reconcile_settles_gate_runs_once_before_it_reads_the_trains() {
        let src = include_str!("conductor.rs");
        let body = &src[src
            .find("pub(super) async fn reconcile(&self, now: DateTime<Utc>) -> Result<()> {")
            .expect("reconcile")..];
        let body = &body[..body.find("\n    }\n").expect("the end of reconcile")];
        let call = "self.settle_gate_runs(now).await;";
        assert_eq!(
            body.matches(call).count(),
            1,
            "reconcile calls the gate-run half once"
        );
        assert!(
            body.find(call).unwrap()
                < body
                    .find("kind=pr-train&status=open")
                    .expect("the trains read"),
            "the gate-run half runs before the trains are read"
        );
        for direct in ["self.record_carried_verdicts(", "self.reap_dead_gate_runs("] {
            assert!(
                !body.contains(direct),
                "reconcile calls `{direct}` beside settle_gate_runs — the tested lines are no longer the ones it runs"
            );
        }
    }

    /// An orphan queued for a head a car already vouches for.
    fn vouched_orphan(now: DateTime<Utc>) -> (Value, Value) {
        const HEAD: &str = "1c82e857aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
        let mut run = idle_gate_run(now, 40);
        run["metadata"]["sha"] = json!(HEAD);
        run["metadata"]["queued_at"] =
            json!(crate::gate::stamp(now - chrono::Duration::minutes(40)));
        let receipt =
            format!("{{\"verdict\": \"green\", \"head\": \"{HEAD}\", \"mode\": \"auto\"}}");
        let car = json!({
            "id": "59e1f436-0000-4000-8000-000000000000", "kind": "ship-a-change",
            "status": "open",
            "metadata": { "branch": "fix/orphan" },
            "steps": [
                {"spec_slug": "gate", "title": boss_jobs::car::GATE, "status": "completed",
                 "metadata": {"receipt": receipt}},
                {"spec_slug": "review", "title": boss_jobs::car::REVIEW, "status": "ready"},
            ]
        });
        (run, car)
    }

    /// Backlog b1c82a82's acceptance. Gate-run 8c2f644a (2026-09-28) was
    /// queued for a head car 59e1f436 already carried green, its waiter
    /// died, and this settle closed it `lost` — a dead runner on the
    /// record for a run nobody needed. When the head is already vouched
    /// for, the settle WITHDRAWS instead, naming the car it defers to;
    /// no human decision is involved.
    #[tokio::test]
    async fn an_orphan_whose_head_a_car_already_carries_is_withdrawn_not_lost() {
        let now = Utc::now();
        let (run, car) = vouched_orphan(now);
        let (jobs, writes) = settle_api_with_cars(run.clone(), false, Some(vec![car]), false).await;
        let cluster = |_: &str, _: &str| -> Result<Vec<String>> { Ok(vec![]) };
        settle_conductor(jobs)
            .settle_orphaned_gate_run("orph", &run, now, &cluster)
            .await
            .expect("a vouched orphan is withdrawn");
        let w = writes.lock().unwrap().clone();
        assert_eq!(w[0].1, "/api/jobs/orph/steps/s-v/metadata", "{w:?}");
        assert_eq!(w[0].2["verdict"], "withdrawn", "{w:?}");
        let receipt: Value =
            serde_json::from_str(w[0].2["receipt"].as_str().expect("a JSON string")).unwrap();
        assert_eq!(receipt["verdict"], "withdrawn");
        assert_eq!(
            receipt["defers_to"]["car"],
            "59e1f436-0000-4000-8000-000000000000"
        );
        assert!(
            receipt["withdrawn_because"]
                .as_str()
                .is_some_and(|s| s.contains("car 59e1f436")),
            "{receipt}"
        );
        assert_eq!(
            w[1].1, "/api/jobs/orph/steps/s-v",
            "the status flip follows"
        );
    }

    /// With no car carrying the head the settle is unchanged — lost —
    /// and when the car list cannot be read it settles NOTHING this pass:
    /// "could not tell" is neither withdrawn nor lost.
    #[tokio::test]
    async fn an_unvouched_orphan_is_still_lost_and_an_unread_car_list_settles_nothing() {
        let now = Utc::now();
        let (run, _) = vouched_orphan(now);
        let cluster = |_: &str, _: &str| -> Result<Vec<String>> { Ok(vec![]) };
        let (jobs, writes) = settle_api_with_cars(run.clone(), false, Some(vec![]), false).await;
        settle_conductor(jobs)
            .settle_orphaned_gate_run("orph", &run, now, &cluster)
            .await
            .expect("settled");
        assert_eq!(writes.lock().unwrap()[0].2["verdict"], "lost");

        let (jobs, writes) = settle_api_with_cars(run.clone(), false, None, false).await;
        settle_conductor(jobs)
            .settle_orphaned_gate_run("orph", &run, now, &cluster)
            .await
            .expect("an unread car list is logged and retried next pass");
        assert!(
            writes.lock().unwrap().is_empty(),
            "nothing is settled on a list nobody read: {:?}",
            writes.lock().unwrap()
        );
    }

    /// A PACKET ADMITTED UNDER gate-run v2 CANNOT SAY `withdrawn`: its
    /// verdict field was materialized from the four-word enum, so the
    /// completion is refused. The settle then does what it did before
    /// this car — `lost`, in the same pass — rather than leaving the
    /// orphan open to the three-hour clock, which would be a regression
    /// dressed as a refinement.
    #[tokio::test]
    async fn a_refused_withdrawal_falls_back_to_lost_in_the_same_pass() {
        let now = Utc::now();
        let (run, car) = vouched_orphan(now);
        let (jobs, writes) = settle_api_with_cars(run.clone(), false, Some(vec![car]), true).await;
        let cluster = |_: &str, _: &str| -> Result<Vec<String>> { Ok(vec![]) };
        settle_conductor(jobs)
            .settle_orphaned_gate_run("orph", &run, now, &cluster)
            .await
            .expect("the fallback settles");
        let w = writes.lock().unwrap().clone();
        let verdicts: Vec<&Value> = w
            .iter()
            .filter(|(m, p, _)| m == "PATCH" && p.ends_with("/steps/s-v/metadata"))
            .map(|(_, _, b)| &b["verdict"])
            .collect();
        assert_eq!(
            verdicts,
            vec![&json!("withdrawn"), &json!("lost")],
            "withdrawn tried, lost written: {w:?}"
        );
        assert!(
            w.iter()
                .any(|(m, p, _)| m == "PUT" && p == "/api/jobs/orph/steps/s-v"),
            "and the lost verdict completed the step: {w:?}"
        );
    }

    #[tokio::test]
    async fn a_refused_step_write_leaves_no_evidence_on_the_open_packet() {
        let now = Utc::now();
        let run = idle_gate_run(now, 40);
        let (jobs, writes) = settle_api(run.clone(), true).await;
        let cluster = |_: &str, _: &str| -> Result<Vec<String>> { Ok(vec![]) };
        let res = settle_conductor(jobs)
            .settle_orphaned_gate_run("orph", &run, now, &cluster)
            .await;
        assert!(res.is_err(), "a refused step write is an error to log");
        assert!(
            !writes
                .lock()
                .unwrap()
                .iter()
                .any(|(_, _, b)| b.get("orphaned_gate_run").is_some()),
            "no orphan evidence rides a packet whose step did not complete: {:?}",
            writes.lock().unwrap()
        );
    }

    /// A GATE-RUN FILED IS A GATE-RUN ANSWERED (backlog 7919fdcc, item
    /// 11). The POST succeeded and the Job could not be started: the run
    /// is settled `lost` with the cause as its receipt, and the error
    /// names the run, so the dock can stamp it instead of filing a twin
    /// on its next pass — which it did every two minutes before.
    #[tokio::test]
    async fn a_gate_run_filed_but_never_started_is_settled_lost_and_named() {
        let run = idle_gate_run(Utc::now(), 0);
        let (jobs, writes) = settle_api(run, false).await;
        let start = |_: &str| -> Result<()> {
            Err(anyhow!(
                "kubectl create failed for the gate of fix/orphan: forbidden"
            ))
        };
        let err = settle_conductor(jobs)
            .file_and_start_gate(
                "fix/orphan",
                "abc1234",
                json!({"regate_of_car": "c"}),
                &start,
            )
            .await
            .expect_err("a Job that never started is a failed launch");
        assert_eq!(err.filed.as_deref(), Some("orph"), "the run is named");
        let text = format!("{err:#}");
        assert!(
            text.contains("forbidden") && text.contains("orph"),
            "{text}"
        );
        let w = writes.lock().unwrap().clone();
        let order: Vec<(String, String)> =
            w.iter().map(|(m, p, _)| (m.clone(), p.clone())).collect();
        assert_eq!(
            order,
            vec![
                ("POST".into(), "/api/jobs".into()),
                ("PATCH".into(), "/api/jobs/orph/metadata".into()),
                ("PATCH".into(), "/api/jobs/orph/steps/s-v/metadata".into()),
                ("PUT".into(), "/api/jobs/orph/steps/s-v".into()),
            ],
            "filed, marked, then settled"
        );
        assert_eq!(w[2].2["verdict"], "lost");
        let receipt = w[2].2["receipt"].as_str().expect("a receipt");
        assert!(
            receipt.contains("forbidden") && receipt.contains("fix/orphan"),
            "the cause is the receipt, verbatim: {receipt}"
        );
    }

    /// The control: a launch that starts its Job settles nothing.
    #[tokio::test]
    async fn a_gate_run_that_starts_is_not_settled() {
        let run = idle_gate_run(Utc::now(), 0);
        let (jobs, writes) = settle_api(run, false).await;
        let started = std::sync::Mutex::new(Vec::new());
        let start = |id: &str| -> Result<()> {
            started.lock().unwrap().push(id.to_string());
            Ok(())
        };
        let run_id = settle_conductor(jobs)
            .file_and_start_gate(
                "fix/orphan",
                "abc1234",
                json!({"regate_of_car": "c"}),
                &start,
            )
            .await
            .expect("launched");
        assert_eq!(run_id, "orph");
        assert_eq!(*started.lock().unwrap(), vec!["orph".to_string()]);
        let w = writes.lock().unwrap().clone();
        assert!(
            w.iter().all(|(_, p, _)| !p.contains("/steps/")),
            "no verdict is written for a run that started: {w:?}"
        );
        // And it is stamped launched, AFTER the marks and the start — the
        // key the yard dates a bay's running age from (backlog 4d088a7e).
        let last = w.last().expect("writes");
        assert_eq!(
            (last.0.as_str(), last.1.as_str()),
            ("PATCH", "/api/jobs/orph/metadata")
        );
        assert!(
            last.2
                .get(crate::gate::LAUNCHED_AT)
                .and_then(Value::as_str)
                .is_some_and(|s| chrono::DateTime::parse_from_rfc3339(s).is_ok()),
            "the run is stamped launched: {w:?}"
        );
    }

    /// And the dock's side of it: the run the launch filed rides the stamp,
    /// so the next pass reads its `lost` verdict and the red rule bounds
    /// the retry — the stamp never goes back to `gate_run ''`.
    #[test]
    fn an_unstarted_regate_names_its_run_and_how_it_is_retried() {
        let stamp = dock_regate::RegateStamp {
            head: "4ba3a93d5555".into(),
            gate_run: "9f1e2d3c4b5a".into(),
            ..dock_regate::RegateStamp::for_main("0c0ffee12345", &[])
        };
        let why = "kubectl create failed for the gate of feat/g: forbidden";
        let line = dock_regate::unstarted_reason(&stamp, why);
        for want in ["9f1e2d3c", "forbidden", "settled lost", "main moves"] {
            assert!(line.contains(want), "{want}: {line}");
        }
    }

    /// f2ba226e: pr-train 06e5610f was admitted with nine of its ten
    /// steps — the `cancelled` terminal never written — and `boss train
    /// cancel` refused with "step missing on job", AFTER it had already
    /// closed the PR and released the cars. The refusal now comes
    /// FIRST, before any forge or jobs-API write, and names what it
    /// found: the graph diverged from the pinned version, which step
    /// is unpaired, and the door that repairs it.
    #[tokio::test]
    async fn cancel_refuses_a_train_whose_terminal_is_missing_before_any_write() {
        // The 06e5610f shape: `cancelled` has no row on this job.
        let mut train = requested_open_train();
        train["workflow_version"] = json!(10);
        train["steps"]
            .as_array_mut()
            .unwrap()
            .retain(|s| s["spec_slug"] != "cancelled");
        let (jobs, job_merges, step_puts, step_patches) =
            cancel_request_jobs_api(train, struck_boarded_car()).await;
        let (c, close_called) = cancel_request_conductor(jobs, true);

        let err = c
            .cancel_train("t1", "bad consist", false, false)
            .await
            .expect_err("a train with no terminal cannot be cancelled");
        let msg = err.to_string();
        assert!(
            msg.contains("diverged from its workflow spec"),
            "names the divergence: {msg}"
        );
        assert!(
            msg.contains("cancelled") && msg.contains("v10"),
            "names the unpaired step and the pinned version: {msg}"
        );
        assert!(
            msg.contains("re-materialise") && msg.contains("f2ba226e"),
            "names the repair door and the item: {msg}"
        );
        assert!(
            !msg.contains("step missing on job"),
            "the old, uninformative refusal: {msg}"
        );

        assert!(
            !*close_called.lock().unwrap(),
            "refused BEFORE the forge write — the PR is still open"
        );
        assert!(job_merges.lock().unwrap().is_empty(), "no car was released");
        assert!(
            step_puts.lock().unwrap().is_empty() && step_patches.lock().unwrap().is_empty(),
            "no step was completed"
        );
    }

    /// 5186c5e1: the `cancelled` terminal's `ready_when` waits on the
    /// `empty` marker, which a train that boarded cars never carries,
    /// and the step API opens a Pending step by hand only where its
    /// predicate holds — except an ABORT (`outcome_kind = aborted`),
    /// which completes from any open state. pr-train's row says
    /// `aborted` (pinned in boss-jobs platform_bundle_pr_train.rs); a
    /// train pinned to a version that did not would answer the
    /// completion 409 AFTER the PR was closed and the cars released —
    /// a half-cancelled train. Refused FIRST, before any write.
    #[tokio::test]
    async fn cancel_refuses_a_train_whose_terminal_cannot_complete_before_any_write() {
        let mut train = requested_open_train();
        train["workflow_version"] = json!(7);
        for s in train["steps"].as_array_mut().unwrap() {
            if s["spec_slug"] == "cancelled" {
                s["status"] = json!("pending");
                s["metadata"] = json!({});
            }
        }
        let (jobs, job_merges, step_puts, step_patches) =
            cancel_request_jobs_api(train, struck_boarded_car()).await;
        let (c, close_called) = cancel_request_conductor(jobs, true);

        let err = c
            .cancel_train("t1", "bad consist", false, false)
            .await
            .expect_err("a terminal the step API will refuse cannot be completed");
        let msg = err.to_string();
        assert!(
            msg.contains("cancelled") && msg.contains("v7") && msg.contains("aborted"),
            "names the step, the pinned version and what it lacks: {msg}"
        );
        assert!(
            msg.contains("Nothing was written"),
            "says the train is intact: {msg}"
        );
        assert!(
            !*close_called.lock().unwrap(),
            "refused BEFORE the forge write — the PR is still open"
        );
        assert!(job_merges.lock().unwrap().is_empty(), "no car was released");
        assert!(
            step_puts.lock().unwrap().is_empty() && step_patches.lock().unwrap().is_empty(),
            "no step was completed"
        );
    }

    /// Control: the shape pr-train actually materialises — `cancelled`
    /// Pending on its marker, `outcome_kind = aborted` — is cancelled.
    #[tokio::test]
    async fn cancel_completes_a_pending_terminal_that_is_an_abort() {
        let mut train = requested_open_train();
        for s in train["steps"].as_array_mut().unwrap() {
            if s["spec_slug"] == "cancelled" {
                s["status"] = json!("pending");
                s["metadata"] = json!({ "outcome_kind": "aborted" });
            }
        }
        let (jobs, job_merges, step_puts, _) =
            cancel_request_jobs_api(train, struck_boarded_car()).await;
        let (c, close_called) = cancel_request_conductor(jobs, true);

        c.cancel_train("t1", "bad consist", false, false)
            .await
            .expect("an abort terminal completes from any open state");
        assert!(*close_called.lock().unwrap(), "the PR is closed");
        assert!(
            job_merges.lock().unwrap().iter().any(|(id, _)| id == "c1"),
            "the car was released"
        );
        assert!(
            step_puts
                .lock()
                .unwrap()
                .iter()
                .any(|(id, sid, _)| id == "t1" && sid == "s-cancelled"),
            "the cancelled terminal was completed"
        );
    }

    // -- the yard's cancel button, honoured non-fatally -------------------

    /// The forge the cancel button meets: `close_pr` answers as told and
    /// records the call; CI cancellation and branch deletion are the
    /// no-ops a cancel tolerates.
    struct OperatorCancelForge {
        close_ok: bool,
        close_called: std::sync::Arc<std::sync::Mutex<bool>>,
    }
    #[async_trait::async_trait]
    impl Forge for OperatorCancelForge {
        async fn pr_info(&self, _url: &str) -> Result<Value> {
            bail!("not exercised")
        }
        async fn pr_create(
            &self,
            _repo: &str,
            _head_branch: &str,
            _title: &str,
            _body: &str,
        ) -> Result<String> {
            bail!("not exercised")
        }
        async fn merge(&self, _url: &str, _message: &str) -> Result<()> {
            bail!("not exercised")
        }
        async fn close_pr(&self, _url: &str) -> Result<()> {
            *self.close_called.lock().unwrap() = true;
            if self.close_ok {
                Ok(())
            } else {
                bail!("HTTP 502: forge unreachable")
            }
        }
        async fn delete_branch(&self, _branch: &str) -> Result<bool> {
            Ok(true)
        }
        async fn branch_head(&self, _branch: &str) -> Result<Option<String>> {
            bail!("not exercised")
        }
        async fn cancel_ci_runs(&self, _pr_index: &str, _head_sha: &str) -> Result<usize> {
            Ok(0)
        }
    }

    /// An open train carrying the operator's stamp and one boarded car
    /// that already took a strike on an earlier consist.
    fn requested_open_train() -> Value {
        json!({
            "id": "t1", "kind": "pr-train", "status": "open",
            "metadata": {
                "boarded_jobs": ["c1"], "train_ref": "train/x@abcdef1",
                "cancel_requested": {"by": "emp-david", "reason": "bad consist", "at": "2026-09-07T01:00:00Z"}
            },
            "steps": [
                {"id":"s-pr","spec_slug":"pr","title":"Open the batched PR","status":"completed","metadata":{"pr_url":"https://forge.example/david/boss/pulls/9"}},
                {"id":"s-collect","spec_slug":"collect","title":"Collect what is ready to board","status":"completed","metadata":{}},
                {"id":"s-cancelled","spec_slug":"cancelled","title":"Cancelled — nothing to board","status":"ready","metadata":{}},
                {"id":"s-merged","spec_slug":"merged","title":"Merged into main","status":"ready","metadata":{}}
            ]
        })
    }
    fn struck_boarded_car() -> Value {
        json!({
            "id": "c1", "kind": "ship-a-change", "status": "open",
            "metadata": { "train": "t1", "branch": "fix/x", "red_trains": 1 },
            "steps": [{"id":"c-rev","spec_slug":"review","title":"Open for review","status":"ready","metadata":{}}]
        })
    }

    type JobMerges = std::sync::Arc<std::sync::Mutex<Vec<(String, Value)>>>;
    type StepPuts = std::sync::Arc<std::sync::Mutex<Vec<(String, String, Value)>>>;

    /// The step PUT as David's decided end state has it (design
    /// 93d2bddb, backlog e39a9d2a): a body carrying `metadata` is
    /// REFUSED 409, and the fields go through the merge door. The stubs
    /// below answer this way so the conductor's completions are pinned
    /// to the form that survives the tighten, not only to today's
    /// omission rule.
    fn end_state_step_put(body: &Value) -> Option<(axum::http::StatusCode, axum::Json<Value>)> {
        body.get("metadata").map(|_| {
            (
                axum::http::StatusCode::CONFLICT,
                axum::Json(json!({"error": "a step PUT carries no metadata; use the merge door"})),
            )
        })
    }

    /// An in-process jobs API holding one train and one car, recording
    /// every write. Serves the open pr-train list and both fetches;
    /// every other list answers empty.
    async fn cancel_request_jobs_api(
        train: Value,
        car: Value,
    ) -> (String, JobMerges, StepPuts, StepPatches) {
        use axum::extract::{Path, RawQuery};
        use axum::routing::{get, patch, put};
        use axum::{Json, Router};
        use std::sync::{Arc, Mutex};

        let job_merges: JobMerges = Arc::new(Mutex::new(Vec::new()));
        let step_puts: StepPuts = Arc::new(Mutex::new(Vec::new()));
        let step_patches: StepPatches = Arc::new(Mutex::new(Vec::new()));
        let (train_list, train_one) = (train.clone(), train);
        let (jp, sp, spp) = (job_merges.clone(), step_puts.clone(), step_patches.clone());
        let app = Router::new()
            .route(
                "/api/jobs",
                get(move |RawQuery(q): RawQuery| {
                    let train = train_list.clone();
                    async move {
                        let open_trains =
                            q.unwrap_or_default().contains("kind=pr-train&status=open");
                        let data: Vec<Value> = if open_trains { vec![train] } else { vec![] };
                        Json(json!({ "data": data }))
                    }
                }),
            )
            .route(
                "/api/jobs/{id}",
                get(move |Path(id): Path<String>| {
                    let (t, c) = (train_one.clone(), car.clone());
                    async move { Json(if id == "t1" { t } else { c }) }
                }),
            )
            .route(
                "/api/jobs/{id}/metadata",
                patch(move |Path(id): Path<String>, Json(body): Json<Value>| {
                    let jp = jp.clone();
                    async move {
                        jp.lock().unwrap().push((id, body));
                        axum::http::StatusCode::NO_CONTENT
                    }
                }),
            )
            .route(
                "/api/jobs/{id}/steps/{sid}",
                put(
                    move |Path((id, sid)): Path<(String, String)>, Json(body): Json<Value>| {
                        let sp = sp.clone();
                        async move {
                            if let Some(refused) = end_state_step_put(&body) {
                                return refused;
                            }
                            sp.lock().unwrap().push((id, sid, body));
                            (axum::http::StatusCode::OK, Json(json!({})))
                        }
                    },
                ),
            )
            .route(
                "/api/jobs/{id}/steps/{sid}/metadata",
                patch(
                    move |Path((id, sid)): Path<(String, String)>, Json(body): Json<Value>| {
                        let spp = spp.clone();
                        async move {
                            spp.lock().unwrap().push((id, sid, body));
                            Json(json!({}))
                        }
                    },
                ),
            );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        (
            format!("http://{addr}"),
            job_merges,
            step_puts,
            step_patches,
        )
    }

    fn cancel_request_conductor(
        jobs: String,
        close_ok: bool,
    ) -> (Conductor, std::sync::Arc<std::sync::Mutex<bool>>) {
        let close_called = std::sync::Arc::new(std::sync::Mutex::new(false));
        let mut c = cleanup_conductor(
            "forgejo",
            Box::new(OperatorCancelForge {
                close_ok,
                close_called: close_called.clone(),
            }),
        );
        c.cfg.jobs = jobs;
        (c, close_called)
    }

    /// The button, honoured: an open train stamped `cancel_requested` is
    /// cancelled the way the operator verb cancels — the car released
    /// UNSTRUCK (`red_trains` untouched) with the operator's reason as
    /// its `skip_reason`, the `cancelled` terminal completed with that
    /// reason — and the request claims the train's pass.
    #[tokio::test]
    async fn a_cancel_request_on_an_open_train_releases_its_cars_unstruck() {
        let (jobs, job_merges, step_puts, step_patches) =
            cancel_request_jobs_api(requested_open_train(), struck_boarded_car()).await;
        let (c, close_called) = cancel_request_conductor(jobs, true);

        assert!(
            c.honour_cancel_request(&requested_open_train(), "t1", Some("OPEN"))
                .await
        );
        assert!(*close_called.lock().unwrap(), "the PR is closed unmerged");

        let merges = job_merges.lock().unwrap();
        let (_, md) = merges
            .iter()
            .find(|(id, _)| id == "c1")
            .expect("the car was released");
        assert_eq!(
            md.get("train"),
            Some(&Value::Null),
            "released: the train stamp is deleted (a merge deletes a null)"
        );
        assert_eq!(
            md["skip_reason"].as_str().unwrap_or_default(),
            "returned to dock: train cancelled (operator cancel: bad consist (by emp-david))"
        );
        assert!(
            md.get("red_trains").is_none(),
            "an operator's cancel strikes no car: the merge leaves its count alone"
        );

        // The reason lands through the merge door, and the flip that
        // completes the terminal carries the status alone (e39a9d2a).
        let patches = step_patches.lock().unwrap();
        let (_, _, reason) = patches
            .iter()
            .find(|(id, sid, _)| id == "t1" && sid == "s-cancelled")
            .expect("the cancelled terminal's reason went through the merge door");
        assert_eq!(
            reason["reason"],
            json!("operator cancel: bad consist (by emp-david)")
        );
        let steps = step_puts.lock().unwrap();
        let (_, _, cancelled) = steps
            .iter()
            .find(|(id, sid, _)| id == "t1" && sid == "s-cancelled")
            .expect("the cancelled terminal was completed");
        assert_eq!(cancelled, &json!({"status": "completed"}));
    }

    /// The forge refuses the close: the train stays intact — no car
    /// released, no terminal completed — the request still claims the
    /// pass (a train under a cancel request must not go on to merge),
    /// and the method RETURNS, because it cannot fail: the reconcile
    /// loop has nothing to `?` and the other trains continue.
    #[tokio::test]
    async fn a_forge_failure_leaves_the_train_intact_and_the_pass_alive() {
        let (jobs, job_merges, step_puts, step_patches) =
            cancel_request_jobs_api(requested_open_train(), struck_boarded_car()).await;
        let (c, close_called) = cancel_request_conductor(jobs, false);

        assert!(
            c.honour_cancel_request(&requested_open_train(), "t1", Some("OPEN"))
                .await
        );
        assert!(*close_called.lock().unwrap(), "close_pr was attempted");
        assert!(
            job_merges.lock().unwrap().is_empty(),
            "no car released, nothing stamped — a retry next pass is clean"
        );
        assert!(
            step_puts.lock().unwrap().is_empty() && step_patches.lock().unwrap().is_empty(),
            "no terminal completed, nothing merged onto it"
        );
    }

    /// A request that arrives after the merge is refused on the record,
    /// once, and does not claim the pass — the landed train goes on to
    /// deploy and converge.
    #[tokio::test]
    async fn a_cancel_request_on_a_merged_train_is_stamped_refused() {
        let mut train = requested_open_train();
        train["steps"][3]["status"] = json!("completed");
        train["steps"][3]["metadata"]["merge_ref"] = json!("abc1234def56");
        let (jobs, job_merges, step_puts, step_patches) =
            cancel_request_jobs_api(train.clone(), struck_boarded_car()).await;
        let (c, close_called) = cancel_request_conductor(jobs, true);

        assert!(!c.honour_cancel_request(&train, "t1", Some("MERGED")).await);
        assert!(!*close_called.lock().unwrap(), "nothing closed");
        assert!(
            step_puts.lock().unwrap().is_empty() && step_patches.lock().unwrap().is_empty(),
            "no terminal completed"
        );
        let merges = job_merges.lock().unwrap();
        assert_eq!(
            merges.len(),
            1,
            "one stamp on the train, nothing on the car"
        );
        let (id, body) = &merges[0];
        assert_eq!(id, "t1");
        assert_eq!(
            body["cancel_refused"],
            json!("already merged at abc1234def56")
        );
    }

    /// A three-car train where car 2's close write fails. Cars 1 and 3
    /// must STILL get their review closed and `metadata.merged=true` (the
    /// marker the dispatcher watches to close the car Job); only the one
    /// bad car counts as a failure, and the pass does not abort. This is
    /// the orphan bug: the pre-fix loop used `?` and completed `merged`
    /// first, so one bad car left the rest as open residue forever —
    /// inflating the open-car count and starving boarding.
    #[tokio::test]
    async fn a_bad_car_does_not_orphan_the_rest_of_the_train() {
        use axum::extract::Path;
        use axum::routing::{get, patch, put};
        use axum::{Json, Router};
        use std::sync::{Arc, Mutex};

        fn car(id: &str) -> Value {
            json!({
                "id": id, "kind": "ship-a-change", "status": "open",
                "metadata": { "train": "t1", "branch": format!("fix/{id}") },
                "steps": [{"id": format!("{id}-rev"), "spec_slug": "review",
                           "title": "Open for review", "status": "ready", "metadata": {}}]
            })
        }
        let cars = json!({ "c1": car("c1"), "c2": car("c2"), "c3": car("c3") });

        // (car id, endpoint) of every WRITE the conductor made.
        let writes: Arc<Mutex<Vec<(String, String)>>> = Arc::new(Mutex::new(Vec::new()));

        let cars_get = cars.clone();
        let writes_step = writes.clone();
        let writes_meta = writes.clone();
        let writes_merge = writes.clone();
        let app = Router::new()
            .route(
                "/api/jobs/{id}",
                get(move |Path(id): Path<String>| {
                    let cars = cars_get.clone();
                    async move { Json(cars.get(&id).cloned().unwrap_or(Value::Null)) }
                }),
            )
            .route(
                "/api/jobs/{id}/metadata",
                patch(move |Path(id): Path<String>, _b: Json<Value>| {
                    let writes = writes_meta.clone();
                    async move {
                        // Car 2's metadata write is the one the SoR refuses
                        // (422 — an answer, not a blip, so it is not retried).
                        if id == "c2" {
                            return (
                                axum::http::StatusCode::UNPROCESSABLE_ENTITY,
                                Json(json!({"error": "no"})),
                            );
                        }
                        writes.lock().unwrap().push((id, "meta".into()));
                        (axum::http::StatusCode::OK, Json(json!({})))
                    }
                }),
            )
            .route(
                "/api/jobs/{id}/steps/{sid}/metadata",
                patch(
                    move |Path((id, _sid)): Path<(String, String)>, _b: Json<Value>| {
                        let writes = writes_merge.clone();
                        async move {
                            writes.lock().unwrap().push((id, "merge".into()));
                            Json(json!({}))
                        }
                    },
                ),
            )
            .route(
                "/api/jobs/{id}/steps/{sid}",
                put(
                    move |Path((id, _sid)): Path<(String, String)>, Json(b): Json<Value>| {
                        let writes = writes_step.clone();
                        async move {
                            if let Some(refused) = end_state_step_put(&b) {
                                return refused;
                            }
                            if id == "c2" {
                                return (
                                    axum::http::StatusCode::UNPROCESSABLE_ENTITY,
                                    Json(json!({"error": "no"})),
                                );
                            }
                            writes.lock().unwrap().push((id, "review".into()));
                            (axum::http::StatusCode::OK, Json(json!({})))
                        }
                    },
                ),
            );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });

        let mut c = cleanup_conductor(
            "forgejo",
            Box::new(FakeForge {
                deleted: Arc::new(Mutex::new(Vec::new())),
                fail_deletes: false,
            }),
        );
        c.cfg.jobs = format!("http://{addr}");

        let boarded = vec!["c1".to_string(), "c2".to_string(), "c3".to_string()];
        let failures = c
            .close_boarded_cars("t1", &boarded, "abcdef123456", "https://forge/pulls/9")
            .await;

        assert_eq!(failures, 1, "exactly the one bad car (c2) is a failure");
        let w = writes.lock().unwrap();
        for good in ["c1", "c3"] {
            assert!(
                w.contains(&(good.to_string(), "review".to_string())),
                "car {good} must still have its review closed — a bad car must not orphan it"
            );
            assert!(
                w.contains(&(good.to_string(), "meta".to_string())),
                "car {good} must still get metadata.merged — the dispatcher's close marker"
            );
        }
        assert!(
            !w.contains(&("c2".to_string(), "meta".to_string())),
            "c2's write failed, so its close marker must NOT be recorded"
        );
    }

    /// A partial pass leaves the failed car for the next reconcile, and
    /// the re-run is idempotent for the cars that already closed. Pass 1:
    /// car 2's write fails (the other two close). Pass 2: the server now
    /// reports the already-closed reviews as `completed` and car 2's write
    /// succeeds — so `close_boarded_cars` returns 0, re-closes only car 2,
    /// and issues NO duplicate review write for cars 1 and 3.
    #[tokio::test]
    async fn a_partial_close_retries_the_failed_car_idempotently() {
        use axum::extract::Path;
        use axum::routing::{get, patch, put};
        use axum::{Json, Router};
        use std::collections::HashSet;
        use std::sync::{Arc, Mutex};

        // Reviews the server has seen closed; drives idempotence — a car
        // in here reports `review: completed`, so `complete_step`
        // early-returns and issues no second write.
        let reviewed: Arc<Mutex<HashSet<String>>> = Arc::new(Mutex::new(HashSet::new()));
        // Car 2 heals between passes.
        let heal_c2 = Arc::new(Mutex::new(false));
        // (car id, endpoint) of every WRITE, across both passes.
        let writes: Arc<Mutex<Vec<(String, String)>>> = Arc::new(Mutex::new(Vec::new()));

        let reviewed_get = reviewed.clone();
        let reviewed_step = reviewed.clone();
        let heal_step = heal_c2.clone();
        let writes_step = writes.clone();
        let writes_meta = writes.clone();
        let writes_merge = writes.clone();
        let app = Router::new()
            .route(
                "/api/jobs/{id}",
                get(move |Path(id): Path<String>| {
                    let reviewed = reviewed_get.clone();
                    async move {
                        let status = if reviewed.lock().unwrap().contains(&id) {
                            "completed"
                        } else {
                            "ready"
                        };
                        Json(json!({
                            "id": id, "kind": "ship-a-change", "status": "open",
                            "metadata": { "train": "t1", "branch": format!("fix/{id}") },
                            "steps": [{"id": format!("{id}-rev"), "spec_slug": "review",
                                       "title": "Open for review", "status": status, "metadata": {}}]
                        }))
                    }
                }),
            )
            .route(
                "/api/jobs/{id}/metadata",
                patch(move |Path(id): Path<String>, _b: Json<Value>| {
                    let (heal, writes) = (heal_step.clone(), writes_meta.clone());
                    async move {
                        if id == "c2" && !*heal.lock().unwrap() {
                            return (
                                axum::http::StatusCode::UNPROCESSABLE_ENTITY,
                                Json(json!({"error": "no"})),
                            );
                        }
                        writes.lock().unwrap().push((id, "meta".into()));
                        (axum::http::StatusCode::OK, Json(json!({})))
                    }
                }),
            )
            .route(
                "/api/jobs/{id}/steps/{sid}/metadata",
                patch(
                    move |Path((id, _sid)): Path<(String, String)>, _b: Json<Value>| {
                        let writes = writes_merge.clone();
                        async move {
                            writes.lock().unwrap().push((id, "merge".into()));
                            Json(json!({}))
                        }
                    },
                ),
            )
            .route(
                "/api/jobs/{id}/steps/{sid}",
                put(
                    move |Path((id, _sid)): Path<(String, String)>, Json(b): Json<Value>| {
                        let (reviewed, writes) = (reviewed_step.clone(), writes_step.clone());
                        async move {
                            if let Some(refused) = end_state_step_put(&b) {
                                return refused;
                            }
                            writes.lock().unwrap().push((id.clone(), "review".into()));
                            reviewed.lock().unwrap().insert(id);
                            (axum::http::StatusCode::OK, Json(json!({})))
                        }
                    },
                ),
            );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });

        let mut c = cleanup_conductor(
            "forgejo",
            Box::new(FakeForge {
                deleted: Arc::new(Mutex::new(Vec::new())),
                fail_deletes: false,
            }),
        );
        c.cfg.jobs = format!("http://{addr}");
        let boarded = vec!["c1".to_string(), "c2".to_string(), "c3".to_string()];

        // Pass 1: car 2's metadata write is refused — but c2's review PUT
        // still lands first, so only its `meta` write is missing.
        let pass1 = c
            .close_boarded_cars("t1", &boarded, "abcdef123456", "https://forge/pulls/9")
            .await;
        assert_eq!(pass1, 1, "car 2 fails its metadata write on pass 1");

        // Car 2 heals; retry.
        *heal_c2.lock().unwrap() = true;
        let pass2 = c
            .close_boarded_cars("t1", &boarded, "abcdef123456", "https://forge/pulls/9")
            .await;
        assert_eq!(pass2, 0, "the retry recovers car 2 — nothing left orphaned");

        let w = writes.lock().unwrap();
        let review_writes = |id: &str| {
            w.iter()
                .filter(|(cid, ep)| cid == id && ep == "review")
                .count()
        };
        assert_eq!(
            review_writes("c1"),
            1,
            "car 1's review is written once — the retry must NOT re-close a done step"
        );
        assert_eq!(
            review_writes("c3"),
            1,
            "car 3's review is written once — the retry is idempotent"
        );
        assert!(
            w.contains(&("c2".to_string(), "meta".to_string())),
            "car 2's close marker lands on the retry"
        );
    }

    /// The forge as a call recorder for the WHOLE sweep: `delete_branch`
    /// notes the branch, `branch_head` answers a fixed head (so the
    /// guard reads `Delete`), everything else is unreachable. The seam
    /// the sweep's per-branch isolation is proven through.
    struct SweepForge {
        deleted: std::sync::Arc<std::sync::Mutex<Vec<String>>>,
        head: String,
        /// An honest forge forgets a branch it deleted, so the read-back
        /// after DELETE answers None. `false` models the 2026-09-11
        /// forge: answers the delete, keeps the branch (1096b1a4).
        performs_deletes: bool,
    }
    #[async_trait::async_trait]
    impl Forge for SweepForge {
        async fn pr_info(&self, _url: &str) -> Result<Value> {
            bail!("not exercised")
        }
        async fn pr_create(
            &self,
            _repo: &str,
            _head_branch: &str,
            _title: &str,
            _body: &str,
        ) -> Result<String> {
            bail!("not exercised")
        }
        async fn merge(&self, _url: &str, _message: &str) -> Result<()> {
            bail!("not exercised")
        }
        async fn close_pr(&self, _url: &str) -> Result<()> {
            bail!("not exercised")
        }
        async fn delete_branch(&self, branch: &str) -> Result<bool> {
            self.deleted.lock().unwrap().push(branch.to_string());
            Ok(true)
        }
        async fn branch_head(&self, branch: &str) -> Result<Option<String>> {
            if self.performs_deletes && self.deleted.lock().unwrap().iter().any(|b| b == branch) {
                return Ok(None);
            }
            Ok(Some(self.head.clone()))
        }
        async fn cancel_ci_runs(&self, _pr_index: &str, _head_sha: &str) -> Result<usize> {
            bail!("not exercised")
        }
    }

    /// THE FLEET-LEVEL ISOLATION, end to end. Two arrived trains are
    /// pending a sweep; train A's arrival-report write (a PATCH to the
    /// jobs API) returns 500 every pass — the exact shape of a boarded
    /// car deleted (404), a malformed report, or a forge blip. Before
    /// the fix, the `?` on that write aborted the WHOLE sweep, so every
    /// LATER pending train went unswept and its landed branch
    /// accumulated on the forge (recurring disk debt). The sweep is now
    /// best-effort per train: A is isolated and B is still swept.
    ///
    /// Ordered A-then-B deliberately — A is processed first, so an
    /// abort takes B down with it under the old code. The assertion is
    /// simply that B's branch WAS deleted.
    #[tokio::test]
    async fn one_trains_sweep_failing_does_not_block_the_next_train() {
        use axum::extract::{Path, RawQuery};
        use axum::http::StatusCode;
        use axum::response::IntoResponse;
        use axum::routing::get;
        use axum::{Json, Router};
        use std::sync::{Arc, Mutex};

        const HEAD: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";

        // A closed (arrived) train, `arrived` completed, one boarded
        // car — the shape the sweep filters in as pending.
        let train = |tid: &str| {
            json!({
                "id": tid, "kind": "pr-train", "status": "closed",
                "metadata": { "boarded_jobs": [format!("car-{tid}")] },
                "steps": [
                    {"id":"s-arr","spec_slug":"arrived","title":"Train arrived","status":"completed","metadata":{}}
                ]
            })
        };
        // A landed car: closed + merged, its branch and boarded head on
        // record, so `deletable_branches` yields it and the guard reads
        // Delete.
        // Car B carries the gate's tier stamp (ba429e7f); car A does
        // not — so the arrival PATCH this harness records shows both
        // the union and the unclassified count.
        let car = |tid: &str, branch: &str| {
            let mut c = json!({
                "id": format!("car-{tid}"), "kind": "ship-a-change", "status": "closed",
                "metadata": { "train": tid, "branch": branch, "outcome": "merged", "boarded_head": HEAD },
                "steps": []
            });
            if tid == "tB" {
                c["metadata"]["software_tiers"] = json!(["core", "frontend"]);
            }
            c
        };

        let train_a = train("tA");
        let train_b = train("tB");
        let car_a = car("tA", "fix/a");
        let car_b = car("tB", "fix/b");

        // A-then-B: the failing train is swept first, so an all-or-
        // nothing abort strands B.
        let closed_list = json!({ "data": [train_a.clone(), train_b.clone()], "total": 2 });

        let by_id: std::collections::HashMap<String, Value> = [
            ("tA".to_string(), train_a),
            ("tB".to_string(), train_b),
            ("car-tA".to_string(), car_a),
            ("car-tB".to_string(), car_b),
        ]
        .into_iter()
        .collect();
        let by_id = Arc::new(by_id);

        let list_route = closed_list.clone();
        let by_id_get = by_id.clone();
        let patched: Arc<Mutex<Vec<(String, Value)>>> = Arc::new(Mutex::new(Vec::new()));
        let app = Router::new()
            .route(
                "/api/jobs",
                get(move |RawQuery(q): RawQuery| {
                    let list = list_route.clone();
                    async move {
                        let q = q.unwrap_or_default();
                        // pr-train closed → the pending trains; the open
                        // ship-a-change list (open_car_branches) → none.
                        if q.contains("pr-train") {
                            Json(list)
                        } else {
                            Json(json!({ "data": [], "total": 0 }))
                        }
                    }
                }),
            )
            .route(
                "/api/jobs/{id}",
                get(move |Path(id): Path<String>| {
                    let by_id = by_id_get.clone();
                    async move { Json(by_id.get(&id).cloned().unwrap_or(json!({}))) }
                })
                .put(|Path(_id): Path<String>, _b: Json<Value>| async move { Json(json!({})) }),
            )
            .route(
                "/api/jobs/{id}/metadata",
                axum::routing::patch({
                    let patched = patched.clone();
                    move |Path(id): Path<String>, Json(b): Json<Value>| async move {
                        // Train A's arrival report cannot be written — the
                        // persistent per-train failure this test isolates.
                        if id == "tA" {
                            (StatusCode::INTERNAL_SERVER_ERROR, "boom").into_response()
                        } else {
                            patched.lock().unwrap().push((id, b));
                            Json(json!({})).into_response()
                        }
                    }
                }),
            );

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });

        let deleted = Arc::new(Mutex::new(Vec::new()));
        // github forge_kind: the train's OWN branch cleanup is a no-op
        // (the repo auto-deletes merged PR heads), keeping the test on
        // the CAR-branch sweep the isolation guards.
        let mut c = cleanup_conductor(
            "github",
            Box::new(SweepForge {
                deleted: deleted.clone(),
                head: HEAD.to_string(),
                performs_deletes: true,
            }),
        );
        c.cfg.jobs = format!("http://{addr}");

        // The sweep stays green — housekeeping is best-effort — and B's
        // branch is deleted despite A failing.
        c.sweep_landed_branches().await.unwrap();

        let deleted = deleted.lock().unwrap().clone();
        assert!(
            deleted.contains(&"fix/b".to_string()),
            "train B's landed branch went unswept because train A failed first — \
             one bad train stranded the fleet (disk debt): {deleted:?}"
        );
        assert!(
            !deleted.contains(&"fix/a".to_string()),
            "train A aborted before its branch loop, so its branch is untouched \
             this pass and retried next: {deleted:?}"
        );

        // THE TRAIN'S TIERS RIDE THE ARRIVAL PATCH (ba429e7f): train B's
        // report carries the union of its cars' stamps and the cars per
        // tier, beside the report — one row per train for the series.
        let patched = patched.lock().unwrap().clone();
        let (_, body) = patched
            .iter()
            .find(|(id, b)| id == "tB" && b.get("arrival_report").is_some())
            .expect("train B's arrival report was written");
        assert_eq!(body["software_tiers"], json!(["core", "frontend"]));
        assert_eq!(
            body["software_tier_counts"],
            json!({ "core": 1, "frontend": 1 })
        );
    }

    /// A forge that ANSWERS the delete and does not perform it: the
    /// 2026-09-11 shape (backlog 1096b1a4) — six landed branches on the
    /// forge the next morning under trains stamped swept. The sweep
    /// must read the branch back, count it a failure, leave the train
    /// UNSTAMPED, and write a `sweep_report` on the train that names
    /// the branch and what the forge said — the evidence that used to
    /// live only in a journal outside anyone's reach.
    #[tokio::test]
    async fn a_delete_the_forge_answered_but_did_not_perform_keeps_the_train_pending() {
        use axum::extract::{Path, RawQuery};
        use axum::routing::get;
        use axum::{Json, Router};
        use std::sync::{Arc, Mutex};

        const HEAD: &str = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
        let train = json!({
            "id": "tP", "kind": "pr-train", "status": "closed",
            "metadata": { "boarded_jobs": ["car-tP"] },
            "steps": [
                {"id":"s-arr","spec_slug":"arrived","title":"Train arrived","status":"completed","metadata":{}}
            ]
        });
        let car = json!({
            "id": "car-tP", "kind": "ship-a-change", "status": "closed",
            "metadata": { "train": "tP", "branch": "fix/stays", "outcome": "merged", "boarded_head": HEAD },
            "steps": []
        });
        let closed_list = json!({ "data": [train.clone()], "total": 1 });
        let by_id: std::collections::HashMap<String, Value> =
            [("tP".to_string(), train), ("car-tP".to_string(), car)]
                .into_iter()
                .collect();
        let by_id = Arc::new(by_id);
        let patches: Arc<Mutex<Vec<(String, Value)>>> = Arc::new(Mutex::new(Vec::new()));

        let list_route = closed_list.clone();
        let by_id_get = by_id.clone();
        let patches_w = patches.clone();
        let puts_w = patches.clone();
        let app = Router::new()
            .route(
                "/api/jobs",
                get(move |RawQuery(q): RawQuery| {
                    let list = list_route.clone();
                    async move {
                        if q.unwrap_or_default().contains("pr-train") {
                            Json(list)
                        } else {
                            Json(json!({ "data": [], "total": 0 }))
                        }
                    }
                }),
            )
            .route(
                "/api/jobs/{id}",
                get(move |Path(id): Path<String>| {
                    let by_id = by_id_get.clone();
                    async move { Json(by_id.get(&id).cloned().unwrap_or(json!({}))) }
                })
                // The stamp and the report arrive through the merge door
                // below (merge_job_metadata is a PATCH since design
                // 38f3a488); a whole-job PUT is recorded too, so a write
                // through either door is read.
                .put(move |Path(id): Path<String>, Json(b): Json<Value>| {
                    let puts = puts_w.clone();
                    async move {
                        puts.lock().unwrap().push((id, b));
                        Json(json!({}))
                    }
                }),
            )
            .route(
                "/api/jobs/{id}/metadata",
                axum::routing::patch(move |Path(id): Path<String>, Json(b): Json<Value>| {
                    let patches = patches_w.clone();
                    async move {
                        patches.lock().unwrap().push((id, b));
                        Json(json!({}))
                    }
                }),
            );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });

        // SweepForge answers every DELETE with Ok(true) and every
        // branch_head with the same head — a forge that says "deleted"
        // and keeps the branch.
        let deleted = Arc::new(Mutex::new(Vec::new()));
        let mut c = cleanup_conductor(
            "github",
            Box::new(SweepForge {
                deleted: deleted.clone(),
                head: HEAD.to_string(),
                performs_deletes: false,
            }),
        );
        c.cfg.jobs = format!("http://{addr}");
        c.sweep_landed_branches().await.unwrap();

        assert!(
            deleted.lock().unwrap().contains(&"fix/stays".to_string()),
            "the delete was attempted"
        );
        let patches = patches.lock().unwrap().clone();
        let train_patches: Vec<&Value> = patches
            .iter()
            .filter(|(id, _)| id == "tP")
            .map(|(_, b)| b)
            .collect();
        let stamped = train_patches.iter().any(|b| {
            truthy(
                b.pointer("/metadata/branches_swept")
                    .or_else(|| b.get("branches_swept")),
            )
        });
        assert!(
            !stamped,
            "the train was stamped swept while its branch is still on the forge: {train_patches:?}"
        );
        let report = train_patches
            .iter()
            .find_map(|b| {
                b.pointer("/metadata/sweep_report")
                    .or_else(|| b.get("sweep_report"))
            })
            .expect("the sweep wrote a report on the train even though it did not stamp it");
        let text = report.to_string();
        assert!(
            text.contains("fix/stays")
                && text.contains("still present")
                && text.contains("deleted"),
            "the report names the branch, that it is still present, and what the forge said: {text}"
        );
    }

    /// END TO END, the branch the sweep could never see (packet
    /// 473fda1b, generator 1). One arrived train, one landed car that a
    /// rerail had moved onto `feat/x-rerail` — and the original
    /// `feat/x`, which was never a car of its own. The train deletes
    /// what it MERGED, so before this fix the original survived every
    /// sweep forever; now the car's recorded origin is swept in the
    /// same pass, through the same head guard.
    #[tokio::test]
    async fn the_sweep_deletes_a_landed_rerail_original_alongside_its_twin() {
        use axum::extract::{Path, RawQuery};
        use axum::routing::get;
        use axum::{Json, Router};
        use std::sync::{Arc, Mutex};

        const HEAD: &str = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";

        let train = json!({
            "id": "t1", "kind": "pr-train", "status": "closed",
            "metadata": { "boarded_jobs": ["car-1"] },
            "steps": [
                {"id":"s-arr","spec_slug":"arrived","title":"Train arrived","status":"completed","metadata":{}}
            ]
        });
        // The car as `boss rerail` leaves it: riding the rerail branch,
        // with the branch it was moved off and that branch's head on the
        // record.
        let car = json!({
            "id": "car-1", "kind": "ship-a-change", "status": "closed",
            "metadata": {
                "train": "t1", "branch": "feat/x-rerail", "outcome": "merged",
                "boarded_head": HEAD,
                "rerail_origins": [{ "branch": "feat/x", "head": HEAD }]
            },
            "steps": []
        });

        let by_id: std::collections::HashMap<String, Value> = [
            ("t1".to_string(), train.clone()),
            ("car-1".to_string(), car),
        ]
        .into_iter()
        .collect();
        let by_id = Arc::new(by_id);
        let closed_list = json!({ "data": [train], "total": 1 });

        let app = Router::new()
            .route(
                "/api/jobs",
                get(move |RawQuery(q): RawQuery| {
                    let list = closed_list.clone();
                    async move {
                        if q.unwrap_or_default().contains("pr-train") {
                            Json(list)
                        } else {
                            Json(json!({ "data": [], "total": 0 }))
                        }
                    }
                }),
            )
            .route(
                "/api/jobs/{id}",
                get(move |Path(id): Path<String>| {
                    let by_id = by_id.clone();
                    async move { Json(by_id.get(&id).cloned().unwrap_or(json!({}))) }
                })
                .put(|Path(_id): Path<String>, _b: Json<Value>| async move { Json(json!({})) }),
            )
            .route(
                "/api/jobs/{id}/metadata",
                axum::routing::patch(|Path(_id): Path<String>, _b: Json<Value>| async move {
                    Json(json!({}))
                }),
            );

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });

        let deleted = Arc::new(Mutex::new(Vec::new()));
        let mut c = cleanup_conductor(
            "github",
            Box::new(SweepForge {
                deleted: deleted.clone(),
                head: HEAD.to_string(),
                performs_deletes: true,
            }),
        );
        c.cfg.jobs = format!("http://{addr}");
        c.sweep_landed_branches().await.unwrap();

        let deleted = deleted.lock().unwrap().clone();
        assert!(
            deleted.contains(&"feat/x-rerail".to_string()),
            "the branch the train merged must still be swept: {deleted:?}"
        );
        assert!(
            deleted.contains(&"feat/x".to_string()),
            "the rerail original was never a car, so only its record can \
             reach it — leaked forever without this: {deleted:?}"
        );
    }

    // -- a judged red brakes the car it names (a2d4d842) -------------------
    //
    // Train f7bd1e9d's shape: two cars aboard, the verdict locating its
    // failure in apps/web/src/it/yard/phone-strip.test.ts, which only car
    // G changed. Real git for the per-car diff (the conductor's clone,
    // with origin/main at the base both cars branched from) and an
    // in-process jobs API recording the step-metadata PATCH, because the
    // claim is that the hold lands on the RIGHT car's review step through
    // the door `boss hold` uses — a faked diff would only prove this file
    // agrees with itself.

    fn rev_parse_head(dir: &std::path::Path) -> String {
        let out = std::process::Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(["rev-parse", "HEAD"])
            .output()
            .expect("git rev-parse");
        String::from_utf8_lossy(&out.stdout).trim().to_string()
    }

    fn commit_file(clone: &std::path::Path, from: &str, path: &str, body: &str) -> String {
        git_ok(clone, &["checkout", "-q", from]);
        let file = clone.join(path);
        std::fs::create_dir_all(file.parent().expect("parent")).expect("mkdir");
        std::fs::write(&file, body).expect("write");
        git_ok(clone, &["add", "-A"]);
        git_ok(clone, &["commit", "-qm", path]);
        rev_parse_head(clone)
    }

    type StepPatches = std::sync::Arc<std::sync::Mutex<Vec<(String, String, Value)>>>;

    async fn hold_jobs_api(cars: Vec<Value>) -> (String, StepPatches) {
        use axum::extract::Path;
        use axum::routing::{get, patch};
        use axum::{Json, Router};
        use std::sync::{Arc, Mutex};

        let patches: StepPatches = Arc::new(Mutex::new(Vec::new()));
        let rec = patches.clone();
        let app = Router::new()
            .route(
                "/api/jobs/{id}",
                get(move |Path(id): Path<String>| {
                    let cars = cars.clone();
                    async move {
                        Json(
                            cars.into_iter()
                                .find(|c| c["id"] == json!(id))
                                .unwrap_or(json!({})),
                        )
                    }
                }),
            )
            .route(
                "/api/jobs/{id}/steps/{sid}/metadata",
                patch(
                    move |Path((id, sid)): Path<(String, String)>, Json(body): Json<Value>| {
                        let rec = rec.clone();
                        async move {
                            rec.lock().unwrap().push((id, sid, body));
                            Json(json!({}))
                        }
                    },
                ),
            );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        (format!("http://{addr}"), patches)
    }

    fn aboard_car(id: &str, head: &str, review_md: Value) -> Value {
        json!({
            "id": id, "kind": "ship-a-change", "status": "open",
            "metadata": {"train": "t1", "boarded_head": head},
            "steps": [{"id": format!("{id}-rev"), "spec_slug": "review",
                       "title": "Open for review", "status": "ready", "metadata": review_md}]
        })
    }

    const PHONE_STRIP: &str = "apps/web/src/it/yard/phone-strip.test.ts";

    fn judged_excerpt() -> Vec<(String, String)> {
        vec![(
            "svelte-check".to_string(),
            format!(
                "/gate-target/repo/{PHONE_STRIP}:83:26\n\
                 Error: Conversion of type '{{ thirds: {{ third: string; }}[]; }}' may be a mistake\n"
            ),
        )]
    }

    #[tokio::test]
    async fn a_judged_red_holds_the_one_car_that_changed_the_failing_file() {
        let (_g, clone) = clone_fixture("judged-red-hold");
        let base = rev_parse_head(&clone);
        let car_g = commit_file(&clone, &base, PHONE_STRIP, "the cast\n");
        let shed = commit_file(
            &clone,
            &base,
            "crates/core/boss-jobs/src/car.rs",
            "// shed\n",
        );
        let cars = vec![
            aboard_car("c-g", &car_g, json!({})),
            aboard_car("c-shed", &shed, json!({})),
        ];
        let (jobs, patches) = hold_jobs_api(cars).await;
        let mut c = cleanup_conductor(
            "forgejo",
            Box::new(OperatorCancelForge {
                close_ok: true,
                close_called: Default::default(),
            }),
        );
        c.cfg.jobs = jobs;
        c.cfg.clone = clone.display().to_string();
        let train = json!({"id": "t1", "metadata": {"boarded_jobs": ["c-g", "c-shed"]}});
        let named = vec![
            "CI / web (pull_request)".to_string(),
            "svelte-check".to_string(),
        ];

        c.hold_named_cars(&train, "t1", &named, &[], &judged_excerpt(), None)
            .await;

        let patches = patches.lock().unwrap().clone();
        assert_eq!(
            patches.len(),
            1,
            "one car held, the shed car not: {patches:?}"
        );
        let (id, sid, body) = &patches[0];
        assert_eq!((id.as_str(), sid.as_str()), ("c-g", "c-g-rev"));
        let hold = body["hold"].as_str().unwrap_or_default();
        assert!(
            hold.contains(PHONE_STRIP) && hold.contains("CI / web (pull_request)"),
            "the hold names the file and the check: {hold}"
        );
        assert_eq!(
            boss_jobs::stranded::hold_reason(body).as_deref(),
            Some(hold),
            "written in the one shape the dock's hold predicate reads"
        );
    }

    #[tokio::test]
    async fn a_car_already_held_keeps_its_own_reason() {
        let (_g, clone) = clone_fixture("judged-red-held");
        let base = rev_parse_head(&clone);
        let car_g = commit_file(&clone, &base, PHONE_STRIP, "the cast\n");
        let cars = vec![aboard_car(
            "c-g",
            &car_g,
            json!({"hold": "waiting on a rebase by hand"}),
        )];
        let (jobs, patches) = hold_jobs_api(cars).await;
        let mut c = cleanup_conductor(
            "forgejo",
            Box::new(OperatorCancelForge {
                close_ok: true,
                close_called: Default::default(),
            }),
        );
        c.cfg.jobs = jobs;
        c.cfg.clone = clone.display().to_string();
        let train = json!({"id": "t1", "metadata": {"boarded_jobs": ["c-g"]}});

        c.hold_named_cars(
            &train,
            "t1",
            &["svelte-check".into()],
            &[],
            &judged_excerpt(),
            None,
        )
        .await;

        assert!(
            patches.lock().unwrap().is_empty(),
            "an operator's hold is not overwritten"
        );
    }

    // -- the dock's re-gate on current main (backlog 969a1092) -------------

    /// A car branch cut from the fixture's main, carrying one file at
    /// `path`, published to both remotes — its head.
    fn park_car(clone: &std::path::Path, branch: &str, path: &str) -> String {
        git_ok(clone, &["checkout", "-q", "-b", branch, "main"]);
        let file = clone.join(path);
        std::fs::create_dir_all(file.parent().expect("parent")).expect("mkdir");
        std::fs::write(&file, branch).expect("write");
        git_ok(clone, &["add", "-A"]);
        git_ok(clone, &["commit", "-qm", branch]);
        git_ok(clone, &["push", "-q", "origin", branch]);
        git_ok(clone, &["push", "-q", "fork", branch]);
        git_ok(clone, &["checkout", "-q", "main"]);
        rev(clone, branch)
    }

    /// Main lands a change at `path` — car F, in the measured case.
    fn land_on_main(clone: &std::path::Path, path: &str) {
        let file = clone.join(path);
        std::fs::create_dir_all(file.parent().expect("parent")).expect("mkdir");
        std::fs::write(&file, "landed").expect("write");
        git_ok(clone, &["add", "-A"]);
        git_ok(clone, &["commit", "-qm", "a car lands"]);
        git_ok(clone, &["push", "-q", "origin", "main"]);
    }

    /// The head the FORGE carries for `branch` — what a moved branch
    /// would show, read from the bare remote itself.
    fn forge_branch(clone: &std::path::Path, branch: &str) -> String {
        let origin = clone.parent().expect("root").join("origin.git");
        rev(&origin, &format!("refs/heads/{branch}"))
    }

    /// The `regate_owed` marker a red train's cancel leaves on a car it
    /// released (backlog 96f02540) — what sends a car through the dock's
    /// judgement at all.
    fn owed() -> Value {
        dock_regate::owed_stamp("t-red", &[], &[], Utc::now())
    }

    fn dock_conductor(clone: &std::path::Path) -> Conductor {
        let mut c = cleanup_conductor(
            "forgejo",
            Box::new(FakeForge {
                deleted: std::sync::Arc::default(),
                fail_deletes: false,
            }),
        );
        c.cfg.clone = clone.display().to_string();
        c
    }

    /// Car G's shape on a real clone: parked on an old main, main lands a
    /// change BESIDE its file, and the dock owes it a re-gate. Here the
    /// means to gate are absent (no runner manifest, as on a box with no
    /// cluster), so the car BOARDS AS GATED — and, the half that matters
    /// most, its branch has not moved: a replay the dock could not follow
    /// with a gate would leave a car no receipt vouches for.
    #[tokio::test]
    async fn a_touched_car_with_no_means_to_regate_boards_and_its_branch_stays_put() {
        let (_g, clone) = clone_fixture("dock-no-means");
        let head = park_car(&clone, "feat/g", "apps/web/src/it/yard/phone-strip.ts");
        land_on_main(&clone, "apps/web/src/it/yard/regions.ts");
        let c = dock_conductor(&clone);
        let car = json!({"id": "car-g", "metadata": {
            "branch": "feat/g", "summary": "G", dock_regate::REGATE_OWED: owed(),
        }});
        assert_eq!(
            c.base_hold(&car, "car-g-000", "feat/g", &head).await,
            None,
            "a failure of the means must never freeze a landing"
        );
        assert_eq!(forge_branch(&clone, "feat/g"), head, "nothing was replayed");
    }

    /// THE RE-GATE ON LANDING IS OFF (backlog 96f02540). Car G's shape
    /// again — main landed a change beside its file after its gate — but
    /// no red train owes it a re-gate: it boards as gated, and the dock
    /// neither replays it nor reads its base. On 2026-09-28 this was five
    /// of seven parked cars queued 40-60 minutes behind saturated bays for
    /// a test the train gate runs again on the assembled consist.
    #[tokio::test]
    async fn a_car_main_moved_beside_boards_as_gated_unless_a_red_owes_it_a_regate() {
        let (_g, clone) = clone_fixture("dock-landing-off");
        let head = park_car(&clone, "feat/g", "apps/web/src/it/yard/phone-strip.ts");
        land_on_main(&clone, "apps/web/src/it/yard/regions.ts");
        let c = dock_conductor(&clone);
        let car = json!({"id": "car-g", "metadata": {"branch": "feat/g", "summary": "G"}});
        assert_eq!(
            c.base_hold(&car, "car-g-000", "feat/g", &head).await,
            None,
            "no red owes it a re-gate: it boards"
        );
        assert_eq!(forge_branch(&clone, "feat/g"), head, "nothing was replayed");
        // Control: the same car, owed a re-gate by a red train, and the
        // bound already spent on this main, is HELD — the judgement runs.
        let main = rev(&clone, "origin/main");
        let refused = dock_regate::RegateStamp {
            refused: "boss gate --rebase: REFUSED — hit a conflict in: x".into(),
            ..dock_regate::RegateStamp::for_main(&main, &[])
        };
        let owing = json!({"id": "car-g", "metadata": {
            "branch": "feat/g",
            dock_regate::REGATE_OWED: owed(),
            dock_regate::BASE_REGATE: refused.to_value(Utc::now()),
        }});
        assert!(
            c.base_hold(&owing, "car-g-000", "feat/g", &head)
                .await
                .is_some(),
            "a car a red train owes a re-gate is judged as before"
        );
    }

    /// The bound, end to end: a car already re-gated for the main it would
    /// board on is held on the recorded answer and nothing is launched —
    /// here the refused replay, whose reason names the repair.
    #[tokio::test]
    async fn a_car_already_regated_for_this_main_is_held_without_a_second_launch() {
        let (_g, clone) = clone_fixture("dock-bound");
        let head = park_car(&clone, "feat/g", "apps/web/src/it/yard/phone-strip.ts");
        land_on_main(&clone, "apps/web/src/it/yard/regions.ts");
        let main = rev(&clone, "origin/main");
        let stamp = dock_regate::RegateStamp {
            refused: "boss gate --rebase: REFUSED — hit a conflict in: x".into(),
            ..dock_regate::RegateStamp::for_main(&main, &[])
        };
        let car = json!({"id": "car-g", "metadata": {
            "branch": "feat/g",
            dock_regate::REGATE_OWED: owed(),
            dock_regate::BASE_REGATE: stamp.to_value(Utc::now()),
        }});
        let c = dock_conductor(&clone);
        let DockHold {
            reason,
            stamp: write,
            waiting,
        } = c
            .base_hold(&car, "car-g-000", "feat/g", &head)
            .await
            .expect("held");
        assert!(reason.contains("boss rerail car-g-00"), "{reason}");
        assert!(
            waiting.is_none(),
            "held on the recorded answer, it waits for no bay, so it claims none"
        );
        assert!(write.is_none(), "the recorded answer is not rewritten");
        assert_eq!(forge_branch(&clone, "feat/g"), head);
    }

    /// A car a judged red held (`hold_named_cars`) is never re-gated by
    /// the dock: the hold lands on its review step, `parked_ready` refuses
    /// it, and `candidates` drops it before `base_hold` is asked. A
    /// re-gate would replay a car its own red already named, and the
    /// refresh on its green would not release the hold anyway.
    #[test]
    fn a_car_held_by_a_judged_red_never_reaches_the_dock_regate() {
        let reason = judged_red_hold_reason("t1", &["svelte-check".into()], &[]);
        let review_md = crate::steps::hold_patch(&reason);
        let car = json!({
            "id": "c-g", "kind": "ship-a-change", "status": "open",
            "metadata": {"branch": "feat/g"},
            "steps": [{"id": "c-g-rev", "spec_slug": "review", "title": "Open for review",
                       "status": "ready", "metadata": review_md}]
        });
        assert!(
            !parked_ready(&car),
            "a held car must not board, so it is never judged for a re-gate"
        );
        let unheld = json!({
            "id": "c-g", "kind": "ship-a-change", "status": "open",
            "metadata": {"branch": "feat/g"},
            "steps": [{"id": "c-g-rev", "spec_slug": "review", "title": "Open for review",
                       "status": "ready", "metadata": {}}]
        });
        assert!(
            parked_ready(&unheld),
            "control: the same car unheld is parked"
        );
    }

    /// A car main moved nowhere near, and a car on current main, board
    /// exactly as before this rule existed.
    #[tokio::test]
    async fn an_untouched_or_current_car_boards_as_before() {
        let (_g, clone) = clone_fixture("dock-untouched");
        let far = park_car(&clone, "feat/far", "infra/lint/a-lint.sh");
        land_on_main(&clone, "apps/web/src/it/yard/regions.ts");
        let fresh = park_car(&clone, "feat/fresh", "apps/web/src/it/yard/phone-strip.ts");
        let c = dock_conductor(&clone);
        let car =
            |b: &str| json!({"id": b, "metadata": {"branch": b, dock_regate::REGATE_OWED: owed()}});
        assert_eq!(
            c.base_hold(&car("feat/far"), "far", "feat/far", &far).await,
            None
        );
        assert_eq!(
            c.base_hold(&car("feat/fresh"), "fresh", "feat/fresh", &fresh)
                .await,
            None
        );
        assert_eq!(forge_branch(&clone, "feat/far"), far);
    }

    // -- a red dock re-gate is retried, then garaged (backlog 2fccbfd6) --

    /// A jobs API that answers one read: every gate-run is `run`. (It
    /// served the cadence rules too, for the re-gate hold the red retry's
    /// bound was read from; that bound is fixed since backlog 96f02540.)
    async fn red_regate_api(run: Value) -> String {
        use axum::routing::get;
        use axum::{Json, Router};
        let app = Router::new().route(
            "/api/jobs/{id}",
            get(move || {
                let r = run.clone();
                async move { Json(r) }
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        format!("http://{addr}")
    }

    /// Gate-run e77c4e30's shape: judged `failed` on `test`, closed
    /// `minutes_ago`.
    fn red_run(minutes_ago: i64) -> Value {
        let receipt = json!({"verdict": "failed", "checks": [
            {"name": "clippy", "result": "pass"},
            {"name": "test", "result": "fail"},
        ]});
        json!({
            "id": "gr-red", "kind": "gate-run", "status": "closed",
            "metadata": {"closed_at": (Utc::now() - chrono::Duration::minutes(minutes_ago)).to_rfc3339(),
                         "outcome": "failed"},
            "steps": [{"spec_slug": "record-verdict", "status": "completed",
                       "metadata": {"verdict": "failed", "receipt": receipt.to_string()}}],
        })
    }

    /// The car the dock replayed onto `main` and re-gated as `gr-red`.
    fn replayed_car(main: &str, head: &str, reds: u32) -> (Value, dock_regate::RegateStamp) {
        let stamp = dock_regate::RegateStamp {
            head: head.into(),
            gate_run: "gr-red".into(),
            reds,
            ..dock_regate::RegateStamp::for_main(main, &[])
        };
        let car = json!({"id": "car-g", "metadata": {
            "branch": "feat/g",
            dock_regate::BASE_REGATE: stamp.to_value(Utc::now() - chrono::Duration::minutes(20)),
        }});
        (car, stamp)
    }

    /// THE MEASURED CASE, end to end (2fccbfd6). A red dock re-gate on a
    /// main that has not moved holds the car ON the red, with the red on
    /// its stamp for the yard and orient to read; the pass after main
    /// moves owes the retry and attempts it (here the means to gate are
    /// absent, so it says it could not launch — and the branch stays put).
    /// Before this, the second pass was identical to the first, forever.
    #[tokio::test]
    async fn a_red_dock_regate_is_retried_once_main_moves() {
        let (_g, clone) = clone_fixture("dock-red-retry");
        let head = park_car(&clone, "feat/g", "apps/web/src/it/yard/phone-strip.ts");
        let main = rev(&clone, "origin/main");
        let (car, stamp) = replayed_car(&main, &head, 0);
        let mut c = dock_conductor(&clone);
        c.cfg.jobs = red_regate_api(red_run(5)).await;

        let still = c
            .regate_in_flight(&car, "car-g-000", "feat/g", stamp.clone())
            .await;
        assert!(still.reason.contains("NOT boardable"), "{}", still.reason);
        assert!(
            !still.reason.contains("could not launch"),
            "{}",
            still.reason
        );
        let written = json!({"metadata": {
            dock_regate::BASE_REGATE: still.stamp.clone().expect("the red is written"),
        }});
        let red = boss_jobs::dock_red::regate_red(&written["metadata"]).expect("a red");
        assert_eq!(red.checks, vec!["test"]);
        assert_eq!(red.gate_run, "gr-red");
        assert!(!red.garaged);

        land_on_main(&clone, "apps/web/src/it/yard/regions.ts");
        let moved = c.regate_in_flight(&car, "car-g-000", "feat/g", stamp).await;
        assert!(
            moved.reason.contains("could not launch"),
            "main moved: the retry is attempted — {}",
            moved.reason
        );
        assert_eq!(forge_branch(&clone, "feat/g"), head, "nothing was replayed");
    }

    /// A RETRY THAT GOES RED TOO GARAGES THE CAR at that head, naming the
    /// check — and the next main move still re-gates it once, because a
    /// red main shares (the measured lint regression) is not the car's
    /// (HIGH 1 of the adversarial review of car 5eb1967e).
    #[tokio::test]
    async fn a_red_retry_garages_the_head_and_a_main_move_still_retries_it() {
        let (_g, clone) = clone_fixture("dock-red-garage");
        let head = park_car(&clone, "feat/g", "apps/web/src/it/yard/phone-strip.ts");
        let main = rev(&clone, "origin/main");
        let (car, stamp) = replayed_car(&main, &head, 1);
        let mut c = dock_conductor(&clone);
        c.cfg.jobs = red_regate_api(red_run(45)).await;
        let hold = c
            .regate_in_flight(&car, "car-g-000", "feat/g", stamp.clone())
            .await;
        assert!(hold.reason.contains("garaged"), "{}", hold.reason);
        assert!(!hold.reason.contains("could not launch"), "{}", hold.reason);
        let written = json!({"metadata": {dock_regate::BASE_REGATE: hold.stamp.expect("written")}});
        let red = boss_jobs::dock_red::regate_red(&written["metadata"]).expect("a red");
        assert!(
            red.garaged,
            "past its bound, but a head judged red twice is not re-gated"
        );
        assert_eq!(red.checks, vec!["test"]);

        // The next pass reads the car as the first one LEFT it — garaged
        // red and all — since "already garaged" is read off the car's own
        // red (backlog f8383a38, L2), not off its red count.
        let mut car = car;
        car["metadata"][dock_regate::BASE_REGATE] =
            written["metadata"][dock_regate::BASE_REGATE].clone();
        land_on_main(&clone, "apps/web/src/it/yard/regions.ts");
        let moved = c.regate_in_flight(&car, "car-g-000", "feat/g", stamp).await;
        assert!(
            moved.reason.contains("could not launch"),
            "main moved: the garaged car gets its fresh-base retry — {}",
            moved.reason
        );
        // A retry that could not launch leaves the car as garaged as it
        // was (round-2 re-review, N1): still a car only a person frees,
        // so the board's walk still refuses it as needs-human.
        let still = json!({"metadata": {
            dock_regate::BASE_REGATE: moved.stamp.expect("the red is re-written"),
        }});
        assert!(
            boss_jobs::dock_red::regate_red(&still["metadata"]).is_some_and(|r| r.garaged),
            "{still}"
        );
    }

    /// L2 OF THE ROUND-3 REVIEW OF CAR 5eb1967e (backlog f8383a38), end to
    /// end. The car's first red was judged and retried (so its stamp
    /// carries `reds: 1`); the retry came back LOST — an unjudged red,
    /// which never garages. Main moves, the retry is owed, and here it
    /// cannot launch: the red it is held on must still say NOT garaged,
    /// because nothing garaged it. `reds > 0` said it was.
    #[tokio::test]
    async fn a_lost_retry_that_cannot_relaunch_is_not_garaged() {
        let (_g, clone) = clone_fixture("dock-lost-retry");
        let head = park_car(&clone, "feat/g", "apps/web/src/it/yard/phone-strip.ts");
        let main = rev(&clone, "origin/main");
        let (car, stamp) = replayed_car(&main, &head, 1);
        let mut lost = red_run(5);
        lost["steps"][0]["metadata"] = json!({"verdict": "lost"});
        lost["metadata"]["outcome"] = json!("lost");
        let mut c = dock_conductor(&clone);
        c.cfg.jobs = red_regate_api(lost).await;
        land_on_main(&clone, "apps/web/src/it/yard/regions.ts");
        let moved = c.regate_in_flight(&car, "car-g-000", "feat/g", stamp).await;
        assert!(
            moved.reason.contains("could not launch"),
            "main moved: the retry is attempted — {}",
            moved.reason
        );
        let still = json!({"metadata": {
            dock_regate::BASE_REGATE: moved.stamp.expect("the red is written"),
        }});
        let red = boss_jobs::dock_red::regate_red(&still["metadata"]).expect("a red");
        assert_eq!(red.verdict, "lost", "{still}");
        assert!(!red.garaged, "a lost red garages nothing: {still}");
    }

    /// A jobs API with state: jobs by id, a list door filtered by `kind`
    /// and `status`, the metadata merge (a `null` deletes) and a create
    /// that records every body. What the departure's counting writes is
    /// read back off it.
    #[derive(Clone, Default)]
    struct Sor {
        review_protocol: std::sync::Arc<std::sync::Mutex<Option<Value>>>,
        jobs: std::sync::Arc<std::sync::Mutex<std::collections::BTreeMap<String, Value>>>,
        posts: std::sync::Arc<std::sync::Mutex<Vec<Value>>>,
    }

    impl Sor {
        fn put(&self, job: Value) {
            let id = job["id"].as_str().expect("id").to_string();
            self.jobs.lock().unwrap().insert(id, job);
        }
        fn get(&self, id: &str) -> Value {
            self.jobs
                .lock()
                .unwrap()
                .get(id)
                .cloned()
                .unwrap_or_default()
        }
        /// Apply `f` to step `sid` of job `id`: 204, 404 when either is
        /// not there, and 409 when the step is terminal — the real step
        /// API's rule (e39a9d2a), which this fake used to wave through,
        /// so a close aimed at a finished triage step passed here and
        /// failed live (backlog e61093a1).
        fn with_step(
            &self,
            id: &str,
            sid: &str,
            f: impl FnOnce(&mut Value),
        ) -> axum::http::StatusCode {
            let mut jobs = self.jobs.lock().unwrap();
            let step = jobs.get_mut(id).and_then(|j| {
                j["steps"]
                    .as_array_mut()?
                    .iter_mut()
                    .find(|s| s["id"] == json!(sid))
            });
            match step {
                Some(step) if step_done(Some(step)) => axum::http::StatusCode::CONFLICT,
                Some(step) => {
                    f(step);
                    axum::http::StatusCode::NO_CONTENT
                }
                None => axum::http::StatusCode::NOT_FOUND,
            }
        }
        async fn serve(&self) -> String {
            use axum::extract::{Path, Query};
            use axum::http::StatusCode;
            use axum::routing::get;
            use axum::{Json, Router};
            use std::collections::HashMap;
            let (list, create, one, merge) =
                (self.clone(), self.clone(), self.clone(), self.clone());
            let (step_merge, step_put) = (self.clone(), self.clone());
            let protocol = self.review_protocol.clone();
            let app = Router::new()
                .route("/api/workflows/ship-a-change/versions/7", get(move || { let protocol = protocol.clone(); async move { Json(protocol.lock().unwrap().clone().unwrap_or_else(|| json!({"kind":"ship-a-change","version":7,"status":"active","steps":[{"title":"review"}]}))) } }))
                .route(
                    "/api/jobs",
                    get(move |Query(q): Query<HashMap<String, String>>| {
                        let s = list.clone();
                        async move {
                            let rows: Vec<Value> = s
                                .jobs
                                .lock()
                                .unwrap()
                                .values()
                                .filter(|j| q.get("kind").is_none_or(|k| j["kind"] == k.as_str()))
                                .filter(|j| {
                                    q.get("status").is_none_or(|v| j["status"] == v.as_str())
                                })
                                .cloned()
                                .collect();
                            let total = rows.len();
                            let offset: usize =
                                q.get("offset").and_then(|o| o.parse().ok()).unwrap_or(0);
                            let page: Vec<Value> = rows.into_iter().skip(offset).collect();
                            Json(json!({"data": page, "total": total}))
                        }
                    })
                    .post(move |Json(b): Json<Value>| {
                        let s = create.clone();
                        async move {
                            let id = format!("alarm-{}", s.posts.lock().unwrap().len() + 1);
                            s.posts.lock().unwrap().push(b.clone());
                            let mut job = b;
                            job["id"] = json!(id);
                            s.put(job);
                            (StatusCode::CREATED, Json(json!({"id": id})))
                        }
                    }),
                )
                .route(
                    "/api/jobs/{id}",
                    get(move |Path(id): Path<String>| {
                        let s = one.clone();
                        async move {
                            let j = s.get(&id);
                            if j.is_null() {
                                (
                                    StatusCode::NOT_FOUND,
                                    Json(json!({"error": "job not found"})),
                                )
                            } else {
                                (StatusCode::OK, Json(j))
                            }
                        }
                    }),
                )
                .route(
                    "/api/jobs/{id}/metadata",
                    axum::routing::patch(move |Path(id): Path<String>, Json(b): Json<Value>| {
                        let s = merge.clone();
                        async move {
                            let mut jobs = s.jobs.lock().unwrap();
                            if let Some(j) = jobs.get_mut(&id)
                                && let Some(patch) = b.as_object()
                            {
                                for (k, v) in patch {
                                    if v.is_null() {
                                        if let Some(md) = j["metadata"].as_object_mut() {
                                            md.remove(k);
                                        }
                                    } else {
                                        j["metadata"][k] = v.clone();
                                    }
                                }
                            }
                            StatusCode::NO_CONTENT
                        }
                    }),
                )
                // The step doors a close uses: the merge door, then the
                // status PUT (`step_completion_writes`).
                .route(
                    "/api/jobs/{id}/steps/{sid}/metadata",
                    axum::routing::patch(
                        move |Path((id, sid)): Path<(String, String)>, Json(b): Json<Value>| {
                            let s = step_merge.clone();
                            async move {
                                s.with_step(&id, &sid, |step| {
                                    for (k, v) in b.as_object().cloned().unwrap_or_default() {
                                        step["metadata"][k] = v;
                                    }
                                })
                            }
                        },
                    ),
                )
                .route(
                    "/api/jobs/{id}/steps/{sid}",
                    axum::routing::put(
                        move |Path((id, sid)): Path<(String, String)>, Json(b): Json<Value>| {
                            let s = step_put.clone();
                            async move {
                                s.with_step(&id, &sid, |step| {
                                    step["status"] = b["status"].clone();
                                })
                            }
                        },
                    ),
                );
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let addr = listener.local_addr().unwrap();
            tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
            format!("http://{addr}")
        }
    }

    /// A parked car at review on `branch`, with `md` merged in.
    fn parked_at_review(id: &str, branch: &str, md: Value) -> Value {
        let mut metadata = json!({"branch": branch});
        for (k, v) in md.as_object().cloned().unwrap_or_default() {
            metadata[k] = v;
        }
        json!({"id": id, "kind": "ship-a-change", "workflow_version":7, "status": "open", "metadata": metadata,
               "steps": [{"id": format!("{id}-rev"), "spec_slug": "review",
                          "title": "Open for review", "status": "ready", "metadata": {}}]})
    }

    #[tokio::test]
    async fn boarding_cannot_downgrade_the_pinned_protocol_through_projection() {
        let (_guard, clone) = clone_fixture("dock-immutable-executor-pin");
        let head = park_car(&clone, "feat/immutable-pin", "docs/immutable-pin.md");
        let mut car = parked_at_review("car-immutable-pin", "feat/immutable-pin", json!({}));
        car["steps"][0]["metadata"]["agent_executor_provenance"] = json!("advisory");
        let sor = Sor::default();
        *sor.review_protocol.lock().unwrap() = Some(
            json!({"kind":"ship-a-change","version":7,"status":"active","steps":[{"title":"review","agent":{"profile":"reviewer","model":"gpt-6.1-sol","budget_usd":1,"effort":"low","executor_provenance":"verified"}}]}),
        );
        let mut conductor = dock_conductor(&clone);
        conductor.cfg.jobs = sor.serve().await;
        conductor.cfg.dry = true;
        assert!(
            conductor
                .review_hold(&car, "car-immutable-pin", Some(&head))
                .await
                .is_some(),
            "Dock::Board cannot use an editable advisory projection"
        );
    }

    #[tokio::test]
    async fn verified_executor_provenance_is_required_even_when_the_diff_needs_no_ops_review() {
        let (_guard, clone) = clone_fixture("dock-verified-executor");
        let head = park_car(&clone, "feat/verified-docs", "docs/verified-notes.md");
        let mut car = parked_at_review("car-verified-docs", "feat/verified-docs", json!({}));
        car["steps"][0]["metadata"]["agent_executor_provenance"] = json!("verified");
        let sor = Sor::default();
        *sor.review_protocol.lock().unwrap() = Some(
            json!({"kind":"ship-a-change","version":7,"status":"active","steps":[{"title":"review","agent":{"profile":"reviewer","model":"gpt-6.1-sol","budget_usd":1,"effort":"low","executor_provenance":"verified"}}]}),
        );
        let mut conductor = dock_conductor(&clone);
        conductor.cfg.jobs = sor.serve().await;
        conductor.cfg.dry = true;
        assert!(
            conductor
                .review_hold(&car, "car-verified-docs", Some(&head))
                .await
                .is_some(),
            "Dock::Board cannot bypass a pinned verified executor requirement"
        );
    }

    /// THE STREAK IS THE DOCK'S OWN (MEDIUM 3 of the adversarial review
    /// of car 5eb1967e). A car the dock holds — here a branch on neither
    /// remote — is what a departure counts; a car held for its two red
    /// TRAINS is the strike rule's, already waiting on a person, and is
    /// not. (Assembly's conflicts never enter the walk's `held` at all:
    /// they are refused after it, and count `skips`.)
    #[tokio::test]
    async fn a_departure_counts_the_docks_own_holds_and_not_a_struck_car() {
        let (_g, clone) = clone_fixture("dock-left-behind");
        let sor = Sor::default();
        sor.put(parked_at_review(
            "car-missing",
            "feat/never-pushed",
            json!({}),
        ));
        sor.put(parked_at_review(
            "car-struck",
            "feat/struck",
            json!({"red_trains": 5}),
        ));
        let mut c = dock_conductor(&clone);
        c.cfg.jobs = sor.serve().await;
        let pass = c.candidates().await.expect("the walk reads the dock");
        assert_eq!(
            pass.held,
            vec!["car-missing".to_string()],
            "{:?}",
            pass.held
        );
        c.count_left_behind(&pass.held).await;
        assert_eq!(sor.get("car-missing")["metadata"][LEFT_BEHIND_TRAINS], 1);
        assert!(
            sor.get("car-struck")["metadata"]
                .get(LEFT_BEHIND_TRAINS)
                .is_none(),
            "the struck car is not the dock's hold"
        );
        assert!(
            sor.get("car-missing")["metadata"].get("skips").is_none(),
            "assembly's counter is not touched"
        );
    }

    /// THE CONDUCTOR RE-JUDGES EVERY CAR'S DIFF WHERE IT BOARDS (backlog
    /// b7b02024 car 1, design 7cedfa29 D1), end to end on a real clone:
    /// a car that reached the dock unheld and touches the ops runner is
    /// HELD on its review step — the marker every dock reader reads, and
    /// the head it judged beside it — rather than skipped in silence; a
    /// car released at the head it was held at boards; a release at
    /// another head is judged again; and a docs car boards, the control
    /// that shows the walk reached boarding at all. Car 2 of the design:
    /// the release boards only when it is at the fork's head AND the
    /// review run it names recorded RELEASE there (read in one GET); a
    /// run whose verdict was CHANGES, or a review hold cleared bare, is
    /// held again.
    #[tokio::test]
    async fn the_dock_holds_an_unheld_car_whose_diff_touches_an_ops_verb() {
        use crate::review_verdict::{RELEASE, release_record, review_record};
        let (_g, clone) = clone_fixture("dock-rejudge");
        let ops = park_car(&clone, "feat/ops", "infra/ops/ops-runner.sh");
        let released = park_car(&clone, "feat/released", "infra/ops/released.sh");
        let moved = park_car(&clone, "feat/moved", "infra/ops/moved.sh");
        let bare = park_car(&clone, "feat/bare", "infra/ops/bare.sh");
        let changes = park_car(&clone, "feat/changes", "infra/ops/changes.sh");
        park_car(&clone, "feat/docs", "docs/notes.md");
        let sor = Sor::default();
        let at = |id: &str, branch: &str, review_md: Value| {
            let mut car = parked_at_review(id, branch, json!({}));
            car["steps"][0]["metadata"] = review_md;
            car
        };
        // Two reviewer runs: one recorded RELEASE at feat/released's
        // head, the other CHANGES at feat/changes's.
        let run = |id: &str, car: &Value, head: &str, verdict: &str| {
            json!({"id": id, "kind": "agent-run", "status": "open", "metadata": {
                crate::review_verdict::REVIEW_KEY: review_record(car, head, verdict, "", "t")}})
        };
        let release_by = |review: &str, head: &str| {
            json!({ boss_jobs::car::HOLD_SHA: head,
                    boss_jobs::car::RELEASE: release_record(head, review, "emp-david", "t", head) })
        };
        let car_released = at(
            "car-released",
            "feat/released",
            release_by("run-rel", &released),
        );
        let car_changes = at(
            "car-changes",
            "feat/changes",
            release_by("run-chg", &changes),
        );
        sor.put(run("run-rel", &car_released, &released, RELEASE));
        sor.put(run("run-chg", &car_changes, &changes, "changes"));
        sor.put(car_released);
        sor.put(car_changes);
        sor.put(at("car-ops", "feat/ops", json!({})));
        sor.put(at(
            "car-moved",
            "feat/moved",
            release_by("run-rel", &"0".repeat(40)),
        ));
        sor.put(at(
            "car-bare",
            "feat/bare",
            json!({boss_jobs::car::HOLD_SHA: bare}),
        ));
        sor.put(at("car-docs", "feat/docs", json!({})));
        let mut c = dock_conductor(&clone);
        c.cfg.jobs = sor.serve().await;

        let pass = c.candidates().await.expect("the walk reads the dock");
        let boards: Vec<&str> = pass
            .boardable
            .iter()
            .map(|(j, _)| j["id"].as_str().unwrap_or_default())
            .collect();
        assert_eq!(boards, vec!["car-docs", "car-released"], "{boards:?}");
        assert_eq!(
            pass.judged.get("car-released"),
            Some(&released),
            "assembly is handed the head the dock judged (F4)"
        );

        for (id, head, why) in [
            ("car-ops", &ops, "touches an ops verb"),
            ("car-moved", &moved, "released at 000000000000"),
            ("car-bare", &bare, "cleared with no release"),
            ("car-changes", &changes, "not \"release\""),
        ] {
            let review = sor.get(id)["steps"][0]["metadata"].clone();
            let hold = boss_jobs::stranded::hold_reason(&review)
                .unwrap_or_else(|| panic!("{id} carries the hold every reader reads: {review}"));
            assert!(
                hold.starts_with(&format!("conductor re-judge at {}", &head[..12])),
                "{hold}"
            );
            assert!(hold.contains(why), "{id}: {why} in {hold}");
            assert_eq!(review[crate::mutating_verb::HOLD_SHA], json!(head));
            assert!(
                pass.left_behind
                    .iter()
                    .any(|l| l["car_id_short"] == json!(id8(id))
                        && l["reason"].as_str().is_some_and(|r| r == hold)),
                "the train's books name {id}: {:?}",
                pass.left_behind
            );
        }
        assert!(
            pass.held.is_empty(),
            "a review hold waits on a person, not the dock: {:?}",
            pass.held
        );
        assert!(
            !parked_ready(&sor.get("car-ops")),
            "so the next pass and the cadence count no longer see it"
        );
    }

    /// A RELEASE SURVIVES THE DOCK'S OWN REPLAY (review F3), end to end on
    /// a real clone. Two released cars, both replayed onto a main that
    /// moved: one replay carries the car's change unchanged and boards,
    /// its release recorded at the new head and still naming the head the
    /// review READ; the other's replay also changed the car's own file,
    /// and it is held for a review of the new head.
    #[tokio::test]
    async fn a_release_is_carried_over_the_docks_replay_and_nothing_else() {
        use crate::review_verdict::{RELEASE, release_record, review_record};
        let (_g, clone) = clone_fixture("dock-release-carry");
        let h1 = park_car(&clone, "feat/carry", "infra/ops/carry.sh");
        let e1 = park_car(&clone, "feat/edited", "infra/ops/edited.sh");
        land_on_main(&clone, "apps/web/src/other.ts");
        // The dock's replay of each car onto the new main.
        let replay = |branch: &str, h: &str, extra: Option<&str>| -> String {
            git_ok(&clone, &["checkout", "-q", "-B", branch, "main"]);
            git_ok(&clone, &["cherry-pick", h]);
            if let Some(path) = extra {
                std::fs::write(clone.join(path), "and more").expect("write");
                git_ok(&clone, &["commit", "-qam", "the replay changed it"]);
            }
            git_ok(&clone, &["push", "-qf", "fork", branch]);
            git_ok(&clone, &["push", "-qf", "origin", branch]);
            git_ok(&clone, &["checkout", "-q", "main"]);
            rev(&clone, branch)
        };
        let h2 = replay("feat/carry", &h1, None);
        let e2 = replay("feat/edited", &e1, Some("infra/ops/edited.sh"));
        let sor = Sor::default();
        let car = |id: &str, branch: &str, from: &str, to: &str, run: &str| {
            let mut car = parked_at_review(
                id,
                branch,
                json!({ dock_regate::BASE_REGATE: {"main": "m", "base": "b", "head": to,
                                                   "from": from} }),
            );
            car["steps"][0]["metadata"] = json!({ boss_jobs::car::HOLD_SHA: from,
                boss_jobs::car::RELEASE: release_record(from, run, "emp-david", "t", from) });
            car
        };
        let carried = car("car-carry", "feat/carry", &h1, &h2, "run-a");
        let edited = car("car-edited", "feat/edited", &e1, &e2, "run-b");
        for (run, c, h) in [("run-a", &carried, &h1), ("run-b", &edited, &e1)] {
            sor.put(
                json!({"id": run, "kind": "agent-run", "status": "open", "metadata": {
                crate::review_verdict::REVIEW_KEY: review_record(c, h, RELEASE, "", "t")}}),
            );
        }
        sor.put(carried);
        sor.put(edited);
        let mut c = dock_conductor(&clone);
        c.cfg.jobs = sor.serve().await;

        let pass = c.candidates().await.expect("the walk reads the dock");
        let boards: Vec<&str> = pass
            .boardable
            .iter()
            .map(|(j, _)| j["id"].as_str().unwrap_or_default())
            .collect();
        assert_eq!(boards, vec!["car-carry"], "{boards:?}");
        let rel = sor.get("car-carry")["steps"][0]["metadata"][boss_jobs::car::RELEASE].clone();
        assert_eq!(
            rel["sha"],
            json!(h2),
            "the release moved to the replayed head"
        );
        assert_eq!(
            rel["reviewed_sha"],
            json!(h1),
            "and names the head its review read"
        );
        assert_eq!(rel["carried_from"], json!(h1));

        let review = sor.get("car-edited")["steps"][0]["metadata"].clone();
        let hold = boss_jobs::stranded::hold_reason(&review).expect("held");
        assert!(hold.contains("its own diff changed"), "{hold}");
        assert_eq!(review[boss_jobs::car::HOLD_SHA], json!(e2));
    }

    /// WHAT RIDES IS WHAT WAS JUDGED (review F4, N3 of its re-review). The
    /// dock judges a docs car boardable; its fork branch then moves
    /// before assembly. Assembly is handed the judged head, finds the
    /// fork no longer carries it, and leaves the car for the next walk —
    /// it never merges the unjudged head.
    #[tokio::test]
    async fn a_car_whose_fork_head_moved_after_the_judge_is_left_behind() {
        let (_g, clone) = clone_fixture("dock-judged-head");
        let judged_at = park_car(&clone, "feat/docs", "docs/notes.md");
        let sor = Sor::default();
        sor.put(parked_at_review("car-docs", "feat/docs", json!({})));
        let mut c = dock_conductor(&clone);
        c.cfg.jobs = sor.serve().await;
        let pass = c.candidates().await.expect("the walk reads the dock");
        assert_eq!(pass.judged.get("car-docs"), Some(&judged_at));
        let clone_s = clone.display().to_string();
        assert_eq!(
            judged_head(&clone_s, &pass.judged, "car-docs", "feat/docs").unwrap(),
            Ok(judged_at.clone()),
            "control: unmoved, the judged head is what rides"
        );

        // The branch moves on the fork after the judge (a builder's push).
        git_ok(&clone, &["checkout", "-q", "feat/docs"]);
        std::fs::write(clone.join("docs/unjudged.md"), "an unjudged change\n").expect("write");
        git_ok(&clone, &["add", "-A"]);
        git_ok(&clone, &["commit", "-qm", "an unjudged change"]);
        git_ok(&clone, &["push", "-q", "fork", "feat/docs"]);
        git_ok(&clone, &["checkout", "-q", "main"]);
        git_ok(&clone, &["fetch", "-q", "fork"]);

        let left = judged_head(&clone_s, &pass.judged, "car-docs", "feat/docs")
            .unwrap()
            .expect_err("the moved head does not ride");
        assert!(left.contains(&judged_at[..8]), "{left}");
        assert!(left.contains("judged again next window"), "{left}");
        // Never judged at all: nothing rides.
        assert!(
            judged_head(&clone_s, &Default::default(), "car-docs", "feat/docs")
                .unwrap()
                .is_err()
        );
        // And assembly calls it before any merge.
        let src = include_str!("conductor.rs");
        let loop_at = src
            .find("for (j, branch) in cands {")
            .expect("the assembly loop");
        let body = &src[loop_at..];
        assert!(
            body.find("judged_head(clone, &judged, &jid, &branch)?")
                .expect("the guard")
                < body.find("\"merge\",").expect("the merge"),
            "assembly asks for the judged head before it merges"
        );
    }

    /// A HOLD THAT CANNOT BE WRITTEN STILL KEEPS THE CAR OFF THIS TRAIN.
    /// The dock here has no step door at all, so the write fails; the car
    /// does not board, the walk completes, and the failure rides in the
    /// train's books beside the reason.
    #[tokio::test]
    async fn a_failed_review_hold_write_still_keeps_the_car_off_the_train() {
        let (_g, clone) = clone_fixture("dock-rejudge-unwritable");
        park_car(&clone, "feat/ops", "infra/ops/ops-runner.sh");
        let merges = walk_dock_in(
            &clone,
            vec![parked_car("car-ops", json!({"branch": "feat/ops"}))],
            vec![],
        )
        .await;
        assert!(
            merges.is_empty(),
            "no job-metadata write stands in for the hold: {merges:?}"
        );
    }

    /// A DOCK OF A GARAGED CAR IS NOT SILENT (the re-review of car
    /// 5eb1967e, A), end to end. The first walk of the dock garages a car
    /// whose re-gate went red twice on an unmoved main and writes the red
    /// on it; the cadence probe then still counts that car — so the depth
    /// board fires — while a red the dock is still retrying is not
    /// counted; and that board's walk departs nothing and refuses as
    /// needs-human, which persists and so alarms (`refusal_persists`).
    /// Before the fix the probe excluded every red, the board never fired,
    /// and nothing reached the refusal at all.
    #[tokio::test]
    async fn a_dock_of_a_garaged_car_fires_a_board_that_refuses_as_needs_human() {
        let (_g, clone) = clone_fixture("dock-garaged-board");
        let main = rev(&clone, "origin/main");
        let sor = Sor::default();
        sor.put(red_run(5));
        for (id, branch, path, reds) in [
            (
                "car-garaged",
                "feat/g",
                "apps/web/src/it/yard/phone-strip.ts",
                1,
            ),
            ("car-red", "feat/r", "apps/web/src/it/yard/regions.ts", 0),
        ] {
            let head = park_car(&clone, branch, path);
            let (car, _) = replayed_car(&main, &head, reds);
            // The shape the dock leaves: the car's green receipt vouches
            // for the head it was gated at, and the fork now carries the
            // dock's replay — so the walk reads the re-gate's verdict.
            let mut md = car["metadata"].clone();
            md["branch"] = json!(branch);
            md["regate_receipt"] =
                json!(json!({"verdict": "green", "dirty": false, "head": main}).to_string());
            sor.put(parked_at_review(id, branch, md));
        }
        let mut c = dock_conductor(&clone);
        c.cfg.jobs = sor.serve().await;

        let first = c.candidates().await.expect("the refresh walks the dock");
        assert!(first.boardable.is_empty());
        assert_eq!(
            crate::cadence::probe_dock_depth(&crate::gate::machine_client().unwrap(), &c.cfg.jobs)
                .await
                .expect("the probe reads the dock"),
            1,
            "the garaged car fires the board; the red still retrying does not"
        );

        let board = c.candidates().await.expect("the board walks the dock");
        assert!(board.boardable.is_empty(), "nothing departs");
        let refusal = empty_dock_refusal(&board.left_behind);
        assert_eq!(
            refusal,
            NoDeparture::HeldOnEdges {
                cars: "car-gara".into(),
                needs_human: "car-gara".into(),
            }
        );
        assert!(refusal_persists(&refusal), "so the stall alarm is reached");
    }

    /// A GARAGED CAR'S RETRY IS AT BUILDER PRIORITY (the re-review of car
    /// 5eb1967e, B). Its once-per-main-move re-gate goes through the same
    /// launch as any dock re-gate, which would claim the next bay ahead of
    /// every builder (`waiting`). For a car whose red is likely its own,
    /// that is not owed: the retry still happens, and waits its turn like
    /// a builder's gate.
    #[test]
    fn a_garaged_cars_retry_claims_no_bay() {
        let launched = DockHold {
            reason: "re-gating on current main".into(),
            stamp: Some(json!({"main": "m2"})),
            waiting: Some("m2".into()),
        };
        let garaged = retry_hold(launched.clone(), 1);
        assert_eq!(garaged.waiting, None, "no bay is claimed ahead of builders");
        assert_eq!(
            garaged.stamp, launched.stamp,
            "the launch is still recorded"
        );
        assert_eq!(garaged.reason, launched.reason);
        assert_eq!(
            retry_hold(launched.clone(), 0),
            launched,
            "a first red's retry keeps its claim: its red may be main's"
        );
    }

    /// THE ALARM IS FILED ONCE PER STREAK, across departures and across
    /// a lost stamp (LOW 7): the third departure files it and stamps the
    /// car; the fourth sees the stamp; and a car whose stamp never landed
    /// finds its alarm already open and adopts it instead of filing twice.
    #[tokio::test]
    async fn the_left_behind_alarm_is_filed_once_across_two_departures() {
        let (_g, clone) = clone_fixture("dock-left-alarm");
        let sor = Sor::default();
        sor.put(parked_at_review(
            "car-a",
            "feat/a",
            json!({LEFT_BEHIND_TRAINS: 2,
            "skip_reason": "branch feat/a is on neither remote"}),
        ));
        let mut c = dock_conductor(&clone);
        c.cfg.jobs = sor.serve().await;
        c.count_left_behind(&["car-a".to_string()]).await;
        assert_eq!(
            sor.posts.lock().unwrap().len(),
            1,
            "the third departure files it"
        );
        let alarm = sor.posts.lock().unwrap()[0].clone();
        assert_eq!(alarm["metadata"]["left_behind_car"], "car-a");
        assert_eq!(sor.get("car-a")["metadata"][LEFT_BEHIND_ALARM], "alarm-1");
        c.count_left_behind(&["car-a".to_string()]).await;
        assert_eq!(sor.posts.lock().unwrap().len(), 1, "the fourth does not");
        assert_eq!(sor.get("car-a")["metadata"][LEFT_BEHIND_TRAINS], 4);

        // The stamp lost (a failed write, or a hand edit): the open alarm
        // for this car is found and adopted, never twinned.
        let mut lost = sor.get("car-a");
        lost["metadata"][LEFT_BEHIND_ALARM] = Value::Null;
        sor.put(lost);
        c.count_left_behind(&["car-a".to_string()]).await;
        assert_eq!(sor.posts.lock().unwrap().len(), 1, "adopted, not twinned");
        assert_eq!(sor.get("car-a")["metadata"][LEFT_BEHIND_ALARM], "alarm-1");
    }

    /// ITEM 6 OF BACKLOG 7919fdcc, end to end: the reconcile pass closes a
    /// left-behind alarm whose car has boarded — on its `stale` terminal,
    /// marked as the machine's clear — and leaves the alarm of a car still
    /// being left behind exactly as it is. Before this, boarding took the
    /// streak and the alarm's id off the car and the alarm stayed open,
    /// urgent, naming a car that had left.
    #[tokio::test]
    async fn a_left_behind_alarm_closes_itself_once_its_car_boards() {
        let sor = Sor::default();
        for (alarm, car) in [("alarm-1", "car-a"), ("alarm-2", "car-b")] {
            sor.put(json!({
                "id": alarm, "kind": "backlog-item", "status": "open",
                "metadata": {"left_behind_car": car, "left_behind_branch": "feat/x"},
                "steps": [{"id": format!("{alarm}-triage"), "spec_slug": "triage",
                           "title": "Measure the claim, choose a route",
                           "status": "ready", "metadata": {}}],
            }));
        }
        sor.put(
            json!({"id": "car-a", "kind": "ship-a-change", "status": "open",
                       "metadata": {"branch": "feat/a", "train": "train-1"}}),
        );
        sor.put(
            json!({"id": "car-b", "kind": "ship-a-change", "status": "open",
                       "metadata": {"branch": "feat/b", LEFT_BEHIND_TRAINS: 3,
                                    LEFT_BEHIND_ALARM: "alarm-2"}}),
        );
        settle_conductor(sor.serve().await)
            .alarm_stranded_greens(Utc::now())
            .await
            .expect("the pass runs");

        let closed = sor.get("alarm-1")["steps"][0].clone();
        assert_eq!(closed["status"], "completed", "{closed}");
        assert_eq!(closed["metadata"]["disposition"], "stale");
        assert_eq!(closed["metadata"]["cleared_by"], LEFT_BEHIND_CLEARED_BY);
        assert!(
            closed["metadata"]["evidence"]
                .as_str()
                .is_some_and(|e| e.contains("feat/a") && e.contains("boarded")),
            "{closed}"
        );
        let standing = sor.get("alarm-2")["steps"][0].clone();
        assert_eq!(standing["status"], "ready", "still left behind: {standing}");
        assert!(sor.posts.lock().unwrap().is_empty(), "nothing is filed");
    }

    /// File a LEFT BEHIND alarm for `car` through the real body, on the
    /// real jobs router, and return its id.
    async fn file_left_behind_alarm(c: &Conductor, car: &Value) -> String {
        let made = c
            .api(
                Method::POST,
                "/api/jobs",
                Some(left_behind_alarm_body(
                    car,
                    LEFT_BEHIND_ALARM_TRAINS,
                    Utc::now(),
                    "emp-bootstrap-admin",
                )),
            )
            .await
            .expect("the alarm is filed")
            .expect("with a body");
        made["id"].as_str().expect("an id").to_string()
    }

    /// Every open backlog-item, as the reconcile pass reads them.
    async fn open_backlog_items(c: &Conductor) -> Vec<Value> {
        let page = c
            .api(
                Method::GET,
                "/api/jobs?kind=backlog-item&status=open&limit=100",
                None,
            )
            .await
            .expect("the list reads")
            .expect("with a body");
        assert_eq!(
            page["total"].as_u64(),
            page["data"].as_array().map(|d| d.len() as u64),
            "one page holds them all: {page:#}"
        );
        page["data"].as_array().cloned().unwrap_or_default()
    }

    /// BACKLOG e61093a1, against the REAL jobs router and the real
    /// backlog-item row. Measured 2026-09-28 07:10Z: the conductor found
    /// alarms 11ea6b8b and 80b961bf ended (their cars had left the dock)
    /// and could not close either — both had been triaged to `build`
    /// hours earlier, and the close PATCHed the COMPLETED triage step,
    /// which the step API refuses 409 as terminal. So an alarm closes at
    /// the step its packet is waiting on: `triage` while it is open, a
    /// READY `build` with `disposition = stale` once a route opened it
    /// (the row's `stale` terminal reads both), and an ACTIVE `build` —
    /// an executor holds it — is told once on the packet rather than
    /// completed from under its executor. Every close carries the
    /// machine's `cleared_by`, and a closed alarm drops out of the open
    /// list the next pass judges, so nothing retries it.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_left_behind_alarm_closes_on_its_own_terminal_wherever_its_route_stands() {
        let c = settle_conductor(crate::car_unland::tests::serve().await);
        let car = |id: &str| {
            json!({"id": id, "kind": "ship-a-change", "status": "open",
                   "metadata": {"branch": format!("feat/{id}"),
                                "skip_reason": "main moved into this car's files"}})
        };
        let (at_triage, at_build, held) = (car("car-t"), car("car-b"), car("car-h"));
        let alarm_t = file_left_behind_alarm(&c, &at_triage).await;
        let alarm_b = file_left_behind_alarm(&c, &at_build).await;
        let alarm_h = file_left_behind_alarm(&c, &held).await;

        // Two of them routed to `build` by a person, as 11ea6b8b was.
        for alarm in [&alarm_b, &alarm_h] {
            let job = c.get_job(alarm).await.unwrap();
            c.complete_step(
                &job,
                find_step(&job, "triage", ""),
                &[
                    ("disposition", Some("build".to_string())),
                    ("evidence", Some("measured: the car is held".to_string())),
                ],
            )
            .await
            .expect("triaged to build");
        }
        // And one of those two claimed by an executor.
        let job = c.get_job(&alarm_h).await.unwrap();
        let build = find_step(&job, "build", "").expect("a build step");
        assert_eq!(build["status"], "ready", "{job:#}");
        let bid = build["id"].as_str().unwrap().to_string();
        c.api(
            Method::POST,
            &format!("/api/jobs/{alarm_h}/steps/{bid}/claim"),
            Some(json!({})),
        )
        .await
        .expect("the executor claims the build");

        // Every car has left the dock: every alarm's claim has ended.
        let gone: Vec<Value> = [&at_triage, &at_build, &held]
            .into_iter()
            .map(|c| {
                let mut c = c.clone();
                c["status"] = json!("closed");
                c
            })
            .collect();
        let open = open_backlog_items(&c).await;
        assert_eq!(open.len(), 3, "{open:#?}");
        c.clear_left_behind_alarms(&open, &gone).await;

        // Which terminal the packet reached. An `outcome` step is a
        // marker: it goes READY here and the dispatcher's
        // `complete-marker-on-step-ready` completes it live, closing the
        // packet — this router has no dispatcher, so READY is the read.
        let reached = |job: &Value| -> Vec<String> {
            ["stale", "closed", "duplicate", "declined"]
                .into_iter()
                .filter(|s| find_step(job, s, "").is_some_and(|st| st["status"] == "ready"))
                .map(str::to_string)
                .collect()
        };

        // AT TRIAGE: closed `stale` through the triage step.
        let t = c.get_job(&alarm_t).await.unwrap();
        assert_eq!(reached(&t), vec!["stale"], "{t:#}");
        let triage = find_step(&t, "triage", "").unwrap();
        assert_eq!(triage["metadata"]["disposition"], "stale");
        assert_eq!(triage["metadata"]["cleared_by"], LEFT_BEHIND_CLEARED_BY);

        // AT BUILD: closed `stale` through the build step; the person's
        // route on the triage step is left exactly as they recorded it.
        let b = c.get_job(&alarm_b).await.unwrap();
        assert_eq!(reached(&b), vec!["stale"], "{b:#}");
        let build = find_step(&b, "build", "").unwrap();
        assert_eq!(build["status"], "completed", "{build:#}");
        assert_eq!(build["metadata"]["disposition"], "stale");
        assert_eq!(build["metadata"]["cleared_by"], LEFT_BEHIND_CLEARED_BY);
        assert!(
            build["metadata"]["evidence"]
                .as_str()
                .is_some_and(|e| e.contains("feat/car-b") && e.contains("left the dock")),
            "{build:#}"
        );
        let triage = find_step(&b, "triage", "").unwrap();
        assert_eq!(triage["metadata"]["disposition"], "build");
        assert!(triage["metadata"].get("cleared_by").is_none(), "{triage:#}");

        // HELD BY AN EXECUTOR: open, told once on the packet.
        let h = c.get_job(&alarm_h).await.unwrap();
        assert_eq!(h["status"], "open", "{h:#}");
        assert!(reached(&h).is_empty(), "{h:#}");
        assert_eq!(
            find_step(&h, "build", "").unwrap()["status"],
            "active",
            "never completed from under its executor"
        );
        assert_eq!(h["metadata"]["recovered_by"], LEFT_BEHIND_CLEARED_BY);
        let told_at = h["metadata"]["recovered_at"].clone();
        assert!(
            h["metadata"]["recovery"]
                .as_str()
                .is_some_and(|r| r.contains("active")),
            "{h:#}"
        );

        // The marker rule's half, done here as it is live: the ready
        // `stale` terminal completes, and the packet CLOSES on it.
        for alarm in [&alarm_t, &alarm_b] {
            let job = c.get_job(alarm).await.unwrap();
            c.complete_step(&job, find_step(&job, "stale", ""), &[])
                .await
                .expect("the marker completes");
            let job = c.get_job(alarm).await.unwrap();
            assert_eq!(job["status"], "closed", "{job:#}");
            assert_eq!(job["metadata"]["outcome"], "stale", "{job:#}");
        }

        // THE NEXT PASS: the two closed alarms are no longer open, so no
        // pass judges them again; the held one is not told twice.
        let open = open_backlog_items(&c).await;
        let still: Vec<String> = left_behind_alarms_to_clear(&open, &gone)
            .into_iter()
            .map(|(id, _, _)| id)
            .collect();
        assert_eq!(still, vec![alarm_h.clone()]);
        c.clear_left_behind_alarms(&open, &gone).await;
        let h = c.get_job(&alarm_h).await.unwrap();
        assert_eq!(h["metadata"]["recovered_at"], told_at, "told once");
    }

    /// A red on a main that never moves is re-gated at the SAME head once
    /// its bound passes (twice the 15-minute hold): here the gate cannot
    /// be filed, so the stamp records the launch owed — the head kept, no
    /// gate-run, the red count carried and the red itself cleared — and
    /// the next pass files it.
    #[tokio::test]
    async fn a_red_past_its_bound_on_a_still_main_is_regated_at_the_same_head() {
        let (_g, clone) = clone_fixture("dock-red-bound");
        let head = park_car(&clone, "feat/g", "apps/web/src/it/yard/phone-strip.ts");
        let main = rev(&clone, "origin/main");
        let (car, stamp) = replayed_car(&main, &head, 0);
        let mut c = dock_conductor(&clone);
        c.cfg.jobs = red_regate_api(red_run(45)).await;
        let hold = c.regate_in_flight(&car, "car-g-000", "feat/g", stamp).await;
        let written = hold.stamp.expect("the relaunch is stamped");
        assert_eq!(written["head"], head);
        assert_eq!(written["gate_run"], "");
        assert_eq!(written["reds"], 1);
        assert!(written.get(boss_jobs::dock_red::RED).is_none(), "{written}");
        assert!(
            hold.reason.contains("could not be filed"),
            "{}",
            hold.reason
        );
    }

    // -- the merge-lost arm, end to end (backlog f9256445) --------------
    //
    // The real jobs router with the real platform bundle, a real forge
    // whose main lost the train's merge, and the conductor's own reads
    // and writes: what the arm did is read back off the packets.

    /// The forge as the arm reads it: the PR merged, with the merge's
    /// full sha and the forge's merged_at. Nothing else is exercised.
    struct LostMergeForge {
        oid: String,
    }

    #[async_trait]
    impl Forge for LostMergeForge {
        async fn pr_info(&self, _url: &str) -> Result<Value> {
            Ok(json!({
                "state": "MERGED",
                "mergeCommit": {"oid": self.oid},
                "mergedAt": "2026-09-25T20:10:41Z",
                "statusCheckRollup": [],
            }))
        }
        async fn pr_create(&self, _: &str, _: &str, _: &str, _: &str) -> Result<String> {
            bail!("not exercised")
        }
        async fn merge(&self, _url: &str, _message: &str) -> Result<()> {
            bail!("not exercised")
        }
        async fn close_pr(&self, _url: &str) -> Result<()> {
            bail!("not exercised")
        }
        async fn delete_branch(&self, _branch: &str) -> Result<bool> {
            bail!("not exercised")
        }
        async fn branch_head(&self, _branch: &str) -> Result<Option<String>> {
            bail!("not exercised")
        }
        async fn cancel_ci_runs(&self, _pr_index: &str, _head_sha: &str) -> Result<usize> {
            bail!("not exercised")
        }
    }

    /// Train 2026-09-25 20:04's shape: one car landed as the merge, the
    /// train through `deployed`, and forge main rewound to the commit
    /// before the merge. The first pass records one NOT reading and ends
    /// nothing; the second ends the train on `merge-lost` with its four
    /// fields, unlands the car (a successor at `gate`), and files the one
    /// item — the train no longer waits at `converged` for ever.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_train_whose_merge_main_lost_ends_merge_lost_on_the_second_reading() {
        use crate::car_unland::tests as fx;
        let (clone, base, merge) = fx::forge_that_lost_a_merge("merge-lost-arm");
        let work = clone.parent().unwrap().join("work");
        fx::git_in(
            &work,
            &[
                "push",
                "-q",
                "-f",
                "origin",
                &format!("{base}:refs/heads/main"),
            ],
        );
        let mut c = cleanup_conductor("forgejo", Box::new(LostMergeForge { oid: merge.clone() }));
        c.cfg.jobs = fx::serve().await;
        c.cfg.clone = clone.display().to_string();

        let car = fx::landed(&c, "fix/a-stamp-cannot-land", &merge).await;
        let made = c
            .api(
                Method::POST,
                "/api/jobs",
                Some(json!({
                    "kind": "pr-train",
                    "subject": {"subject_kind": "custom", "id": "train/20260925-2004"},
                    "title": "PR train 2026-09-25 20:04",
                    "owner_id": "emp-bootstrap-admin",
                    "priority": "standard",
                    "status": "open",
                    "tags": [],
                    "metadata": {"boarded_jobs": [car]},
                })),
            )
            .await
            .unwrap()
            .unwrap();
        let tid = made["id"].as_str().unwrap().to_string();
        for (slug, key, value) in [
            ("collect", "boarded", "1".to_string()),
            (
                "assemble",
                "train_ref",
                "train/20260925-2004@c85941b4".to_string(),
            ),
            ("pr", "pr_url", "http://forge/boss/pulls/687".to_string()),
            ("merged", "merge_ref", merge[..12].to_string()),
            (
                "deployed",
                "deployed",
                NO_PLAYGROUND_DEPLOY_EVIDENCE.to_string(),
            ),
        ] {
            let t = c.get_job(&tid).await.unwrap();
            c.complete_step(&t, find_step(&t, slug, ""), &[(key, Some(value))])
                .await
                .unwrap();
        }

        // PASS ONE: one NOT reading, recorded; the train stands.
        let t = c.get_job(&tid).await.unwrap();
        assert_eq!(find_step(&t, "converged", "").unwrap()["status"], "ready");
        c.verify_convergence(&t, crate::car_unland::tests::now())
            .await
            .unwrap();
        let t = c.get_job(&tid).await.unwrap();
        assert_eq!(t["status"], "open", "one reading ends nothing: {t:#}");
        let first = first_lost_reading(&t).expect("the first reading rides the train");
        assert_eq!(first["main"], base.as_str());

        // PASS TWO: the second NOT reading ends it.
        c.verify_convergence(&t, crate::car_unland::tests::now())
            .await
            .unwrap();
        let t = c.get_job(&tid).await.unwrap();
        assert_eq!(t["status"], "closed", "{t:#}");
        assert_eq!(t["metadata"]["outcome"], MERGE_LOST_SLUG);
        let lost = find_step(&t, MERGE_LOST_SLUG, "").unwrap();
        assert_eq!(lost["metadata"]["merge_ref"], &merge[..12]);
        assert_eq!(lost["metadata"]["main_at_read"], base.as_str());
        let evidence = lost["metadata"]["evidence"].as_str().unwrap();
        assert!(evidence.contains("two readings"), "{evidence}");
        assert!(evidence.contains("2026-09-25T20:10:41Z"), "{evidence}");
        assert_eq!(
            find_step(&t, "cancelled", "").unwrap()["status"],
            "skipped",
            "never recorded as closed unmerged"
        );

        // The car rides again; the one item is filed and named.
        let car_after = c.get_job(&car).await.unwrap();
        assert_eq!(
            car_after["metadata"]["outcome"], "unlanded",
            "{car_after:#}"
        );
        let successor = car_after["metadata"]["superseded_by"].as_str().unwrap();
        let next = c.get_job(successor).await.unwrap();
        assert_eq!(find_step(&next, "gate", "").unwrap()["status"], "ready");
        assert_eq!(t["metadata"][MERGE_LOST_FOLLOWUP], "done");
        let item = c
            .get_job(t["metadata"][MERGE_LOST_ITEM].as_str().unwrap())
            .await
            .unwrap();
        assert_eq!(item["kind"], "backlog-item");
        assert_eq!(item["metadata"]["input_channel"], "pipeline-failure");
        assert!(
            item["metadata"]["description"]
                .as_str()
                .unwrap()
                .contains(&format!("car {} unlanded", id8(&car))),
            "{item:#}"
        );
    }
}
