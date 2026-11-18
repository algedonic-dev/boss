//! THE BAY IS TAKEN BEFORE THE JOB EXISTS (backlog ac8d3157).
//!
//! Admission at the gate bound was a READ followed by a CREATE: count the
//! live gate Jobs, and if the count was under the bound, create one. Two
//! launchers that read the same free bay both started. Measured over three
//! days to 2026-10-17 (policy v7-v9, bound 3): 147 car launches broke the
//! bound four times, about 120 minutes at 4 and 8 at 5 — two cars started
//! in the same second at 2026-10-15 13:01:51, and cars b1e25027 and
//! 541a0b79 one second apart at 2026-10-17 06:02. No train was involved.
//!
//! A bay is now a NAMED object, `gate-bay-<i>` for `i` below the bound: a
//! suspended, podless Job carrying the packet it was taken for. The API
//! server refuses a second object of one name, so two launchers that pick
//! the same free bay cannot both have it — one CREATE answers
//! AlreadyExists and that launcher reads again. Only the holder of a bay
//! creates its gate Job, and only after re-reading that the bay is still
//! its own and the bound still holds. The claim is a Job because the
//! launchers' Role (`dev-session-gates`) already creates, lists and
//! deletes Jobs in the gate namespace; nothing is widened for it.
//!
//! A BAY FREES ONLY ON PROOF THE GATE ENDED: the gate Job carrying the
//! claim's packet exists and is terminal ([`job_live`]), or no gate Job
//! for it was ever made and the claim is older than [`PENDING_GRACE_SECS`]
//! — ten times past any create that could still land, because the create
//! follows the claim by one conditional write and is itself bounded to 45
//! seconds. An unreadable census, a claim with no packet or no creation
//! time, or a Job still counting active or terminating pods keeps the bay.
//! A free bay is retaken by deleting its claim with a UID precondition,
//! so a launcher holding a stale reading can never delete the bay someone
//! else just took.
//!
//! Every gate Job still counts against the bound whether or not it holds a
//! claim, so a launcher built before this car — or a gate created by hand —
//! is still seen; it is the one way past the claim, and it is outside it.

use super::*;

/// The label every bay claim carries. Gate readers select `app=gate-runner`
/// and so never count a claim as a gate.
///
/// This line and `CLAIM_PREFIX`'s are READ AS TEXT by
/// infra/cluster/undeclared-objects.sh, which declares every live Job
/// carrying this label and a `<prefix><index>` name from them (backlog
/// 7b3b1263: the claims read as orphans on every dev-pod pre-flight). Keep
/// each a single `pub(crate) const NAME: &str = "...";` line; any other
/// shape makes that derivation refuse to answer rather than guess.
pub(crate) const CLAIM_APP: &str = "gate-bay-claim";
/// A bay's object name is this prefix and its index.
pub(crate) const CLAIM_PREFIX: &str = "gate-bay-";
/// The label naming the gate-run a claim, a gate Job, holds the bay for.
const PACKET_LABEL: &str = "boss.dev/packet";
/// How long a claim whose gate Job was never made keeps its bay.
pub(crate) const PENDING_GRACE_SECS: i64 = 600;

/// No bay could be taken: the bound is reached, or another launcher took
/// the last one first. A PLACE, NOT A FAULT — `boss gate --wait` takes its
/// place in the line again and the conductor keeps its train first.
#[derive(Debug)]
pub(crate) struct NoGateBay(pub(crate) String);

impl std::fmt::Display for NoGateBay {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "no gate bay is free: {}", self.0)
    }
}

impl std::error::Error for NoGateBay {}

/// The three reads a decision is taken from. Each is a complete list.
#[derive(Debug, Clone, Default)]
pub(super) struct Census {
    pub(super) gates: Vec<Value>,
    pub(super) pods: Vec<Value>,
    pub(super) claims: Vec<Value>,
}

/// PURE: the Job's terminal condition — `Complete` or `Failed`, status
/// `True` — when the cluster has set one. Read by [`job_live`] here and,
/// through it, by the conductor's recorder (`train::gate_jobs_from_json`),
/// which also takes its reason and time from it: ONE reading of a gate
/// Job's end, where the recorder's pod counts alone left a Job that failed
/// before any pod existed (`failed: 0`) running until its TTL (backlog
/// db6bdb4f).
pub(crate) fn terminal_condition(job: &Value) -> Option<&Value> {
    job.pointer("/status/conditions")
        .and_then(Value::as_array)?
        .iter()
        .find(|condition| {
            condition.get("status").and_then(Value::as_str) == Some("True")
                && matches!(
                    condition.get("type").and_then(Value::as_str),
                    Some("Complete" | "Failed")
                )
        })
}

/// PURE: whether a Job still holds its bay. Only a terminal condition
/// ends it, and only once no pod is active or still terminating: pod
/// counters do not terminate a Job, and retries can still create a runner.
pub(crate) fn job_live(job: &Value) -> bool {
    let count = |key: &str| {
        job.pointer(&format!("/status/{key}"))
            .and_then(Value::as_u64)
            .unwrap_or(0)
    };
    count("active") > 0 || count("terminating") > 0 || terminal_condition(job).is_none()
}

fn pod_live(pod: &Value) -> bool {
    !matches!(
        pod.pointer("/status/phase").and_then(Value::as_str),
        Some("Succeeded" | "Failed")
    )
}

fn name_of(row: &Value) -> Option<&str> {
    row.pointer("/metadata/name")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
}

fn packet_of(row: &Value) -> Option<&str> {
    row.pointer("/metadata/labels")
        .and_then(|labels| labels.get(PACKET_LABEL))
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
}

fn uid_of(row: &Value) -> Option<&str> {
    row.pointer("/metadata/uid")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
}

/// What a claim is doing with its bay.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum ClaimState {
    /// Its gate Job is live: the Job is what counts against the bound.
    Running,
    /// No gate Job for it yet, inside the grace — or a claim this reader
    /// cannot judge. Counts against the bound itself.
    Pending,
    /// Its gate ended, with proof. The bay may be retaken.
    Free,
}

/// PURE: a claim's standing, judged off the census.
pub(super) fn claim_state(
    claim: &Value,
    census: &Census,
    now: chrono::DateTime<chrono::Utc>,
) -> ClaimState {
    let Some(packet) = packet_of(claim) else {
        return ClaimState::Pending;
    };
    let gates: Vec<&Value> = census
        .gates
        .iter()
        .filter(|g| packet_of(g) == Some(packet))
        .collect();
    if !gates.is_empty() {
        return if gates.iter().any(|g| job_live(g)) {
            ClaimState::Running
        } else {
            ClaimState::Free
        };
    }
    let created = claim
        .pointer("/metadata/creationTimestamp")
        .and_then(Value::as_str)
        .and_then(|s| chrono::DateTime::parse_from_rfc3339(s).ok())
        .map(|t| t.with_timezone(&chrono::Utc));
    match created {
        Some(at) if (now - at).num_seconds() >= PENDING_GRACE_SECS => ClaimState::Free,
        _ => ClaimState::Pending,
    }
}

/// PURE: everything holding a bay, each named, so a refusal can say who.
/// Every live gate Job counts, claimed or not; a live runner pod whose Job
/// is gone counts; a claim whose gate Job does not exist yet counts.
pub(super) fn occupants(census: &Census, now: chrono::DateTime<chrono::Utc>) -> Vec<String> {
    let mut held: Vec<String> = census
        .gates
        .iter()
        .filter(|g| job_live(g))
        .map(|g| name_of(g).unwrap_or("an unnamed gate Job").to_string())
        .collect();
    let jobs: std::collections::BTreeSet<&str> = census.gates.iter().filter_map(name_of).collect();
    let orphans: std::collections::BTreeSet<&str> = census
        .pods
        .iter()
        .filter(|p| pod_live(p))
        .filter_map(|p| {
            p.pointer("/metadata/labels/job-name")
                .and_then(Value::as_str)
                .or_else(|| name_of(p))
        })
        .filter(|job| !jobs.contains(job))
        .collect();
    held.extend(
        orphans
            .into_iter()
            .map(|job| format!("a runner pod of {job}")),
    );
    held.extend(
        census
            .claims
            .iter()
            .filter(|c| claim_state(c, census, now) == ClaimState::Pending)
            .map(|c| {
                format!(
                    "{} for {}",
                    name_of(c).unwrap_or("an unnamed claim"),
                    packet_of(c).map_or("no packet", |p| &p[..8.min(p.len())])
                )
            }),
    );
    held
}

pub(super) fn bay_name(index: usize) -> String {
    format!("{CLAIM_PREFIX}{index}")
}

/// What a reading allows.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum Plan {
    /// Take bay `index`, first deleting the free claim named, by its UID.
    Take {
        index: usize,
        replace: Option<(String, String)>,
    },
    /// Nothing to take, and why.
    Full(String),
}

/// PURE: the lowest bay this reading may take, under `max`.
pub(super) fn plan(census: &Census, max: usize, now: chrono::DateTime<chrono::Utc>) -> Plan {
    let held = occupants(census, now);
    if held.len() >= max {
        return Plan::Full(format!(
            "{} of {max} in use ({})",
            held.len(),
            held.join(", ")
        ));
    }
    for index in 0..max {
        let name = bay_name(index);
        match census
            .claims
            .iter()
            .find(|c| name_of(c) == Some(name.as_str()))
        {
            None => {
                return Plan::Take {
                    index,
                    replace: None,
                };
            }
            Some(claim) if claim_state(claim, census, now) == ClaimState::Free => {
                let Some(uid) = uid_of(claim) else { continue };
                return Plan::Take {
                    index,
                    replace: Some((name, uid.to_string())),
                };
            }
            Some(_) => {}
        }
    }
    Plan::Full(format!(
        "every one of the {max} bay claims is held, though only {} gate(s) run",
        held.len()
    ))
}

/// The claim object for bay `index`: a suspended Job, so it never makes a
/// pod. `image` is the gate's own, so whatever admits the gate admits it.
pub(super) fn claim_document(index: usize, packet: &str, namespace: &str, image: &str) -> Value {
    json!({
        "apiVersion": "batch/v1",
        "kind": "Job",
        "metadata": {
            "name": bay_name(index),
            "namespace": namespace,
            "labels": { "app": CLAIM_APP, PACKET_LABEL: packet },
        },
        "spec": {
            "suspend": true,
            "backoffLimit": 0,
            "template": {
                "metadata": { "labels": { "app": CLAIM_APP } },
                "spec": {
                    "restartPolicy": "Never",
                    "automountServiceAccountToken": false,
                    "containers": [{
                        "name": "claim",
                        "image": image,
                        "command": ["true"],
                        "securityContext": {
                            "allowPrivilegeEscalation": false,
                            "runAsNonRoot": true,
                            "runAsUser": 65534,
                            "capabilities": { "drop": ["ALL"] },
                            "seccompProfile": { "type": "RuntimeDefault" }
                        },
                        "resources": {
                            "requests": { "cpu": "1m", "memory": "1Mi" },
                            "limits": { "cpu": "1m", "memory": "8Mi" }
                        }
                    }]
                }
            }
        }
    })
}

/// The answer to a claim CREATE.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum Created {
    Made {
        uid: String,
    },
    /// AlreadyExists: another launcher has this bay.
    Taken,
}

/// The cluster, as a reservation needs it.
#[async_trait::async_trait]
pub(super) trait BayPort: Send + Sync {
    async fn census(&self) -> Result<Census>;
    async fn create(&self, claim: &Value) -> Result<Created>;
    /// Delete `name` only if it is still `uid`. `Ok(false)` when it is
    /// already gone or is another object now.
    async fn delete(&self, name: &str, uid: &str) -> Result<bool>;
    fn now(&self) -> chrono::DateTime<chrono::Utc>;
}

/// A bay this process holds.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Held {
    pub(super) name: String,
    pub(super) uid: String,
    /// Everything holding a bay on the re-read that confirmed this one,
    /// this claim included — what the gate-run records as the bays
    /// occupied when it took its own (backlog 3d2bc001).
    pub(super) occupants: Vec<String>,
}

/// Take a bay for `packet`, or say why none is free (`NoGateBay`). Any
/// other error is a census or a write that could not be judged, and no
/// bay is held for it beyond a claim's own grace.
///
/// Read, pick the lowest free bay, take it by NAME, then read again: the
/// bay must still be this claim (its UID) and the bound must still hold
/// with it counted. A launcher that lost the name reads again, so a race
/// costs one more read rather than a refusal while a bay is still free.
pub(super) async fn reserve(
    port: &dyn BayPort,
    packet: &str,
    namespace: &str,
    image: &str,
    max: usize,
) -> Result<Held> {
    if max == 0 {
        bail!("the gate bound is zero; no bay can be taken");
    }
    // Each lost name is one bay someone else now holds, so more than
    // `max` losses in one reservation is not a race this can win.
    for _ in 0..=max {
        let census = port.census().await?;
        let (index, replace) = match plan(&census, max, port.now()) {
            Plan::Full(why) => return Err(NoGateBay(why).into()),
            Plan::Take { index, replace } => (index, replace),
        };
        if let Some((name, uid)) = replace {
            // Gone already, or someone retook it: the CREATE says which.
            port.delete(&name, &uid).await?;
        }
        let uid = match port
            .create(&claim_document(index, packet, namespace, image))
            .await?
        {
            Created::Made { uid } => uid,
            Created::Taken => continue,
        };
        let held = Held {
            name: bay_name(index),
            uid,
            occupants: Vec::new(),
        };
        let after = match port.census().await {
            Ok(after) => after,
            Err(e) => {
                release(port, &held).await;
                return Err(e.context("re-reading the bay just taken"));
            }
        };
        let ours = after.claims.iter().any(|c| {
            name_of(c) == Some(held.name.as_str()) && uid_of(c) == Some(held.uid.as_str())
        });
        if !ours {
            bail!(
                "bay {} was taken (uid {}) but is not that claim on re-read; nothing is launched",
                held.name,
                held.uid
            );
        }
        let occupied = occupants(&after, port.now());
        if occupied.len() > max {
            release(port, &held).await;
            return Err(NoGateBay(format!(
                "{} of {max} in use once this bay was counted ({})",
                occupied.len(),
                occupied.join(", ")
            ))
            .into());
        }
        return Ok(Held {
            occupants: occupied,
            ..held
        });
    }
    Err(NoGateBay(format!(
        "another launcher took each bay first, {} times",
        max + 1
    ))
    .into())
}

/// The live port: the same kubectl door, client flags and 45-second bound
/// every native create in this module goes through. Raw REST, so a list
/// keeps the server's own completeness markers and a refusal keeps the
/// server's own reason.
pub(super) struct NativeBays<'a> {
    pub(super) doors: &'a Doors<'a>,
    pub(super) namespace: &'a str,
    /// The launch's own clock, handed down (no wallclock read here). An
    /// older `now` only makes a claim look younger, so a stale clock keeps
    /// a bay longer — never frees one early.
    pub(super) now: chrono::DateTime<chrono::Utc>,
}

impl NativeBays<'_> {
    async fn list(&self, path: &str, selector: &str) -> Result<Vec<Value>> {
        let mut url = reqwest::Url::parse("http://native.invalid")?;
        url.set_path(path);
        url.query_pairs_mut().append_pair("labelSelector", selector);
        let raw = format!("--raw={}?{}", url.path(), url.query().unwrap_or_default());
        let out = native_read(self.doors, self.namespace, &["get", &raw]).await?;
        if !out.status.success() {
            bail!(
                "bay census: GET {path} ({selector}) refused: {}",
                String::from_utf8_lossy(&out.stderr).trim()
            );
        }
        let list: Value = serde_json::from_slice(&out.stdout)
            .with_context(|| format!("bay census: {path} did not answer JSON"))?;
        let partial = list
            .pointer("/metadata/continue")
            .and_then(Value::as_str)
            .is_some_and(|c| !c.is_empty())
            || list
                .pointer("/metadata/remainingItemCount")
                .and_then(Value::as_u64)
                .is_some_and(|n| n > 0);
        if partial {
            bail!(
                "bay census: {path} ({selector}) answered a partial list; no bay is judged from it"
            );
        }
        list.get("items")
            .and_then(Value::as_array)
            .cloned()
            .with_context(|| format!("bay census: {path} ({selector}) answered no items"))
    }

    fn jobs_path(&self) -> String {
        format!("/apis/batch/v1/namespaces/{}/jobs", self.namespace)
    }

    async fn send(&self, verb: &str, raw: &str, body: &[u8]) -> Result<std::process::Output> {
        let mut command = native_command(self.doors, self.namespace);
        command.args([verb, &format!("--raw={raw}"), "-f", "-"]);
        let (out, input_error) = native_output(native_create(command, body).await)?;
        if let Some(e) = input_error {
            bail!("bay {verb}: the request body was not delivered whole ({e}); outcome UNKNOWN");
        }
        Ok(out)
    }
}

#[async_trait::async_trait]
impl BayPort for NativeBays<'_> {
    async fn census(&self) -> Result<Census> {
        let jobs = self.jobs_path();
        let pods = format!("/api/v1/namespaces/{}/pods", self.namespace);
        Ok(Census {
            gates: self.list(&jobs, "app=gate-runner").await?,
            pods: self.list(&pods, "app=gate-runner").await?,
            claims: self.list(&jobs, &format!("app={CLAIM_APP}")).await?,
        })
    }

    async fn create(&self, claim: &Value) -> Result<Created> {
        let raw = format!("{}?fieldValidation=Strict", self.jobs_path());
        let out = self
            .send("create", &raw, &serde_json::to_vec(claim)?)
            .await?;
        let stderr = String::from_utf8_lossy(&out.stderr);
        if !out.status.success() {
            if stderr.contains("(AlreadyExists)") {
                return Ok(Created::Taken);
            }
            bail!(
                "bay claim {} refused or UNKNOWN: {} — if it was made, it holds its bay for {}s",
                name_of(claim).unwrap_or("?"),
                stderr.trim(),
                PENDING_GRACE_SECS
            );
        }
        let made: Value =
            serde_json::from_slice(&out.stdout).context("bay claim CREATE did not answer JSON")?;
        let uid = uid_of(&made)
            .filter(|_| name_of(&made) == name_of(claim))
            .context("bay claim CREATE answered no uid for the name sent")?;
        Ok(Created::Made {
            uid: uid.to_string(),
        })
    }

    async fn delete(&self, name: &str, uid: &str) -> Result<bool> {
        let options = json!({
            "kind": "DeleteOptions",
            "apiVersion": "v1",
            "preconditions": { "uid": uid },
            "propagationPolicy": "Background",
        });
        let raw = format!("{}/{name}", self.jobs_path());
        let out = self
            .send("delete", &raw, &serde_json::to_vec(&options)?)
            .await?;
        let stderr = String::from_utf8_lossy(&out.stderr);
        if out.status.success() {
            Ok(true)
        } else if stderr.contains("(NotFound)") || stderr.contains("(Conflict)") {
            Ok(false)
        } else {
            bail!("bay {name} DELETE refused or UNKNOWN: {}", stderr.trim())
        }
    }

    fn now(&self) -> chrono::DateTime<chrono::Utc> {
        self.now
    }
}

/// The bound a reservation holds to: the `gates` equipment station's
/// `bays`, the TOTAL with trains in it (design 1e787fc2; the delivery
/// policy's `gate_max_concurrent` until item 0a78f8f8), tightened —
/// never loosened — by `BOSS_GATE_MAX_CONCURRENT`, the same way the
/// resident supervisor takes it.
pub(super) async fn bound_on(doors: &Doors<'_>) -> Result<Bound> {
    let gates = staging::gates_on(doors).await?;
    let env = std::env::var("BOSS_GATE_MAX_CONCURRENT").ok();
    bound_from(&gates, env.as_deref())
}

/// The bound a bay was taken under, as the gate-run records it.
///
/// WHY IT IS RECORDED (backlog d777b20a). The gate capacity moved
/// 3->4->3->4->3 between 2026-09-28 and 10-07 while green car gates went
/// from a median 6.0 to 34.6 minutes, and no gate-run said which bound it
/// had queued behind: the bound was printed on the launcher's stderr and
/// nowhere else. `max` is the bound in force; `station_bays` is the
/// gates row's own, so a launcher that tightened it by env is visible
/// too, and `station` names the row version that set it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Bound {
    pub(super) max: usize,
    station_bays: usize,
    station: String,
    version: i64,
}

impl Bound {
    /// PURE: the `gate_bay` record for the bay `name` taken under it,
    /// with `occupants` — everything holding a bay on the re-read that
    /// confirmed it, this claim included.
    ///
    /// WHY THE OCCUPANCY IS RECORDED (backlog 3d2bc001). On 2026-11-04
    /// three gate-runs read as running on two bays, and gate-bay-1 as
    /// issued twice, because a completed `Gate launched` step closes when
    /// the packet is ACCEPTED into the line, not when its Job is made: a
    /// gate waiting for a bay looked like a gate in one. The gate Jobs'
    /// own creation and end times showed two at most all day. Each
    /// gate-run now says how many bays were occupied when it took its own
    /// and by whom, so that question is one read of one packet.
    pub(super) fn record(&self, name: &str, occupants: &[String]) -> Value {
        json!({
            "bay": name,
            "bound": self.max,
            "station_bays": self.station_bays,
            "station": {"name": self.station, "version": self.version},
            "occupied": occupants.len(),
            "occupants": occupants,
        })
    }
}

/// PURE: the bound off the active `gates` station row and the raw env
/// override, which tightens it and never loosens it.
pub(super) fn bound_from(gates: &Value, env: Option<&str>) -> Result<Bound> {
    let total =
        crate::gate::bays_of(gates).context("the gates station's bays are not a positive count")?;
    Ok(Bound {
        max: crate::gate::max_concurrent_from(env, total)?.min(total),
        station_bays: total,
        station: gates
            .get("name")
            .and_then(Value::as_str)
            .context("the gates station row names no station")?
            .to_string(),
        version: gates
            .get("version")
            .and_then(Value::as_i64)
            .context("the gates station row carries no version")?,
    })
}

/// The gate's own image, so the claim is admitted by whatever admits the
/// gate. A request with none refuses: it is not a gate Job. Read from the
/// container that runs the gate, which on the judged-init layout is an
/// init container (backlog 3e2c8ddc) and not `containers/0`.
pub(super) fn gate_image(job: &Value) -> Result<String> {
    super::gate_executor(job)
        .ok()
        .and_then(|executor| job.pointer(&format!("{executor}/image")))
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .context("the gate Job names no container image; no bay is taken for it")
}

/// Take a bay through the live door, saying which.
pub(super) async fn reserve_on(
    doors: &Doors<'_>,
    namespace: &str,
    packet: &str,
    job: &Value,
    now: chrono::DateTime<chrono::Utc>,
) -> Result<(Held, Value)> {
    let image = gate_image(job)?;
    let bound = bound_on(doors).await?;
    let max = bound.max;
    let port = NativeBays {
        doors,
        namespace,
        now,
    };
    let held = reserve(&port, packet, namespace, &image, max).await?;
    eprintln!(
        "boss gate: bay {} taken for {} (bound {max}; {} occupied counting it: {}) — it frees when this gate's Job ends",
        held.name,
        &packet[..8.min(packet.len())],
        held.occupants.len(),
        held.occupants.join(", ")
    );
    let record = bound.record(&held.name, &held.occupants);
    Ok((held, record))
}

/// Give a bay back through the live door.
pub(super) async fn release_on(
    doors: &Doors<'_>,
    namespace: &str,
    held: &Held,
    now: chrono::DateTime<chrono::Utc>,
) {
    release(
        &NativeBays {
            doors,
            namespace,
            now,
        },
        held,
    )
    .await;
}

/// Give back a bay this process took and will not use. Best effort: a
/// claim left behind holds its bay only for [`PENDING_GRACE_SECS`].
pub(super) async fn release(port: &dyn BayPort, held: &Held) {
    match port.delete(&held.name, &held.uid).await {
        Ok(_) => {}
        Err(e) => eprintln!(
            "boss gate: could not give back bay {} ({e:#}) — it frees itself in {}s",
            held.name, PENDING_GRACE_SECS
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use tokio::sync::{Barrier, Mutex};

    const NOW: &str = "2026-10-17T06:02:30Z";

    fn t(s: &str) -> chrono::DateTime<chrono::Utc> {
        chrono::DateTime::parse_from_rfc3339(s)
            .unwrap()
            .with_timezone(&chrono::Utc)
    }

    fn gate(name: &str, packet: &str, live: bool) -> Value {
        let status = if live {
            json!({"active": 1})
        } else {
            json!({"succeeded": 1, "conditions": [{"type": "Complete", "status": "True"}]})
        };
        json!({"metadata": {"name": name, "uid": format!("uid-{name}"),
            "labels": {"app": "gate-runner", PACKET_LABEL: packet}}, "status": status})
    }

    fn claim(index: usize, packet: &str, created: &str) -> Value {
        let mut c = claim_document(index, packet, "boss-dev", "img");
        c["metadata"]["uid"] = json!(format!("uid-bay-{index}-{packet}"));
        c["metadata"]["creationTimestamp"] = json!(created);
        c
    }

    /// backlog d777b20a: the record names the bound in force AND the
    /// gates row's own, so an env override that tightened it shows, and
    /// one that tried to loosen it shows that it did not. Since item
    /// 0a78f8f8 the bound is the gates station's bays, and the record
    /// names the station version that set it.
    #[test]
    fn the_bay_record_names_the_bound_in_force_and_the_station_that_set_it() {
        let gates = json!({"name": "gates", "version": 4, "kind": "equipment",
            "automaton": "automation:gate-runner", "bays": 3, "status": "active"});
        let plain = bound_from(&gates, None).unwrap();
        let alone = vec!["gate-bay-1 for p".to_string()];
        assert_eq!(
            plain.record("gate-bay-1", &alone),
            json!({"bay": "gate-bay-1", "bound": 3, "station_bays": 3,
                "station": {"name": "gates", "version": 4},
                "occupied": 1, "occupants": ["gate-bay-1 for p"]})
        );
        assert_eq!(
            bound_from(&gates, Some("2")).unwrap().record("b", &alone)["bound"],
            2
        );
        assert_eq!(
            bound_from(&gates, Some("9")).unwrap().record("b", &alone)["bound"],
            3
        );
        assert!(
            bound_from(&json!({"name": "gates", "bays": 3}), None).is_err(),
            "a row with no version cannot say which bound it set"
        );
        // The policy's old key is no bound at all: a delivery policy row
        // handed here (the pre-0a78f8f8 reader) is refused, never obeyed.
        assert!(
            bound_from(
                &json!({"name": "train-conductor", "version": 11, "gate_max_concurrent": 2}),
                None
            )
            .is_err()
        );
        for bays in [json!(0), json!(-1), json!(null)] {
            let mut row = gates.clone();
            row["bays"] = bays.clone();
            assert!(bound_from(&row, None).is_err(), "bays {bays} is no bound");
        }
    }

    /// The active row is read out of the gates station's lineage — the
    /// bare array the versions route answers, or a `data` envelope — and
    /// a lineage with no active row has no bound.
    #[test]
    fn the_gate_bound_is_the_active_gates_rows_bays() {
        let lineage = json!([
            {"name": "gates", "version": 1, "status": "retired", "bays": 3},
            {"name": "gates", "version": 2, "status": "active", "bays": 2},
            {"name": "gates", "version": 3, "status": "draft", "bays": 5},
        ]);
        let active = crate::gate::active_station_row(&lineage).unwrap();
        assert_eq!(active["version"], 2);
        assert_eq!(crate::gate::bays_of(active), Some(2));
        assert_eq!(
            crate::gate::active_station_row(&json!({"data": lineage}))
                .and_then(crate::gate::bays_of),
            Some(2)
        );
        assert!(crate::gate::active_station_row(&json!([])).is_none());
        assert_eq!(
            crate::gate::gates_versions_path(),
            "/api/stations/gates/versions"
        );
    }

    #[test]
    fn a_job_holds_its_bay_until_a_terminal_condition_with_no_pod_left() {
        assert!(job_live(&json!({"status": {"active": 1}})));
        assert!(job_live(&json!({"status": {}})), "no condition yet is live");
        assert!(job_live(&json!({"status": {"terminating": 1,
            "conditions": [{"type": "Failed", "status": "True"}]}})));
        assert!(!job_live(&json!({"status": {
            "conditions": [{"type": "Failed", "status": "True"}]}})));
        assert!(
            job_live(&json!({"status": {"failed": 1,
            "conditions": [{"type": "Failed", "status": "False"}]}})),
            "a pod counter is not an end"
        );
    }

    #[test]
    fn a_bay_frees_only_on_proof_its_gate_ended() {
        let now = t(NOW);
        let census = |gates: Vec<Value>, claims: Vec<Value>| Census {
            gates,
            pods: vec![],
            claims,
        };
        let c = claim(0, "p1", "2026-10-17T06:00:00Z");
        // The gate it was taken for is running.
        let running = census(vec![gate("gate-p1", "p1", true)], vec![c.clone()]);
        assert_eq!(claim_state(&c, &running, now), ClaimState::Running);
        // That gate ended.
        let ended = census(vec![gate("gate-p1", "p1", false)], vec![c.clone()]);
        assert_eq!(claim_state(&c, &ended, now), ClaimState::Free);
        // No gate yet, inside the grace: the create may still land.
        let pending = census(vec![], vec![c.clone()]);
        assert_eq!(claim_state(&c, &pending, now), ClaimState::Pending);
        // No gate ever, past the grace.
        let old = claim(0, "p1", "2026-10-17T05:50:00Z");
        assert_eq!(
            claim_state(&old, &census(vec![], vec![old.clone()]), now),
            ClaimState::Free
        );
        // A claim it cannot judge keeps its bay.
        let mut blind = old.clone();
        blind["metadata"]["creationTimestamp"] = json!("not a time");
        assert_eq!(
            claim_state(&blind, &census(vec![], vec![]), now),
            ClaimState::Pending
        );
        let mut nameless = old.clone();
        nameless["metadata"]["labels"] = json!({"app": CLAIM_APP});
        assert_eq!(
            claim_state(&nameless, &census(vec![], vec![]), now),
            ClaimState::Pending
        );
    }

    /// A CLAIM TAKEN AFTER THE WAITER STARTED STILL AGES OUT (backlog
    /// fc788e84). Another launcher took bay 0 a minute into this waiter's
    /// `boss gate --wait` and died before its gate Job existed. Eleven
    /// and a half minutes in, the waiter reaches the bay: on the clock it
    /// has reached the claim is past its grace and the bay is retaken; on
    /// the clock the waiter started with — the one `boss gate` handed
    /// `reservation::launch` — the claim is a minute in the future and
    /// holds its bay for as long as the waiter lives.
    #[test]
    fn a_claim_taken_after_the_waiter_started_frees_on_the_waiters_clock() {
        let started = t("2026-10-19T01:00:00Z");
        let abandoned = claim(0, "dead", "2026-10-19T01:01:00Z");
        let census = Census {
            gates: vec![gate("gate-a", "a", true)],
            claims: vec![abandoned.clone(), claim(1, "a", "2026-10-19T00:59:00Z")],
            ..Census::default()
        };
        let reached = crate::gate::carried_forward(started, std::time::Duration::from_secs(690));
        assert_eq!(claim_state(&abandoned, &census, reached), ClaimState::Free);
        assert_eq!(
            plan(&census, 2, reached),
            Plan::Take {
                index: 0,
                replace: Some(("gate-bay-0".into(), "uid-bay-0-dead".into()))
            }
        );
        // The frozen clock: the same claim never crosses its grace.
        assert_eq!(
            claim_state(&abandoned, &census, started),
            ClaimState::Pending
        );
        assert!(matches!(plan(&census, 2, started), Plan::Full(_)));
    }

    #[test]
    fn every_live_gate_counts_claimed_or_not_and_a_running_pod_outlives_its_job() {
        let census = Census {
            gates: vec![
                gate("gate-a", "a", true),
                gate("gate-legacy-x1", "x", true),
                gate("gate-done", "d", false),
            ],
            pods: vec![
                json!({"metadata": {"name": "gate-gone-1", "labels": {"job-name": "gate-gone"}},
                    "status": {"phase": "Running"}}),
                json!({"metadata": {"name": "gate-done-1", "labels": {"job-name": "gate-done"}},
                    "status": {"phase": "Succeeded"}}),
            ],
            claims: vec![
                claim(0, "a", "2026-10-17T06:00:00Z"),
                claim(1, "p", "2026-10-17T06:02:00Z"),
            ],
        };
        let held = occupants(&census, t(NOW));
        assert_eq!(held.len(), 4, "{held:?}");
        assert!(
            held.iter().any(|h| h == "gate-legacy-x1"),
            "an unclaimed gate counts: {held:?}"
        );
        assert!(held.iter().any(|h| h.contains("gate-gone")), "{held:?}");
        assert!(
            held.iter().any(|h| h.starts_with("gate-bay-1 for p")),
            "{held:?}"
        );
    }

    #[test]
    fn the_plan_takes_the_lowest_free_bay_and_refuses_at_the_bound() {
        let now = t(NOW);
        let two = vec![gate("gate-a", "a", true), gate("gate-b", "b", true)];
        let empty = Census {
            gates: two.clone(),
            ..Census::default()
        };
        assert_eq!(
            plan(&empty, 3, now),
            Plan::Take {
                index: 0,
                replace: None
            }
        );
        let ended = claim(0, "z", "2026-10-17T05:00:00Z");
        let retake = Census {
            gates: [two.clone(), vec![gate("gate-z", "z", false)]].concat(),
            claims: vec![ended.clone(), claim(1, "a", "2026-10-17T05:00:00Z")],
            ..Census::default()
        };
        assert_eq!(
            plan(&retake, 3, now),
            Plan::Take {
                index: 0,
                replace: Some(("gate-bay-0".into(), "uid-bay-0-z".into()))
            }
        );
        let full = Census {
            gates: [two, vec![gate("gate-c", "c", true)]].concat(),
            ..Census::default()
        };
        match plan(&full, 3, now) {
            Plan::Full(why) => assert!(
                why.starts_with("3 of 3 in use (gate-a, gate-b, gate-c"),
                "{why}"
            ),
            other => panic!("{other:?}"),
        }
    }

    /// One shared API server: names are unique, as the real one keeps them.
    #[derive(Default)]
    struct Server {
        gates: Vec<Value>,
        claims: std::collections::BTreeMap<String, Value>,
        serial: u64,
    }

    /// One launcher's view of it. `barrier` holds this launcher's FIRST
    /// census until every launcher has taken one — the two reads of the
    /// same free bay that the measurement found.
    struct Launcher {
        server: Arc<Mutex<Server>>,
        barrier: Arc<Barrier>,
        first: Mutex<bool>,
    }

    #[async_trait::async_trait]
    impl BayPort for Launcher {
        async fn census(&self) -> Result<Census> {
            let census = {
                let s = self.server.lock().await;
                Census {
                    gates: s.gates.clone(),
                    pods: vec![],
                    claims: s.claims.values().cloned().collect(),
                }
            };
            let mut first = self.first.lock().await;
            if *first {
                *first = false;
                drop(first);
                self.barrier.wait().await;
            }
            Ok(census)
        }
        async fn create(&self, claim: &Value) -> Result<Created> {
            let mut s = self.server.lock().await;
            let name = name_of(claim).unwrap().to_string();
            if s.claims.contains_key(&name) {
                return Ok(Created::Taken);
            }
            s.serial += 1;
            let uid = format!("uid-{name}-{}", s.serial);
            let mut stored = claim.clone();
            stored["metadata"]["uid"] = json!(uid);
            stored["metadata"]["creationTimestamp"] = json!(NOW);
            s.claims.insert(name, stored);
            Ok(Created::Made { uid })
        }
        async fn delete(&self, name: &str, uid: &str) -> Result<bool> {
            let mut s = self.server.lock().await;
            match s.claims.get(name) {
                Some(c) if uid_of(c) == Some(uid) => {
                    s.claims.remove(name);
                    Ok(true)
                }
                _ => Ok(false),
            }
        }
        fn now(&self) -> chrono::DateTime<chrono::Utc> {
            t(NOW)
        }
    }

    /// Reserve, and on a bay create the gate Job — what create_on does.
    async fn launch(port: Arc<Launcher>, packet: &'static str, max: usize) -> Result<Held> {
        let held = reserve(port.as_ref(), packet, "boss-dev", "img", max).await?;
        port.server
            .lock()
            .await
            .gates
            .push(gate(&format!("gate-{packet}"), packet, true));
        Ok(held)
    }

    /// THE ACCEPTANCE LINE (ac8d3157): two separate launchers racing for
    /// the last bay — 2 running, bound 3 — produce exactly one Job, and
    /// the other is told no bay is free. Each read the same free bay
    /// before either created anything, which is the shape the four
    /// breaches had.
    #[tokio::test]
    async fn two_launchers_racing_for_the_last_bay_make_exactly_one_job() {
        let server = Arc::new(Mutex::new(Server {
            gates: vec![gate("gate-a", "a", true), gate("gate-b", "b", true)],
            ..Server::default()
        }));
        let barrier = Arc::new(Barrier::new(2));
        let launcher = || {
            Arc::new(Launcher {
                server: server.clone(),
                barrier: barrier.clone(),
                first: Mutex::new(true),
            })
        };
        let (one, two) = tokio::join!(launch(launcher(), "c1", 3), launch(launcher(), "c2", 3));
        let s = server.lock().await;
        let live = s.gates.iter().filter(|g| job_live(g)).count();
        assert_eq!(live, 3, "exactly one more Job: {:?}", s.gates);
        assert!(
            one.is_ok() != two.is_ok(),
            "one launch, one refusal: {one:?} {two:?}"
        );
        let refused = one.err().or(two.err()).unwrap();
        assert!(refused.downcast_ref::<NoGateBay>().is_some(), "{refused:#}");
    }

    /// The same race over a bay that FREED: both read bay 0's gate as
    /// ended, both delete it by its old UID — one finds it gone — and only
    /// one claim of that name can be made. The loser reads again, finds
    /// the bound reached, and makes nothing; it never deletes the claim
    /// the winner just took, because that one has another UID.
    #[tokio::test]
    async fn two_launchers_retaking_one_ended_bay_make_exactly_one_job() {
        let mut ended = claim(0, "z", "2026-10-17T05:00:00Z");
        ended["metadata"]["uid"] = json!("uid-old-bay-0");
        let server = Arc::new(Mutex::new(Server {
            gates: vec![
                gate("gate-a", "a", true),
                gate("gate-b", "b", true),
                gate("gate-z", "z", false),
            ],
            claims: [("gate-bay-0".to_string(), ended)].into_iter().collect(),
            ..Server::default()
        }));
        let barrier = Arc::new(Barrier::new(2));
        let launcher = || {
            Arc::new(Launcher {
                server: server.clone(),
                barrier: barrier.clone(),
                first: Mutex::new(true),
            })
        };
        let (one, two) = tokio::join!(launch(launcher(), "c1", 3), launch(launcher(), "c2", 3));
        let s = server.lock().await;
        assert_eq!(
            s.gates.iter().filter(|g| job_live(g)).count(),
            3,
            "{:?}",
            s.gates
        );
        assert!(one.is_ok() != two.is_ok(), "{one:?} {two:?}");
        let won = one.as_ref().or(two.as_ref()).unwrap();
        assert_eq!(
            s.claims.get("gate-bay-0").and_then(uid_of),
            Some(won.uid.as_str())
        );
    }

    fn launchers(server: &Arc<Mutex<Server>>, racing: usize) -> impl Fn() -> Arc<Launcher> {
        let server = server.clone();
        let barrier = Arc::new(Barrier::new(racing));
        move || {
            Arc::new(Launcher {
                server: server.clone(),
                barrier: barrier.clone(),
                first: Mutex::new(true),
            })
        }
    }

    async fn live_gates(server: &Arc<Mutex<Server>>) -> usize {
        server
            .lock()
            .await
            .gates
            .iter()
            .filter(|g| job_live(g))
            .count()
    }

    /// THE ACCEPTANCE LINE OF BACKLOG 3d2bc001, in the shape of its
    /// 18:39Z reading (2026-11-04): gate-bay-1 is held by a car gate whose
    /// claim is long past its grace, and the conductor's train gate and a
    /// car gate both read ONE bay in use before either took anything.
    /// Exactly one gets a bay — gate-bay-0, never the held gate-bay-1 —
    /// and the other is told no bay is free, which is a place in the line
    /// (`NoGateBay`), not a fault. When the held gate ends, the waiter
    /// takes the bay it freed: still two, never three. Train gates and car
    /// gates are the same gate Jobs to the census, so they count inside
    /// the same bays.
    ///
    /// Measured the same day off every gate-run's own `launched_at` (the
    /// native Job's creationTimestamp) and its gate end: no two gate Jobs
    /// held one bay and none ran three wide after the bound became 2 on
    /// 2026-11-01. The "three wide" readings counted a completed `Gate
    /// launched` step, which closes when the packet is ACCEPTED into the
    /// line, as a running gate: 797d371b read launched at 18:39:19Z and its
    /// Job was made at 18:58:59Z, after 577e13e1's gate ended at 18:58:15Z.
    #[tokio::test]
    async fn two_launches_in_one_second_with_one_bay_held_issue_exactly_one_bay() {
        let server = Arc::new(Mutex::new(Server {
            gates: vec![gate("gate-held", "held", true)],
            claims: [(
                "gate-bay-1".to_string(),
                claim(1, "held", "2026-10-17T05:20:00Z"),
            )]
            .into_iter()
            .collect(),
            ..Server::default()
        }));
        let racer = launchers(&server, 2);
        let (train, car) = tokio::join!(launch(racer(), "train", 2), launch(racer(), "car", 2));
        assert!(train.is_ok() != car.is_ok(), "{train:?} {car:?}");
        let (won, lost, waiter) = match (&train, &car) {
            (Ok(won), Err(lost)) => (won, lost, "car"),
            (Err(lost), Ok(won)) => (won, lost, "train"),
            other => panic!("one bay, one place in the line: {other:?}"),
        };
        assert_eq!(won.name, "gate-bay-0", "a held bay is never issued");
        assert!(lost.downcast_ref::<NoGateBay>().is_some(), "{lost:#}");
        assert_eq!(live_gates(&server).await, 2);
        assert_eq!(
            server
                .lock()
                .await
                .claims
                .get("gate-bay-1")
                .and_then(uid_of),
            Some("uid-bay-1-held"),
            "the held bay's claim is untouched"
        );
        // The held gate ends; the waiter's next attempt takes ITS bay.
        server.lock().await.gates[0] = gate("gate-held", "held", false);
        let later = launch(launchers(&server, 1)(), waiter, 2).await.unwrap();
        assert_eq!(later.name, "gate-bay-1");
        assert_eq!(live_gates(&server).await, 2);
    }

    /// Three launches inside one second against two free bays: two bays,
    /// one each, and the third waits (backlog 3d2bc001).
    #[tokio::test]
    async fn three_launches_against_two_free_bays_take_two_and_the_third_waits() {
        let server = Arc::new(Mutex::new(Server::default()));
        let racer = launchers(&server, 3);
        let (a, b, c) = tokio::join!(
            launch(racer(), "a", 2),
            launch(racer(), "b", 2),
            launch(racer(), "c", 2)
        );
        let mut bays: Vec<String> = [&a, &b, &c]
            .iter()
            .filter_map(|r| r.as_ref().ok().map(|h| h.name.clone()))
            .collect();
        bays.sort();
        assert_eq!(bays, ["gate-bay-0", "gate-bay-1"], "{a:?} {b:?} {c:?}");
        let waiting: Vec<&anyhow::Error> = [&a, &b, &c]
            .into_iter()
            .filter_map(|r| r.as_ref().err())
            .collect();
        assert_eq!(waiting.len(), 1);
        assert!(
            waiting[0].downcast_ref::<NoGateBay>().is_some(),
            "{:#}",
            waiting[0]
        );
        assert_eq!(live_gates(&server).await, 2);
    }

    /// The gate-run says how many bays were occupied when it took its
    /// own, and by whom — this claim counted (backlog 3d2bc001). Without
    /// it, a reader who counts packets by their steps cannot tell a gate
    /// that ran beside two others from one that waited for a bay.
    #[tokio::test]
    async fn the_bay_record_says_how_many_bays_were_occupied_when_it_was_taken() {
        let server = Arc::new(Mutex::new(Server {
            gates: vec![gate("gate-held", "held", true)],
            claims: [(
                "gate-bay-1".to_string(),
                claim(1, "held", "2026-10-17T05:20:00Z"),
            )]
            .into_iter()
            .collect(),
            ..Server::default()
        }));
        let held = launch(launchers(&server, 1)(), "car", 2).await.unwrap();
        assert_eq!(held.occupants, ["gate-held", "gate-bay-0 for car"]);
        let gates = json!({"name": "gates", "version": 1, "kind": "equipment",
            "automaton": "automation:gate-runner", "bays": 2, "status": "active"});
        assert_eq!(
            bound_from(&gates, None)
                .unwrap()
                .record(&held.name, &held.occupants),
            json!({"bay": "gate-bay-0", "bound": 2, "station_bays": 2,
                "station": {"name": "gates", "version": 1},
                "occupied": 2, "occupants": ["gate-held", "gate-bay-0 for car"]})
        );
    }

    /// A census that cannot be read takes no bay: fail closed.
    #[tokio::test]
    async fn an_unreadable_census_takes_no_bay() {
        struct Dark;
        #[async_trait::async_trait]
        impl BayPort for Dark {
            async fn census(&self) -> Result<Census> {
                bail!("kubectl get --raw refused")
            }
            async fn create(&self, _: &Value) -> Result<Created> {
                panic!("nothing may be created blind")
            }
            async fn delete(&self, _: &str, _: &str) -> Result<bool> {
                panic!("nothing may be deleted blind")
            }
            fn now(&self) -> chrono::DateTime<chrono::Utc> {
                t(NOW)
            }
        }
        let e = reserve(&Dark, "p", "boss-dev", "img", 3).await.unwrap_err();
        assert!(
            e.downcast_ref::<NoGateBay>().is_none(),
            "a fault is not a place: {e:#}"
        );
    }

    #[test]
    fn a_claim_is_a_podless_job_the_gate_readers_do_not_select() {
        let c = claim_document(2, "p", "boss-dev", "registry/img:tag");
        assert_eq!(c["metadata"]["name"], "gate-bay-2");
        assert_eq!(c["spec"]["suspend"], true);
        assert_ne!(c["metadata"]["labels"]["app"], "gate-runner");
        assert_eq!(c["metadata"]["labels"][PACKET_LABEL], "p");
        assert_eq!(
            gate_image(&json!({"spec": {"template": {"spec": {
            "containers": [{"image": "registry/img:tag"}]}}}}))
            .unwrap(),
            "registry/img:tag"
        );
        assert!(gate_image(&json!({})).is_err());
    }
}
