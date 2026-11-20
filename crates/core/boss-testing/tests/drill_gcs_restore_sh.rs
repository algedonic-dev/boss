//! THE RESTORE DRILL BEFORE THE POSTGRES MOVE (backlog 156eb6e5; design
//! 3c475a19 car 4's risk section: "A backup nobody has restored is a
//! belief"). The ops verbs `drill-gcs-restore` and `read-restore-drill`
//! restore the newest offsite dump into a scratch database on the data
//! node's postgres disk and compare it with the live system of record,
//! read-only.
//!
//! TWO VERBS, ONE SCRIPT (backlog a1cbd16c). The first real run,
//! ops-request 24fd49e7 (2026-11-14 20:25-21:11Z), waited 47 minutes on
//! the Job inside the forge's single ops-runner lane, failed on the Job's
//! DeadlineExceeded at 2700 s, and then removed everything — the Job
//! controller had already deleted the active pod, so the record could
//! not say how far the 7.5 GB dump's restore got. Now `drill-gcs-restore`
//! creates the Job and RETURNS, and `read-restore-drill` reads it: while
//! it runs, progress and nothing removed (exit 75); once it has finished,
//! the whole logs, the placement and the verdict, and only then the
//! removal. The pod ends ITSELF at an inner deadline below the Job's
//! backstop, so a slow restore leaves a Failed pod with its log.
//!
//! Three pieces, each held here without a cluster or a database:
//!   * the declared shape — both verb files, the Job (pinned by
//!     boss.dev/purpose=database, an ephemeral claim of the drill's class,
//!     labelled, an inner deadline below its backstop), and the class
//!     (one replica, diskSelector postgres);
//!   * infra/forge/drill-gcs-restore.sh against a stub kubectl door: what
//!     the start creates and that it returns leaving the Job standing;
//!     that the read removes nothing while the drill runs, prints the
//!     logs BEFORE it removes a finished one, removes it on success AND on
//!     every failure, and says so when the pod is already gone; that a
//!     bound refuses before any create; that it never reads a Secret's
//!     data; and the verdict (dump <= live on each count, the dump's
//!     newest audit id present live, the claim on the postgres disk);
//!   * infra/forge/restore-drill-pod.sh against stub psql/initdb/pg_ctl:
//!     every live session read-only twice, the live URL's password never
//!     printed, the restore fed the decompressed dump, progress printed,
//!     and the inner deadline ending the pod with a line that says so.
use boss_testing::{repo_root, scratch_dir, write_exec, write_file};
use std::path::Path;
use std::process::Command;

const NAME: &str = "boss-restore-drill";

fn read(rel: &str) -> String {
    std::fs::read_to_string(repo_root().join(rel)).unwrap_or_else(|e| panic!("read {rel}: {e}"))
}

fn verb_file(name: &str) -> serde_json::Value {
    serde_json::from_str(&read(&format!("infra/ops/verbs/{name}.json"))).unwrap()
}

/// The Job's backstop (`activeDeadlineSeconds`) and the restore
/// container's inner deadline (`DRILL_RESTORE_DEADLINE_S`), as the
/// manifest declares them.
fn deadlines() -> (u64, u64) {
    let job = read("infra/forge/restore-drill-job.yaml");
    let backstop = job
        .lines()
        .find_map(|l| l.strip_prefix("  activeDeadlineSeconds: "))
        .and_then(|v| v.trim().parse().ok())
        .expect("the Job declares one activeDeadlineSeconds");
    let inner = job
        .lines()
        .find_map(|l| {
            l.trim()
                .strip_prefix("- {name: DRILL_RESTORE_DEADLINE_S, value: \"")
        })
        .and_then(|v| v.strip_suffix("\"}"))
        .and_then(|v| v.parse().ok())
        .expect("the restore container declares DRILL_RESTORE_DEADLINE_S");
    (backstop, inner)
}

#[test]
fn the_verbs_take_no_word_run_on_the_forge_and_need_no_passkey() {
    for (name, argv) in [
        (
            "drill-gcs-restore",
            serde_json::json!(["infra/forge/drill-gcs-restore.sh"]),
        ),
        (
            "read-restore-drill",
            serde_json::json!(["infra/forge/drill-gcs-restore.sh", "--read"]),
        ),
    ] {
        let verb = verb_file(name);
        assert_eq!(verb["hosts"], serde_json::json!(["forge"]), "{name}");
        assert_eq!(verb["params"], serde_json::json!([]), "{name}");
        assert_eq!(verb["argv"], argv, "{name}");
        let about = verb["about"].as_str().unwrap();
        assert!(
            about.starts_with("MUTATING"),
            "{name}: it creates or removes the drill's own objects"
        );
        for phrase in [
            "READ-ONLY to the system of record",
            "NO PASSKEY",
            "preconditions.uid",
            "24fd49e7",
        ] {
            assert!(
                about.contains(phrase),
                "{name}: the about must say {phrase:?}"
            );
        }
        // registry-probe's and prove-gate-volume's class: no approval, no plan.
        assert!(verb.get("requires_approval").is_none(), "{name}");
        assert!(verb.get("plan_verb").is_none(), "{name}");
    }
    // The start holds the forge's one lane for its bounds and its two
    // creates, never for the restore: inside the queue's 5-minute bound.
    let start = verb_file("drill-gcs-restore")["timeout"].as_u64().unwrap();
    assert!(
        start <= 300,
        "drill-gcs-restore's timeout {start} must stay inside the forge queue's 5-minute bound — it returns, it does not wait"
    );
    let script = read("infra/forge/drill-gcs-restore.sh");
    assert!(
        !script.contains("WAIT_S"),
        "the start verb must not wait out the Job's deadline"
    );
    // The read holds it for reads and at most one removal read back.
    let read_t = verb_file("read-restore-drill")["timeout"].as_u64().unwrap();
    assert!(
        (300 + 120..=900).contains(&read_t),
        "read-restore-drill's timeout {read_t} must cover one read-back removal (300 s) and its reads, and no wait"
    );
    // The pod ends itself first, so the controller never deletes it: the
    // backstop sits above the inner deadline by more than the fetch leg
    // took on 24fd49e7 (under 18 minutes).
    let (backstop, inner) = deadlines();
    assert!(
        inner >= 4 * 3600,
        "the inner deadline {inner}s must be generous: the only measurement is >2700 s unfinished (24fd49e7)"
    );
    assert!(
        backstop >= inner + 1800,
        "the backstop {backstop}s must sit at least 30 minutes above the inner deadline {inner}s"
    );
}

#[test]
fn the_job_is_pinned_to_the_data_node_on_the_postgres_disk_and_labelled() {
    let job = read("infra/forge/restore-drill-job.yaml");
    for must in [
        "  name: boss-restore-drill\n  namespace: boss\n",
        "      nodeSelector:\n        boss.dev/purpose: database\n",
        "                storageClassName: longhorn-restore-drill\n",
        "              metadata:\n                labels: {app: boss-restore-drill}\n",
        "    app: boss-restore-drill\n",
        // One attempt: a retry would be a second pod, and the first one's
        // log is the record.
        "  backoffLimit: 0\n",
        // A finished drill waits a day for its read, not an hour: the read
        // is a second request now, and the TTL deletes the pod's log.
        "  ttlSecondsAfterFinished: 86400\n",
        "          secret: {secretName: boss-gcs-offsite, defaultMode: 0400}\n",
        "              valueFrom: {secretKeyRef: {name: boss-secrets, key: database-url}}\n",
        "          image: $BOSS_FORGE_REGISTRY_HOST/david/postgres:16-alpine\n",
        "          configMap: {name: boss-restore-drill, defaultMode: 0444}\n",
        "      automountServiceAccountToken: false\n",
    ] {
        assert!(
            job.contains(must),
            "restore-drill-job.yaml must carry {must:?}"
        );
    }
    // The live database's claim, Pod and Service account are never named.
    for never in ["pgdata", "claimName", "hostPath", "serviceAccountName"] {
        assert!(
            !job.contains(never),
            "the drill Job must not mount {never:?}"
        );
    }
    // The label is the one the estate registry gives the data node.
    assert!(
        read("infra/estate/estate.toml").contains("boss.dev/purpose=database"),
        "the estate registry names the data node's purpose label"
    );
    // The postgres major version is the system of record's.
    assert!(read("infra/cluster/manifests/boss.yaml").contains("/david/postgres:16\n"));

    let class = read("infra/cluster/manifests/longhorn-restore-drill.yaml");
    for must in [
        "  name: longhorn-restore-drill\n",
        "provisioner: driver.longhorn.io\n",
        "reclaimPolicy: Delete\n",
        "volumeBindingMode: WaitForFirstConsumer\n",
        "  numberOfReplicas: \"1\"\n",
        "  diskSelector: \"postgres\"\n",
    ] {
        assert!(
            class.contains(must),
            "the drill's class must carry {must:?}"
        );
    }
}

/// The stub door: every `sudo` call is logged; `docker run … kubectl
/// --kubeconfig=/kc <args>` is answered from a little state directory.
const SUDO: &str = r#"#!/bin/bash
set -u
printf '%s\n' "$*" >> "$STUB_LOG"
args="$*"
case "$args" in
  *'docker image inspect '*|*'docker container inspect '*|*'docker create '*|*'docker rm '*) exit 0 ;;
esac
k="${args#*kubectl --kubeconfig=/kc }"
[ "$k" != "$args" ] || { echo "unexpected sudo: $args" >&2; exit 2; }
S="$STUB_STATE"
mode="${STUB_MODE:-ok}"
live() { [ -f "$S/job" ] && [ ! -f "$S/deleted" ]; }
case "$k" in
  'get storageclass longhorn-restore-drill --ignore-not-found -o json '*)
    [ "$mode" = no-class ] && exit 0
    echo '{"kind":"StorageClass","provisioner":"driver.longhorn.io","parameters":{"numberOfReplicas":"1","diskSelector":"postgres"}}' ;;
  'get nodes -l boss.dev/purpose=database -o json '*)
    if [ "$mode" = tainted ]; then
      echo '{"items":[{"metadata":{"name":"w-3"},"spec":{"taints":[{"key":"boss.dev/data","value":"postgres","effect":"NoSchedule"}]}}]}'
    else
      echo '{"items":[{"metadata":{"name":"w-3"},"spec":{}}]}'
    fi ;;
  'get nodes.longhorn.io w-3 -n longhorn-system -o json '*)
    tags='["durable","postgres"]'; [ "$mode" = untagged ] && tags='[]'
    printf '{"spec":{"disks":{"u-postgres":{"path":"/var/mnt/postgres","tags":%s,"allowScheduling":true}}}}\n' "$tags" ;;
  'get secret boss-gcs-offsite -n boss -o jsonpath={.metadata.name} '*) printf 'boss-gcs-offsite' ;;
  'get secret boss-secrets -n boss -o jsonpath={.metadata.name} '*) printf 'boss-secrets' ;;
  'get job boss-restore-drill -n boss --ignore-not-found -o json '*)
    if live; then
      st='"startTime":"2026-11-14T16:00:00Z","completionTime":"2026-11-14T17:30:00Z"'
      cond='[{"type":"Complete","status":"True"}]'
      case "$mode" in
        job-failed)
          st='"startTime":"2026-11-14T16:00:00Z"'
          cond='[{"type":"Failed","status":"True","reason":"DeadlineExceeded","message":"Job was active longer than specified deadline","lastTransitionTime":"2026-11-14T21:00:00Z"}]' ;;
        inner-deadline)
          st='"startTime":"2026-11-14T16:00:00Z"'
          cond='[{"type":"Failed","status":"True","reason":"BackoffLimitExceeded","message":"Job has reached the specified backoff limit","lastTransitionTime":"2026-11-14T20:05:00Z"}]' ;;
        running|image-stuck)
          st='"startTime":"2026-11-14T16:00:00Z"'; cond='[]' ;;
      esac
      printf '{"metadata":{"name":"boss-restore-drill","uid":"job-uid-1","creationTimestamp":"2026-11-14T16:00:00Z","labels":{"app":"boss-restore-drill"}},"status":{"conditions":%s,%s}}\n' "$cond" "$st"
    fi ;;
  'get pvc -n boss -l app=boss-restore-drill -o json '*)
    if live; then echo '{"items":[{"metadata":{"name":"boss-restore-drill-abcde-scratch"}}]}'; else echo '{"items":[]}'; fi ;;
  'get configmap boss-restore-drill -n boss --ignore-not-found -o json '*)
    if live && [ -f "$S/cm" ]; then echo '{"metadata":{"name":"boss-restore-drill"}}'; fi ;;
  'create -f - -n boss -o json '*)
    body="$(cat)"
    case "$body" in
      *'"kind": "ConfigMap"'*)
        [ "$mode" = cm-refused ] && { echo 'configmaps is forbidden' >&2; exit 1; }
        printf '%s' "$body" > "$S/cm.json"; touch "$S/cm"; echo '{"metadata":{"uid":"cm-uid-1"}}' ;;
      *'kind: Job'*)
        printf '%s' "$body" > "$S/job.yaml"; touch "$S/job"; echo '{"metadata":{"uid":"job-uid-1","creationTimestamp":"2026-11-14T16:00:00Z"}}' ;;
      *) echo 'unexpected create body' >&2; exit 2 ;;
    esac ;;
  'get pods -n boss -l batch.kubernetes.io/controller-uid=job-uid-1 -o json '*)
    node=w-3; [ "$mode" = wrong-node ] && node=w-1
    init='{"name":"fetch","state":{"terminated":{"exitCode":0}}}'
    main='{"name":"restore","state":{"terminated":{"exitCode":0}}}'
    case "$mode" in
      # DeadlineExceeded: the Job controller deletes the pod it was running.
      job-failed) echo '{"items":[]}'; exit 0 ;;
      inner-deadline) main='{"name":"restore","state":{"terminated":{"exitCode":1}}}' ;;
      running) main='{"name":"restore","state":{"running":{"startedAt":"2026-11-14T16:05:00Z"}}}' ;;
      image-stuck)
        init='{"name":"fetch","state":{"waiting":{"reason":"ErrImagePull"}}}'
        main='{"name":"restore","state":{"waiting":{"reason":"PodInitializing"}}}' ;;
    esac
    printf '{"items":[{"metadata":{"name":"boss-restore-drill-abcde","creationTimestamp":"2026-11-14T16:00:00Z"},"spec":{"nodeName":"%s"},"status":{"initContainerStatuses":[%s],"containerStatuses":[%s]}}]}\n' "$node" "$init" "$main" ;;
  'logs boss-restore-drill-abcde -n boss -c fetch '*)
    [ "$mode" = image-stuck ] && { echo 'container "fetch" is waiting to start: ErrImagePull' >&2; exit 1; }
    echo 'checkpoint: {"stage":"pinned-download"}'
    printf 'receipt: {"result":"ok","stage":"verified","claim":"remote-archive-integrity","object":"boss-20261114-091000.sql.gz","generation":"44","sha256":"%s","object_bytes":10,"object_created_at":"2026-11-14T09:12:00Z","saved":true}\n' "$(printf 'a%.0s' $(seq 1 64))" ;;
  'logs boss-restore-drill-abcde -n boss -c restore '*)
    # One transient failure of the apiserver's log transport, then the
    # same log as a finished success (Codex review 32f56fc7, P1).
    if [ "$mode" = log-flaky ] && [ ! -f "$S/log-failed-once" ]; then
      touch "$S/log-failed-once"
      echo 'Error from server: temporary apiserver log transport failure' >&2; exit 1
    fi
    case "$mode" in
      image-stuck) echo 'container "restore" is waiting to start: PodInitializing' >&2; exit 1 ;;
      running)
        echo 'restore-drill: scratch server 16.4 started on a unix socket only, database drill created'
        echo 'restore-drill: progress: 1080s of 14400s, database drill holds 2147483648 bytes; running: COPY public.audit_log'
        exit 0 ;;
      inner-deadline)
        echo 'restore-drill: progress: 14340s of 14400s, database drill holds 51539607552 bytes; running: CREATE INDEX steps_job_idx'
        echo 'restore-drill: FAILED — the restore exceeded its inner deadline after 14400s (DRILL_RESTORE_DEADLINE_S=14400); the last progress line above says how far it got'
        exit 0 ;;
    esac
    djobs=900; [ "$mode" = dump-ahead ] && djobs=1200
    has=1; [ "$mode" = id-missing ] && has=0
    daudit=5000; [ "$mode" = empty-dump ] && daudit=0
    echo 'restore-drill: restored dump.sql.gz into database drill in 4800s, 32000000000 bytes'
    printf 'restore-drill: counts {"scratch_server_version":"16.4","live_server_version":"16.4","restore_seconds":4800,"restored_bytes":32000000000,"live_roles_created":2,"dump":{"audit_log":%s,"jobs":%s,"steps":4000,"max_audit_id":5100},"live":{"audit_log":5600,"jobs":1000,"steps":4400,"max_audit_id":5700},"live_rows_at_dump_max_audit_id":%s}\n' "$daudit" "$djobs" "$has" ;;
  'get pvc boss-restore-drill-abcde-scratch -n boss --ignore-not-found -o json '*)
    echo '{"spec":{"volumeName":"pvc-0001"}}' ;;
  'get replicas.longhorn.io -n longhorn-system -l longhornvolume=pvc-0001 -o json '*)
    path=/var/mnt/postgres; [ "$mode" = wrong-disk ] && path=/var/lib/longhorn
    printf '{"items":[{"spec":{"volumeName":"pvc-0001","nodeID":"w-3","diskPath":"%s/"}}]}\n' "$path" ;;
  'delete --raw /apis/batch/v1/namespaces/boss/jobs/boss-restore-drill -f - '*)
    cat > "$S/delete.json"; touch "$S/deleted"; echo '{"kind":"Status"}' ;;
  *) echo "unexpected kubectl: $k" >&2; exit 2 ;;
esac
"#;

struct Run {
    code: i32,
    out: String,
    calls: String,
    state: std::path::PathBuf,
}

/// One run of the script against the stub door, in the mode's own state
/// directory — `--read` when `read`, the start otherwise. A start and the
/// read after it share the state, as the cluster would.
fn drill(mode: &str, dir: &Path, read: bool) -> Run {
    let bin = dir.join("bin");
    let state = dir.join(format!(
        "state-{}",
        if mode.is_empty() { "ok" } else { mode }
    ));
    std::fs::create_dir_all(&bin).unwrap();
    std::fs::create_dir_all(&state).unwrap();
    write_exec(&bin.join("sudo"), SUDO);
    let log = state.join("argv");
    write_file(&log, "");
    let mut cmd = Command::new("bash");
    cmd.arg(repo_root().join("infra/forge/drill-gcs-restore.sh"));
    if read {
        cmd.arg("--read");
    }
    let output = cmd
        .env(
            "PATH",
            format!("{}:{}", bin.display(), std::env::var("PATH").unwrap()),
        )
        .env("BOSS_SOR_ENV", dir.join("absent-sor.env"))
        .env("BOSS_FORGE_REGISTRY_HOST", "forge.test:3000")
        .env("DRILL_POLL_S", "0")
        .env("STUB_LOG", &log)
        .env("STUB_STATE", &state)
        .env("STUB_MODE", mode)
        .output()
        .unwrap();
    Run {
        code: output.status.code().unwrap_or(-1),
        out: format!(
            "{}{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        ),
        calls: std::fs::read_to_string(&log).unwrap(),
        state,
    }
}

/// The start, which must succeed and leave the Job standing, then the read.
fn start_then_read(mode: &str, dir: &Path) -> Run {
    let start = drill(mode, dir, false);
    assert_eq!(start.code, 0, "{mode}: the start: {}", start.out);
    assert!(
        !start.state.join("delete.json").exists(),
        "{mode}: the start removed the Job it started:\n{}",
        start.out
    );
    drill(mode, dir, true)
}

/// A verb's own effect pattern, judged by the engine the runner uses.
fn effect_matches(verb: &str, line: &str) -> Option<bool> {
    let verb = verb_file(verb);
    let re = verb["effect"].as_str().unwrap();
    let out = Command::new("jq")
        .args([
            "-n",
            "--arg",
            "re",
            re,
            "--arg",
            "l",
            line,
            "$l | test($re)",
        ])
        .output()
        .ok()?;
    Some(String::from_utf8_lossy(&out.stdout).trim() == "true")
}

fn never_touches_what_it_did_not_make(run: &Run, mode: &str) {
    for line in run.calls.lines().filter(|l| l.contains("kubectl")) {
        let k = line
            .split_once("kubectl --kubeconfig=/kc ")
            .map(|x| x.1)
            .unwrap_or(line);
        assert!(!k.starts_with("exec"), "{mode}: the drill never execs: {k}");
        assert!(
            !k.starts_with("get secret") || k.contains("jsonpath={.metadata.name}"),
            "{mode}: a Secret is read by name only, never its data: {k}"
        );
        if k.starts_with("delete") {
            assert!(
                k.starts_with(&format!(
                    "delete --raw /apis/batch/v1/namespaces/boss/jobs/{NAME} "
                )),
                "{mode}: the only delete is this verb's own Job: {k}"
            );
        }
        for verb in [
            "patch", "apply", "replace", "edit", "scale", "label", "annotate",
        ] {
            assert!(!k.starts_with(verb), "{mode}: no {verb}: {k}");
        }
    }
}

fn removed_its_job(run: &Run, mode: &str) {
    let del: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(run.state.join("delete.json"))
            .unwrap_or_else(|_| panic!("{mode}: the Job was not deleted:\n{}", run.out)),
    )
    .unwrap();
    assert_eq!(del["preconditions"]["uid"], "job-uid-1", "{mode}");
    assert_eq!(del["propagationPolicy"], "Foreground", "{mode}");
    assert!(
        run.out.contains("removed Job boss/boss-restore-drill (uid job-uid-1) — the Job, its scratch claim and its ConfigMap read back gone"),
        "{mode}: the removal is read back:\n{}",
        run.out
    );
}

fn removed_nothing(run: &Run, mode: &str) {
    assert!(
        !run.state.join("delete.json").exists(),
        "{mode}: nothing may be removed:\n{}",
        run.out
    );
    assert!(
        !run.calls.contains("delete "),
        "{mode}: no delete was sent:\n{}",
        run.calls
    );
}

#[test]
fn the_start_creates_the_drill_and_returns_without_waiting_on_it() {
    let dir = scratch_dir("drill-gcs-restore-start");
    let run = drill("", &dir, false);
    assert_eq!(run.code, 0, "{}", run.out);
    never_touches_what_it_did_not_make(&run, "start");
    // It returns with the Job standing: no pod read, no log, no removal.
    removed_nothing(&run, "start");
    for never in ["get pods ", "logs ", "replicas.longhorn.io"] {
        assert!(
            !run.calls.contains(never),
            "the start must not wait on the drill ({never:?} was called):\n{}",
            run.calls
        );
    }

    let job = std::fs::read_to_string(run.state.join("job.yaml")).unwrap();
    assert!(job.contains("image: forge.test:3000/david/postgres:16-alpine"));
    assert!(!job.contains("$BOSS_FORGE_REGISTRY_HOST"));
    assert!(job.contains("boss.dev/purpose: database"));
    let cm: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(run.state.join("cm.json")).unwrap()).unwrap();
    assert_eq!(cm["metadata"]["labels"]["app"], NAME);
    assert_eq!(cm["metadata"]["ownerReferences"][0]["uid"], "job-uid-1");
    assert_eq!(cm["metadata"]["ownerReferences"][0]["kind"], "Job");
    assert_eq!(
        cm["data"]["gcs-readback.py"].as_str().unwrap(),
        read("infra/forge/gcs-readback.py")
    );
    assert_eq!(
        cm["data"]["restore-drill.sh"].as_str().unwrap(),
        read("infra/forge/restore-drill-pod.sh")
    );
    // The Job exists before its ConfigMap, so the ConfigMap can be owned.
    let job_at = run.calls.find("create -f - -n boss").unwrap();
    assert!(run.calls[job_at + 1..].contains("create -f - -n boss"));

    // The answer names the uid, when it was made, when it must end, and
    // the exact verb that reads it.
    let (backstop, inner) = deadlines();
    let started = run
        .out
        .lines()
        .find(|l| l.starts_with("drill-gcs-restore: STARTED "))
        .unwrap_or_else(|| panic!("the started line:\n{}", run.out));
    for must in [
        "(uid job-uid-1)".to_string(),
        "at 2026-11-14T16:00:00Z".to_string(),
        format!("inner deadline of {inner}s"),
        format!("backstop deadline of {backstop}s"),
        "boss ops forge read-restore-drill --wait".to_string(),
    ] {
        assert!(started.contains(&must), "{must:?} missing from: {started}");
    }
    if let Some(m) = effect_matches("drill-gcs-restore", started) {
        assert!(
            m,
            "the start's effect pattern must match its line: {started}"
        );
    }
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn a_finished_drill_is_read_whole_then_removed() {
    let dir = scratch_dir("drill-gcs-restore-ok");
    let run = start_then_read("", &dir);
    assert_eq!(run.code, 0, "{}", run.out);
    never_touches_what_it_did_not_make(&run, "ok");
    removed_its_job(&run, "ok");
    assert!(
        !run.calls.contains("create -f -"),
        "the read creates nothing:\n{}",
        run.calls
    );

    let receipt: serde_json::Value = serde_json::from_str(
        run.out
            .lines()
            .find_map(|l| l.strip_prefix("receipt: "))
            .expect("one receipt line"),
    )
    .unwrap();
    assert_eq!(receipt["result"], "ok");
    assert_eq!(receipt["dump"]["object"], "boss-20261114-091000.sql.gz");
    assert_eq!(receipt["dump"]["stamp"], "20261114-091000");
    assert_eq!(receipt["dump"]["sha256"].as_str().unwrap().len(), 64);
    assert_eq!(receipt["counts"]["dump"]["audit_log"], 5000);
    assert_eq!(receipt["counts"]["live"]["audit_log"], 5600);
    assert_eq!(receipt["counts"]["dump"]["max_audit_id"], 5100);
    assert_eq!(receipt["live_rows_at_dump_max_audit_id"], 1);
    assert_eq!(receipt["scratch"]["node"], "w-3");
    assert_eq!(receipt["scratch"]["disk"], "u-postgres");
    // The numbers the next sizing reads (24fd49e7 left none).
    assert_eq!(receipt["scratch"]["restore_seconds"], 4800);
    assert_eq!(receipt["scratch"]["restored_bytes"], 32000000000_u64);
    assert_eq!(receipt["job_seconds"], 5400);
    assert_eq!(receipt["live"]["read_only"], true);
    assert_eq!(receipt["removed"], true);

    let effect = run
        .out
        .lines()
        .find(|l| l.starts_with("read-restore-drill: RESTORED "))
        .expect("the effect line");
    assert!(effect.contains("audit_log 5000/5600, jobs 900/1000, steps 4000/4400"));
    assert!(effect.contains("newest audit id 5100 present"));
    assert!(
        effect.contains("in 4800s (32000000000 bytes restored)"),
        "{effect}"
    );
    if let Some(m) = effect_matches("read-restore-drill", effect) {
        assert!(
            m,
            "the read's effect pattern must match its own line: {effect}"
        );
    }
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn a_log_that_cannot_be_read_keeps_the_finished_drill_for_the_next_read() {
    // A finished, successful drill whose pod still stands, and one
    // transient failure reading its restore log: that log is the drill's
    // only record, so the read removes NOTHING and says to read again
    // (exit 75, retryable). The next read fetches the whole log and only
    // then removes the drill (Codex review 32f56fc7, P1).
    let dir = scratch_dir("drill-gcs-restore-log-flaky");
    let first = start_then_read("log-flaky", &dir);
    assert_eq!(
        first.code, 75,
        "a log read that failed is retryable:\n{}",
        first.out
    );
    removed_nothing(&first, "log-flaky");
    assert!(
        first.out.contains("NOT REMOVED")
            && first
                .out
                .contains("temporary apiserver log transport failure"),
        "the refusal names what failed and that nothing was removed:\n{}",
        first.out
    );

    let second = drill("log-flaky", &dir, true);
    assert_eq!(
        second.code, 0,
        "the retry reads the drill whole:\n{}",
        second.out
    );
    removed_its_job(&second, "log-flaky");
    let counts = second
        .out
        .find("restore-drill: counts ")
        .expect("the restore log is on the record");
    let removal = second
        .out
        .find("removed Job boss/boss-restore-drill")
        .expect("the removal is read back");
    assert!(
        counts < removal,
        "the restore log is printed before the drill is removed:\n{}",
        second.out
    );
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn a_running_drill_is_read_for_progress_and_nothing_is_removed() {
    let dir = scratch_dir("drill-gcs-restore-running");
    let run = start_then_read("running", &dir);
    assert_eq!(run.code, 75, "still running is a not-yet: {}", run.out);
    never_touches_what_it_did_not_make(&run, "running");
    removed_nothing(&run, "running");
    let still = run
        .out
        .lines()
        .find(|l| l.starts_with("read-restore-drill: STILL RUNNING "))
        .unwrap_or_else(|| panic!("the still-running line:\n{}", run.out));
    for must in [
        "(uid job-uid-1)",
        "started 2026-11-14T16:00:00Z",
        "s ago",
        "on w-3",
        "Nothing was removed",
    ] {
        assert!(still.contains(must), "{must:?} missing from: {still}");
    }
    assert!(
        run.out.contains(
            "progress: 1080s of 14400s, database drill holds 2147483648 bytes; running: COPY public.audit_log"
        ),
        "the restore container's progress is on the record:\n{}",
        run.out
    );
    if let Some(m) = effect_matches("read-restore-drill", still) {
        assert!(!m, "still running is not the read's effect: {still}");
    }
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn an_inner_deadline_failure_is_read_with_its_log_before_the_removal() {
    let dir = scratch_dir("drill-gcs-restore-inner");
    let run = start_then_read("inner-deadline", &dir);
    assert_eq!(run.code, 1, "{}", run.out);
    never_touches_what_it_did_not_make(&run, "inner-deadline");
    removed_its_job(&run, "inner-deadline");
    let said = "the restore exceeded its inner deadline after 14400s";
    let log_at = run
        .out
        .find(said)
        .unwrap_or_else(|| panic!("the pod's own last words are on the record:\n{}", run.out));
    let removed_at = run.out.find("removed Job boss/boss-restore-drill").unwrap();
    assert!(
        log_at < removed_at,
        "the log must be printed BEFORE the removal:\n{}",
        run.out
    );
    let logs_call = run.calls.find(" logs ").expect("the logs were read");
    let delete_call = run.calls.find(" delete ").expect("the Job was deleted");
    assert!(
        logs_call < delete_call,
        "logs read before the delete:\n{}",
        run.calls
    );
    // The verdict names how far it got: the last progress line.
    assert!(
        run.out.contains(
            "database drill holds 51539607552 bytes; running: CREATE INDEX steps_job_idx"
        ),
        "{}",
        run.out
    );
    assert!(
        run.out.contains("the drill did not complete"),
        "{}",
        run.out
    );
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn a_pod_the_controller_already_deleted_is_said_so() {
    let dir = scratch_dir("drill-gcs-restore-podless");
    let run = start_then_read("job-failed", &dir);
    assert_eq!(run.code, 1, "{}", run.out);
    removed_its_job(&run, "job-failed");
    assert!(
        run.out
            .contains("the Job's Failed condition says DeadlineExceeded"),
        "{}",
        run.out
    );
    assert!(
        run.out.contains(
            "no pod of Job boss/boss-restore-drill stands — on DeadlineExceeded the Job controller deletes the pod it was running"
        ),
        "a missing log is said out loud, not silently skipped:\n{}",
        run.out
    );
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn every_failure_after_the_create_still_removes_the_job() {
    let dir = scratch_dir("drill-gcs-restore-fail");
    for (mode, says) in [
        (
            "dump-ahead",
            "the dump is AHEAD of the live system of record on jobs",
        ),
        (
            "id-missing",
            "holds 0 row(s) at the dump's newest audit id 5100",
        ),
        ("empty-dump", "the dump restored no audit_log row"),
        ("wrong-disk", "not all on w-3 at /var/mnt/postgres"),
        ("wrong-node", "its pod ran on 'w-1', not the data node w-3"),
        // A pod that cannot start has no log to lose: the read ends it.
        ("image-stuck", "its pod has been waiting on ErrImagePull"),
    ] {
        let run = start_then_read(mode, &dir);
        assert_eq!(run.code, 1, "{mode}: {}", run.out);
        assert!(
            run.out.contains(says),
            "{mode}: expected {says:?} in\n{}",
            run.out
        );
        assert!(
            !run.out.contains("read-restore-drill: RESTORED "),
            "{mode}: no effect line on a failure"
        );
        never_touches_what_it_did_not_make(&run, mode);
        removed_its_job(&run, mode);
    }
    // The start's own failure after its create removes what it made.
    let run = drill("cm-refused", &dir, false);
    assert_eq!(run.code, 1, "cm-refused: {}", run.out);
    assert!(
        run.out
            .contains("did not create ConfigMap boss/boss-restore-drill"),
        "{}",
        run.out
    );
    assert!(!run.out.contains("drill-gcs-restore: STARTED "));
    never_touches_what_it_did_not_make(&run, "cm-refused");
    removed_its_job(&run, "cm-refused");
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn a_bound_that_does_not_hold_creates_nothing() {
    let dir = scratch_dir("drill-gcs-restore-refuse");
    for (mode, says) in [
        (
            "no-class",
            "StorageClass longhorn-restore-drill does not exist",
        ),
        (
            "untagged",
            "no schedulable disk at /var/mnt/postgres tagged postgres",
        ),
        (
            "tainted",
            "carries the taint(s) boss.dev/data=postgres:NoSchedule",
        ),
    ] {
        let run = drill(mode, &dir, false);
        assert_eq!(run.code, 78, "{mode}: {}", run.out);
        assert!(
            run.out.contains(says),
            "{mode}: expected {says:?} in\n{}",
            run.out
        );
        assert!(
            !run.calls.contains("create -f -"),
            "{mode}: nothing is created"
        );
        removed_nothing(&run, mode);
    }
    // A drill still running is never touched.
    let running = dir.join("state-running");
    std::fs::create_dir_all(&running).unwrap();
    write_file(&running.join("job"), "");
    let run = drill("running", &dir, false);
    assert_eq!(run.code, 78, "{}", run.out);
    assert!(
        run.out
            .contains("is a drill still running — it is never touched")
    );
    assert!(!run.calls.contains("create -f -"));
    removed_nothing(&run, "running");
    // A FINISHED drill whose verdict nobody read is not removed by the
    // start: its log is the record, and only the read removes it.
    let finished = dir.join("state-ok");
    std::fs::create_dir_all(&finished).unwrap();
    write_file(&finished.join("job"), "");
    let run = drill("", &dir, false);
    assert_eq!(run.code, 78, "{}", run.out);
    assert!(
        run.out.contains(
            "its verdict has not been read — boss ops forge read-restore-drill --wait reads it and removes it"
        ),
        "{}",
        run.out
    );
    assert!(!run.calls.contains("create -f -"));
    removed_nothing(&run, "finished");
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn a_read_with_no_drill_standing_says_so_and_touches_nothing() {
    let dir = scratch_dir("drill-gcs-restore-none");
    let run = drill("", &dir, true);
    assert_eq!(run.code, 78, "{}", run.out);
    assert!(
        run.out
            .contains("no drill stands (no Job boss/boss-restore-drill)"),
        "{}",
        run.out
    );
    assert!(!run.calls.contains("create -f -"));
    removed_nothing(&run, "none");
    // And the script takes no other word.
    let bin = dir.join("bin");
    let out = Command::new("bash")
        .arg(repo_root().join("infra/forge/drill-gcs-restore.sh"))
        .arg("--force")
        .env(
            "PATH",
            format!("{}:{}", bin.display(), std::env::var("PATH").unwrap()),
        )
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(78));
    let _ = std::fs::remove_dir_all(dir);
}

/// The pod script's stubs: psql answers by side (a URL first is the live
/// system of record; `-h <socket>` is the scratch server).
const PSQL: &str = r#"#!/bin/bash
printf 'PGOPTIONS=%s ARGS=%s\n' "${PGOPTIONS:-}" "$*" >> "$STUB_LOG"
side=scratch
case "$1" in postgres://*) side=live ;; esac
cs=(); prev=""
for a in "$@"; do [ "$prev" = -c ] && cs+=("$a"); prev="$a"; done
if [ ${#cs[@]} -eq 0 ]; then
  case "$*" in
    *'-d drill'*)
      # A slow restore runs until the scratch server is stopped under it,
      # as psql would lose its connection.
      if [ -n "${STUB_SLOW_RESTORE:-}" ]; then
        for _ in $(seq 1 300); do [ -f "$STUB_STOPPED" ] && { echo 'server closed the connection' >&2; exit 2; }; sleep 0.1; done
      fi
      cat > "$STUB_RESTORED" ;;
    *) cat > /dev/null ;;
  esac
  exit 0
fi
last="${cs[${#cs[@]}-1]}"
if [ "$side" = live ]; then
  if [ "${STUB_LIVE_FAIL:-}" = 1 ]; then
    echo "psql: error: connection to server at postgres://boss:SECRETPW@postgres.boss.svc:5432/boss failed" >&2
    exit 2
  fi
  [ "${cs[0]}" = 'SET default_transaction_read_only = on' ] || { echo 'a live session that is not read-only first' >&2; exit 9; }
fi
case "$side:$last" in
  *:'SHOW server_version') echo '16.4' ;;
  live:*pg_roles*) printf 'boss\nboss_gateway_audit\n' ;;
  scratch:'CREATE DATABASE drill') ;;
  scratch:*pg_database_size*) echo 123456 ;;
  scratch:*pg_stat_activity*) echo 'COPY public.audit_log (id, at) FROM stdin' ;;
  scratch:'SELECT count(*) FROM audit_log') echo 100 ;;
  live:'SELECT count(*) FROM audit_log') echo 150 ;;
  scratch:'SELECT count(*) FROM jobs') echo 10 ;;
  live:'SELECT count(*) FROM jobs') echo 12 ;;
  scratch:'SELECT count(*) FROM steps') echo 40 ;;
  live:'SELECT count(*) FROM steps') echo 44 ;;
  scratch:'SELECT coalesce(max(id), 0) FROM audit_log') echo 100 ;;
  live:'SELECT coalesce(max(id), 0) FROM audit_log') echo 150 ;;
  live:'SELECT count(*) FROM audit_log WHERE id = 100') echo 1 ;;
  *) echo "unexpected sql on $side: $last" >&2; exit 3 ;;
esac
"#;

/// pg_ctl's stub: a `stop` leaves a mark the slow restore reads.
const PG_CTL: &str = r#"#!/bin/sh
echo pg_ctl "$@"
for a in "$@"; do last="$a"; done
[ "$last" = stop ] && touch "$STUB_STOPPED"
exit 0
"#;

struct Pod<'a> {
    live_fail: bool,
    slow: bool,
    inner_s: &'a str,
}

fn pod(dir: &Path, how: Pod) -> (i32, String, String) {
    let bin = dir.join("bin");
    let scratch = dir.join("claim");
    std::fs::create_dir_all(&bin).unwrap();
    std::fs::create_dir_all(&scratch).unwrap();
    write_exec(&bin.join("psql"), PSQL);
    write_exec(&bin.join("initdb"), "#!/bin/sh\necho initdb \"$@\"\n");
    write_exec(&bin.join("pg_ctl"), PG_CTL);
    let sql = scratch.join("dump.sql");
    write_file(
        &sql,
        "CREATE TABLE audit_log ();\n-- PostgreSQL database dump complete\n",
    );
    let status = Command::new("gzip").arg(&sql).status().unwrap();
    assert!(status.success());
    let log = dir.join("psql.log");
    write_file(&log, "");
    let output = Command::new("sh")
        .arg(repo_root().join("infra/forge/restore-drill-pod.sh"))
        .env(
            "PATH",
            format!("{}:{}", bin.display(), std::env::var("PATH").unwrap()),
        )
        .env("DRILL_SCRATCH", &scratch)
        .env("DRILL_AS", "")
        .env("DRILL_RESTORE_DEADLINE_S", how.inner_s)
        .env("DRILL_PROGRESS_S", "1")
        .env(
            "LIVE_DATABASE_URL",
            "postgres://boss:SECRETPW@postgres.boss.svc:5432/boss",
        )
        .env("STUB_LOG", &log)
        .env("STUB_RESTORED", dir.join("restored.sql"))
        .env("STUB_STOPPED", dir.join("stopped"))
        .env("STUB_LIVE_FAIL", if how.live_fail { "1" } else { "" })
        .env("STUB_SLOW_RESTORE", if how.slow { "1" } else { "" })
        .output()
        .unwrap();
    (
        output.status.code().unwrap_or(-1),
        format!(
            "{}{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        ),
        std::fs::read_to_string(&log).unwrap(),
    )
}

#[test]
fn the_pod_reads_the_live_side_read_only_and_never_prints_its_password() {
    let dir = scratch_dir("restore-drill-pod");
    let (code, out, calls) = pod(
        &dir,
        Pod {
            live_fail: false,
            slow: false,
            inner_s: "600",
        },
    );
    assert_eq!(code, 0, "{out}");
    let counts: serde_json::Value = serde_json::from_str(
        out.lines()
            .find_map(|l| l.strip_prefix("restore-drill: counts "))
            .expect("one counts line"),
    )
    .unwrap();
    assert_eq!(counts["dump"]["audit_log"], 100);
    assert_eq!(counts["live"]["audit_log"], 150);
    assert_eq!(counts["dump"]["jobs"], 10);
    assert_eq!(counts["live"]["steps"], 44);
    assert_eq!(counts["dump"]["max_audit_id"], 100);
    assert_eq!(counts["live_rows_at_dump_max_audit_id"], 1);
    assert_eq!(counts["live_roles_created"], 2);
    // The size the restore reached, for the next sizing of the deadline.
    assert_eq!(counts["restored_bytes"], 123456);
    assert!(counts["restore_seconds"].is_u64(), "{counts}");
    // Every live session: PGOPTIONS read-only AND the SET first.
    let live: Vec<&str> = calls
        .lines()
        .filter(|l| l.contains("ARGS=postgres://"))
        .collect();
    assert_eq!(live.len(), 7, "the live side is read seven times: {calls}");
    for l in &live {
        assert!(
            l.starts_with("PGOPTIONS=-c default_transaction_read_only=on ARGS="),
            "{l}"
        );
        assert!(
            l.contains("-c SET default_transaction_read_only = on -c SET statement_timeout"),
            "{l}"
        );
    }
    // The restore was fed the decompressed dump, strictly.
    assert_eq!(
        std::fs::read_to_string(dir.join("restored.sql")).unwrap(),
        "CREATE TABLE audit_log ();\n-- PostgreSQL database dump complete\n"
    );
    assert!(calls.contains("-d drill -X -q -v ON_ERROR_STOP=1"));
    assert!(
        !out.contains("SECRETPW"),
        "the live password was printed:\n{out}"
    );
    let _ = std::fs::remove_dir_all(&dir);

    let dir = scratch_dir("restore-drill-pod-dark");
    let (code, out, _) = pod(
        &dir,
        Pod {
            live_fail: true,
            slow: false,
            inner_s: "600",
        },
    );
    assert_eq!(code, 1, "{out}");
    assert!(
        out.contains("the live read-only query for server_version failed"),
        "{out}"
    );
    assert!(
        out.contains("postgres://<masked>@postgres.boss.svc"),
        "{out}"
    );
    assert!(
        !out.contains("SECRETPW"),
        "a failed live read printed the password:\n{out}"
    );
    assert!(!out.contains("restore-drill: counts"));
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn the_pod_ends_itself_at_its_inner_deadline_and_says_how_far_it_got() {
    let dir = scratch_dir("restore-drill-pod-deadline");
    let (code, out, _) = pod(
        &dir,
        Pod {
            live_fail: false,
            slow: true,
            inner_s: "3",
        },
    );
    assert_eq!(code, 1, "the pod ends Failed, with its log kept: {out}");
    assert!(
        out.contains("restore-drill: FAILED — the restore exceeded its inner deadline after "),
        "{out}"
    );
    assert!(
        out.contains("(DRILL_RESTORE_DEADLINE_S=3)"),
        "the line names the deadline it ran into: {out}"
    );
    // Progress every DRILL_PROGRESS_S: elapsed, bytes, the statement.
    assert!(
        out.contains("restore-drill: progress: ")
            && out.contains(
                "s of 3s, database drill holds 123456 bytes; running: COPY public.audit_log"
            ),
        "{out}"
    );
    assert!(!out.contains("restore-drill: counts"), "{out}");
    assert!(
        dir.join("stopped").exists(),
        "the scratch server is stopped under the restore"
    );
    // A missing deadline is a failure, not an unbounded restore.
    let dir2 = scratch_dir("restore-drill-pod-nodeadline");
    let (code, out, _) = pod(
        &dir2,
        Pod {
            live_fail: false,
            slow: false,
            inner_s: "",
        },
    );
    assert_eq!(code, 1, "{out}");
    assert!(out.contains("DRILL_RESTORE_DEADLINE_S"), "{out}");
    let _ = std::fs::remove_dir_all(dir);
    let _ = std::fs::remove_dir_all(dir2);
}
