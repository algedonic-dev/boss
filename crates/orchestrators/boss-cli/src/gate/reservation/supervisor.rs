//! One bounded pass over accepted requests, owned by the existing conductor.
use super::*;

#[derive(Default, Debug, Serialize)]
pub(super) struct Report {
    pub launched: Vec<String>,
    pub observed: Vec<String>,
    pub retained: Vec<Value>,
    pub reads: Vec<Value>,
}

async fn pages(http: &Doors<'_>, query: &str, report: &mut Report) -> Result<Vec<Value>> {
    let mut rows = Vec::new();
    let mut total = None;
    let mut ids = std::collections::HashSet::new();
    for _ in 0..1000 {
        let path = format!("/api/jobs?{query}&full=true&limit=50&offset={}", rows.len());
        let page = api(http, reqwest::Method::GET, &path, None)
            .await?
            .context("queue list returned no document")?;
        report.reads.push(json!({"path":path,"reply":page}));
        let count = page
            .get("total")
            .and_then(Value::as_u64)
            .context("queue list has no declared total")?;
        if total.is_some_and(|expected| expected != count) {
            bail!("queue list changed its declared total during pagination");
        }
        total = Some(count);
        let data = page
            .get("data")
            .and_then(Value::as_array)
            .context("queue list has no complete data array")?;
        for row in data {
            let id = row
                .get("id")
                .and_then(Value::as_str)
                .filter(|id| !id.is_empty())
                .context("queue list has a row without identity")?;
            if !ids.insert(id.to_owned()) {
                bail!("queue list repeated a packet while paginating");
            }
            rows.push(row.clone());
        }
        if rows.len() as u64 > count {
            bail!("queue list exceeds its declared total");
        }
        if rows.len() as u64 == count {
            return Ok(rows);
        }
        if data.is_empty() {
            bail!("queue list ended before its declared total");
        }
    }
    bail!("queue list exceeded the bounded complete-read budget; no truncated list is usable")
}

async fn exact_branch(
    http: &Doors<'_>,
    checkout: &Path,
    packet: &str,
    branch: &str,
    head: &str,
    report: &mut Report,
) -> Result<()> {
    if head.len() != 40 || !head.bytes().all(|b| b.is_ascii_hexdigit()) || branch.is_empty() {
        bail!("submission lacks an exact full source head/branch");
    }
    let reference = format!("refs/heads/{branch}");
    let mut command = crate::git_auth::command();
    command
        .current_dir(checkout)
        .args(["ls-remote", "origin", reference.as_str()]);
    let output = tokio::process::Command::from(command)
        .kill_on_drop(true)
        .output()
        .await?;
    let artifact=retain(http,packet,&serde_json::to_vec(&json!({"operation":"gate-queue-exact-branch",
        "reference":reference,"status":output.status.code(),"stdout":output.stdout,"stderr":output.stderr}))?).await?;
    report.reads.push(json!({"operation":"gate-queue-exact-branch","file_id":artifact.file_id,"sha256":artifact.sha256,"size_bytes":artifact.size_bytes}));
    if !output.status.success() {
        bail!(
            "exact source branch read failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
    let text = std::str::from_utf8(&output.stdout)?;
    let fields: Vec<_> = text.split_whitespace().collect();
    if fields != [head, reference.as_str()] {
        bail!(
            "source branch moved or is absent; immutable request is retained without following its tip"
        );
    }
    Ok(())
}

fn live(job: &Value) -> bool {
    let terminal = job
        .pointer("/status/conditions")
        .and_then(Value::as_array)
        .is_some_and(|conditions| {
            conditions.iter().any(|condition| {
                condition.get("status").and_then(Value::as_str) == Some("True")
                    && matches!(
                        condition.get("type").and_then(Value::as_str),
                        Some("Complete" | "Failed")
                    )
            })
        });
    let active = job
        .pointer("/status/active")
        .and_then(Value::as_u64)
        .unwrap_or(0);
    // Pod counters do not terminate a Job: retries can still create a
    // runner. Only a terminal Job condition frees its physical bay.
    active > 0 || !terminal
}

pub(super) async fn observation_on(
    http: &Doors<'_>,
    checkout: &Path,
    namespace: &str,
    max: usize,
    now: chrono::DateTime<chrono::Utc>,
) -> Value {
    let mut report = Report::default();
    let result = pass_inner(http, checkout, namespace, max, now, &mut report).await;
    json!({"contract_version":1,"state":if result.is_ok(){"completed"}else{"unknown"},
        "observed_at":now,"source_build":crate::built_from::built_from(),"report":report,
        "error":result.err().map(|error|format!("{error:#}"))})
}

#[cfg(test)]
pub(super) async fn pass_on(
    http: &Doors<'_>,
    checkout: &Path,
    namespace: &str,
    max: usize,
    now: chrono::DateTime<chrono::Utc>,
) -> Result<Report> {
    let mut report = Report::default();
    let result = pass_inner(http, checkout, namespace, max, now, &mut report).await;
    if let Err(error) = result {
        bail!(
            "resident gate supervision UNKNOWN: {error:#}; complete observation {}",
            serde_json::to_string(&report)?
        );
    }
    Ok(report)
}

async fn pass_inner(
    http: &Doors<'_>,
    checkout: &Path,
    namespace: &str,
    max: usize,
    now: chrono::DateTime<chrono::Utc>,
    report: &mut Report,
) -> Result<()> {
    if max == 0 {
        bail!("gate capacity is zero");
    }
    let gates = pages(http, "kind=gate-run&status=open", report).await?;
    let mut accepted = Vec::new();
    for row in &gates {
        let Some(metadata) = row.get("metadata") else {
            bail!("open gate packet has no metadata");
        };
        if metadata.get(model::SUBMISSION).is_none() {
            continue;
        }
        let packet = row
            .get("id")
            .and_then(Value::as_str)
            .context("gate packet lacks identity")?;
        match submission::original_on(http,packet).await {
            Ok((submission,receipt))=>{
                report.reads.push(json!({"operation":"original-submission-acceptance","packet":packet,"receipt":receipt}));
                if submission.intent.namespace!=namespace {report.retained.push(json!({"packet":packet,"reason":"submission belongs to a different namespace"}));continue;}
                accepted.push((receipt.applied_at,packet.to_owned(),submission));
            }
            Err(error)=>report.retained.push(json!({"packet":packet,"reason":format!("unreadable original submission: {error:#}")})),
        }
    }
    accepted.sort_by(|a, b| (&a.0, &a.1).cmp(&(&b.0, &b.1)));
    let Some((_, anchor, _)) = accepted.first() else {
        return Ok(());
    };
    let policy = staging::policy_on(http).await?;
    let policy_max = policy
        .get("gate_max_concurrent")
        .and_then(Value::as_u64)
        .and_then(|n| usize::try_from(n).ok())
        .context("current policy capacity is invalid")?;
    let max = max.min(policy_max);
    report
        .reads
        .push(json!({"operation":"current-gate-queue-policy","policy":policy}));
    let trains = pages(http, "kind=pr-train&status=open", report).await?;
    let cars = pages(http, "kind=ship-a-change&status=open", report).await?;
    // A failed or stale priority read cannot be demoted to zero.
    let train_claims = trains_claiming(&trains);
    if !train_claims.is_empty() {
        let firing = api(
            http,
            reqwest::Method::GET,
            &format!("/api/cadence/rules/{TRAIN_RECONCILE_RULE}/last-firing"),
            None,
        )
        .await?;
        report
            .reads
            .push(json!({"operation":"train-priority-liveness","reply":firing}));
        let firing: Option<boss_jobs::cadence::LastFiring> =
            firing.map(serde_json::from_value).transpose()?;
        trains_waiting(&trains, firing.as_ref(), now, QUEUE_PLACE_TTL_SECS)
            .map_err(anyhow::Error::msg)?;
    }
    let dock = dock_claims(&cars);
    if dock > 0 {
        let firing = api(
            http,
            reqwest::Method::GET,
            &format!("/api/cadence/rules/{DOCK_REFRESH_RULE}/last-firing"),
            None,
        )
        .await?;
        report
            .reads
            .push(json!({"operation":"dock-priority-liveness","reply":firing}));
        let firing: Option<boss_jobs::cadence::LastFiring> =
            firing.map(serde_json::from_value).transpose()?;
        dock_waiting(&cars, firing.as_ref(), now, QUEUE_PLACE_TTL_SECS)
            .map_err(anyhow::Error::msg)?;
    }
    let census = native_census_on(http, namespace, "app=gate-runner", anchor).await?;
    report.reads.push(json!({"operation":"native-capacity-census","file_id":census.artifact.file_id,
        "sha256":census.artifact.sha256,"size_bytes":census.artifact.size_bytes,"list":census.list}));
    let native = census.list["items"]
        .as_array()
        .context("native census lost items")?;
    if native.iter().any(|job| {
        job.pointer("/metadata/labels/app").and_then(Value::as_str) != Some("gate-runner")
    }) {
        bail!("native capacity census returned a Job outside its declared selector");
    }
    let mut occupied = native.iter().filter(|job| live(job)).count();
    // A possibly sent invocation still owns one admission even when a
    // current list cannot prove whether its delayed create will arrive.
    for row in &gates {
        let Some(md) = row.get("metadata") else {
            bail!("gate capacity row lacks metadata");
        };
        if md.get(model::OBSERVATION).is_some()
            || (md.get(model::INTENT).is_none() && md.get(attempt::KEY).is_none())
            || native
                .iter()
                .any(|job| job.pointer("/metadata/labels/boss.dev~1packet") == row.get("id"))
        {
            continue;
        }
        let no_child: Result<()> = async {
            let previous = attempt::read(md)?.context("native intent has no guarded attempt")?;
            if previous.state != attempt::State::NotStarted {
                bail!("native send may still take effect");
            }
            let value = md
                .get(model::INTENT)
                .context("no-child attempt lacks its intent")?;
            if md.pointer("/_immutable_metadata/gate_launch_intent") != Some(value) {
                bail!("no-child intent lacks its immutable binding");
            }
            let intent: LaunchIntent = serde_json::from_value(value.clone())?;
            if row.get("id").and_then(Value::as_str) != Some(intent.packet.as_str())
                || intent.namespace != namespace
            {
                bail!("no-child proof belongs to another native target");
            }
            verify_previous_no_child(http, &intent, &previous).await
        }
        .await;
        if let Err(error) = no_child {
            occupied += 1;
            report.retained.push(json!({"packet":row["id"],"reason":format!("possible native occupancy retained: {error:#}")}));
        }
    }
    let legacy: Vec<_> = gates
        .iter()
        .filter(|row| row.pointer("/metadata/gate_submission").is_none())
        .cloned()
        .collect();
    let ahead = train_claims.len() + dock + queue_order(&legacy, now, QUEUE_PLACE_TTL_SECS).len();
    for (_, packet, submission) in accepted {
        let attempt: Result<bool> = async {
            let current = read_on(http, &packet).await?;
            if let Some(why) = launch_target_refusal(&packet, &current) {
                bail!(why);
            }
            let source =
                model::source_metadata(current.get("metadata").context("gate lost metadata")?)
                    .map_err(anyhow::Error::msg)?;
            let branch = source
                .get("branch")
                .and_then(Value::as_str)
                .context("durable source lacks branch")?;
            if source.get("sha").and_then(Value::as_str) != Some(submission.intent.head.as_str()) {
                bail!("source head disagrees with immutable native request");
            }
            if let Some(name) = existing_on(http, namespace, &packet).await? {
                report.observed.push(packet.clone());
                report
                    .reads
                    .push(json!({"operation":"observed-native","packet":packet,"name":name}));
                return Ok(false);
            }
            source_claim::verify(http, source, &submission.submitted_by, &mut report.reads).await?;
            exact_branch(
                http,
                checkout,
                &packet,
                branch,
                &submission.intent.head,
                report,
            )
            .await?;
            match gated_car_guard(
                branch,
                &submission.intent.head,
                &cars,
                source.get("force_regate_reason").and_then(Value::as_str),
            ) {
                GatedGuard::Proceed => {}
                GatedGuard::Forced(note) => report
                    .reads
                    .push(json!({"operation":"original-force-regate","packet":packet,"note":note})),
                GatedGuard::Refuse(reason) => bail!(reason),
            }
            if !admits(occupied, ahead, max, Requester::Car) {
                return Ok(false);
            }
            submission::launch_on(http, &packet, now).await?;
            Ok(true)
        }
        .await;
        match attempt {
            Ok(true) => {
                report.launched.push(packet);
                occupied += 1;
            }
            Ok(false) => {}
            Err(error) => report
                .retained
                .push(json!({"packet":packet,"reason":format!("{error:#}")})),
        }
    }
    Ok(())
}
