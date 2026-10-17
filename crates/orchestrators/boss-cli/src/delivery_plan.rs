//! One shared channel judgement: the same source CI and deploy execute.

use anyhow::{Context, Result, bail};
use serde_json::Value;
use serde_json::json;
use std::path::Path;
use std::process::Stdio;
use tokio::io::AsyncWriteExt;

/// Embedded at build time: a train cannot execute an unreviewed planner out of
/// the branch it is assembling. CI and deploy run this same source. Losing the
/// interpreter or a frozen object is a refusal, never a lighter-path grant.
const JUDGEMENT: &str = include_str!("../../../../infra/forge/delivery-plan.py");

/// The admitted row, not the currently active registry row, decides which
/// evidence can complete this train. An old required cluster commit stays real.
pub(crate) fn compatible_completion(train: &Value) -> bool {
    let Some(fields) = boss_jobs::car::find_step(train, "converged", "Cluster converged")
        .and_then(|s| s["fields"].as_array())
    else {
        return false;
    };
    fields.iter().any(|f| {
        f["name"] == "delivery_evidence" && f["field_type"] == "object" && f["required"] == true
    }) && !fields
        .iter()
        .any(|f| f["name"] == "cluster_commit" && f["required"] == true)
}

pub(crate) fn completion_plan<'a>(
    train: &'a Value,
    policy: &crate::delivery_policy::DeliveryPolicy,
) -> Option<&'a Value> {
    if !compatible_completion(train)
        || !policy.is_from_registry()
        || crate::delivery_policy::pinned_version(train) != Some(policy.version)
    {
        return None;
    }
    let plan = train.pointer("/metadata/delivery_plan")?;
    (plan["route"] == "data" && plan["policy"] == json!({
        "name":crate::delivery_policy::POLICY_NAME,"version":policy.version,"channels":policy.channels
    })).then_some(plan)
}

pub(crate) async fn request(input: &Value) -> Result<Value> {
    let mut child = tokio::process::Command::new("python3")
        .args(["-I", "-c", JUDGEMENT])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .context("starting the shared delivery-plan judgement")?;
    child
        .stdin
        .take()
        .context("delivery-plan stdin unavailable")?
        .write_all(&serde_json::to_vec(input)?)
        .await?;
    let output = child.wait_with_output().await?;
    if !output.status.success() {
        bail!(
            "delivery-plan refused ({}): {}",
            output.status,
            String::from_utf8_lossy(&output.stderr)
        );
    }
    serde_json::from_slice(&output.stdout).context("delivery-plan answer malformed")
}

pub(crate) async fn git_bytes(repo: &Path, args: &[&str]) -> Result<Vec<u8>> {
    let output = tokio::process::Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(args)
        .kill_on_drop(true)
        .output()
        .await?;
    if !output.status.success() {
        bail!(
            "delivery source git {args:?} refused ({}): {}",
            output.status,
            String::from_utf8_lossy(&output.stderr)
        );
    }
    Ok(output.stdout)
}

async fn git_text(repo: &Path, args: &[&str]) -> Result<String> {
    String::from_utf8(git_bytes(repo, args).await?).context("delivery source is not UTF-8")
}

fn root_main(answer: &str) -> Result<&str> {
    let mut lines = answer.lines();
    let line = lines
        .next()
        .context("root publication reader returned no main")?;
    if lines.next().is_some() {
        bail!("root publication reader returned multiple refs");
    }
    let (sha, reference) = line
        .split_once('\t')
        .context("root publication ref is malformed")?;
    if reference != "refs/heads/main"
        || ![40, 64].contains(&sha.len())
        || !sha.bytes().all(|b| b.is_ascii_hexdigit())
    {
        bail!("root publication reader did not name one full main identity");
    }
    Ok(sha)
}

/// Only the immutable, root-owned generation's exact reviewed reader may run.
/// An arbitrary source checkout is data, never an executable credential door.
async fn root_reading(repo: &Path, reader: &Path, head: &str) -> Result<Value> {
    use std::os::unix::fs::MetadataExt;
    if !reader.is_absolute() || reader.canonicalize()? != reader {
        bail!("root publication reader is not an absolute physical path");
    }
    for ancestor in reader.ancestors() {
        let metadata = std::fs::symlink_metadata(ancestor)?;
        if metadata.uid() != 0 || metadata.mode() & 0o022 != 0 || metadata.file_type().is_symlink()
        {
            bail!(
                "root publication reader has an unqualified owner or writable ancestor: {}",
                ancestor.display()
            );
        }
    }
    let generation = reader
        .parent()
        .and_then(Path::parent)
        .and_then(Path::parent)
        .context("root publication generation missing")?;
    let stamp = std::fs::symlink_metadata(generation.join(".boss-tree-sha"))?;
    if !stamp.is_file() || stamp.uid() != 0 || stamp.mode() & 0o022 != 0 {
        bail!("root publication generation stamp is unqualified");
    }
    if reader.strip_prefix(generation)? != Path::new("infra/forge/root-tree.sh")
        || std::fs::read_to_string(generation.join(".boss-tree-sha"))?.trim() != head
    {
        bail!("root publication reader is not bound to this source generation");
    }
    let bytes = std::fs::read(reader)?;
    if bytes != git_bytes(repo, &["show", &format!("{head}:infra/forge/root-tree.sh")]).await? {
        bail!("root publication reader differs from its frozen source blob");
    }
    let mut command = tokio::process::Command::new("/bin/bash");
    command
        .arg(reader)
        .arg("read-main")
        .env_clear()
        .env(
            "PATH",
            "/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin",
        )
        .env("HOME", "/nonexistent")
        .kill_on_drop(true);
    let output = tokio::time::timeout(std::time::Duration::from_secs(330), command.output())
        .await
        .context("root publication reader timed out")??;
    let stdout =
        String::from_utf8(output.stdout).context("root publication answer is not UTF-8")?;
    let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
    if !output.status.success() {
        bail!(
            "root publication read refused ({}): {stdout}\n{stderr}",
            output.status
        );
    }
    let main = root_main(&stdout)?;
    Ok(
        json!({"main":main,"read_at":boss_clock_client::wall_now().to_rfc3339(),
        "reader":reader,"argv":["/bin/bash",reader.to_str().context("reader path is not UTF-8")?,"read-main"],
        "stdout":stdout,"stderr":stderr,"status":output.status.code()}),
    )
}

/// Obtain publication evidence through the existing forge reader, then bind
/// the complete published tree to the independently recomputed frozen plan.
/// Neither a metadata stamp nor a caller-supplied observed flag is authority.
pub(crate) async fn source_observation(
    repo: &Path,
    plan: &Value,
    publication: &str,
) -> Result<Value> {
    source_observation_with_reader(repo, plan, publication, None).await
}

async fn source_observation_with_reader(
    repo: &Path,
    plan: &Value,
    publication: &str,
    reader: Option<&Path>,
) -> Result<Value> {
    let base = plan["base"].as_str().context("delivery base unread")?;
    let head = plan["head"].as_str().context("delivery head unread")?;
    for sha in [base, head, publication] {
        if sha.len() != 40 || !sha.bytes().all(|b| b.is_ascii_hexdigit()) {
            bail!("delivery observation requires full commit identities");
        }
    }
    let actual =
        request(&json!({"operation":"git_plan", "repo":repo, "base":base, "head":head})).await?;
    if actual != *plan || actual["route"] != "data" {
        bail!("delivery plan differs from the complete frozen source judgement");
    }
    let publication_read = if let Some(reader) = reader {
        let mut reading = root_reading(repo, reader, head).await?;
        let main = reading["main"].as_str().context("root main unread")?;
        // Missing local objects refuse; a read must not refresh the deployment tree.
        git_bytes(repo, &["merge-base", "--is-ancestor", publication, main]).await?;
        reading["merge"] = json!(publication);
        reading["carries"] = json!(true);
        reading
    } else {
        let root = repo.to_path_buf();
        let merge = publication.to_owned();
        let reading = tokio::task::spawn_blocking(move || {
            crate::car_unland::read_main(&root, &merge, boss_clock_client::wall_now())
        })
        .await?
        .map_err(anyhow::Error::msg)?;
        if !reading.carries {
            bail!(
                "delivery publication is absent from forge main: {}",
                reading.evidence()
            );
        }
        json!({"main":reading.main,"merge":reading.merge,"read_at":reading.read_at,
            "carries":reading.carries,"evidence":reading.evidence()})
    };
    let tree = git_text(repo, &["rev-parse", &format!("{publication}^{{tree}}")]).await?;
    let expected = git_text(repo, &["rev-parse", &format!("{head}^{{tree}}")]).await?;
    if tree != expected {
        bail!(
            "published tree {} differs from frozen delivery tree {}",
            tree.trim(),
            expected.trim()
        );
    }
    let mut receipts = Vec::new();
    for obligation in plan["obligations"]
        .as_array()
        .context("delivery obligations unread")?
    {
        let mut receipt = obligation
            .as_object()
            .context("delivery obligation unread")?
            .clone();
        receipt.insert("head".into(), json!(head));
        receipt.insert("observed".into(), json!(true));
        receipts.push(Value::Object(receipt));
    }
    let mut publication_read = publication_read;
    publication_read["tree"] = json!(tree.trim());
    Ok(json!({"schema":1, "plan":plan,
        "publication":publication_read,
        "receipts":receipts}))
}

async fn native_read(door: &dyn crate::car_retire::Door, service: &str, path: &str) -> Value {
    match door.send(reqwest::Method::GET, path, None).await {
        Ok(answer) => json!({"service":service,"path":path,"answer":answer}),
        Err(error) => json!({"service":service,"path":path,"error":format!("{error:#}")}),
    }
}

/// Unlike a best-effort report, a proof of absence must account for the total.
/// Retain every page, and refuse a partial, duplicated or changing population.
pub(crate) async fn read_all(
    door: &dyn crate::car_retire::Door,
    query: &str,
    reads: &mut Vec<Value>,
) -> Result<Vec<Value>> {
    let mut rows = Vec::new();
    let mut total = None;
    let mut ids = std::collections::BTreeSet::new();
    loop {
        let path = format!(
            "{query}&limit={}&offset={}",
            crate::train::PAGE_LIMIT,
            rows.len()
        );
        let reading = native_read(door, "jobs", &path).await;
        reads.push(reading);
        let answer = &reads.last().context("native page missing")?["answer"];
        let expected = answer["total"]
            .as_u64()
            .and_then(|n| usize::try_from(n).ok())
            .context("native population total unread")?;
        if total.is_some_and(|n| n != expected) {
            bail!("native population changed during pagination");
        }
        total = Some(expected);
        let page = answer["data"]
            .as_array()
            .context("native population page unread")?;
        if page.len() > crate::train::PAGE_LIMIT {
            bail!("native page exceeds its requested population");
        }
        if page.is_empty() && rows.len() != expected {
            bail!("native population ended before its declared total");
        }
        for row in page {
            let id = row["id"]
                .as_str()
                .filter(|s| !s.is_empty())
                .context("native population identity unread")?;
            if !ids.insert(id.to_owned()) {
                bail!("native population repeats identity {id}");
            }
            rows.push(row.clone());
        }
        if rows.len() > expected {
            bail!("native population exceeds its declared total");
        }
        if rows.len() == expected {
            return Ok(rows);
        }
    }
}

/// Judge registry answers without discarding any native response. The caller
/// first obtains source_observation; these receipts add equality, not publication.
async fn registry_observation(
    plan: &Value,
    sources: &std::collections::BTreeMap<String, String>,
    jobs: &dyn crate::car_retire::Door,
    dispatcher: &dyn crate::car_retire::Door,
) -> Result<Value> {
    let mut reads = Vec::new();
    let mut gaps = Vec::new();
    let mut receipts = Vec::new();
    let policy_path = format!(
        "/api/delivery/policy/train-conductor/versions/{}",
        plan["policy"]["version"]
            .as_i64()
            .context("delivery policy version unread")?
    );
    let policy_read = native_read(jobs, "jobs", &policy_path).await;
    let policy_result = (|| -> Result<()> {
        let row: boss_jobs::delivery::DeliveryPolicyRow =
            serde_json::from_value(policy_read["answer"].clone())
                .context("native delivery policy unread")?;
        boss_jobs::delivery::check_policy(&row).map_err(anyhow::Error::msg)?;
        crate::delivery_policy::parse(row.clone())?;
        if json!({"name":row.name,"version":row.version,"channels":row.channels}) != plan["policy"]
        {
            bail!("native pinned delivery policy differs from frozen source policy");
        }
        Ok(())
    })();
    let mut selection_ready = policy_result.is_ok();
    if let Err(error) = policy_result {
        gaps.push(json!({"path":policy_path,"error":format!("{error:#}")}));
    }
    reads.push(policy_read);
    let protocol_path = "/api/workflows/pr-train";
    let protocol_read = native_read(jobs, "jobs", protocol_path).await;
    let protocol_result = (|| -> Result<()> {
        let source = sources
            .get("infra/platform/workflows/pr-train.toml")
            .context("frozen train protocol unread")?;
        let live = &protocol_read["answer"];
        if !registry_equal("workflow_equality", "pr-train", source, live)? {
            bail!("native train protocol differs from the frozen completion contract");
        }
        let mut admitted = live.clone();
        for step in admitted["steps"]
            .as_array_mut()
            .context("native train protocol steps unread")?
        {
            step["spec_slug"] = step["title"].clone();
        }
        if !compatible_completion(&admitted) {
            bail!("native train protocol still requires image completion");
        }
        Ok(())
    })();
    selection_ready &= protocol_result.is_ok();
    if let Err(error) = protocol_result {
        gaps.push(json!({"path":protocol_path,"error":format!("{error:#}")}));
    }
    reads.push(protocol_read);
    let pending_path = "/api/jobs?kind=pr-train&status=open&full=true";
    match read_all(jobs, pending_path, &mut reads).await {
        Ok(pending) => {
            for train in pending {
                if train["kind"] != "pr-train" || train["status"] != "open" {
                    selection_ready = false;
                    gaps.push(json!({"path":pending_path,"error":"native pending train scope unread","train":train["id"]}));
                } else if boss_jobs::car::find_step(&train, "merged", "Merged into main")
                    .and_then(|s| s.pointer("/metadata/merge_ref"))
                    .and_then(Value::as_str)
                    .is_some_and(|s| !s.is_empty())
                    && !compatible_completion(&train)
                {
                    selection_ready = false;
                    gaps.push(json!({"path":pending_path,"error":"a merged train still owes its admitted image completion","train":train["id"]}));
                }
            }
        }
        Err(error) => {
            selection_ready = false;
            gaps.push(json!({"path":pending_path,"error":format!("{error:#}")}));
        }
    }
    for obligation in plan["obligations"]
        .as_array()
        .context("delivery obligations unread")?
    {
        let mut receipt = obligation
            .as_object()
            .context("delivery obligation unread")?
            .clone();
        receipt.insert("head".into(), plan["head"].clone());
        receipt.insert("observed".into(), json!(true));
        let evidence = obligation["evidence"]
            .as_str()
            .context("delivery evidence kind unread")?;
        if evidence != "forge_blob" {
            let path = obligation["path"]
                .as_str()
                .context("delivery file path unread")?;
            let name = path
                .rsplit('/')
                .next()
                .and_then(|s| s.strip_suffix(".toml"))
                .context("registry file name unread")?;
            let encoded =
                percent_encoding::utf8_percent_encode(name, percent_encoding::NON_ALPHANUMERIC);
            let (service, native_path, door) = match evidence {
                "workflow_equality" => ("jobs", format!("/api/workflows/{encoded}"), jobs),
                "rule_equality" => (
                    "dispatcher",
                    format!("/api/dispatcher/rules/{encoded}/versions"),
                    dispatcher,
                ),
                _ => bail!("unsupported delivery evidence kind {evidence}"),
            };
            let reading = native_read(door, service, &native_path).await;
            let equality = (|| -> Result<bool> {
                let source = sources.get(path).context("frozen registry source unread")?;
                let answer = &reading["answer"];
                let live = if evidence == "rule_equality" {
                    let active: Vec<_> = answer
                        .as_array()
                        .context("native rule versions unread")?
                        .iter()
                        .filter(|r| r["status"] == "active")
                        .collect();
                    if active.len() != 1 {
                        bail!("native rule does not have exactly one active version");
                    }
                    active[0]
                } else {
                    answer
                };
                registry_equal(evidence, name, source, live)
            })();
            match equality {
                Ok(true) => {
                    receipt.insert("equal".into(), json!(true));
                }
                Ok(false) => {
                    gaps.push(json!({"path":native_path,"error":"native registry content differs from frozen source"}));
                }
                Err(error) => {
                    gaps.push(json!({"path":native_path,"error":format!("{error:#}")}));
                }
            }
            reads.push(reading);
        }
        receipts.push(Value::Object(receipt));
    }
    let satisfied =
        request(&json!({"operation":"satisfied","plan":plan,"receipts":receipts})).await?;
    Ok(
        json!({"schema":1,"arrived":gaps.is_empty() && satisfied == true,
        "selection_ready":selection_ready,"plan":plan,"native_reads":reads,"receipts":receipts,"gaps":gaps}),
    )
}

/// One observation path for the conductor, deploy runner and supported CLI
/// freshness check. It performs reads only; no answer grants publication.
pub(crate) async fn observe(
    repo: &Path,
    plan: &Value,
    publication: &str,
    jobs_base: &str,
) -> Result<Value> {
    observe_with_reader(repo, plan, publication, jobs_base, None).await
}

pub(crate) async fn observe_with_reader(
    repo: &Path,
    plan: &Value,
    publication: &str,
    jobs_base: &str,
    reader: Option<&Path>,
) -> Result<Value> {
    let source = match reader {
        None => source_observation(repo, plan, publication).await?,
        Some(reader) => {
            source_observation_with_reader(repo, plan, publication, Some(reader)).await?
        }
    };
    let mut sources = std::collections::BTreeMap::new();
    let head = plan["head"].as_str().context("delivery head unread")?;
    sources.insert(
        "infra/platform/workflows/pr-train.toml".into(),
        git_text(
            repo,
            &[
                "show",
                &format!("{head}:infra/platform/workflows/pr-train.toml"),
            ],
        )
        .await?,
    );
    for obligation in plan["obligations"]
        .as_array()
        .context("delivery obligations unread")?
    {
        if obligation["evidence"] != "forge_blob" {
            let path = obligation["path"]
                .as_str()
                .context("delivery source path unread")?;
            let blob = obligation["new"]
                .as_str()
                .context("delivery source blob unread")?;
            sources.insert(
                path.to_owned(),
                git_text(repo, &["cat-file", "blob", blob]).await?,
            );
        }
    }
    let jobs = crate::steps::Wire::at(jobs_base.to_owned(), crate::identity::caller())?;
    let dispatcher = crate::steps::Wire::at(
        crate::tenant_publish::service_on_door(jobs_base, "dispatcher")?,
        crate::identity::caller(),
    )?;
    let mut observation = registry_observation(plan, &sources, &jobs, &dispatcher).await?;
    observation
        .as_object_mut()
        .context("delivery observation malformed")?
        .insert("publication".into(), source["publication"].clone());
    Ok(observation)
}

pub(crate) async fn run(
    plan_path: &Path,
    repo: Option<&Path>,
    publication: &str,
    reader: Option<&Path>,
) -> Result<()> {
    let raw = tokio::fs::read_to_string(plan_path)
        .await
        .with_context(|| format!("reading delivery plan {}", plan_path.display()))?;
    let plan: Value = serde_json::from_str(&raw).context("delivery plan JSON unread")?;
    let current = std::env::current_dir()?;
    let mut report = observe_with_reader(
        repo.unwrap_or(&current),
        &plan,
        publication,
        &crate::sor_client::jobs_base()?,
        reader,
    )
    .await?;
    report
        .as_object_mut()
        .context("delivery observation malformed")?
        .insert("plan_json".into(), json!(raw));
    println!("{}", serde_json::to_string_pretty(&report)?);
    Ok(())
}

/// The existing publisher's typed validation and equality, not another list of
/// fields a delivery reader guesses are significant. The comparators are already
/// pinned to the standing drift readers in boss-testing.
pub(crate) fn registry_equal(
    evidence: &str,
    name: &str,
    source: &str,
    live: &Value,
) -> Result<bool> {
    match evidence {
        "workflow_equality" => {
            // This evidence kind observes the product's platform bundle,
            // loaded by load_workflows with the platform owner. A tenant's
            // equal-looking row cannot supply publication of that bundle.
            let specs = boss_jobs::seed_loader::parse_workflows(source, "platform", name)?;
            if specs.len() != 1 || specs[0].kind != name {
                bail!("workflow file does not declare exactly its bound kind {name}");
            }
            Ok(live.get("kind").and_then(Value::as_str) == Some(name)
                && live.get("owning_team").and_then(Value::as_str)
                    == Some(specs[0].owning_team.as_str())
                && live.get("status").and_then(Value::as_str) == Some("active")
                && live
                    .get("version")
                    .and_then(Value::as_i64)
                    .is_some_and(|v| v > 0)
                && boss_jobs::bootstrap::workflow_changes(live, &specs[0]).is_empty())
        }
        "rule_equality" => {
            let rules = boss_dispatcher::rules::registry::parse_raw(source)?;
            if rules.rules.len() != 1 || rules.rules[0].name != name {
                bail!("rule file does not declare exactly its bound name {name}");
            }
            let file = &rules.rules[0];
            boss_dispatcher::rules::authoring::validate(file)?;
            let row: boss_dispatcher::rules::authoring::RuleVersion =
                serde_json::from_value(live.clone()).context("live rule row unread")?;
            Ok(row.name == name
                && row.source.is_none()
                && row.status == "active"
                && row.version > 0
                && u32::try_from(row.version).ok() == Some(file.version)
                && boss_dispatcher::rules::seed::differing_fields(file, &row.to_raw()).is_empty())
        }
        _ => bail!("unsupported registry evidence contract {evidence}"),
    }
}

#[cfg(test)]
mod tests {
    #[tokio::test]
    async fn an_unqualified_root_reader_is_never_executed() {
        let dir = std::env::temp_dir().join(format!("delivery-reader-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let reader = dir.join("root-tree.sh");
        let marker = dir.join("executed");
        std::fs::write(&reader, format!("touch '{}'\n", marker.display())).unwrap();
        assert!(
            super::root_reading(&dir, &reader, &"a".repeat(40))
                .await
                .is_err()
        );
        assert!(!marker.exists());
        std::fs::remove_dir_all(dir).unwrap();
    }
    #[test]
    fn root_publication_names_one_exact_native_main() {
        let sha = "a".repeat(40);
        assert_eq!(
            super::root_main(&format!("{sha}\trefs/heads/main\n")).unwrap(),
            sha
        );
        for invalid in [
            String::new(),
            format!("{sha}\trefs/heads/other\n"),
            format!("{sha}\trefs/heads/main\n{sha}\trefs/heads/main\n"),
            "abc\trefs/heads/main\n".into(),
        ] {
            assert!(super::root_main(&invalid).is_err(), "{invalid:?}");
        }
    }
    use super::*;
    use serde_json::json;

    #[test]
    fn old_pinned_train_protocols_keep_their_required_image_completion() {
        let mut train = json!({"steps":[{"spec_slug":"converged","title":"In transit — cluster converged","fields":[
            {"name":"cluster_commit","field_type":"string","required":true},
            {"name":"verified","field_type":"string","required":true}]}]});
        assert!(!compatible_completion(&train));
        train["steps"][0]["fields"]
            .as_array_mut()
            .unwrap()
            .push(json!({"name":"delivery_evidence","field_type":"object","required":true}));
        assert!(!compatible_completion(&train));
        train["steps"][0]["fields"][0]["required"] = json!(false);
        assert!(compatible_completion(&train));
        train["steps"][0]["fields"][2]["required"] = json!(false);
        assert!(!compatible_completion(&train));
        assert!(!compatible_completion(&json!({})));
    }

    #[test]
    fn completion_selection_requires_the_recorded_native_policy_and_frozen_plan() {
        let mut policy = crate::delivery_policy::DeliveryPolicy::compiled();
        policy.version = 8;
        policy.channels = json!({"schema":1,"enabled":true,"rules":[]});
        let mut train = json!({"metadata":{"delivery_policy_version":8,"delivery_plan":{
            "route":"data","policy":{"name":"train-conductor","version":8,"channels":policy.channels}}},
            "steps":[{"spec_slug":"converged","fields":[{"name":"delivery_evidence","field_type":"object","required":true}]}]});
        assert!(completion_plan(&train, &policy).is_some());
        train["metadata"]["delivery_policy_version"] = json!(7);
        assert!(completion_plan(&train, &policy).is_none());
        train["metadata"]["delivery_policy_version"] = json!(8);
        train["metadata"]["delivery_plan"]["policy"]["channels"]["enabled"] = json!(false);
        assert!(completion_plan(&train, &policy).is_none());
        assert!(
            completion_plan(&train, &crate::delivery_policy::DeliveryPolicy::compiled()).is_none()
        );
    }

    #[test]
    fn the_product_train_row_declares_channel_evidence_without_fabricating_a_cluster_commit() {
        let source = include_str!("../../../../infra/platform/workflows/pr-train.toml");
        let specs =
            boss_jobs::seed_loader::parse_workflows(source, "platform", "pr-train.toml").unwrap();
        let mut train = serde_json::to_value(&specs[0]).unwrap();
        for step in train["steps"].as_array_mut().unwrap() {
            step["spec_slug"] = step["title"].clone();
        }
        assert!(compatible_completion(&train));
    }

    struct Readings(std::collections::BTreeMap<String, Value>);

    #[async_trait::async_trait]
    impl crate::car_retire::Door for Readings {
        async fn send(
            &self,
            method: reqwest::Method,
            path: &str,
            body: Option<Value>,
        ) -> Result<Option<Value>> {
            assert_eq!(method, reqwest::Method::GET);
            assert!(body.is_none());
            Ok(self.0.get(path).cloned())
        }
    }

    #[tokio::test]
    async fn native_population_closure_counts_every_identity_and_retains_failed_pages() {
        let query = "/api/jobs?kind=pr-train&full=true";
        let path = |offset| format!("{query}&limit={}&offset={offset}", crate::train::PAGE_LIMIT);
        let good = [
            json!({"total":2,"data":[{"id":"one"}]}),
            json!({"total":2,"data":[{"id":"two"}]}),
        ];
        let door = Readings(std::collections::BTreeMap::from([
            (path(0), good[0].clone()),
            (path(1), good[1].clone()),
        ]));
        let mut originals = Vec::new();
        assert_eq!(
            read_all(&door, query, &mut originals).await.unwrap().len(),
            2
        );
        assert_eq!(originals.len(), 2);
        assert_eq!(originals[1]["answer"], good[1]);
        for pages in [
            [json!({"total":1,"data":[]}), Value::Null],
            [good[0].clone(), json!({"total":2,"data":[{"id":"one"}]})],
            [good[0].clone(), json!({"total":3,"data":[{"id":"two"}]})],
            [good[0].clone(), json!({"total":2,"data":[]})],
            [json!({"total":1,"data":[{"id":""}]}), Value::Null],
            [
                json!({"total":1,"data":[{"id":"one"},{"id":"two"}]}),
                Value::Null,
            ],
            [json!({"total":2.0,"data":[{"id":"one"}]}), Value::Null],
        ] {
            let door = Readings(std::collections::BTreeMap::from([
                (path(0), pages[0].clone()),
                (path(1), pages[1].clone()),
            ]));
            let mut originals = Vec::new();
            assert!(
                read_all(&door, query, &mut originals).await.is_err(),
                "{pages:?}"
            );
            assert_eq!(originals[0]["answer"], pages[0]);
            if originals.len() == 2 {
                assert_eq!(originals[1]["answer"], pages[1]);
            }
        }
    }

    #[tokio::test]
    async fn native_registry_observations_conserve_answers_and_refuse_missing_or_different_rows() {
        let source = include_str!("../../../../infra/platform/workflows/learning.toml");
        let specs =
            boss_jobs::seed_loader::parse_workflows(source, "platform", "learning.toml").unwrap();
        let mut live = serde_json::to_value(&specs[0]).unwrap();
        live["status"] = json!("active");
        live["version"] = json!(1);
        let policy_source = std::fs::read_to_string(
            boss_testing::repo_root().join("infra/platform/delivery-policy/train-conductor.toml"),
        )
        .unwrap();
        let mut policy = boss_jobs::seed_loader::parse_delivery_policies(&policy_source, "policy")
            .unwrap()
            .remove(0)
            .row;
        policy.channels = json!({"schema":1,"enabled":true,"rules":[{"prefix":"infra/platform/workflows/","suffix":".toml","evidence":"workflow_equality"}]});
        let facts = json!({"base":"a".repeat(40),"head":"b".repeat(40),
            "policy":{"name":policy.name,"version":policy.version,"channels":policy.channels},
            "changes":[{"path":"infra/platform/workflows/learning.toml","old":"c".repeat(40),"new":"d".repeat(40),"old_mode":"100644","new_mode":"100644"}]});
        let plan = request(&facts).await.unwrap();
        let train_source = include_str!("../../../../infra/platform/workflows/pr-train.toml");
        let train_specs =
            boss_jobs::seed_loader::parse_workflows(train_source, "platform", "pr-train.toml")
                .unwrap();
        let mut protocol = serde_json::to_value(&train_specs[0]).unwrap();
        protocol["status"] = json!("active");
        protocol["version"] = json!(1);
        let sources = std::collections::BTreeMap::from([
            (
                "infra/platform/workflows/learning.toml".to_owned(),
                source.to_owned(),
            ),
            (
                "infra/platform/workflows/pr-train.toml".to_owned(),
                train_source.to_owned(),
            ),
        ]);
        let path = format!(
            "/api/delivery/policy/train-conductor/versions/{}",
            policy.version
        );
        let pending_path = format!(
            "/api/jobs?kind=pr-train&status=open&full=true&limit={}&offset=0",
            crate::train::PAGE_LIMIT
        );
        let mut rows = Readings(std::collections::BTreeMap::from([
            (path.clone(), serde_json::to_value(&policy).unwrap()),
            ("/api/workflows/learning".into(), live.clone()),
            ("/api/workflows/pr-train".into(), protocol.clone()),
            (pending_path.clone(), json!({"data":[],"total":0})),
        ]));
        let original = plan.clone();
        let observed = registry_observation(&plan, &sources, &rows, &rows)
            .await
            .unwrap();
        assert_eq!(observed["arrived"], true);
        assert_eq!(observed["selection_ready"], true);
        assert!(
            observed["native_reads"]
                .as_array()
                .unwrap()
                .iter()
                .any(|r| r["path"] == "/api/workflows/learning" && r["answer"] == live)
        );
        rows.0.remove("/api/workflows/pr-train");
        assert_eq!(
            registry_observation(&plan, &sources, &rows, &rows)
                .await
                .unwrap()["arrived"],
            false
        );
        assert_eq!(
            registry_observation(&plan, &sources, &rows, &rows)
                .await
                .unwrap()["selection_ready"],
            false
        );
        rows.0.insert("/api/workflows/pr-train".into(), protocol);
        rows.0.insert(pending_path.clone(),json!({"data":[{"id":"old-pin","kind":"pr-train","status":"open","steps":[
            {"spec_slug":"merged","metadata":{"merge_ref":"abcdef0123456789"}},
            {"spec_slug":"converged","fields":[{"name":"cluster_commit","required":true}]}]}],"total":1}));
        assert_eq!(
            registry_observation(&plan, &sources, &rows, &rows)
                .await
                .unwrap()["arrived"],
            false
        );
        assert_eq!(
            registry_observation(&plan, &sources, &rows, &rows)
                .await
                .unwrap()["selection_ready"],
            false
        );
        rows.0.insert(pending_path, json!({"data":[],"total":0}));
        live["steps"][0]["ready_when"] = json!("false");
        rows.0
            .insert("/api/workflows/learning".into(), live.clone());
        let wrong = registry_observation(&plan, &sources, &rows, &rows)
            .await
            .unwrap();
        assert_eq!(wrong["arrived"], false);
        assert_eq!(
            wrong["selection_ready"], true,
            "an unpublished body is a pending equality obligation, not an image input"
        );
        assert!(
            wrong["native_reads"]
                .as_array()
                .unwrap()
                .iter()
                .any(|r| r["path"] == "/api/workflows/learning" && r["answer"] == live)
        );
        rows.0.remove("/api/workflows/learning");
        assert_eq!(
            registry_observation(&plan, &sources, &rows, &rows)
                .await
                .unwrap()["arrived"],
            false
        );
        rows.0.remove(&path);
        assert_eq!(
            registry_observation(&plan, &sources, &rows, &rows)
                .await
                .unwrap()["arrived"],
            false
        );
        assert_eq!(
            registry_observation(&plan, &sources, &rows, &rows)
                .await
                .unwrap()["selection_ready"],
            false
        );
        assert_eq!(plan, original);
    }

    #[tokio::test]
    async fn publication_is_read_from_forge_and_binds_the_whole_frozen_tree() {
        use crate::car_unland::tests::git_in;
        let root = boss_testing::scratch::scratch_dir("delivery-publication");
        let forge = root.join("forge.git");
        let repo = root.join("work");
        std::fs::create_dir_all(&forge).unwrap();
        std::fs::create_dir_all(&repo).unwrap();
        std::fs::create_dir_all(repo.join("infra/platform/delivery-policy")).unwrap();
        std::fs::create_dir_all(repo.join("docs")).unwrap();
        git_in(&forge, &["init", "-q", "--bare", "-b", "main"]);
        git_in(&repo, &["init", "-q", "-b", "main"]);
        boss_testing::scratch::write_file(
            &repo.join("infra/platform/delivery-policy/train-conductor.toml"),
            "[[delivery_policy]]\nname='train-conductor'\nversion=8\n[delivery_policy.channels]\nschema=1\nenabled=true\n[[delivery_policy.channels.rules]]\nprefix='docs/'\nsuffix='.md'\nevidence='forge_blob'\n",
        );
        boss_testing::scratch::write_file(&repo.join("docs/a.md"), "old\n");
        git_in(&repo, &["add", "."]);
        git_in(&repo, &["commit", "-qm", "base"]);
        let base = git_in(&repo, &["rev-parse", "HEAD"]);
        boss_testing::scratch::write_file(&repo.join("docs/a.md"), "published\n");
        git_in(&repo, &["commit", "-qam", "documentation"]);
        let head = git_in(&repo, &["rev-parse", "HEAD"]);
        git_in(&repo, &["remote", "add", "origin", forge.to_str().unwrap()]);
        git_in(&repo, &["push", "-q", "origin", "main"]);
        let plan = request(&json!({"operation":"git_plan", "repo":repo, "base":base, "head":head}))
            .await
            .unwrap();
        let observed = source_observation(&repo, &plan, &head).await.unwrap();
        assert_eq!(observed["publication"]["carries"], true);
        assert_eq!(
            observed["publication"]["tree"],
            git_in(&repo, &["rev-parse", "HEAD^{tree}"])
        );
        assert_eq!(
            observed["receipts"][0]["new"],
            plan["obligations"][0]["new"]
        );
        assert_eq!(observed["receipts"][0]["observed"], true);
        let mut forged = plan.clone();
        forged["obligations"][0]["new"] = json!("0".repeat(40));
        assert!(source_observation(&repo, &forged, &head).await.is_err());
        git_in(
            &repo,
            &["push", "-q", "--force", "origin", &format!("{base}:main")],
        );
        assert!(source_observation(&repo, &plan, &head).await.is_err());
        // A different published tree cannot borrow this documentation receipt.
        assert!(source_observation(&repo, &plan, &base).await.is_err());
    }

    #[tokio::test]
    async fn the_cli_runs_the_shared_judgement_and_keeps_missing_evidence_unknown() {
        let facts = json!({
            "base": "a".repeat(40), "head": "b".repeat(40),
            "policy": {"name": "train-conductor", "version": 8,
                "channels": {"schema": 1, "enabled": true, "rules": [{"prefix": "docs/", "suffix": ".md", "evidence": "forge_blob"}]}},
            "changes": [{"path": "docs/example.md", "old": "c".repeat(40), "new": "d".repeat(40),
                "old_mode": "100644", "new_mode": "100644"}]
        });
        let plan = request(&facts).await.unwrap();
        assert_eq!(plan["route"], "data");
        assert_eq!(plan["head"], facts["head"]);
        assert_eq!(
            request(&json!({"operation": "satisfied", "plan": plan, "receipts": []}))
                .await
                .unwrap(),
            false
        );
        assert_eq!(request(&json!({})).await.unwrap()["route"], "software");
    }

    #[test]
    fn workflow_delivery_uses_the_actual_typed_loader_and_existing_whole_row_comparator() {
        let source = include_str!("../../../../infra/platform/workflows/learning.toml");
        let specs =
            boss_jobs::seed_loader::parse_workflows(source, "platform", "learning.toml").unwrap();
        let mut live = serde_json::to_value(&specs[0]).unwrap();
        live["status"] = json!("active");
        live["version"] = json!(1);
        assert!(registry_equal("workflow_equality", "learning", source, &live).unwrap());
        live["owning_team"] = json!("another-tenant");
        let silent_owner = source
            .lines()
            .filter(|line| !line.trim_start().starts_with("owning_team ="))
            .collect::<Vec<_>>()
            .join("\n");
        assert!(!registry_equal("workflow_equality", "learning", &silent_owner, &live).unwrap());
        live["owning_team"] = json!("platform");
        live["steps"][0]["ready_when"] = json!("false");
        assert!(!registry_equal("workflow_equality", "learning", source, &live).unwrap());
        live["status"] = json!("draft");
        assert!(!registry_equal("workflow_equality", "learning", source, &live).unwrap());
        assert!(registry_equal("workflow_equality", "learning", "not valid toml", &live).is_err());
    }

    #[test]
    fn rule_delivery_requires_typed_active_version_and_every_real_content_field() {
        let source = std::fs::read_to_string(
            boss_testing::repo_root().join("infra/dispatcher/rules/check-gcs-backup-weekly.toml"),
        )
        .unwrap();
        let registry = boss_dispatcher::rules::registry::parse_raw(&source).unwrap();
        let rule = &registry.rules[0];
        let mut live = serde_json::to_value(rule).unwrap();
        live["status"] = json!("active");
        live["created_at"] = json!("2026-10-16T00:00:00Z");
        assert!(registry_equal("rule_equality", &rule.name, &source, &live).unwrap());
        live["source"] = json!("tenant:other");
        assert!(!registry_equal("rule_equality", &rule.name, &source, &live).unwrap());
        live["source"] = Value::Null;
        live["when"] = json!("false");
        assert!(!registry_equal("rule_equality", &rule.name, &source, &live).unwrap());
        live["version"] = json!(rule.version + 1);
        assert!(!registry_equal("rule_equality", &rule.name, &source, &live).unwrap());
        assert!(registry_equal("rule_equality", &rule.name, "not valid toml", &live).is_err());
    }
}
