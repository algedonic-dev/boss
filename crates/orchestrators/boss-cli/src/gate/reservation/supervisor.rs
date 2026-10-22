//! One bounded pass over accepted requests, owned by the existing conductor.
use super::*;

#[derive(Default, Debug, Serialize)]
pub(super) struct Report {
    pub launched: Vec<String>,
    pub observed: Vec<String>,
    pub retained: Vec<Value>,
    pub reads: Vec<Value>,
    /// Requests this pass closed because it PROVED they will never
    /// launch, each with the cause and the evidence it read (backlog
    /// 5db91d0e). Never a request waiting for a bay, never an UNKNOWN.
    pub settled: Vec<Value>,
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
        // Its digest, not the page: the report rides the cadence outcome
        // POST, and whole pages made it 6.3 MB live (e1943ef0).
        let mut read = read_digest("queue-list-page", &page);
        read["path"] = json!(path);
        report.reads.push(read);
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

/// A rule's last firing as the pass keeps it: the three fields the
/// liveness decision reads (`rule_keeps_claims`: `firing_id`, `fired_at`,
/// `rc`) and the size and digest of the whole reply — never the reply.
///
/// WHY (backlog 443023a4). The train-reconcile firing carries this pass's
/// own previous observation, so a whole copy embedded the previous report,
/// which embedded the one before it: with a train waiting and a submission
/// queued every pass grew by one pass, and the live firing reached
/// 6,654,578 bytes at 07:34Z on 2026-10-19 — past the outcome door's
/// 2 MiB, so every pass recorded `gate_queue` unknown (413) and left a
/// file that size in the conductor's temp dir. A never-fired rule (`null`)
/// is recorded as null: absence is a reading, not a digest.
fn firing_digest(operation: &str, reply: Option<&Value>) -> Value {
    match reply {
        None | Some(Value::Null) => json!({"operation":operation,"reply":null}),
        Some(reply) => {
            let mut read = read_digest(operation, reply);
            for key in ["firing_id", "fired_at", "rc"] {
                read[key] = reply.get(key).cloned().unwrap_or(Value::Null);
            }
            read
        }
    }
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
    // The forge's own HEAD rides the same read as its CONTROL (backlog
    // 5db91d0e): a branch missing from an answer that also lacks HEAD is
    // a wrong or empty target speaking, not a deleted branch.
    command
        .current_dir(checkout)
        .args(["ls-remote", "origin", "HEAD", reference.as_str()]);
    let output = tokio::process::Command::from(command)
        .kill_on_drop(true)
        .output()
        .await?;
    let artifact=retain(http,packet,&serde_json::to_vec(&json!({"operation":"gate-queue-exact-branch",
        "reference":reference,"status":output.status.code(),"stdout":output.stdout,"stderr":output.stderr}))?).await?;
    let evidence = json!({"operation":"gate-queue-exact-branch","reference":reference,
        "file_id":artifact.file_id,"sha256":artifact.sha256,"size_bytes":artifact.size_bytes});
    report.reads.push(evidence.clone());
    if !output.status.success() {
        bail!(
            "exact source branch read failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
    let text = std::str::from_utf8(&output.stdout)?;
    let because = match branch_answer(text, &reference, head) {
        BranchAnswer::AtHead => return Ok(()),
        BranchAnswer::Unknown(why) => bail!(
            "source branch answer is UNKNOWN ({why}); immutable request is retained, not settled"
        ),
        BranchAnswer::Moved(tip) => format!(
            "{branch} moved to {tip} after this request was accepted at {head}; a resident \
             request never follows its tip, so it can never launch"
        ),
        BranchAnswer::Absent => format!(
            "{branch} is absent from the forge (its HEAD answered on the same read) after \
             this request was accepted at {head}, so it can never launch"
        ),
    };
    Err(NeverLaunches { because, evidence }.into())
}

/// A refusal the pass PROVED permanent, with the read that proved it
/// (backlog 5db91d0e). Only a positive forge answer builds one; every
/// other refusal stays a plain error and its request is retained.
#[derive(Debug)]
struct NeverLaunches {
    because: String,
    evidence: Value,
}
impl std::fmt::Display for NeverLaunches {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "never launches: {}", self.because)
    }
}
impl std::error::Error for NeverLaunches {}

#[derive(Debug, PartialEq, Eq)]
enum BranchAnswer {
    AtHead,
    /// The branch carries another head — the tip it moved to.
    Moved(String),
    /// The branch is not listed, and the forge's HEAD is.
    Absent,
    Unknown(String),
}

/// PURE: what one `ls-remote origin HEAD <reference>` answer says of the
/// accepted `head`. Absence counts only beside the control (`HEAD`
/// listed on the same read), because a wrong or empty remote answers an
/// empty list with exit 0 rather than an error.
fn branch_answer(text: &str, reference: &str, head: &str) -> BranchAnswer {
    let mut control = false;
    let mut tips = Vec::new();
    for line in text.lines().filter(|l| !l.trim().is_empty()) {
        let fields: Vec<_> = line.split_whitespace().collect();
        let [sha, name] = fields[..] else {
            return BranchAnswer::Unknown(format!("unparsed line {line:?}"));
        };
        if sha.len() != 40 || !sha.bytes().all(|b| b.is_ascii_hexdigit()) {
            return BranchAnswer::Unknown(format!("unparsed object name {sha:?}"));
        }
        match name {
            "HEAD" => control = true,
            n if n == reference => tips.push(sha),
            _ => {}
        }
    }
    match tips[..] {
        [tip] if tip == head => BranchAnswer::AtHead,
        [tip] => BranchAnswer::Moved(tip.to_owned()),
        [] if control => BranchAnswer::Absent,
        [] => {
            BranchAnswer::Unknown("the forge listed neither the branch nor its own HEAD".to_owned())
        }
        _ => BranchAnswer::Unknown("the branch was listed more than once".to_owned()),
    }
}

/// Close a request this pass proved will never launch: the guarded
/// negative claim first (`settlement::claim_on`, which refuses a packet
/// that may have a native Job or a possibly effective send), then the
/// verdict through the same two writes `boss gate --withdraw` takes. A
/// claim an earlier pass committed is the decision of record, so a
/// settle interrupted between the claim and the verdict resumes with
/// that receipt rather than a second one the immutable claim refuses.
async fn settle_never_launches(
    http: &Doors<'_>,
    packet: &str,
    namespace: &str,
    never: &NeverLaunches,
    now: chrono::DateTime<chrono::Utc>,
) -> Result<Value> {
    let row = read_on(http, packet).await?;
    if let Some(why) = withdraw_refusal(packet, &row) {
        bail!(why);
    }
    let prior = row
        .pointer("/metadata/gate_orphan_claim/verdict_receipt")
        .filter(|r| r.get("settled_by").and_then(Value::as_str) == Some(SETTLED_BY));
    let receipt = match prior {
        Some(prior) => prior.clone(),
        None => {
            let mut receipt = withdrawal_receipt(&never.because, None);
            receipt["settled_by"] = json!(SETTLED_BY);
            receipt["evidence"] = never.evidence.clone();
            receipt
        }
    };
    settlement::claim_on(http, packet, namespace, &receipt, now).await?;
    let step_id = crate::train::find_step(&row, "record-verdict", "Record the receipt")
        .and_then(|s| s.get("id"))
        .and_then(Value::as_str)
        .context("gate-run has no verdict step")?;
    for (method, path, body) in verdict_writes(packet, step_id, &receipt)? {
        api(http, method, &path, Some(body)).await?;
    }
    Ok(receipt)
}

/// Who a supervisor settle names as its writer, in the receipt itself.
const SETTLED_BY: &str = "resident-supervisor";

/// A source run that has FINISHED is a refusal as permanent as a moved
/// branch: the supervisor launches only under the exact OPEN run, and a
/// closed packet never reopens (backlog 36352452). Measured 2026-10-21
/// (backlog 599aed02): seven places accepted under builder runs the hourly
/// clock later closed as `died` were retained on every pass for eleven
/// hours with "original source run is not the exact open agent-run",
/// holding their places while newer gates launched. Every other source
/// refusal is returned as it came and its request is retained.
fn source_run_ended(error: anyhow::Error) -> anyhow::Error {
    let Some(ended) = error.downcast_ref::<source_claim::SourceRunEnded>() else {
        return error;
    };
    let outcome = ended
        .outcome
        .as_deref()
        .map(|o| format!(" with outcome {o}"))
        .unwrap_or_default();
    NeverLaunches {
        because: format!(
            "source run {} is {}{outcome}; a resident request launches only under its exact \
             open agent-run and a closed run never reopens, so it can never launch — re-gate \
             the head from a live run",
            ended.run, ended.status
        ),
        evidence: json!({"operation":"original-source-run","run":ended.run,
            "status":ended.status,"outcome":ended.outcome,"read":ended.read}),
    }
    .into()
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
                // The acceptance's identity and the receipt's digest; the
                // receipt itself carries the whole job (e1943ef0).
                report.reads.push(json!({"operation":"original-submission-acceptance","packet":packet,
                    "command":submission.command,"applied_at":receipt.applied_at,"actor":receipt.actor,
                    "event_id":receipt.event_id,
                    "receipt":read_digest("acceptance-receipt",&serde_json::to_value(&receipt)?)}));
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
            .push(firing_digest("train-priority-liveness", firing.as_ref()));
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
            .push(firing_digest("dock-priority-liveness", firing.as_ref()));
        let firing: Option<boss_jobs::cadence::LastFiring> =
            firing.map(serde_json::from_value).transpose()?;
        dock_waiting(&cars, firing.as_ref(), now, QUEUE_PLACE_TTL_SECS)
            .map_err(anyhow::Error::msg)?;
    }
    let census = native_census_on(http, namespace, "app=gate-runner", anchor).await?;
    // The retained artifact names the census; the list is not copied in
    // here again (e1943ef0).
    report.reads.push(
        json!({"operation":"native-capacity-census","file_id":census.artifact.file_id,
        "sha256":census.artifact.sha256,"size_bytes":census.artifact.size_bytes}),
    );
    let native = census.list["items"]
        .as_array()
        .context("native census lost items")?;
    if native.iter().any(|job| {
        job.pointer("/metadata/labels/app").and_then(Value::as_str) != Some("gate-runner")
    }) {
        bail!("native capacity census returned a Job outside its declared selector");
    }
    // The bay's own liveness rule (backlog ac8d3157), so the supervisor's
    // count and the reservation that guards its create cannot disagree.
    let mut occupied = native
        .iter()
        .filter(|job| super::bay::job_live(job))
        .count();
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
    // ONE LINE, BOTH OWNERS (backlog b56bfc29): a legacy place is ahead
    // of a resident request only when it was taken before that request's
    // original acceptance — the comparison a legacy waiter makes the
    // other way round (`mixed_line`). Counting the whole legacy line
    // ahead of every resident let gates queued later take freed bays.
    let legacy = queue_places(&legacy, now, QUEUE_PLACE_TTL_SECS);
    for (accepted_at, packet, submission) in accepted {
        let ahead = train_claims.len() + dock + places_taken_before(&legacy, accepted_at, &packet);
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
            source_claim::verify(http, source, &submission.submitted_by, &mut report.reads)
                .await
                .map_err(source_run_ended)?;
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
            // PROVED never to launch: settled here, by the pass that read
            // the proof, with the cause and the evidence on the packet
            // (backlog 5db91d0e). Before this it was retained on every
            // pass, forever — the conductor's clock no longer settles a
            // resident still in line (b56bfc29) — holding its place
            // ahead of later waiters until someone withdrew it by hand.
            // A settle that cannot complete is retained, never guessed.
            Err(error) => match error.downcast_ref::<NeverLaunches>() {
                Some(never) => {
                    match settle_never_launches(http, &packet, namespace, never, now).await {
                        Ok(receipt) => report.settled.push(json!({"packet":packet,
                            "verdict":WITHDRAWN_VERDICT,"because":never.because,"receipt":receipt})),
                        Err(settle) => report.retained.push(json!({"packet":packet,
                            "reason":format!("{error:#}; the settle was not completed: {settle:#}")})),
                    }
                }
                None => report
                    .retained
                    .push(json!({"packet":packet,"reason":format!("{error:#}")})),
            },
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const REF: &str = "refs/heads/fix/x";
    const HEAD: &str = "097e370a27a0c773cf1ce015e242215ee200f8ec";
    const TIP: &str = "1111111111111111111111111111111111111111";

    /// Backlog 5db91d0e: only a positive answer is permanent. A moved
    /// tip is; a missing branch is only beside the forge's own HEAD on
    /// the same read; silence, a malformed line or a doubled ref holds.
    #[test]
    fn only_a_positive_forge_answer_proves_a_request_will_never_launch() {
        let at = |lines: &[(&str, &str)]| {
            let text: String = lines.iter().map(|(s, r)| format!("{s}\t{r}\n")).collect();
            branch_answer(&text, REF, HEAD)
        };
        assert_eq!(at(&[(TIP, "HEAD"), (HEAD, REF)]), BranchAnswer::AtHead);
        assert_eq!(at(&[(HEAD, REF)]), BranchAnswer::AtHead);
        assert_eq!(
            at(&[(HEAD, "HEAD"), (TIP, REF)]),
            BranchAnswer::Moved(TIP.into())
        );
        assert_eq!(at(&[(TIP, "HEAD")]), BranchAnswer::Absent);
        assert!(matches!(at(&[]), BranchAnswer::Unknown(_)));
        // A ref that only ends like HEAD is not the forge's HEAD.
        assert!(matches!(
            at(&[(TIP, "refs/remotes/origin/HEAD")]),
            BranchAnswer::Unknown(_)
        ));
        assert!(matches!(
            at(&[(TIP, "HEAD"), (HEAD, REF), (TIP, REF)]),
            BranchAnswer::Unknown(_)
        ));
        assert!(matches!(
            branch_answer("not-a-sha\tHEAD\n", REF, HEAD),
            BranchAnswer::Unknown(_)
        ));
        assert!(matches!(
            branch_answer(&format!("{TIP}\n"), REF, HEAD),
            BranchAnswer::Unknown(_)
        ));
    }
}
