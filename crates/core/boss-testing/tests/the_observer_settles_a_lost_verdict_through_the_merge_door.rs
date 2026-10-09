//! THE ESTATE OBSERVER SETTLES A DEAD RUNNER'S VERDICT THROUGH THE STEP
//! MERGE DOOR; NO PUT CARRIES METADATA.
//!
//! Backlog e39a9d2a (correction 2026-09-23, measured again 2026-09-24):
//! the observer settled every gate runner that died with its node as ONE
//! step PUT of `{status: "completed", metadata: {verdict: "lost",
//! receipt}}` built with no read. The step PUT replaces metadata
//! wholesale, and the registry materializes keys onto every step at
//! admission (`metadata_defaults` — gate-run.toml gives the verdict step
//! `heartbeat_at` — plus `authority_role`, `station`, `audience`,
//! `claimable`), so each settle shed them. It is the same step the
//! gate-runner (`infra/gate-runner/run.sh`) and `boss gate`'s refusals
//! already write the merge-then-flip way (cars 2 and 3 of that item);
//! the observer is the third writer of it and now reads the same.
//!
//! A text pin, in the shape of `run_sh_verdict`'s: the script lives in a
//! CronJob manifest whose body needs kubectl and a live API, so what is
//! held is the shape of the two writes, by name.

use boss_testing::repo_root;

const OBSERVER: &str = "infra/cluster/manifests/boss-estate-observe.yaml";

/// The dead-runner settle, from its heading comment to the end of its
/// loop, with comment lines dropped — so prose that NAMES the old shape
/// cannot satisfy or fail the pin.
fn settle_block() -> String {
    let path = repo_root().join(OBSERVER);
    let text = std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("reading {}: {e}", path.display()));
    let start = text
        .find("A DEAD RUNNER IS SETTLED BY WHOEVER CAN SEE IT DIE")
        .expect("the observer still settles dead gate runners");
    let rest = &text[start..];
    let end = rest
        .find("done <\"$WORK/dead-gates.tsv\"")
        .expect("the settle loop reads dead-gates.tsv");
    rest[..end]
        .lines()
        .filter(|l| !l.trim_start().starts_with('#'))
        .collect::<Vec<_>>()
        .join("\n")
}

#[test]
fn the_lost_verdict_is_merged_then_the_step_flips_alone() {
    let block = settle_block();
    assert!(
        block.contains("-X PATCH"),
        "the verdict and receipt must be written with a PATCH:\n{block}"
    );
    assert!(
        block.contains("\"$JOBS_API/api/jobs/$packet/steps/$step/metadata\""),
        "…through the step merge door, PATCH …/steps/{{id}}/metadata:\n{block}"
    );
    assert!(
        block.contains("'{\"status\":\"completed\"}'"),
        "the completion must be a status-only PUT body:\n{block}"
    );
    assert!(
        !block.contains("metadata: {"),
        "no settle body may nest the verdict under `metadata` — a PUT carrying \
         metadata replaces the step's stored keys wholesale (e39a9d2a):\n{block}"
    );
}

/// Merge FIRST: `verdict` is required at done and the flip is where that
/// is judged, so a flip sent before the merge would be refused — and a
/// flip sent after a FAILED merge would be refused the same way, so the
/// flip is only attempted once the merge answered 2xx.
#[test]
fn the_merge_goes_before_the_flip() {
    let block = settle_block();
    let merge = block
        .find("/steps/$step/metadata\"")
        .expect("the merge door is written");
    let flip = block
        .find("'{\"status\":\"completed\"}'")
        .expect("the status-only flip is written");
    assert!(merge < flip, "the merge must precede the flip:\n{block}");
    assert!(
        block[merge..flip].contains("2??)"),
        "the flip must be gated on the merge answering 2xx:\n{block}"
    );
}

/// A RUNNER THAT LEAVES ITS VERDICT IS NOT THIS PASS'S TO SETTLE (backlog
/// 934ccad1). A Job of the pod-log layout writes nothing itself, so its
/// red gate is a Failed Job over an OPEN packet until the conductor
/// records the receipt — and this pass would settle that `lost`, turning
/// a real `failed` into an infrastructure death. The pass's own jq
/// program is lifted from the manifest and run over a Job list: the
/// old-layout corpse is still listed, the carrier one is not.
#[test]
fn a_failed_job_that_leaves_its_verdict_is_not_listed_as_a_dead_runner() {
    let path = repo_root().join(OBSERVER);
    let text = std::fs::read_to_string(&path).unwrap();
    let start = text
        .find("jq -r '(.items // []) as $all")
        .expect("the dead-runner selection");
    let program = &text[start + "jq -r '".len()..];
    let program = &program[..program
        .find("' \"$WORK/gate-jobs.json\"")
        .expect("the selection reads gate-jobs.json")];
    let failed = |name: &str, packet: &str, carrier: Option<&str>| {
        let mut labels = serde_json::json!({"app": "gate-runner", "boss.dev/packet": packet});
        if let Some(c) = carrier {
            labels["boss.dev/verdict-carrier"] = serde_json::json!(c);
        }
        serde_json::json!({
            "metadata": {"name": name, "labels": labels},
            "status": {"failed": 1, "conditions": [
                {"type": "Failed", "reason": "BackoffLimitExceeded",
                 "lastTransitionTime": "2026-10-07T19:00:00Z", "message": "m"}]}
        })
    };
    let list = serde_json::json!({"items": [
        failed("gate-old-11111", "p-old", None),
        failed("gate-new-22222", "p-new", Some("pod-log")),
        // Any other word under the label is not that layout: still listed.
        failed("gate-odd-33333", "p-odd", Some("true")),
    ]});
    let dir = boss_testing::scratch_dir("observer-dead-runner-selection");
    let file = dir.join("gate-jobs.json");
    boss_testing::write_file(&file, &list.to_string());
    let out = std::process::Command::new("jq")
        .args(["-r", program])
        .arg(&file)
        .output()
        .expect("jq runs");
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let listed: Vec<String> = String::from_utf8_lossy(&out.stdout)
        .lines()
        .map(|l| l.split('\t').next().unwrap_or_default().to_string())
        .collect();
    assert_eq!(
        listed,
        vec!["gate-old-11111".to_string(), "gate-odd-33333".to_string()],
        "the observer's dead-runner pass lists a Job whose verdict the conductor records"
    );
}
