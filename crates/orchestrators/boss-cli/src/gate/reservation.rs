//! Audited queue transitions. No retry reconstructs a timed-out command
//! with a new idempotency key; conflicts require a fresh observation.
use super::*;
use boss_jobs::conditional_metadata::{Command, Receipt};
use boss_jobs::gate_reservation as model;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use uuid::Uuid;
mod attempt;
pub(super) mod bay;
#[cfg(test)]
mod identity_tests;
mod settlement;
mod source_claim;
mod staging;
mod submission;
mod supervisor;

pub(super) async fn guard_settlement(
    http: &crate::machine_http::MachineClient,
    packet: &str,
    namespace: &str,
    receipt: &Value,
    now: chrono::DateTime<chrono::Utc>,
) -> Result<()> {
    settlement::guard_on(&Doors::live(http)?, packet, namespace, receipt, now).await
}

pub(super) async fn guard_settlement_for_conductor(
    http: &crate::machine_http::MachineClient,
    jobs: &str,
    packet: &str,
    namespace: &str,
    receipt: &Value,
    now: chrono::DateTime<chrono::Utc>,
) -> Result<()> {
    settlement::guard_on(
        &Doors::conductor(http, jobs)?,
        packet,
        namespace,
        receipt,
        now,
    )
    .await
}

pub(super) async fn supervise_for_conductor(
    http: &crate::machine_http::MachineClient,
    jobs: &str,
    checkout: &Path,
    namespace: &str,
    max: usize,
    now: chrono::DateTime<chrono::Utc>,
) -> Result<Value> {
    let doors = Doors::conductor(http, jobs)?;
    Ok(supervisor::observation_on(&doors, checkout, namespace, max, now).await)
}

pub(super) async fn launch_for_conductor(
    http: &crate::machine_http::MachineClient,
    jobs: &str,
    packet: &str,
    namespace: &str,
    head: &str,
    mode: &str,
    manifest: &str,
    now: chrono::DateTime<chrono::Utc>,
) -> Result<String> {
    let doors = Doors::conductor(http, jobs)?;
    launch_legacy_on(&doors, packet, namespace, head, mode, manifest, now).await
}

async fn launch_legacy_on(
    doors: &Doors<'_>,
    packet: &str,
    namespace: &str,
    head: &str,
    mode: &str,
    manifest: &str,
    now: chrono::DateTime<chrono::Utc>,
) -> Result<String> {
    let policy = staging::policy_on(doors).await?;
    let row = read_on(doors, packet).await?;
    if staging::legacy_on(doors, &row, &policy).await? {
        let names = native_jobs(doors, namespace, packet).await?;
        if !names.is_empty() {
            bail!("legacy gate has retained native identity; inspect it rather than resend");
        }
        eprintln!(
            "boss gate: explicit process-owned legacy transport; policy {} pinned protocol {}",
            policy["version"], row["workflow_version"]
        );
        // The legacy transport takes the same bay as every other launch
        // (backlog ac8d3157): it is a path to a Job, so it is no way past
        // the bound. Its Job is named by the server, and found again by
        // the packet label every gate Job carries.
        let desired = client_document(doors, packet, namespace, manifest).await?;
        let (bay, _) = bay::reserve_on(doors, namespace, packet, &desired, now).await?;
        let mut command = native_command(doors, namespace);
        command.args(["create", "-f", "-"]);
        let outcome = native_create(command, manifest.as_bytes()).await;
        if matches!(outcome, NativeCreate::NotSpawned { .. }) {
            bay::release_on(doors, namespace, &bay, now).await;
        }
        let artifact = retain(
            doors,
            packet,
            &serde_json::to_vec(&json!({
                "operation":"legacy-native-create", "policy":policy,
                "workflow_version":row["workflow_version"], "outcome":outcome
            }))?,
        )
        .await?;
        eprintln!(
            "boss gate: whole legacy create observation {} sha256 {}",
            artifact.file_id, artifact.sha256
        );
        return match outcome {
            NativeCreate::Spawned {
                status: Some(0),
                stdout,
                input_error: None,
                wait_error: None,
                stdout_error: None,
                stderr_error: None,
                ..
            } => Ok(String::from_utf8(stdout)
                .context("legacy native response is not UTF-8")?
                .trim()
                .to_owned()),
            _ => bail!(
                "legacy native create outcome UNKNOWN or refused; no protected/legacy fallback or automatic resend"
            ),
        };
    }
    capability_on(doors).await?;
    if let Some(name) = existing_on(doors, namespace, packet).await? {
        return Ok(name);
    }
    renew_on(doors, packet, now).await?;
    let (intent, bytes) = prepare_on(doors, packet, namespace, head, mode, manifest).await?;
    create_on(doors, &intent, &bytes, now).await
}

pub(super) async fn ownership_policy(http: &crate::machine_http::MachineClient) -> Result<Value> {
    staging::policy_on(&Doors::live(http)?).await
}

pub(super) async fn launch(
    http: &crate::machine_http::MachineClient,
    packet: &str,
    namespace: &str,
    head: &str,
    mode: &str,
    manifest: &str,
    now: chrono::DateTime<chrono::Utc>,
) -> Result<String> {
    launch_legacy_on(
        &Doors::live(http)?,
        packet,
        namespace,
        head,
        mode,
        manifest,
        now,
    )
    .await
}

pub(super) async fn submit(
    http: &crate::machine_http::MachineClient,
    intent: &LaunchIntent,
    patch: &Value,
) -> Result<Receipt> {
    submission::submit_on(&Doors::live(http)?, intent, patch).await
}

pub(super) async fn observe(http: &crate::machine_http::MachineClient, packet: &str) -> Result<()> {
    let doors = Doors::live(http)?;
    loop {
        let row = read_on(&doors, packet).await?;
        if row.pointer("/metadata/gate_submission").is_some() {
            let (_, receipt) = submission::original_on(&doors, packet).await?;
            eprintln!(
                "boss gate: resident owns {packet}; accepted {} by {}",
                receipt.applied_at, receipt.actor
            );
        } else {
            eprintln!(
                "boss gate: {packet} has legacy ownership; this observer cannot renew or adopt it"
            );
        }
        if let Some(verdict) = own_verdict(packet, &row)? {
            println!("{}", verdict_line(&verdict, packet, &row));
            if verdict != "green" {
                bail!(
                    "gate verdict: {verdict}; {}",
                    red_verdict_detail(&row).unwrap_or_default()
                );
            }
            return Ok(());
        }
        tokio::time::sleep(std::time::Duration::from_secs(30)).await;
    }
}

/// The resident places still waiting for a bay in `namespace`, each at
/// its ORIGINAL ACCEPTANCE — the server receipt's instant, the one the
/// resident supervisor orders by (backlog b56bfc29). A legacy waiter
/// merges these into its line ([`super::mixed_line`]) so a resident
/// place taken before its own is ahead of it, and one taken after is
/// not; the supervisor counts legacy places against the same instant.
/// Read from the receipt and never from the packet's client stamps,
/// because two owners reading two instants for one place can each
/// count the other ahead and hold a free bay between them.
///
/// A resident whose original acceptance cannot be read is not counted:
/// the supervisor retains it unlaunched on the same read, so it holds
/// no place either side would honour. A read that fails in transit is
/// uncounted for that one poll too; the bay taken at the create
/// (backlog ac8d3157) still bounds every launch, so the worst it costs
/// is one waiter taking a bay a pass early, never a bay over the bound.
pub(super) async fn resident_places(
    http: &crate::machine_http::MachineClient,
    open: &[Value],
    namespace: &str,
) -> Result<Vec<(chrono::DateTime<chrono::Utc>, String)>> {
    resident_places_on(&Doors::live(http)?, open, namespace).await
}

async fn resident_places_on(
    http: &Doors<'_>,
    open: &[Value],
    namespace: &str,
) -> Result<Vec<(chrono::DateTime<chrono::Utc>, String)>> {
    let mut places = Vec::new();
    for row in open {
        let waiting = row.get("metadata").is_some_and(model::waiting_for_native);
        let Some(packet) = row.get("id").and_then(Value::as_str).filter(|_| waiting) else {
            continue;
        };
        if let Ok((submission, receipt)) = submission::original_on(http, packet).await
            && submission.intent.namespace == namespace
        {
            places.push((receipt.applied_at, packet.to_owned()));
        }
    }
    places.sort();
    Ok(places)
}

/// Immutable I/O inputs. Live wrappers resolve supported doors once;
/// qualification supplies isolated sockets and native client arguments.
struct Doors<'a> {
    http: &'a crate::machine_http::MachineClient,
    jobs_base: String,
    content_base: String,
    caller: Option<identity::Caller>,
    native_args: Vec<String>,
    create_program: Option<PathBuf>,
}
impl<'a> Doors<'a> {
    fn conductor(http: &'a crate::machine_http::MachineClient, jobs: &str) -> Result<Self> {
        Ok(Self {
            http,
            jobs_base: jobs.to_owned(),
            content_base: crate::tenant_publish::service_on_door(jobs, "content")?,
            caller: Some(identity::Caller {
                id: identity::conductor(),
                source: identity::Source::Env,
            }),
            native_args: vec![],
            create_program: None,
        })
    }
    fn live(http: &'a crate::machine_http::MachineClient) -> Result<Self> {
        let jobs_base = jobs_base()?;
        let content_base = crate::tenant_publish::service_on_door(&jobs_base, "content")?;
        Ok(Self {
            http,
            jobs_base,
            content_base,
            caller: identity::caller(),
            native_args: vec![],
            create_program: None,
        })
    }
}
async fn api(
    http: &Doors<'_>,
    method: reqwest::Method,
    path: &str,
    payload: Option<Value>,
) -> Result<Option<Value>> {
    let signature = identity::signature_for(&method, path, http.caller.clone());
    crate::sor_client::api_at_signed(http.http, &http.jobs_base, method, path, payload, signature)
        .await
}
pub(super) async fn prepare(
    http: &crate::machine_http::MachineClient,
    packet: &str,
    namespace: &str,
    head: &str,
    mode: &str,
    manifest: &str,
) -> Result<(LaunchIntent, Vec<u8>)> {
    prepare_on(&Doors::live(http)?, packet, namespace, head, mode, manifest).await
}
pub(super) async fn capability(http: &crate::machine_http::MachineClient) -> Result<()> {
    capability_on(&Doors::live(http)?).await
}
pub(super) async fn renew(
    http: &crate::machine_http::MachineClient,
    packet: &str,
    now: chrono::DateTime<chrono::Utc>,
) -> Result<Uuid> {
    renew_on(&Doors::live(http)?, packet, now).await
}
pub(super) use model::LaunchIntent;

fn job_name(packet: &str) -> Result<String> {
    Ok(format!("gate-{}", Uuid::parse_str(packet)?.simple()))
}

/// Compare every submitted field, allowing only server-added defaults.
/// An annotation alone is never sufficient identity evidence.
fn contains_desired(native: &Value, desired: &Value) -> bool {
    fn normalized(raw: &Value, identity: &Value) -> Option<Value> {
        let mut value = raw.clone();
        if !value["metadata"]["deletionTimestamp"].is_null() {
            return None;
        }
        // The request has no server identity. Supply the OBSERVED identity
        // only to interpret controller-generated default labels/selectors;
        // it is never retained as an original creation observation.
        value["metadata"]["uid"] = identity["metadata"]["uid"].clone();
        value["metadata"]["resourceVersion"] = identity["metadata"]["resourceVersion"].clone();
        let template = value.pointer_mut("/spec/template")?.as_object_mut()?;
        let metadata = template
            .entry("metadata")
            .or_insert_with(|| json!({}))
            .as_object_mut()?;
        metadata.entry("labels").or_insert_with(|| json!({}));
        // Gate-specific admission defaults. Keep every unrecognized value in
        // the equality comparison; an admitted change is not a default.
        if value["metadata"]["generation"] == json!(1) {
            value["metadata"].as_object_mut()?.remove("generation");
        }
        for collection in ["containers", "initContainers"] {
            if let Some(containers) = value["spec"]["template"]["spec"][collection].as_array_mut() {
                for container in containers {
                    for (key, default) in [
                        ("terminationMessagePath", "/dev/termination-log"),
                        ("terminationMessagePolicy", "File"),
                    ] {
                        if container[key] == json!(default) {
                            container.as_object_mut()?.remove(key);
                        }
                    }
                    // Only the OBSERVED default folds: IfNotPresent for a
                    // pinned or tagged init image (train48bbc0b3's). The
                    // server's Always for an untagged or `:latest` one was
                    // never observed — the init image is a tagged mirror —
                    // so it reads as a difference (P3-1 of review 8c92f75b,
                    // backlog cafa83a5).
                    if collection == "initContainers" {
                        let image = container["image"].as_str()?;
                        let last = image.rsplit('/').next()?;
                        let immutable_or_tagged = image.contains('@')
                            || last
                                .split_once(':')
                                .is_some_and(|(_, tag)| tag != "latest" && !tag.is_empty());
                        if immutable_or_tagged
                            && container["imagePullPolicy"] == json!("IfNotPresent")
                        {
                            container.as_object_mut()?.remove("imagePullPolicy");
                        }
                    }
                    if let Some(env) = container["env"].as_array_mut() {
                        for entry in env {
                            // An EnvVar's `value` is omitempty: the server
                            // stores `{"name":X,"value":""}` as `{"name":X}`,
                            // one value spelled two ways. A full gate's
                            // GATE_MODE is "", so until this line every main
                            // and builder full gate ran while its launch read
                            // UNKNOWN (backlog 0c77e4db). Only EMPTY folds;
                            // a dropped non-empty value still differs.
                            if entry.get("value") == Some(&json!("")) {
                                entry.as_object_mut()?.remove("value");
                            }
                            if let Some(field) = entry
                                .pointer_mut("/valueFrom/fieldRef")
                                .and_then(Value::as_object_mut)
                                && field.get("apiVersion") == Some(&json!("v1"))
                            {
                                field.remove("apiVersion");
                            }
                        }
                    }
                }
            }
        }
        let mut value = crate::launch_binding::normalized_job_v1(&value).ok()?;
        value.as_object_mut()?.remove("status");
        let metadata = value["metadata"].as_object_mut()?;
        for key in [
            "uid",
            "resourceVersion",
            "creationTimestamp",
            "managedFields",
            "deletionTimestamp",
        ] {
            metadata.remove(key);
        }
        Some(value)
    }
    match (normalized(native, native), normalized(desired, native)) {
        (Some(actual), Some(expected)) => actual == expected,
        _ => false,
    }
}

fn native_command(http: &Doors<'_>, namespace: &str) -> std::process::Command {
    let mut command = kubectl(namespace);
    // Client-config flags suppress kubectl's in-cluster fallback
    // (06ae925a). The parent process owns the deadline instead.
    command.args(&http.native_args);
    command
}

async fn native_read(
    http: &Doors<'_>,
    namespace: &str,
    args: &[&str],
) -> Result<std::process::Output> {
    let mut command = native_command(http, namespace);
    command.args(args);
    native_output(native_create(command, &[]).await).map(|(output, _)| output)
}

struct NativeCensus {
    list: Value,
    artifact: crate::attach::Attached,
}

/// A read of the system of record as a RECORD keeps it: which read, and
/// the size and SHA-256 of its exact reply — never the reply itself.
///
/// WHY (backlog e1943ef0, measured 2026-10-19 on gate-run dec70bf9, the
/// first durable submission since 0e7a03d7). Kept whole, the reads were
/// copies of the system of record inside it: the source claim's reads put
/// the builder's run and its whole parent item into the immutable
/// submission (a 499 KB gate packet, so its guarded withdrawal event
/// encoded to 995,839 bytes and the bus bound refused it), and the
/// supervisor's pass report carried every list page whole (6.3 MB, which
/// the cadence outcome door refused 413 every pass). The history of
/// what was read is the event log's; the decision each read fed is
/// recorded beside it (`gate_source_claim`, the report's own verdicts),
/// and the digest says which bytes it was.
fn read_digest(operation: &str, reply: &Value) -> Value {
    let bytes = serde_json::to_vec(reply).unwrap_or_default();
    json!({"operation":operation,
        "id":reply.get("id").cloned().unwrap_or(Value::Null),
        "total":reply.get("total").cloned().unwrap_or(Value::Null),
        "bytes":bytes.len(),"sha256":hex::encode(Sha256::digest(&bytes))})
}

/// What every census reader decides on, per Job: identity, labels and
/// status (`bay::job_live`, the packet label, the name/uid checks) — not
/// the pod template, which is ~40 KB of each live gate Job. Measured
/// 2026-10-19: boss-dev's `app=gate-runner` census, the day's finished
/// gate Jobs included, was 5,376,991 bytes, over the file store's
/// 2,097,152-byte upload bound, so retaining it whole failed every
/// resident pass before admission (backlog e1943ef0, gate-run dec70bf9).
/// The whole reply is named by its size and digest beside this.
fn census_projection(list: &Value) -> Value {
    let items: Vec<Value> = list
        .get("items")
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .map(|job| {
                    let keep = |key: &str| {
                        job.pointer(&format!("/metadata/{key}"))
                            .cloned()
                            .unwrap_or(Value::Null)
                    };
                    json!({"apiVersion":job.get("apiVersion"),"kind":job.get("kind"),
                        "metadata":{"name":keep("name"),"namespace":keep("namespace"),
                            "uid":keep("uid"),"resourceVersion":keep("resourceVersion"),
                            "creationTimestamp":keep("creationTimestamp"),
                            "deletionTimestamp":keep("deletionTimestamp"),"labels":keep("labels")},
                        "status":job.get("status")})
                })
                .collect()
        })
        .unwrap_or_default();
    json!({"apiVersion":list.get("apiVersion"),"kind":list.get("kind"),
        "metadata":list.get("metadata"),"items":items})
}

async fn native_census_on(
    http: &Doors<'_>,
    namespace: &str,
    selector: &str,
    packet: &str,
) -> Result<NativeCensus> {
    let mut command = native_command(http, namespace);
    let mut url = reqwest::Url::parse("http://native.invalid")?;
    url.path_segments_mut()
        .map_err(|_| anyhow::anyhow!("native API path is not hierarchical"))?
        .extend(["apis", "batch", "v1", "namespaces", namespace, "jobs"]);
    url.query_pairs_mut().append_pair("labelSelector", selector);
    // `get jobs -o json` rebuilds a v1/List and discards the server's
    // resourceVersion. The bounded raw GET keeps its actual JobList.
    command.args([
        "get",
        &format!("--raw={}?{}", url.path(), url.query().unwrap_or_default()),
    ]);
    let observation = native_create(command, &[]).await;
    let original = serde_json::to_vec(&json!({"operation":"native-packet-census-process",
        "namespace":namespace,"selector":selector,"packet":packet,"outcome":observation}))?;
    let (output, _) = match native_output(observation) {
        Ok(output) => output,
        Err(error) => {
            let artifact = retain(http, packet, &original).await?;
            return Err(error).with_context(|| {
                format!(
                    "whole census process observation {} sha256 {}",
                    artifact.file_id, artifact.sha256
                )
            });
        }
    };
    // The census is conserved WHOLE whenever the file store can hold it —
    // every packet-scoped census can. Only a reply the store cannot take
    // is retained as `census_projection` plus the whole reply's size and
    // digest, and says so: before this, that case retained nothing and
    // refused the pass (e1943ef0).
    let parsed: Option<Value> = serde_json::from_slice(&output.stdout).ok();
    let record = |stdout: Value| {
        serde_json::to_vec(&json!({"operation":"native-packet-census",
        "namespace":namespace,"selector":selector,"packet":packet,"status":output.status.code(),
        "stdout":stdout,"stderr":output.stderr}))
    };
    let mut bytes = record(json!(output.stdout))?;
    if let Some(list) = &parsed
        && bytes.len() as u64 > crate::attach::largest_file()
    {
        bytes = record(json!({"whole_bytes":output.stdout.len(),
            "whole_sha256":hex::encode(Sha256::digest(&output.stdout)),
            "whole_not_retained":format!("the whole reply exceeds the file store's bound; {}",
                crate::attach::limit_sentence()),
            "projection":census_projection(list)}))?;
    }
    let artifact = retain(http, packet, &bytes).await?;
    if !output.status.success() {
        bail!(
            "native packet census refused: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
    let list: Value = parsed.context("native packet census is not JSON")?;
    census_bound(&list, namespace)?;
    Ok(NativeCensus { list, artifact })
}

/// PURE: whether a census reply is one complete JobList of this
/// namespace's Jobs, each named once and uid'd once.
fn census_bound(list: &Value, namespace: &str) -> Result<()> {
    if list.get("kind").and_then(Value::as_str) != Some("JobList")
        || list.get("apiVersion").and_then(Value::as_str) != Some("batch/v1")
        || list
            .pointer("/metadata/resourceVersion")
            .and_then(Value::as_str)
            .is_none_or(str::is_empty)
        || list
            .pointer("/metadata/continue")
            .is_some_and(|token| !token.is_null() && token.as_str() != Some(""))
        || list
            .pointer("/metadata/remainingItemCount")
            .is_some_and(|count| !count.is_null() && count.as_u64() != Some(0))
    {
        bail!("native packet census is not one complete JobList");
    }
    let mut names = std::collections::HashSet::new();
    let mut uids = std::collections::HashSet::new();
    for job in list
        .get("items")
        .and_then(Value::as_array)
        .context("native packet census lacks its full items array")?
    {
        let name = job
            .pointer("/metadata/name")
            .and_then(Value::as_str)
            .filter(|name| !name.is_empty())
            .context("native packet census has a nameless Job")?;
        let uid = job
            .pointer("/metadata/uid")
            .and_then(Value::as_str)
            .filter(|uid| !uid.is_empty())
            .context("native packet census has a uid-less Job")?;
        // An item's type is its list's: the API leaves `kind` and
        // `apiVersion` off every item of a typed list (measured on boss-dev,
        // 2026-10-20), so an absent one is the batch/v1 Job the JobList
        // above already says it is. One that IS stated must agree. Demanding
        // it refused every non-empty census for the resident supervisor's
        // whole first hour live (backlog d561255c).
        for (key, expected) in [("kind", "Job"), ("apiVersion", "batch/v1")] {
            if let Some(stated) = job.get(key).filter(|stated| !stated.is_null())
                && stated.as_str() != Some(expected)
            {
                bail!(
                    "native packet census contains an unbound Job {name}: its {key} is {stated}, not {expected}; all native work remains untouched"
                );
            }
        }
        // Each refusal names the Job and the reason: the one sentence these
        // replaced named neither, and a dead gate queue was diagnosed from
        // the code instead of from the record (d561255c).
        let found = job.pointer("/metadata/namespace").unwrap_or(&Value::Null);
        if found.as_str() != Some(namespace) {
            bail!(
                "native packet census contains an unbound Job {name}: its namespace is {found}, not {namespace}; all native work remains untouched"
            );
        }
        if !names.insert(name.to_owned()) {
            bail!(
                "native packet census contains a repeated Job: the name {name} appears twice; all native work remains untouched"
            );
        }
        if !uids.insert(uid.to_owned()) {
            bail!(
                "native packet census contains a repeated Job: {name} repeats the uid {uid}; all native work remains untouched"
            );
        }
    }
    Ok(())
}

async fn native_jobs(http: &Doors<'_>, namespace: &str, packet: &str) -> Result<Vec<String>> {
    let census = native_census_on(
        http,
        namespace,
        &format!("boss.dev/packet={packet}"),
        packet,
    )
    .await?;
    let mut names = Vec::new();
    for job in census.list["items"]
        .as_array()
        .context("validated native census lost its items")?
    {
        if job
            .pointer("/metadata/labels/boss.dev~1packet")
            .and_then(Value::as_str)
            != Some(packet)
        {
            bail!("native packet census contains a Job outside the requested packet");
        }
        names.push(
            job["metadata"]["name"]
                .as_str()
                .context("validated Job lost its name")?
                .to_owned(),
        );
    }
    Ok(names)
}

async fn run_with_input(
    command: std::process::Command,
    bytes: &[u8],
) -> Result<(std::process::Output, Option<String>)> {
    native_output(native_create(command, bytes).await)
}

/// The native renderer's dry run of `manifest`: nothing is created.
async fn dry_render(
    http: &Doors<'_>,
    namespace: &str,
    manifest: &str,
) -> Result<(std::process::Output, Option<String>)> {
    let mut command = native_command(http, namespace);
    command.args([
        "create",
        "--dry-run=client",
        "--validate=false",
        "-f",
        "-",
        "-o",
        "json",
    ]);
    run_with_input(command, manifest.as_bytes())
        .await
        .context("rendering native Job JSON without creating it")
}

/// The one Job a dry run returned, or why it returned none.
fn rendered_job(output: &std::process::Output, input_error: Option<&str>) -> Result<Value> {
    if !output.status.success() || input_error.is_some() {
        bail!(
            "native dry renderer failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
    let value: Value = serde_json::from_slice(&output.stdout)?;
    if value.get("kind").and_then(Value::as_str) != Some("Job") {
        bail!("native renderer did not return one Job");
    }
    Ok(value)
}

async fn client_document(
    http: &Doors<'_>,
    packet: &str,
    namespace: &str,
    manifest: &str,
) -> Result<Value> {
    let (output, input_error) = dry_render(http, namespace, manifest).await?;
    retain(http,packet,&serde_json::to_vec(&json!({"operation":"render-native-request","input_error":input_error,"status":output.status.code(),"stdout":output.stdout,"stderr":output.stderr}))?).await?;
    rendered_job(&output, input_error.as_deref())
}

/// PURE: a dry-rendered Job made into the request a launch submits —
/// server identity dropped, the deterministic name and the namespace set.
/// ONE definition for the launch ([`prepare_on`]) and the train gate's
/// unsent preview ([`preview_request_for_conductor`]), so the request the
/// reuse comparison judges is the request a launch would make.
fn finish_request(desired: &mut Value, name: &str, namespace: &str) -> Result<()> {
    let metadata = desired
        .get_mut("metadata")
        .and_then(Value::as_object_mut)
        .context("native Job has no metadata")?;
    metadata.remove("generateName");
    for key in [
        "creationTimestamp",
        "uid",
        "resourceVersion",
        "managedFields",
    ] {
        metadata.remove(key);
    }
    metadata.insert("name".into(), json!(name));
    metadata.insert("namespace".into(), json!(namespace));
    if let Some(metadata) = desired
        .pointer_mut("/spec/template/metadata")
        .and_then(Value::as_object_mut)
    {
        metadata.remove("creationTimestamp");
    }
    Ok(())
}

/// The exact bytes a gate-run's launch intent names, read back from the
/// file store and checked against its sha256 and size — the car gate's
/// side of the train-gate reuse comparison (backlog bbc13a4e).
pub(super) async fn retained_request_for_conductor(
    http: &crate::machine_http::MachineClient,
    jobs: &str,
    intent: &Value,
) -> Result<Vec<u8>> {
    let doors = Doors::conductor(http, jobs)?;
    let intent: LaunchIntent =
        serde_json::from_value(intent.clone()).context("unreadable immutable launch intent")?;
    request_bytes(&doors, &intent).await
}

/// The request a launch of `manifest` on `head` would submit, rendered
/// by the launch's own native path and NEVER sent, filed or retained —
/// the train gate's side of the reuse comparison (backlog bbc13a4e).
#[allow(clippy::too_many_arguments)]
pub(super) async fn preview_request_for_conductor(
    http: &crate::machine_http::MachineClient,
    jobs: &str,
    packet: &str,
    namespace: &str,
    branch: &str,
    head: &str,
    mode: &str,
    manifest: &str,
) -> Result<Vec<u8>> {
    let doors = Doors::conductor(http, jobs)?;
    let (_, argument) = execution_mode(mode)?;
    let (output, input_error) = dry_render(&doors, namespace, manifest).await?;
    let mut desired = rendered_job(&output, input_error.as_deref())?;
    bind_execution(&mut desired, &json!({ "branch": branch }), head, &argument)?;
    finish_request(&mut desired, &job_name(packet)?, namespace)?;
    Ok(serde_json::to_vec(&desired)?)
}

async fn retain(http: &Doors<'_>, packet: &str, bytes: &[u8]) -> Result<crate::attach::Attached> {
    use std::io::Write;
    let mut file = tempfile::Builder::new()
        .prefix("boss-gate-evidence-")
        .suffix(".json")
        .tempfile()?;
    file.write_all(bytes)?;
    file.flush()?;
    let target = crate::attach::Target {
        kind: "job",
        id: packet.into(),
        label: format!("gate packet {packet}"),
    };
    let content = &http.content_base;
    let signature =
        identity::signature_for(&reqwest::Method::POST, "/api/files", http.caller.clone());
    match crate::attach::attach_at(http.http, content, &target, file.path(), signature).await {
        Ok(attached) => Ok(attached),
        Err(error) => {
            let reconciliation = match retained_identical(http, packet, bytes).await {
                Ok(Some(attached)) => return Ok(attached),
                Ok(None) => "no byte-identical attachment on this packet".to_owned(),
                Err(error) => format!("{error:#}"),
            };
            let (_, path) = file.keep().map_err(|e| anyhow!(e.to_string()))?;
            Err(error.context(format!(
                "complete unuploaded evidence retained at {}; existing attachment reconciliation: {reconciliation}",
                path.display()
            )))
        }
    }
}

async fn retained_identical(
    http: &Doors<'_>,
    packet: &str,
    bytes: &[u8],
) -> Result<Option<crate::attach::Attached>> {
    let signer = identity::apply(identity::signature_for(
        &reqwest::Method::GET,
        "/api/files",
        http.caller.clone(),
    ))?;
    let response = http
        .http
        .get(format!("{}/api/files", http.content_base))
        .query(&[("target_kind", "job"), ("target_id", packet)])
        .header("x-boss-user", identity::header(&signer))
        .send()
        .await?;
    if !response.status().is_success() {
        bail!("existing attachment list refused: {}", response.status());
    }
    let rows: Vec<Value> = response.json().await?;
    let sha = hex::encode(Sha256::digest(bytes));
    for row in rows {
        if row.get("sha256").and_then(Value::as_str) != Some(sha.as_str())
            || row.get("size_bytes").and_then(Value::as_u64) != Some(bytes.len() as u64)
        {
            continue;
        }
        let text = |key: &str| {
            row.get(key)
                .and_then(Value::as_str)
                .map(str::to_owned)
                .with_context(|| format!("existing attachment lacks {key}"))
        };
        let id = text("id")?;
        let response = http
            .http
            .get(format!("{}/api/files/{id}", http.content_base))
            .header("x-boss-user", identity::header(&signer))
            .send()
            .await?;
        if !response.status().is_success() {
            bail!(
                "existing attachment readback refused: {}",
                response.status()
            );
        }
        let actual = response.bytes().await?;
        if actual.as_ref() != bytes {
            bail!("existing attachment hash/size claim disagrees with complete byte readback");
        }
        return Ok(Some(crate::attach::Attached {
            file_id: id,
            sha256: sha,
            size_bytes: bytes.len() as u64,
            filename: text("filename")?,
            uploaded_by: text("uploaded_by")?,
            reused: true,
        }));
    }
    Ok(None)
}

async fn request_bytes(http: &Doors<'_>, intent: &LaunchIntent) -> Result<Vec<u8>> {
    let bytes = verified_bytes(http, &intent.file_id, &intent.sha256, intent.size_bytes).await?;
    let value: Value = serde_json::from_slice(&bytes)?;
    if value.pointer("/metadata/name").and_then(Value::as_str) != Some(&intent.name)
        || value.pointer("/metadata/namespace").and_then(Value::as_str) != Some(&intent.namespace)
    {
        bail!("immutable launch manifest identity mismatch");
    }
    Ok(bytes)
}

async fn verified_bytes(
    http: &Doors<'_>,
    file_id: &str,
    sha256: &str,
    size_bytes: u64,
) -> Result<Vec<u8>> {
    let content = &http.content_base;
    let signer = identity::apply(identity::signature_for(
        &reqwest::Method::GET,
        "/api/files",
        http.caller.clone(),
    ))?;
    let response = http
        .http
        .get(format!("{content}/api/files/{file_id}"))
        .header("x-boss-user", identity::header(&signer))
        .send()
        .await?;
    if !response.status().is_success() {
        bail!(
            "immutable launch manifest read failed: {}",
            response.status()
        );
    }
    let bytes = response.bytes().await?.to_vec();
    if bytes.len() as u64 != size_bytes || hex::encode(Sha256::digest(&bytes)) != sha256 {
        bail!("immutable launch manifest byte/hash mismatch");
    }
    Ok(bytes)
}

async fn record_native(
    http: &Doors<'_>,
    intent: &LaunchIntent,
    native: &Value,
    artifact: &crate::attach::Attached,
    // The bay this launcher took and the bound in force (backlog
    // d777b20a), written beside `launched_at` in the same conditional
    // write. None where the observation is recorded by a reader that took
    // no bay — a resumed launch; it then leaves the key as it found it.
    bay: Option<&Value>,
) -> Result<()> {
    let uid = native
        .pointer("/metadata/uid")
        .and_then(Value::as_str)
        .filter(|uid| !uid.is_empty())
        .context("native Job UID absent")?;
    let created = native
        .pointer("/metadata/creationTimestamp")
        .and_then(Value::as_str)
        .filter(|at| model::instant(at).is_some())
        .context("native Job creation timestamp absent/unreadable")?;
    for _ in 0..8 {
        let row = read_on(http, &intent.packet).await?;
        let md = row
            .get("metadata")
            .context("gate packet metadata missing")?;
        if md.get("gate_orphan_claim").is_some() {
            bail!(
                "native object conflicts with an original guarded negative decision; complete observation retained without consumption"
            );
        }
        let recorded = boss_jobs::metadata_immutability::view(md, model::INTENT, "", 1, &[])
            .map_err(anyhow::Error::msg)?;
        if md.get(model::INTENT).is_none() || recorded != &serde_json::to_value(intent)? {
            bail!("native observation lacks the exact original protected intent");
        }
        let previous = attempt::read(md)?
            .context("native observation has no original guarded sending attempt")?;
        if previous.state != attempt::State::Sending
            || previous.intent_sha256 != intent.sha256
            || previous.intent_file_id != intent.file_id
        {
            bail!("native object contradicts the original guarded sending attempt");
        }
        if let Some(observed) = md.get(model::OBSERVATION) {
            if !model::native_observed(md) {
                bail!("stored native observation lacks its original protected binding");
            }
            if observed.get("uid").and_then(Value::as_str) == Some(uid)
                && observed.get("request_sha256").and_then(Value::as_str)
                    == Some(intent.sha256.as_str())
                && observed.get("created_at").and_then(Value::as_str) == Some(created)
            {
                return Ok(());
            }
            bail!(
                "a different native UID or request was already observed; immutable identity cannot be replaced"
            );
        }
        let mut command: Command = serde_json::from_value(
            json!({"contract_version":1,"idempotency_key":Uuid::new_v4(),
            "expected":{"gate_orphan_claim":{"present":false},model::INTENT:{"present":true,"value":intent},model::OBSERVATION:{"present":false},"launched_at":{"present":md.get("launched_at").is_some(),"value":md.get("launched_at").cloned().unwrap_or(Value::Null)}},
            "patch":{model::OBSERVATION:{"uid":uid,"name":intent.name,"namespace":intent.namespace,"created_at":created,"request_sha256":intent.sha256,"native_file_id":artifact.file_id,"native_sha256":artifact.sha256},"launched_at":created},"immutable_keys":[model::OBSERVATION]}),
        )?;
        if let Some(bay) = bay {
            command.patch.insert("gate_bay".into(), bay.clone());
        }
        match apply_on(http, &intent.packet, &command).await {
            Ok(_) => return Ok(()),
            Err(error) if format!("{error:#}").contains("409") => continue,
            Err(error) => return Err(error),
        }
    }
    bail!(
        "native Job exists but its accepted identity has not been recorded; reconciliation remains owed"
    )
}

async fn existing_on(http: &Doors<'_>, namespace: &str, packet: &str) -> Result<Option<String>> {
    let jobs = native_jobs(http, namespace, packet).await?;
    if jobs.is_empty() {
        return Ok(None);
    }
    if jobs.len() != 1 {
        bail!(
            "multiple native Jobs carry this packet; no identity or verdict may be chosen arbitrarily: {jobs:?}"
        );
    }
    let row = read_on(http, packet).await?;
    if row.pointer("/metadata/gate_orphan_claim").is_some() {
        bail!(
            "native object contradicts an original guarded negative decision; retained for reconciliation, never consumed"
        );
    }
    require_protocol(http, &row).await?;
    submission::require_owner_on(
        http,
        packet,
        row.get("metadata").context("gate packet lacks metadata")?,
    )
    .await?;
    let generation =
        reservation_generation(row.get("metadata").context("gate packet has no metadata")?)?;
    let value=row.pointer("/metadata/gate_launch_intent").context("legacy Job has no immutable launch identity; reconcile the retained native record before migration")?;
    let intent: LaunchIntent = serde_json::from_value(value.clone())?;
    if intent.packet != packet
        || intent.namespace != namespace
        || intent.contract_version != 1
        || intent.name != job_name(packet)?
    {
        bail!("immutable native launch identity does not match this packet");
    }
    let bytes = request_bytes(http, &intent).await?;
    let output = native_read(http, namespace, &["get", "job", &intent.name, "-o", "json"]).await?;
    let artifact=retain(http,packet,&serde_json::to_vec(&json!({"intent":intent,"status":output.status.code(),"stdout":output.stdout,"stderr":output.stderr}))?).await?;
    if !output.status.success() {
        bail!(
            "exact recorded Job identity is unreadable: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
    let native: Value = serde_json::from_slice(&output.stdout)?;
    let desired: Value = serde_json::from_slice(&bytes)?;
    if !contains_desired(&native, &desired)
        || native
            .pointer("/metadata/uid")
            .and_then(Value::as_str)
            .is_none()
    {
        bail!("native retained Job differs from its immutable request");
    }
    record_native(http, &intent, &native, &artifact, None).await?;
    consume_on(http, packet, generation).await?;
    Ok(Some(format!("job.batch/{}", intent.name)))
}

pub(super) fn execution_mode(mode: &str) -> Result<(String, String)> {
    let argument = normalize_mode(if mode == RECORDED_FULL_MODE { "" } else { mode })?;
    let recorded = if argument.is_empty() {
        RECORDED_FULL_MODE.to_owned()
    } else {
        argument.clone()
    };
    Ok((recorded, argument))
}

/// PURE: the JSON pointer of the ONE container that runs the gate — the
/// one declaring `GATE_BRANCH`, in EITHER list of the pod template. On the
/// judged-init layout (backlog 3e2c8ddc; design b4bf8bba) the gate is an
/// init container after the postgres sidecar and the one regular container
/// is the `seed-refresh` helper, so a reader of `containers` alone finds
/// the helper. A lone regular container that declares nothing is still
/// taken, as before, unless an init container is named `gate`.
pub(super) fn gate_executor(job: &Value) -> Result<String> {
    let spec = job
        .pointer("/spec/template/spec")
        .context("native gate request has no containers")?;
    let list = |collection: &str| {
        spec.get(collection)
            .and_then(Value::as_array)
            .map(Vec::as_slice)
            .unwrap_or_default()
    };
    let declared: Vec<String> = ["initContainers", "containers"]
        .into_iter()
        .flat_map(|collection| {
            list(collection)
                .iter()
                .enumerate()
                .filter(|(_, container)| {
                    container
                        .get("env")
                        .and_then(Value::as_array)
                        .is_some_and(|env| {
                            env.iter().any(|entry| {
                                entry.get("name").and_then(Value::as_str) == Some("GATE_BRANCH")
                            })
                        })
                })
                .map(move |(index, _)| format!("/spec/template/spec/{collection}/{index}"))
        })
        .collect();
    let init_gate = list("initContainers")
        .iter()
        .any(|c| c.get("name").and_then(Value::as_str) == Some("gate"));
    match (declared.as_slice(), list("containers").len()) {
        ([one], _) => Ok(one.clone()),
        ([], 0) => bail!("native gate request has no containers"),
        ([], 1) if !init_gate => Ok("/spec/template/spec/containers/0".into()),
        _ => bail!("native request does not declare one unambiguous gate executor"),
    }
}

fn bind_execution(
    document: &mut Value,
    metadata: &Value,
    head: &str,
    argument: &str,
) -> Result<()> {
    let pointer = gate_executor(document)?;
    let executor = document
        .pointer_mut(&pointer)
        .and_then(Value::as_object_mut)
        .context("gate executor is not an object")?;
    let env = executor
        .entry("env")
        .or_insert_with(|| json!([]))
        .as_array_mut()
        .context("gate executor environment is not an array")?;
    for (key, value) in [
        ("GATE_SHA", Some(head)),
        ("GATE_MODE", Some(argument)),
        (
            "GATE_BRANCH",
            metadata.get("branch").and_then(Value::as_str),
        ),
    ] {
        let Some(value) = value else { continue };
        let matching: Vec<_> = env
            .iter()
            .enumerate()
            .filter_map(|(index, entry)| {
                (entry.get("name").and_then(Value::as_str) == Some(key)).then_some(index)
            })
            .collect();
        if matching.len() > 1 {
            bail!("gate executor has duplicate {key} bindings");
        }
        let entry = json!({"name":key,"value":value});
        if let Some(index) = matching.first() {
            env[*index] = entry;
        } else {
            env.push(entry);
        }
    }
    Ok(())
}

async fn prepare_on(
    http: &Doors<'_>,
    packet: &str,
    namespace: &str,
    head: &str,
    mode: &str,
    manifest: &str,
) -> Result<(LaunchIntent, Vec<u8>)> {
    if head.len() != 40
        || !head
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        bail!(
            "a native gate request requires one full lowercase commit SHA; no symbolic fallback is executable"
        );
    }
    let (mode, argument) = execution_mode(mode)?;
    let row = read_on(http, packet).await?;
    if let Some(why) = launch_target_refusal(packet, &row) {
        bail!(why);
    }
    let md = row.get("metadata").context("gate packet has no metadata")?;
    if let Some(value) = md.get(model::INTENT) {
        let mut intent: LaunchIntent =
            serde_json::from_value(value.clone()).context("unreadable immutable launch intent")?;
        if intent.contract_version != 1
            || intent.packet != packet
            || intent.namespace != namespace
            || intent.name != job_name(packet)?
            || intent.head != head
            || intent.mode != mode
        {
            bail!("existing launch intent differs; no second native identity will be created");
        }
        let bytes = request_bytes(http, &intent).await?;
        intent.transition_generation = Some(reservation_generation(md)?);
        return Ok((intent, bytes));
    }
    if md.get("gate_orphan_claim").is_some() {
        bail!("orphan settlement already claimed this gate packet");
    }
    // ALL retained legacy Jobs count, including failed/succeeded ones.
    // A legacy gate is attached through its existing path; it is never
    // upgraded into another randomly named execution.
    if !native_jobs(http, namespace, packet).await?.is_empty() {
        bail!(
            "legacy native Job exists; reconcile its terminal/identity before migrating launch intent"
        );
    }
    let mut desired = client_document(http, packet, namespace, manifest).await?;
    bind_execution(&mut desired, md, head, &argument)?;
    let name = job_name(packet)?;
    finish_request(&mut desired, &name, namespace)?;
    let bytes = serde_json::to_vec(&desired)?;
    let artifact = retain(http, packet, &bytes).await?;
    let intent = LaunchIntent {
        contract_version: 1,
        packet: packet.into(),
        namespace: namespace.into(),
        name,
        head: head.into(),
        mode,
        transition_generation: Some(reservation_generation(md)?),
        file_id: artifact.file_id,
        sha256: artifact.sha256,
        size_bytes: artifact.size_bytes,
    };
    // Intent and the FIRST sending attempt bind in one conditional
    // command immediately before spawn. An interrupted preparation
    // must not leave an intent whose prior-send history is unknowable.
    Ok((intent, bytes))
}

#[derive(Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum NativeCreate {
    NotSpawned {
        program: String,
        args: Vec<String>,
        os_error: String,
    },
    Spawned {
        status: Option<i32>,
        signal: Option<i32>,
        stdout: Vec<u8>,
        stderr: Vec<u8>,
        input_error: Option<String>,
        wait_error: Option<String>,
        stdout_error: Option<String>,
        stderr_error: Option<String>,
    },
}

async fn read_pipe<R: tokio::io::AsyncRead + Unpin>(
    pipe: Option<R>,
    deadline: tokio::time::Instant,
) -> (Vec<u8>, Option<String>) {
    use tokio::io::AsyncReadExt;
    let mut bytes = Vec::new();
    let error = match pipe {
        Some(mut pipe) => match tokio::time::timeout_at(deadline, pipe.read_to_end(&mut bytes))
            .await
        {
            Ok(result) => result.err().map(|error| error.to_string()),
            Err(_) => Some(
                "native pipe deadline elapsed; all observed bytes retained, completeness UNKNOWN"
                    .into(),
            ),
        },
        None => Some("native output pipe unavailable".into()),
    };
    (bytes, error)
}

/// Only the OS spawn boundary can produce NotSpawned. Once a child
/// exists, every later error remains possibly effective with full bytes.
async fn native_create(command: std::process::Command, bytes: &[u8]) -> NativeCreate {
    native_create_with_bound(command, bytes, std::time::Duration::from_secs(45)).await
}

fn native_output(observation: NativeCreate) -> Result<(std::process::Output, Option<String>)> {
    use std::os::unix::process::ExitStatusExt;
    match observation {
        NativeCreate::Spawned {
            status,
            signal,
            stdout,
            stderr,
            input_error,
            wait_error: None,
            stdout_error: None,
            stderr_error: None,
        } if status.is_some() || signal.is_some() => Ok((
            std::process::Output {
                status: std::process::ExitStatus::from_raw(
                    status
                        .map(|code| code << 8)
                        .unwrap_or_else(|| signal.unwrap_or_default()),
                ),
                stdout,
                stderr,
            },
            input_error,
        )),
        original => bail!(
            "native process observation UNKNOWN: {}",
            serde_json::to_string(&original)?
        ),
    }
}

async fn native_create_with_bound(
    mut command: std::process::Command,
    bytes: &[u8],
    bound: std::time::Duration,
) -> NativeCreate {
    use std::os::unix::process::ExitStatusExt;
    use tokio::io::AsyncWriteExt;
    let program = command.get_program().to_string_lossy().into_owned();
    let args = command
        .get_args()
        .map(|arg| arg.to_string_lossy().into_owned())
        .collect();
    command
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut command = tokio::process::Command::from(command);
    command.kill_on_drop(true).process_group(0);
    let mut child = match command.spawn() {
        Ok(child) => child,
        Err(error) => {
            return NativeCreate::NotSpawned {
                program,
                args,
                os_error: error.to_string(),
            };
        }
    };
    let input = child.stdin.take();
    let stdout = child.stdout.take();
    let stderr = child.stderr.take();
    // Drain concurrently through the deadline and a bounded kill/reap
    // grace. Buffers live outside the cancellable read future, so even
    // failed cancellation preserves every observed byte explicitly.
    let capture_deadline = tokio::time::Instant::now() + bound + std::time::Duration::from_secs(2);
    let (input_error, (stdout, stdout_error), (stderr, stderr_error), wait) = tokio::join!(
        async {
            match input {
                Some(mut input) => {
                    match tokio::time::timeout(bound, input.write_all(bytes)).await {
                        Ok(result) => result.err().map(|error| error.to_string()),
                        Err(_) => Some("native input deadline elapsed".into()),
                    }
                }
                None => Some("native input pipe unavailable".into()),
            }
        },
        read_pipe(stdout, capture_deadline),
        read_pipe(stderr, capture_deadline),
        async {
            match tokio::time::timeout(bound, child.wait()).await {
                Ok(result) => (result, None),
                Err(_) => {
                    // The leader remains alive and unreaped: its group
                    // cannot have been recycled for another process.
                    let mut reason = format!(
                        "native process deadline elapsed after {}ms; outcome possibly effective",
                        bound.as_millis()
                    );
                    if let Some(pid) = child.id() {
                        let group = format!("-{pid}");
                        let mut kill = tokio::process::Command::new("kill");
                        kill.args(["-KILL", "--", &group]).kill_on_drop(true);
                        match tokio::time::timeout(std::time::Duration::from_secs(1), kill.output())
                            .await
                        {
                            Ok(Ok(output)) if output.status.success() => {}
                            other => {
                                reason.push_str(&format!("; process group cancellation: {other:?}"))
                            }
                        }
                    }
                    if let Err(error) = child.start_kill() {
                        reason.push_str(&format!("; child cancellation: {error}"));
                    }
                    let reaped =
                        tokio::time::timeout(std::time::Duration::from_secs(1), child.wait())
                            .await
                            .unwrap_or_else(|_| {
                                Err(std::io::Error::new(
                                    std::io::ErrorKind::TimedOut,
                                    "native reap deadline elapsed",
                                ))
                            });
                    (reaped, Some(reason))
                }
            }
        },
    );
    let (wait, deadline_error) = wait;
    let (status, signal, wait_error) = match wait {
        Ok(status) => (status.code(), status.signal(), None),
        Err(error) => (None, None, Some(error.to_string())),
    };
    let wait_error = match (wait_error, deadline_error) {
        (Some(wait), Some(deadline)) => Some(format!("{deadline}; {wait}")),
        (wait, deadline) => wait.or(deadline),
    };
    NativeCreate::Spawned {
        status,
        signal,
        stdout,
        stderr,
        input_error,
        wait_error,
        stdout_error,
        stderr_error,
    }
}

async fn verify_previous_no_child(
    http: &Doors<'_>,
    intent: &LaunchIntent,
    previous: &attempt::Attempt,
) -> Result<()> {
    let proof = previous
        .no_child
        .as_ref()
        .context("not_started lacks its retained OS-spawn proof")?;
    let bytes = verified_bytes(http, &proof.file_id, &proof.sha256, proof.size_bytes).await?;
    let value: Value = serde_json::from_slice(&bytes)?;
    let mut sending = previous.clone();
    sending.state = attempt::State::Sending;
    sending.no_child = None;
    if value.get("operation").and_then(Value::as_str) != Some("native-create")
        || value.get("intent") != Some(&serde_json::to_value(intent)?)
        || value.get("attempt") != Some(&serde_json::to_value(sending)?)
        || value.pointer("/outcome/kind").and_then(Value::as_str) != Some("not_spawned")
        || value
            .pointer("/outcome/os_error")
            .and_then(Value::as_str)
            .is_none_or(str::is_empty)
    {
        bail!("retained no-child evidence does not prove this exact prior spawn refusal");
    }
    Ok(())
}

/// One possibly effective send. Rejoined invocations can only reconcile
/// the matching retained native object or report UNKNOWN.
async fn create_on(
    http: &Doors<'_>,
    intent: &LaunchIntent,
    bytes: &[u8],
    now: chrono::DateTime<chrono::Utc>,
) -> Result<String> {
    if bytes.len() as u64 != intent.size_bytes
        || hex::encode(Sha256::digest(bytes)) != intent.sha256
    {
        bail!("native request bytes differ from the immutable launch intent");
    }
    let row = read_on(http, &intent.packet).await?;
    let metadata = row
        .get("metadata")
        .context("native request lost its packet metadata")?;
    let generation = intent
        .transition_generation
        .context("create transition lacks its observed reservation generation")?;
    if reservation_generation(metadata)? != generation {
        bail!("create transition no longer owns its observed reservation generation");
    }
    if let Some(submission) = submission::require_owner_on(http, &intent.packet, metadata).await?
        && serde_json::to_value(intent)? != serde_json::to_value(&submission.intent)?
    {
        bail!("native request differs from its one immutable durable submission");
    }
    if let Some(recorded) = row.pointer("/metadata/gate_launch_intent") {
        let mut original: LaunchIntent = serde_json::from_value(recorded.clone())?;
        let mut requested = intent.clone();
        // Two preparations may retain identical bytes under different
        // file ids. Those artifact copies do not change the request;
        // every other serialized field must still agree exactly.
        requested.file_id = original.file_id.clone();
        requested.transition_generation = None;
        original.transition_generation = None;
        if requested != original {
            bail!(
                "the requested head/mode/native bytes differ from the recorded immutable request"
            );
        }
    }
    if let Some(existing) = existing_on(http, &intent.namespace, &intent.packet).await? {
        return Ok(existing);
    }
    let row = read_on(http, &intent.packet).await?;
    if let Some(why) = launch_target_refusal(&intent.packet, &row) {
        bail!(why);
    }
    require_protocol(http, &row).await?;
    let md = row
        .get("metadata")
        .context("gate packet metadata unavailable before sending claim")?;
    if let Some(previous) = attempt::read(md)?
        && previous.state == attempt::State::NotStarted
    {
        verify_previous_no_child(http, intent, &previous).await?;
    }
    let actor = submission::canonical_actor(http, &intent.packet).await?;
    if let Some(submission) = submission::require_owner_on(http, &intent.packet, md).await? {
        let mut reads = Vec::new();
        let eligible = source_claim::verify(
            http,
            &Value::Object(submission.source),
            &submission.submitted_by,
            &mut reads,
        )
        .await;
        let evidence = retain(
            http,
            &intent.packet,
            &serde_json::to_vec(&json!({
                "operation":"source-claim-before-native-send","reads":reads,
                "error":eligible.as_ref().err().map(|error|format!("{error:#}"))
            }))?,
        )
        .await?;
        eligible.with_context(|| {
            format!(
                "original source is ineligible; complete observation {} sha256 {}",
                evidence.file_id, evidence.sha256
            )
        })?;
    }
    // THE BAY, TAKEN BEFORE ANYTHING IS SENT (backlog ac8d3157). Every
    // launcher — `boss gate`, the conductor's train and dock gates, the
    // resident supervisor — reaches the native create only through here,
    // so the bound holds for all of them at the one place a Job is made.
    // Taken before the guarded attempt, so a refusal leaves no attempt to
    // reconcile; given back on every path below that sends nothing.
    let desired: Value =
        serde_json::from_slice(bytes).context("native request bytes are not a Job document")?;
    let (bay, bay_record) =
        bay::reserve_on(http, &intent.namespace, &intent.packet, &desired, now).await?;
    let permit = async {
        let claim = attempt::Claim::originate(md, intent, actor)?;
        let receipt = apply_on(http, &intent.packet, claim.command()).await?;
        claim.confirm(&receipt)
    }
    .await;
    let permit = match permit {
        Ok(permit) => permit,
        Err(e) => {
            bay::release_on(http, &intent.namespace, &bay, now).await;
            return Err(e);
        }
    };
    // Spending consumes this non-serializable value even when spawn
    // fails. No post-spawn error path can obtain it a second time.
    let sending = permit.spend();
    let mut command = native_command(http, &intent.namespace);
    // The live constructor fixes this to the supported kubectl door.
    // Explicit qualification dependencies exercise the actual OS spawn
    // boundary without changing process-global PATH or credentials.
    if let Some(program) = &http.create_program {
        let mut selected = std::process::Command::new(program);
        selected.args(command.get_args());
        command = selected;
    }
    command.args(["create", "-f", "-"]);
    let outcome = native_create(command, bytes).await;
    let evidence =
        json!({"operation":"native-create","intent":intent,"attempt":sending,"outcome":outcome});
    // Retain full output BEFORE reducing it to a verdict. Even a failed
    // evidence upload keeps complete bytes on disk and refuses progress.
    let artifact = retain(http, &intent.packet, &serde_json::to_vec(&evidence)?).await?;
    eprintln!(
        "boss gate: complete create observation file {} sha256 {}",
        artifact.file_id, artifact.sha256
    );
    if matches!(outcome, NativeCreate::NotSpawned { .. }) {
        // Proved never started: the bay goes back now, not at its grace.
        bay::release_on(http, &intent.namespace, &bay, now).await;
        let row = read_on(http, &intent.packet).await?;
        let md = row
            .get("metadata")
            .context("packet unavailable after proved spawn refusal")?;
        let command = attempt::no_child_command(
            md,
            &sending,
            attempt::Proof {
                file_id: artifact.file_id,
                sha256: artifact.sha256,
                size_bytes: artifact.size_bytes,
            },
        )?;
        apply_on(http, &intent.packet, &command).await?;
        bail!(
            "native OS spawn was proved not started; the exact same intent may be retried through a new guarded claim"
        );
    }
    let read = native_read(
        http,
        &intent.namespace,
        &[
            "get",
            "job",
            &intent.name,
            "--ignore-not-found",
            "-o",
            "json",
        ],
    )
    .await
    .context("reading exact immutable Job identity after create")?;
    let native_artifact=retain(http,&intent.packet,&serde_json::to_vec(&json!({"intent":intent,"status":read.status.code(),"stdout":read.stdout,"stderr":read.stderr}))?).await?;
    if !read.status.success() {
        bail!(
            "native create outcome UNKNOWN; exact Job read refused: {}",
            String::from_utf8_lossy(&read.stderr)
        );
    }
    if read.stdout.is_empty() {
        bail!(
            "native create outcome UNKNOWN; exact Job {} is absent but the possibly effective send cannot be repeated",
            intent.name
        );
    }
    let native: Value = serde_json::from_slice(&read.stdout)?;
    let desired: Value = serde_json::from_slice(bytes)?;
    if !contains_desired(&native, &desired)
        || native
            .pointer("/metadata/uid")
            .and_then(Value::as_str)
            .is_none()
    {
        bail!("native Job at the immutable name does not match the complete recorded request");
    }
    record_native(http, intent, &native, &native_artifact, Some(&bay_record)).await?;
    let _ = now;
    consume_on(
        http,
        &intent.packet,
        intent
            .transition_generation
            .context("create transition lacks its observed reservation generation")?,
    )
    .await?;
    Ok(format!("job.batch/{}", intent.name))
}

fn supports_contract(answer: &Value) -> bool {
    answer.get("contract_version").and_then(Value::as_u64) == Some(2)
        && answer.get("guarded_bindings").and_then(Value::as_u64) == Some(1)
        && answer.get("checked_pair_replay").and_then(Value::as_u64) == Some(1)
        && answer.get("immutable_bindings").and_then(Value::as_u64) == Some(1)
        && answer
            .get("pinned_immutable_fields")
            .and_then(Value::as_u64)
            == Some(1)
}

async fn capability_on(http: &Doors<'_>) -> Result<()> {
    let answer = api(
        http,
        reqwest::Method::GET,
        "/api/jobs/metadata/conditional-capability",
        None,
    )
    .await?;
    if !answer.as_ref().is_some_and(supports_contract) {
        bail!(
            "conditional metadata contract v1 is unverified; no legacy write fallback or gate launch is safe"
        );
    }
    Ok(())
}

async fn read_on(http: &Doors<'_>, packet: &str) -> Result<Value> {
    api(
        http,
        reqwest::Method::GET,
        &format!("/api/jobs/{packet}"),
        None,
    )
    .await?
    .context("gate packet read returned no body")
}

async fn apply_on(http: &Doors<'_>, packet: &str, command: &Command) -> Result<Receipt> {
    let payload = serde_json::to_value(command)?;
    let mut retry = 0;
    let value = loop {
        match api(
            http,
            reqwest::Method::POST,
            &format!("/api/jobs/{packet}/metadata/conditional"),
            Some(payload.clone()),
        )
        .await
        {
            Ok(value) => break value.context("conditional metadata returned no receipt")?,
            Err(error) => {
                // The generic door retains the entire service refusal in
                // its error. Retry only transport failures and its named
                // transient statuses, always using THIS command identity.
                let message = format!("{error:#}");
                let transient = error.chain().any(|cause| {
                    cause.downcast_ref::<reqwest::Error>().is_some_and(|error| {
                        error.is_timeout()
                            || error.is_connect()
                            || error.is_body()
                            || error.is_request()
                    })
                }) || [
                    " -> 408 ", " -> 429 ", " -> 500 ", " -> 502 ", " -> 503 ", " -> 504 ",
                ]
                .iter()
                .any(|status| message.contains(status));
                if !transient || retry == 2 {
                    return Err(error);
                }
                retry += 1;
                eprintln!(
                    "boss gate: conditional reply unavailable; retry {retry}/2 of identical command {}: {message}",
                    command.idempotency_key
                );
                tokio::time::sleep(std::time::Duration::from_millis(100 * retry)).await;
            }
        }
    };
    let receipt: Receipt = serde_json::from_value(value)
        .context("conditional metadata response is not a v1 receipt")?;
    if receipt.contract_version != command.contract_version
        || receipt.command != *command
        || receipt.job_id.to_string() != packet
    {
        bail!("conditional metadata receipt does not match the exact command and packet");
    }
    Ok(receipt)
}

async fn renew_on(
    http: &Doors<'_>,
    packet: &str,
    now: chrono::DateTime<chrono::Utc>,
) -> Result<Uuid> {
    for _ in 0..8 {
        let row = read_on(http, packet).await?;
        if let Some(why) = launch_target_refusal(packet, &row) {
            bail!(why);
        }
        require_protocol(http, &row).await?;
        let md = row.get("metadata").context("gate packet has no metadata")?;
        if md.get("gate_orphan_claim").is_some() {
            bail!(
                "gate packet has an accepted orphan settlement; no queue reservation may revive it"
            );
        }
        let command = model::renew(md, now).map_err(anyhow::Error::msg)?;
        match apply_on(http, packet, &command).await {
            Ok(receipt) => {
                return model::read(&receipt.job.metadata)
                    .map_err(anyhow::Error::msg)?
                    .map(|r| r.generation)
                    .context("renew receipt has no reservation");
            }
            Err(error) if format!("{error:#}").contains("409") => continue,
            Err(error) => return Err(error),
        }
    }
    bail!("gate reservation changed eight times; no place was overwritten")
}

async fn consume_on(http: &Doors<'_>, packet: &str, generation: Uuid) -> Result<()> {
    for _ in 0..8 {
        let row = read_on(http, packet).await?;
        let md = row.get("metadata").context("gate packet has no metadata")?;
        let current = model::read(md)
            .map_err(anyhow::Error::msg)?
            .context("no versioned gate reservation")?;
        if current.generation != generation {
            bail!("a replacement gate reservation exists; stale launch cannot consume it");
        }
        if current.consumed {
            return Ok(());
        }
        let command = model::consume(md, generation).map_err(anyhow::Error::msg)?;
        match apply_on(http, packet, &command).await {
            Ok(_) => return Ok(()),
            Err(error) if format!("{error:#}").contains("409") => continue,
            Err(error) => return Err(error),
        }
    }
    bail!(
        "gate reservation changed during consume; native Job exists but queue reconciliation is unpaid"
    )
}

async fn require_protocol(http: &Doors<'_>, row: &Value) -> Result<()> {
    let version = row
        .get("workflow_version")
        .and_then(Value::as_i64)
        .context("packet lacks its pinned protocol version")?;
    let spec = api(
        http,
        reqwest::Method::GET,
        &format!("/api/workflows/gate-run/versions/{version}"),
        None,
    )
    .await?
    .context("pinned gate protocol read returned no body")?;
    let fields=spec.pointer("/metadata_schema/immutable_fields").and_then(Value::as_array)
        .context("pinned gate protocol lacks immutable first-record declarations; compatible protocol rollout remains required")?;
    if !model::FIRST_RECORD_FIELDS
        .iter()
        .all(|key| fields.iter().any(|field| field.as_str() == Some(key)))
    {
        bail!(
            "pinned gate protocol lacks a required native first record; no legacy launch fallback is safe"
        );
    }
    if !spec
        .pointer("/metadata_schema/guarded_fields")
        .and_then(Value::as_array)
        .is_some_and(|fields| {
            fields
                .iter()
                .any(|field| field.as_str() == Some(attempt::KEY))
        })
    {
        bail!("pinned gate protocol lacks the persistent native-attempt guard");
    }
    Ok(())
}

fn reservation_generation(metadata: &Value) -> Result<Uuid> {
    Ok(model::read(metadata)
        .map_err(anyhow::Error::msg)?
        .context("transition requires a versioned reservation")?
        .generation)
}

#[cfg(test)]
mod native_tests;

#[cfg(test)]
mod tests {
    use super::*;

    /// A census Job as the Kubernetes API itself lists it: `metadata`,
    /// `spec` and `status` only. Items of a typed list carry no `kind`
    /// or `apiVersion` — those live once, on the list; `kubectl get -o
    /// json` adds them back, the raw GET the census makes does not.
    /// Measured 2026-10-20 on boss-dev: `kubectl get --raw
    /// /apis/batch/v1/namespaces/boss-dev/jobs?labelSelector=app=gate-runner`
    /// answered a JobList whose item keys were exactly these three.
    fn listed_job(name: &str, namespace: &str, uid: &str) -> Value {
        json!({"metadata": {"name": name, "namespace": namespace, "uid": uid,
                            "labels": {"app": "gate-runner"}},
               "spec": {}, "status": {}})
    }

    fn job_list(items: Vec<Value>) -> Value {
        json!({"apiVersion": "batch/v1", "kind": "JobList",
               "metadata": {"resourceVersion": "46352202"}, "items": items})
    }

    /// THE CENSUS READS THE API'S OWN SHAPE (backlog d561255c). Every
    /// resident supervisor pass since policy v10 (2026-10-20 ~20:35Z)
    /// refused "unbound or repeated Job" because the per-item kind check
    /// demanded a `kind` the API never puts on a list item, so the first
    /// Job of a non-empty census refused the whole pass and no submission
    /// launched. The fixture apiserver listed its stored Jobs WITH kind,
    /// which is why the car that added the check went green.
    #[test]
    fn a_job_list_as_the_api_answers_it_is_one_bound_census() {
        let list = job_list(vec![
            listed_job("gate-a", "boss-dev", "uid-a"),
            listed_job("gate-b", "boss-dev", "uid-b"),
        ]);
        census_bound(&list, "boss-dev").unwrap();
        // A Job that does state its type, as kubectl's rebuilt list does,
        // still binds.
        let mut typed = listed_job("gate-c", "boss-dev", "uid-c");
        typed["kind"] = json!("Job");
        typed["apiVersion"] = json!("batch/v1");
        census_bound(&job_list(vec![typed]), "boss-dev").unwrap();
    }

    /// Each refusal NAMES the Job and why: the blanket sentence it replaced
    /// named nothing, and that cost the d561255c diagnosis an hour of a
    /// dead gate queue.
    #[test]
    fn an_unbound_or_repeated_job_is_refused_by_name_and_reason() {
        let refusal = |items: Vec<Value>| {
            format!(
                "{:#}",
                census_bound(&job_list(items), "boss-dev").unwrap_err()
            )
        };
        let mut pod = listed_job("gate-a", "boss-dev", "uid-a");
        pod["kind"] = json!("Pod");
        let said = refusal(vec![pod]);
        assert!(said.contains("gate-a") && said.contains("Pod"), "{said}");
        let mut foreign = listed_job("gate-a", "boss-dev", "uid-a");
        foreign["apiVersion"] = json!("batch/v2");
        let said = refusal(vec![foreign]);
        assert!(
            said.contains("gate-a") && said.contains("batch/v2"),
            "{said}"
        );
        let said = refusal(vec![listed_job("gate-a", "elsewhere", "uid-a")]);
        assert!(
            said.contains("gate-a") && said.contains("elsewhere") && said.contains("namespace"),
            "{said}"
        );
        let said = refusal(vec![
            listed_job("gate-a", "boss-dev", "uid-a"),
            listed_job("gate-a", "boss-dev", "uid-b"),
        ]);
        assert!(said.contains("gate-a") && said.contains("name"), "{said}");
        let said = refusal(vec![
            listed_job("gate-a", "boss-dev", "uid-a"),
            listed_job("gate-b", "boss-dev", "uid-a"),
        ]);
        assert!(
            said.contains("gate-b") && said.contains("uid-a") && said.contains("uid"),
            "{said}"
        );
        // The list itself must still be one complete batch/v1 JobList.
        let mut untyped = job_list(vec![listed_job("gate-a", "boss-dev", "uid-a")]);
        untyped["kind"] = json!("List");
        assert!(census_bound(&untyped, "boss-dev").is_err());
    }

    /// THE WRITER AND THE REUSE READER AGREE ON WHAT IS PER-RUN (backlog
    /// bbc13a4e). One dry-rendered Job, finished as a car's full launch
    /// and as a train's `--auto` launch on another head and branch, by the
    /// launch's own `bind_execution` and `finish_request`: the reuse
    /// comparison finds no difference. Handed a different edit level, it
    /// finds one. If a launch ever binds a new per-run value, this fails
    /// rather than leaving every reuse to refuse in silence.
    #[test]
    fn a_car_and_a_train_launch_of_one_manifest_differ_only_in_per_run_values() {
        use crate::train_gate_reuse::{Observed, launch_refusals, sha256_hex};
        let rendered = |branch: &str, packet: &str, level: &str| {
            json!({"apiVersion": "batch/v1", "kind": "Job",
                   "metadata": {"generateName": format!("gate-{}-", branch.replace('/', "-")),
                                "namespace": "boss-dev", "creationTimestamp": null,
                                "labels": {"app": "gate-runner", "boss.dev/packet": packet,
                                           "boss.dev/branch": branch.replace('/', "-")}},
                   "spec": {"template": {"metadata": {"creationTimestamp": null},
                            "spec": {"initContainers": [{"name": "gate", "image": "ci:1",
                                "env": [{"name": "GATE_BRANCH", "value": branch},
                                        {"name": "GATE_RUN_JOB_ID", "value": packet},
                                        {"name": "GATE_MODE", "value": "$GATE_MODE"},
                                        {"name": "BOSS_EDIT_LEVEL_ANSWER", "value": level}]}]}}}})
        };
        let finish = |mut doc: Value, branch: &str, packet: &str, head: &str, mode: &str| {
            let (_, argument) = execution_mode(mode).unwrap();
            bind_execution(&mut doc, &json!({"branch": branch}), head, &argument).unwrap();
            finish_request(&mut doc, &job_name(packet).unwrap(), "boss-dev").unwrap();
            serde_json::to_string(&doc).unwrap()
        };
        let (car_head, train_head) = ("a".repeat(40), "b".repeat(40));
        let car_packet = "9d6b3ae3-cf1e-411e-ab6d-f2a0b7780f18";
        let train_packet = "7f0c3a52-0d4e-4d7e-9a35-6d4f5b1e2c11";
        let level = "{\"edit_level\":null}";
        let car = finish(
            rendered("fix/a", car_packet, level),
            "fix/a",
            car_packet,
            &car_head,
            RECORDED_FULL_MODE,
        );
        let sha = sha256_hex(car.as_bytes());
        let judge = |train: String| {
            launch_refusals(
                &Observed::Known(car.clone()),
                &Observed::Known(train),
                Some(&sha),
                &car_head,
            )
        };
        let train = finish(
            rendered("train/20261019-1200", train_packet, level),
            "train/20261019-1200",
            train_packet,
            &train_head,
            "--auto",
        );
        assert_eq!(judge(train), Vec::<String>::new());
        let other_level = finish(
            rendered("train/x", train_packet, "{\"edit_level\":\"data\"}"),
            "train/x",
            train_packet,
            &train_head,
            "--auto",
        );
        assert!(
            judge(other_level)
                .iter()
                .any(|r| r.contains("launch request differs")),
        );
    }

    #[test]
    fn an_observed_gate_job_accepts_only_its_exact_server_defaults() {
        // Retained request/admitted Job from train48bbc0b3, with registry
        // host redacted identically. Every requested field was unchanged;
        // the server additions caused an effective CREATE to read UNKNOWN.
        let desired: Value =
            serde_json::from_str(include_str!("reservation/fixtures/job-requested.json")).unwrap();
        let actual: Value =
            serde_json::from_str(include_str!("reservation/fixtures/job-admitted.json")).unwrap();
        assert!(contains_desired(&actual, &desired));
        for (pointer, changed) in [
            ("/metadata/generation", json!(2)),
            (
                "/spec/template/spec/containers/0/env/8/valueFrom/fieldRef/apiVersion",
                json!("v2"),
            ),
            (
                "/spec/template/spec/initContainers/0/terminationMessagePath",
                json!("/elsewhere"),
            ),
            (
                "/spec/template/spec/initContainers/0/imagePullPolicy",
                json!("Always"),
            ),
            (
                "/spec/template/spec/containers/0/command",
                json!(["different"]),
            ),
        ] {
            let mut changed_job = actual.clone();
            *changed_job.pointer_mut(pointer).unwrap() = changed;
            assert!(
                !contains_desired(&changed_job, &desired),
                "accepted {pointer}"
            );
        }
        let mut unknown = actual;
        unknown["spec"]["template"]["spec"]["unknownAdmissionField"] = json!(true);
        assert!(!contains_desired(&unknown, &desired));
    }
    #[test]
    fn an_unobserved_pull_policy_default_is_not_accepted_as_identity() {
        // P3-1 of review 8c92f75b (backlog cafa83a5). The comparison also
        // folded `Always` for an untagged or `:latest` init image: a real
        // server default, but never observed — the gate's init image is
        // a tagged mirror (`postgres:16`, pinned by
        // the-build-pulls-only-mirrored-bases.sh) — and untested. Only the
        // observed default folds; an unobserved one reads as a difference,
        // which holds the launch UNKNOWN rather than binding on a guess.
        let mut desired: Value =
            serde_json::from_str(include_str!("reservation/fixtures/job-requested.json")).unwrap();
        let mut actual: Value =
            serde_json::from_str(include_str!("reservation/fixtures/job-admitted.json")).unwrap();
        let init = "/spec/template/spec/initContainers/0";
        assert!(
            desired
                .pointer(&format!("{init}/imagePullPolicy"))
                .is_none()
        );
        for image in [
            "registry.example.invalid/david/postgres",
            "registry.example.invalid/david/postgres:latest",
            "registry.example.invalid:3000/david/postgres",
        ] {
            *desired.pointer_mut(&format!("{init}/image")).unwrap() = json!(image);
            *actual.pointer_mut(&format!("{init}/image")).unwrap() = json!(image);
            *actual
                .pointer_mut(&format!("{init}/imagePullPolicy"))
                .unwrap() = json!("Always");
            assert!(!contains_desired(&actual, &desired), "accepted {image}");
        }
    }
    #[test]
    fn a_full_gate_binds_the_job_whose_empty_mode_the_server_dropped() {
        // Backlog 0c77e4db. A full gate's request carries
        // `{"name":"GATE_MODE","value":""}` (`execution_mode` maps full to
        // the empty argument), and the API server stores that entry as
        // `{"name":"GATE_MODE"}`: an EnvVar's value is omitempty, so "" and
        // absent are one value. Every main full gate and builder full gate
        // since the immutable-name launch read as UNKNOWN while it ran
        // (gate-runs 135d9645, 1c6bc729, d89246c6). The real
        // requested/admitted pair, in the shape those live Jobs had.
        let mut desired: Value =
            serde_json::from_str(include_str!("reservation/fixtures/job-requested.json")).unwrap();
        let mut actual: Value =
            serde_json::from_str(include_str!("reservation/fixtures/job-admitted.json")).unwrap();
        let index = |job: &Value| {
            job["spec"]["template"]["spec"]["containers"][0]["env"]
                .as_array()
                .unwrap()
                .iter()
                .position(|entry| entry["name"] == json!("GATE_MODE"))
                .unwrap()
        };
        let (d, a) = (index(&desired), index(&actual));
        desired["spec"]["template"]["spec"]["containers"][0]["env"][d] =
            json!({"name":"GATE_MODE","value":""});
        actual["spec"]["template"]["spec"]["containers"][0]["env"][a] = json!({"name":"GATE_MODE"});
        assert!(
            contains_desired(&actual, &desired),
            "an empty env value the server dropped is the value that was requested"
        );
        // Only EMPTY is equivalent to absent: a dropped non-empty value, or
        // a different one, is still a different Job.
        let mut requested_auto = desired.clone();
        requested_auto["spec"]["template"]["spec"]["containers"][0]["env"][d] =
            json!({"name":"GATE_MODE","value":"--auto"});
        assert!(!contains_desired(&actual, &requested_auto));
        let mut admitted_other = actual.clone();
        admitted_other["spec"]["template"]["spec"]["containers"][0]["env"][a] =
            json!({"name":"GATE_MODE","value":"--auto"});
        assert!(!contains_desired(&admitted_other, &desired));
    }
    #[tokio::test]
    async fn a_hung_native_child_is_bounded_with_both_original_streams_and_no_no_child_claim() {
        let mut command = std::process::Command::new("sh");
        command.args(["-c", "dd if=/dev/zero bs=65536 count=2 2>/dev/null; dd if=/dev/zero bs=65536 count=2 >&2 2>/dev/null; sleep 60 & wait"]);
        let observed = tokio::time::timeout(
            std::time::Duration::from_secs(2),
            native_create_with_bound(command, &[], std::time::Duration::from_millis(150)),
        )
        .await;
        assert!(
            observed.is_ok(),
            "parent deadline must return despite a child and its pipe-holding descendant"
        );
        match observed.unwrap() {
            NativeCreate::Spawned {
                stdout,
                stderr,
                wait_error,
                ..
            } => {
                assert_eq!(stdout, vec![0; 131072]);
                assert_eq!(stderr, vec![0; 131072]);
                assert!(wait_error.is_some_and(|error| error.contains("deadline")));
            }
            other => panic!(
                "a timed-out started process is possibly effective, not no-child: {}",
                serde_json::to_string(&other).unwrap()
            ),
        }
    }
    #[test]
    fn ambient_native_client_does_not_override_in_cluster_configuration() {
        let client =
            crate::machine_http::MachineClient::unstamped(reqwest::Client::builder()).unwrap();
        let doors = Doors::conductor(&client, "http://127.0.0.1:7900").unwrap();
        let command = native_command(&doors, "fixture");
        assert_eq!(command.get_args().collect::<Vec<_>>(), ["-n", "fixture"]);
    }
    #[test]
    fn a_conditional_api_without_first_binding_enforcement_is_not_compatible() {
        assert!(
            !supports_contract(&json!({"contract_version":1})),
            "a version number alone does not prove immutable first-insertion enforcement"
        );
        assert!(supports_contract(
            &json!({"contract_version":2,"immutable_bindings":1,"pinned_immutable_fields":1,"guarded_bindings":1,"checked_pair_replay":1})
        ));
    }
    #[tokio::test]
    async fn native_process_drains_full_output_while_writing_input() {
        let mut command = std::process::Command::new("sh");
        command.args([
            "-c",
            "dd if=/dev/zero bs=65536 count=2 >&2 2>/dev/null; cat",
        ]);
        let bytes = vec![b'i'; 131072];
        let (output, error) = tokio::time::timeout(
            std::time::Duration::from_secs(10),
            run_with_input(command, &bytes),
        )
        .await
        .unwrap()
        .unwrap();
        assert!(output.status.success());
        assert!(error.is_none());
        assert_eq!(output.stdout, bytes);
        assert_eq!(output.stderr, vec![0; 131072]);
    }
    #[test]
    fn a_new_transition_observes_the_current_reservation() {
        let old = Uuid::new_v4();
        let replacement = Uuid::new_v4();
        let metadata = json!({model::INTENT:{"reservation_generation":old},
            model::KEY:{"contract_version":1,"generation":replacement,
                "queued_at":"2026-10-15T13:00:00Z","heartbeat_at":"2026-10-15T13:00:00Z","consumed":false}});
        assert_eq!(
            reservation_generation(&metadata).unwrap(),
            replacement,
            "a fresh transition observes the current generation before native reconciliation"
        );
    }
    #[test]
    fn native_identity_is_one_full_packet_name_not_a_queue_generation() {
        let packet = "3816ae9b-019c-4916-a41b-1a7a59fbdc3a";
        let first = job_name(packet).unwrap();
        assert_eq!(first, "gate-3816ae9b019c4916a41b1a7a59fbdc3a");
        assert_eq!(job_name(packet).unwrap(), first);
        assert_ne!(
            job_name("3816ae9b-019c-4916-a41b-1a7a59fbdc3b").unwrap(),
            first
        );
        assert!(job_name("not-a-native-packet").is_err());
    }
    /// A judged-init request (backlog 3e2c8ddc; design b4bf8bba): the gate
    /// is the init container after the postgres sidecar and `seed-refresh`
    /// the one regular container.
    fn judged_init_request() -> Value {
        json!({"spec":{"template":{"spec":{
            "initContainers":[
                {"name":"postgres","image":"forge/postgres:16",
                 "env":[{"name":"POSTGRES_USER","value":"boss"}]},
                {"name":"gate","image":"forge/boss-ci:gate",
                 "env":[{"name":"GATE_BRANCH","value":"rendered"},
                        {"name":"GATE_MODE","value":"--auto"}]}],
            "containers":[
                {"name":"seed-refresh","image":"forge/boss-ci:helper",
                 "env":[{"name":"NATIVE_JOB_NAME","value":"x"}]}]}}}})
    }

    /// THE FROZEN EXECUTION BINDS INTO THE CONTAINER THAT RUNS THE GATE,
    /// WHEREVER THE POD DECLARES IT (backlog 3e2c8ddc). The binder used to
    /// read only the regular containers and, finding one that declares no
    /// GATE_BRANCH, took it as the executor: on the judged-init layout that
    /// is the seed helper, so the frozen sha went to the housekeeping and
    /// the gate ran the branch's head unbound.
    #[test]
    fn the_execution_binds_into_the_gate_wherever_the_pod_declares_it() {
        const HEAD: &str = "26b7e3081a52923879dd07cda85c2f3f74e7abdf";
        let request = judged_init_request();
        let mut bound = request.clone();
        bind_execution(&mut bound, &json!({"branch":"fix/x"}), HEAD, "").unwrap();
        let env = |doc: &Value, pointer: &str| {
            doc.pointer(&format!("{pointer}/env"))
                .and_then(Value::as_array)
                .unwrap()
                .iter()
                .map(|e| (e["name"].as_str().unwrap().to_string(), e["value"].clone()))
                .collect::<Vec<_>>()
        };
        let gate = env(&bound, "/spec/template/spec/initContainers/1");
        for (key, value) in [
            ("GATE_SHA", HEAD),
            ("GATE_MODE", ""),
            ("GATE_BRANCH", "fix/x"),
        ] {
            assert!(
                gate.contains(&(key.to_string(), json!(value))),
                "the gate is bound {key}={value}: {gate:?}"
            );
        }
        assert_eq!(
            bound["spec"]["template"]["spec"]["containers"],
            request["spec"]["template"]["spec"]["containers"],
            "the seed helper is not the executor and is bound nothing"
        );
        assert_eq!(
            bound["spec"]["template"]["spec"]["initContainers"][0],
            request["spec"]["template"]["spec"]["initContainers"][0],
            "the sidecar is bound nothing"
        );
        // The layout every manifest rendered before: unchanged.
        let mut old = json!({"spec":{"template":{"spec":{
            "initContainers":[{"name":"postgres"}],
            "containers":[{"name":"gate","env":[{"name":"GATE_BRANCH","value":"rendered"}]}]}}}});
        bind_execution(&mut old, &json!({"branch":"fix/x"}), HEAD, "").unwrap();
        assert!(
            old["spec"]["template"]["spec"]["containers"][0]["env"]
                .as_array()
                .unwrap()
                .contains(&json!({"name":"GATE_SHA","value":HEAD}))
        );
        // A gate declared twice has no one executor.
        let mut twice = request.clone();
        twice["spec"]["template"]["spec"]["containers"][0]["env"]
            .as_array_mut()
            .unwrap()
            .push(json!({"name":"GATE_BRANCH","value":"rendered"}));
        assert!(bind_execution(&mut twice, &json!({}), HEAD, "").is_err());
        // A lone regular container beside an init `gate` that declares no
        // GATE_BRANCH is not taken for the gate.
        let mut unnamed = request;
        unnamed["spec"]["template"]["spec"]["initContainers"][1]["env"] = json!([]);
        assert!(bind_execution(&mut unnamed, &json!({}), HEAD, "").is_err());
        assert_eq!(
            bay::gate_image(&judged_init_request()).unwrap(),
            "forge/boss-ci:gate",
            "a bay is claimed with the gate's image, not the helper's"
        );
    }
    #[test]
    fn matching_annotation_cannot_hide_a_different_native_spec() {
        let desired = json!({"metadata":{"name":"same","labels":{"boss.dev/packet":"p"}},"spec":{"template":{"spec":{"containers":[{"name":"gate","args":["frozen"]}]}}}});
        let mut actual = desired.clone();
        actual["metadata"]["uid"] = json!("native");
        actual["metadata"]["resourceVersion"] = json!("1");
        assert!(contains_desired(&actual, &desired));
        actual["spec"]["template"]["spec"]["containers"][0]["args"] = json!(["different"]);
        assert!(!contains_desired(&actual, &desired));
    }
}
