//! The gate rig is infrastructure, and infrastructure that only exists
//! on a cluster is infrastructure nobody can reproduce.
//!
//! TWO FAILURES ON 2026-08-26, both silent, both found by reading.
//!
//! THE SCRIPT DRIFTED. The gate Job cannot run `run.sh` out of the
//! clone, because `run.sh` is what performs the clone — so it reaches
//! the pod through ConfigMap `gate-runner-script`. The manifest carried
//! the regeneration command in a COMMENT and asked whoever applied it
//! to remember. The live ConfigMap held the script from
//! `perf/the-gate-uses-the-cpu-it-was-given`, a branch measured 16%
//! slower and deliberately never parked, while main carried something
//! else. Ten gates ran that day and every receipt was produced by a
//! script that is not in this repository. CLAUDE.md §9a: a comment
//! asking the next person to keep two copies in sync is not a
//! mechanism. `apply-script-configmap.sh` is the mechanism, and
//! `--check` answers "is the thing that ran the thing I have?".
//!
//! THE MANIFEST WAS NEVER TRACKED AT ALL. The variant that made six
//! concurrent gates possible existed only in an agent job's scratch
//! directory — one `rm -rf` from taking the capability with it. It was
//! tracked as `gate-runner-local.yaml`, and on 2026-09-03 its per-pod
//! workspace became the shape of `gate-runner.yaml` itself (packet
//! 28de3845), which left the sibling a copy nothing launched: `boss
//! gate` renders `gate-runner.yaml` and the conductor its installed
//! copy, and the sibling had drifted to carry no `boss.dev/packet`
//! label — a Job every cluster read of a gate-run would miss. Deleted
//! 2026-09-25 (review of car 2ca8c7e9, backlog 137c176d); `boss gate`
//! now refuses any manifest whose Job lacks that label.

use boss_testing::repo_root;

fn read(rel: &str) -> String {
    let path = repo_root().join(rel);
    std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("reading {}: {e}", path.display()))
}

const SHARED: &str = "infra/gate-runner/gate-runner.yaml";
/// The dev pod: its `postgres` sidecar is the harness database every
/// builder's local suite runs against.
const DEV: &str = "infra/cluster/manifests/boss-dev.yaml";
const APPLY: &str = "infra/gate-runner/apply-script-configmap.sh";
const CONFIGMAP: &str = "gate-runner-script";

/// The per-pod workspace is the whole reason concurrent gates are safe.
/// Since backlog 52ea56ac it is the pod's OWN directory of the gate
/// claim rather than an emptyDir (it must share the seed's filesystem
/// for the seeding copy to reflink); gate_runner_parallel_workspace.rs
/// pins the rest of that shape.
#[test]
fn the_runner_keeps_its_workspace_per_pod() {
    assert!(
        read(SHARED).contains("mountPath: /gate-target, subPathExpr: runs/$(POD_NAME)}"),
        "{SHARED} must give each pod its own /gate-target. A shared workspace cannot be \
         shared by two gates: each `git checkout -f -B` yanks the tree from under the other \
         and both write the same receipt path, which crossed three verdicts on 2026-08-24."
    );
}

/// THE BUILD ROLE IS A LABEL, NOT A MACHINE.
///
/// The scratch copy this was recovered from pinned
/// `kubernetes.io/hostname: w-1`. When w-1 was cordoned for a suspected
/// NVMe fault, a hard pin would have left the gate with no schedulable
/// node and stopped gating entirely; preferred affinity degrades to a
/// slower build on a control plane instead of no build at all.
#[test]
fn the_runner_pins_no_hostname() {
    let text = read(SHARED);
    assert!(
        !text.contains("kubernetes.io/hostname"),
        "{SHARED} pins a specific node. Replacing the build machine must be a label move, \
         not a car."
    );
    assert!(
        text.contains("boss.dev/purpose"),
        "{SHARED} must select the build node by role label"
    );
}

/// The pair that actually drifted: the ConfigMap the Jobs mount and the
/// one the apply script creates have to be the same object.
#[test]
fn the_script_configmap_has_one_name_everywhere() {
    let apply = read(APPLY);
    assert!(
        apply.contains(CONFIGMAP),
        "{APPLY} must create the ConfigMap named {CONFIGMAP}"
    );
    assert!(
        read(SHARED).contains(CONFIGMAP),
        "{SHARED} mounts a script ConfigMap that {APPLY} does not create — the gate would \
         run whatever happened to be in the cluster, which is the failure this file \
         exists for"
    );
}

/// The keys `--from-file=<key>=…` declares on the gate-runner-script
/// ConfigMap in one script. In the converge only the arguments sourced
/// from the `/gate` mount count: the step-plugins ConfigMap is built a
/// few lines above with the same flag.
fn configmap_keys(text: &str, only_from: Option<&str>) -> std::collections::BTreeSet<String> {
    text.split("--from-file=")
        .skip(1)
        .filter_map(|rest| {
            let arg = rest.split_whitespace().next()?.trim_matches('"');
            let (key, source) = arg.split_once('=')?;
            match only_from {
                Some(prefix) if !source.starts_with(prefix) => None,
                _ => Some(key.to_string()),
            }
        })
        .collect()
}

/// THE KEY NAMES LIVE THREE TIMES, so they get an equality test (§9a;
/// design 1d7a1669 pin 3). The converge that writes the ConfigMap on every
/// train, the hand verb that checks it for drift, and the manifest's
/// command that execs one of its keys must agree — a manifest naming a
/// key the live ConfigMap lacks starts a pod that cannot even report.
#[test]
fn the_converge_and_the_hand_verb_build_the_same_script_keys() {
    let converge = configmap_keys(
        &read("infra/forge/cluster-deploy-runner.sh"),
        Some("/gate/"),
    );
    let apply = configmap_keys(&read(APPLY), None);
    let want: std::collections::BTreeSet<String> = ["bootstrap.sh", "run.sh", "seed-refresh.sh"]
        .iter()
        .map(|k| k.to_string())
        .collect();
    assert_eq!(
        converge, want,
        "the converge must write the bootstrap, main's run.sh and the seed housekeeping \
         (backlog 3e2c8ddc) into {CONFIGMAP}"
    );
    assert_eq!(
        apply, converge,
        "{APPLY} and the converge build {CONFIGMAP} with different keys — its --check would \
         answer for an object the converge never writes"
    );
    // EVERY command the manifest runs from the ConfigMap: since 3e2c8ddc
    // the gate's run.sh AND the seed-refresh container's seed-refresh.sh.
    let manifest = read(SHARED);
    let execs: Vec<&str> = manifest
        .lines()
        .map(str::trim)
        .filter(|l| l.starts_with("command:"))
        .filter_map(|l| l.split("/repo-script/").nth(1))
        .filter_map(|rest| rest.split('"').next())
        .collect();
    assert_eq!(
        execs,
        ["run.sh", "seed-refresh.sh"],
        "the manifest's commands run the gate's script and the helper's from /repo-script"
    );
    for key in execs {
        assert!(
            converge.contains(key),
            "{SHARED} execs /repo-script/{key}, a key the converge does not write ({converge:?})"
        );
    }
    assert!(
        read("infra/gate-runner/bootstrap.sh").contains("/run.sh"),
        "the bootstrap's fallback is the ConfigMap's run.sh key"
    );
}

/// The mechanism has to be runnable, not merely present. A regeneration
/// step that is documented and not executable is the comment we already
/// had.
#[test]
fn the_apply_script_is_executable_and_offers_a_drift_check() {
    let path = repo_root().join(APPLY);
    let apply = read(APPLY);
    assert!(
        apply.contains("--check"),
        "{APPLY} must offer --check: the question that went unasked for a day is 'has the \
         live script drifted from the tree?', and it needs an answer that changes nothing"
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(&path)
            .expect("apply script exists")
            .permissions()
            .mode();
        assert!(
            mode & 0o111 != 0,
            "{APPLY} is not executable (mode {mode:o})"
        );
    }
}

/// The manifest must stop TELLING people to regenerate the ConfigMap
/// by hand and point at the thing that does it.
#[test]
fn the_manifest_points_at_the_mechanism_not_at_a_ritual() {
    assert!(
        read(SHARED).contains("apply-script-configmap.sh"),
        "{SHARED} must name {APPLY} where it explains how the script reaches the pod. It \
         used to inline the kubectl command as a comment, and the copy in the cluster \
         drifted to an unlanded branch's version without anyone noticing."
    );
}

/// Container blocks of the pod spec, as (name, block text).
///
/// Hand-scanned rather than YAML-parsed because the workspace carries no
/// YAML parser and one dependency for one lint is a poor trade; the
/// structure needed here is shallow. `volumes:` entries look exactly
/// like containers at this indentation, so the scan is bounded to the
/// `containers:` / `initContainers:` sections — a postgres native
/// sidecar lives in the latter, and a check that silently skipped it
/// would pass on the very container the warning named.
fn pod_containers(text: &str) -> Vec<(String, String)> {
    let mut out: Vec<(String, String)> = Vec::new();
    let mut in_section = false;
    for line in text.lines() {
        let trimmed = line.trim_start();
        let indent = line.len() - trimmed.len();
        if indent == 6 && trimmed.ends_with(':') {
            in_section = matches!(trimmed, "containers:" | "initContainers:");
            continue;
        }
        if !in_section {
            continue;
        }
        if indent == 8
            && let Some(name) = trimmed.strip_prefix("- name: ")
        {
            out.push((name.trim().to_string(), String::new()));
            continue;
        }
        if let Some(last) = out.last_mut() {
            last.1.push_str(line);
            last.1.push('\n');
        }
    }
    out
}

/// The pod spec's containers as (section, name), in order: which list
/// each is declared in is the whole of the judged-init layout.
fn pod_sections(text: &str) -> Vec<(String, String)> {
    let mut out = Vec::new();
    let mut section = String::new();
    for line in text.lines() {
        let trimmed = line.trim_start();
        let indent = line.len() - trimmed.len();
        if indent == 6 && trimmed.ends_with(':') {
            section = trimmed.trim_end_matches(':').to_string();
            continue;
        }
        if indent == 8
            && matches!(section.as_str(), "containers" | "initContainers")
            && let Some(name) = trimmed.strip_prefix("- name: ")
        {
            out.push((section.clone(), name.trim().to_string()));
        }
    }
    out
}

/// The non-comment lines of a block, trimmed.
fn code_lines(block: &str) -> Vec<&str> {
    block
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
        .collect()
}

/// THE GATE IS JUDGED IN AN INIT CONTAINER AND THE SEED IS REFRESHED
/// AFTER IT (backlog 3e2c8ddc, with 82ffaff3; design b4bf8bba, both
/// questions approved 2026-10-16). The conductor records a verdict only
/// from a `gate` container that has ENDED, and the gate used to refresh
/// the warm seed and empty its workspace before ending: a green waited
/// 638 s for its own copy on gate-run 86dd2d48 and about eight minutes of
/// `find -delete` in uninterruptible I/O on 4aad01a9. Car 39ee7932 taught
/// every reader the layout; this pins the manifest that runs it.
///
/// FOUR FACTS TRAVEL TOGETHER, and each one alone is a defect: the Job's
/// layout label (which finished Jobs owe a housekeeping record), the
/// gate's GATE_LAYOUT (run.sh hands off instead of refreshing), `gate`
/// as an init container (it ends on its verdict), and the `seed-refresh`
/// container after it (someone takes the handoff). A handoff nobody takes
/// leaves a workspace for the hour-idle sweep and the seed cold; a label
/// without the helper owes a record nothing writes.
#[test]
fn the_gate_is_judged_in_an_init_container_and_the_seed_refreshed_after_it() {
    let text = read(SHARED);
    let job = text
        .split("\n---")
        .find(|d| d.contains("kind: Job"))
        .expect("the runner manifest carries a Job");
    // The label, on the Job's OWN metadata (the reader selects on Jobs),
    // spelled as the conductor's reader spells it (§9a).
    let reader = read("crates/orchestrators/boss-cli/src/train/carried_verdict.rs");
    for constant in [
        "LAYOUT_LABEL: &str = \"boss.dev/gate-layout\";",
        "LAYOUT_JUDGED_INIT: &str = \"judged-init\";",
    ] {
        assert!(
            reader.contains(constant),
            "the conductor's reader no longer declares {constant}"
        );
    }
    let job_metadata = job
        .split("\nspec:")
        .next()
        .expect("the Job has metadata before its spec");
    assert!(
        code_lines(job_metadata).contains(&"boss.dev/gate-layout: judged-init"),
        "{SHARED}: the Job must carry `boss.dev/gate-layout: judged-init` on its own \
         metadata — the conductor owes a finished Job's housekeeping record only on it"
    );

    assert_eq!(
        pod_sections(&text),
        [
            ("initContainers", "postgres"),
            ("initContainers", "gate"),
            ("containers", "seed-refresh"),
        ]
        .map(|(s, n)| (s.to_string(), n.to_string())),
        "{SHARED}: the judged-init layout is postgres (the native sidecar), then `gate` as \
         a one-shot init container, then `seed-refresh` as the only regular container. A \
         regular container beside the gate would run DURING the checks, and a second one \
         after it would be a second thing the pod waits on"
    );
    let containers = pod_containers(&text);
    let block = |name: &str| {
        containers
            .iter()
            .find(|(n, _)| n == name)
            .map(|(_, b)| b.clone())
            .unwrap_or_else(|| panic!("{SHARED} has no container {name}"))
    };
    let (gate, helper) = (block("gate"), block("seed-refresh"));
    let gate_code = code_lines(&gate);
    let helper_code = code_lines(&helper);

    // The gate: a ONE-SHOT init container. `restartPolicy: Always` would
    // make it a sidecar the kubelet never waits for, and the helper would
    // start beside the checks.
    assert!(
        !gate_code.iter().any(|l| l.starts_with("restartPolicy:")),
        "{SHARED}: the gate must not declare a restartPolicy — Always makes it a sidecar"
    );
    for line in [
        "- {name: GATE_LAYOUT, value: judged-init}",
        "- {name: GATE_VERDICT_CARRIER, value: pod-log}",
        "command: [\"bash\", \"/repo-script/run.sh\"]",
    ] {
        assert!(
            gate_code.contains(&line),
            "{SHARED}: the gate container must declare `{line}` — run.sh hands its seed to \
             the helper only when it is the carried runner AND told this layout"
        );
    }

    // The helper: main's script, nothing of the branch's, no credential.
    for line in [
        "command: [\"bash\", \"/repo-script/seed-refresh.sh\", \"maintain\"]",
        "- {name: NATIVE_JOB_NAME, valueFrom: {fieldRef: {fieldPath: \"metadata.labels['batch.kubernetes.io/job-name']\"}}}",
        "- {name: POD_NAME, valueFrom: {fieldRef: {fieldPath: metadata.name}}}",
        "- {name: GATE_SEED_MAINTENANCE_DEADLINE_SECS, value: \"900\"}",
        "- {name: GATE_SEED_MAINTENANCE_GRACE_SECS, value: \"30\"}",
        "imagePullPolicy: IfNotPresent",
    ] {
        assert!(
            helper_code.contains(&line),
            "{SHARED}: the seed-refresh container must declare `{line}`"
        );
    }
    let image = |code: &[&str]| {
        code.iter()
            .find_map(|l| l.strip_prefix("image: "))
            .map(str::to_string)
    };
    assert_eq!(
        image(&helper_code),
        image(&gate_code),
        "the helper runs in the image the gate just ran in: its tools (flock, timeout, cp) \
         are the ones the old layout's refresh used, and IfNotPresent finds it on the node"
    );
    let mounts: Vec<&str> = helper_code
        .iter()
        .copied()
        .filter(|l| l.contains("mountPath:"))
        .collect();
    assert_eq!(
        mounts,
        [
            "- {name: gate-disk, mountPath: /gate-seed, subPath: seed}",
            "- {name: gate-disk, mountPath: /gate-target, subPathExpr: runs/$(POD_NAME)}",
            "- {name: run-script, mountPath: /repo-script}",
        ],
        "{SHARED}: the helper mounts the seed, THIS pod's workspace and main's scripts — \
         never /gate-runs (every run's workspace), the database or the forge token"
    );
    for absent in [
        "forge-token",
        "GATE_VERDICT_CARRIER",
        "GATE_LAYOUT",
        "/gate-runs",
    ] {
        assert!(
            !helper_code.iter().any(|l| l.contains(absent)),
            "{SHARED}: the seed-refresh container must not name {absent}"
        );
    }
    let ids = |code: &[&str]| {
        code.iter()
            .filter(|l| l.starts_with("runAsUser:") || l.starts_with("runAsGroup:"))
            .map(|l| l.to_string())
            .collect::<Vec<_>>()
    };
    assert_eq!(
        ids(&helper_code),
        ids(&gate_code),
        "the helper deletes what the gate wrote and writes the seed the next gate reads: \
         the same uid and gid"
    );
}

/// THE GATE MUST MEET THE RESTRICTED PROFILE, NOT WARN ABOUT IT.
///
/// Every launch printed `Warning: would violate PodSecurity
/// "restricted:latest"` — the postgres sidecar dropped only NET_RAW and
/// nothing set runAsNonRoot. A warning is the whole consequence while
/// the cluster default enforces `baseline`, which is what made this a
/// CLIFF rather than a slope: tighten that default, or label boss-dev
/// `enforce: restricted`, and every gate Job becomes uncreatable at
/// admission. The gate is the only path a car has to a green receipt,
/// so that is all delivery stopping at once, in a shape (pods refused
/// before any container starts) that reads like a code defect and gets
/// diagnosed as one (backlog bb5a902e).
///
/// `infra/gate.sh` never renders or applies this manifest, so nothing
/// in the gate's own check roster can catch a regression here — a car
/// that re-opened the gap would go green. This test is where that gets
/// read. It asserts the properties, not their formatting, so the
/// manifests stay free to explain themselves.
#[test]
fn the_runner_meets_the_restricted_profile() {
    let rel = SHARED;
    let text = read(rel);
    assert!(
        text.contains("runAsNonRoot: true"),
        "{rel} must set runAsNonRoot: true on the POD, so a container added later \
             inherits it instead of silently re-opening the gap"
    );
    assert!(
        text.contains("seccompProfile: {type: RuntimeDefault}"),
        "{rel} must keep the RuntimeDefault seccomp profile the restricted profile requires"
    );
    let containers = pod_containers(&text);
    assert!(
        containers.iter().any(|(n, _)| n == "gate")
            && containers.iter().any(|(n, _)| n == "postgres"),
        "{rel}: the scan found {:?}, not the gate + postgres pair — either a container was \
             renamed or this scan no longer sees the sections it must cover",
        containers.iter().map(|(n, _)| n).collect::<Vec<_>>()
    );
    for (name, block) in &containers {
        assert!(
            block.contains("allowPrivilegeEscalation: false"),
            "{rel}: container {name} must set allowPrivilegeEscalation: false"
        );
        assert!(
            block.contains(r#"capabilities: {drop: ["ALL"]}"#),
            "{rel}: container {name} must drop ALL capabilities. NET_RAW-only was the \
                 right read of the postgres entrypoint's root-then-gosu hop and the wrong \
                 fix: name uid 999 and the root phase never happens, so nothing needs \
                 CHOWN/SETUID/SETGID"
        );
        let uid = block
            .lines()
            .find_map(|l| l.trim().strip_prefix("runAsUser: ").map(str::trim))
            .unwrap_or_else(|| {
                panic!(
                    "{rel}: container {name} names no runAsUser. runAsNonRoot alone makes \
                         the kubelet REFUSE an image whose user is root, and boss-ci declares \
                         no USER — so the uid has to be explicit here or the pod never starts"
                )
            });
        assert_ne!(
            uid, "0",
            "{rel}: container {name} runs as uid 0, which runAsNonRoot forbids"
        );
    }
}

/// A named uid needs a writable HOME, and boss-ci has no passwd entry
/// for one.
///
/// `run.sh`'s first act is `git config --global credential.helper`,
/// under `set -e`: it writes `$HOME/.gitconfig`. containerd hands an
/// unresolvable uid `HOME=/`, which is not writable, so the gate would
/// die before the clone — on EVERY branch, with a message about git
/// config rather than about a uid. The fix has to be an env var in the
/// manifest and not a `mkdir` in run.sh, because the script reaches the
/// pod through a separately-applied ConfigMap: a manifest depending on
/// a run.sh change would be live before the change was.
#[test]
fn the_non_root_gate_is_given_a_writable_home() {
    let rel = SHARED;
    let text = read(rel);
    let (_, gate) = pod_containers(&text)
        .into_iter()
        .find(|(n, _)| n == "gate")
        .unwrap_or_else(|| panic!("{rel} has no container named gate"));
    assert!(
        gate.contains("{name: HOME, value: /gate-target}"),
        "{rel}: the gate container must set HOME to the workspace mount. It exists before \
             the container starts and fsGroup has already made it group-writable, which an \
             arbitrary path would not be"
    );
}

/// A TEST DATABASE DIES WITH ITS POD, SO DURABILITY BUYS NOTHING.
///
/// Measured 2026-09-23 on the 17:03 train gate's postgres log (gate-run
/// 928665ee): three checkpoints in the whole gate, 36.3s (35.8s of it
/// fsync, 4,279 files) and 200.4s (199.5s fsync, 60,796 files), and a
/// third that never finished before the pod shut down. Every test
/// database is a WAL_LOG copy of the template — hundreds of relation
/// files each — so a checkpoint under fsync=on has to flush them all,
/// and that slowness is what let blocked DROP DATABASE backends pile up
/// to max_connections in two red train gates (backlog afaa90a3).
///
/// PGDATA is an emptyDir: the data cannot outlive the Job, so there is
/// no crash it could be recovered from.
///
/// THE DEV POD'S SIDECAR IS THE SAME DATABASE, AND NOW PINNED WITH THEM
/// (backlog c71cf9d0, 2026-09-24). It was left out on 2026-09-23 because
/// changing it rolls the operator's session; then concurrent builders
/// were measured failing boss-jobs `*_pg` binaries with `too many
/// clients` / `pool timed out`, and the cause was the same checkpoint.
/// Measured that day on the pod's harness Postgres (max_connections
/// 100, fsync on): ONE builder running boss-jobs' 45 `*_pg` binaries in
/// sequence took the server to 54 clients, 50 of them `DROP DATABASE`
/// backends waiting on `CheckpointStart`/`CheckpointDone` behind two
/// checkpoints that spent 93 s and 69 s in `DataFileSync` (the oldest
/// drop waited 2m19s). Five builders held the server AT 100, 98 of them
/// drops, and 124 of 225 binary runs failed. Test threads and pool size
/// were not it: the three binaries the packet names peaked at 15-16
/// clients each, alone. Its PGDATA is an emptyDir too ("until the pod
/// restarts"), so there is nothing a synced file could be recovered for.
#[test]
fn a_test_database_skips_durability_it_cannot_use() {
    for rel in [SHARED, DEV] {
        let text = read(rel);
        let (_, pg) = pod_containers(&text)
            .into_iter()
            .find(|(n, _)| n == "postgres")
            .unwrap_or_else(|| panic!("{rel} has no container named postgres"));
        for setting in [
            "fsync=off",
            "synchronous_commit=off",
            "full_page_writes=off",
        ] {
            assert!(
                pg.contains(&format!("\"-c\", \"{setting}\"")),
                "{rel}: the postgres sidecar must run with `-c {setting}`. Its data lives in \
                 an emptyDir and dies with the pod, and with fsync on a single checkpoint \
                 spent 199.5s flushing 60,796 test-database files in the 2026-09-23 train gate, \
                 while DROP DATABASE backends queued behind it until max_connections refused \
                 every other client"
            );
        }
    }
}
