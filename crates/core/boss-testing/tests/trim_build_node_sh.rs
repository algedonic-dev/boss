//! The ops verb `trim-build-node` (`infra/forge/trim-build-node.sh`,
//! backlog 30a6bf39): run the DECLARED trim of the build node's two
//! filesystems once, now, as a Job made from CronJob `boss-node-trim`.
//!
//! WHY. Twice in two days the gates slowed until w-1's filesystems were
//! trimmed by a pod David made by hand — `privileged: true`, the whole of
//! /var mounted, in kube-system. The scheduled trim landed on train #989;
//! David asked (2026-10-07) that the agent be able to run the same trim
//! at any other hour without him. The verb needs no passkey, so what it
//! can be made to do has to be nothing but that one trim, and what it
//! says has to be true:
//!
//! - no argument reaches kubectl from the caller, and no pod spec exists
//!   in the script — the pod is the CronJob's own template, which the
//!   admission policy bounds;
//! - it refuses, creating nothing, when a trim is already in flight, when
//!   the CronJob, the policy or its binding is absent or is not the bound
//!   the tree declares, when the CronJob is SUSPENDED (the in-cluster
//!   stop switch), and when the CronJob on the server differs from the
//!   checkout's on what the script compares — and the line it prints
//!   before the create names exactly what it compared, and what it did
//!   not (review 3e750cce, findings 2 and 3);
//! - it deletes the Job it made, by name and uid, and no other; a create
//!   that did not answer success is READ BACK before anything is said
//!   about whether a Job was made (finding 1);
//! - a Job that fails, never starts or outlives the wait is FAILED with
//!   the cluster's reason, never ok; a cluster that cannot be read is
//!   CANNOT ANSWER (exit 4), which is neither.
//!
//! HOW THIS IS MEASURED. The script runs for real, with `sudo` stubbed on
//! PATH as a small cluster behind ops_kubectl's `docker run … kubectl
//! --kubeconfig=/kc`: the binding, the CronJob (built here from the
//! manifest by a second, independent reading of its lines), the Jobs and
//! pods of the namespace, and one scenario for the Job the verb creates.
//! `sleep` is stubbed to return at once, so the script's wait runs its
//! whole count of polls in a moment. Every door call lands in `argv`, so
//! a refusal can be shown to have written nothing. Nothing here reaches a
//! cluster.

use boss_testing::{repo_root, scratch_dir, write_exec, write_file};
use serde_json::{Value, json};
use std::path::PathBuf;
use std::process::{Command, Output};

const SCRIPT: &str = "infra/forge/trim-build-node.sh";
const VERB: &str = "infra/ops/verbs/trim-build-node.json";
const MANIFEST: &str = "infra/cluster/manifests/boss-node-maintenance.yaml";
const NS: &str = "boss-node-maintenance";
const CRONJOB: &str = "boss-node-trim";

/// The stand-in behind the kubectl door. State under $STUB_STATE:
/// `binding.json`, `policy.json`, `cronjob.json` (absent answers nothing, as
/// `--ignore-not-found` does), `jobs/<name>.json` one per Job,
/// `pods.json` the namespace's other pods, `polls` how often the made
/// Job was read. STUB_SCENARIO is the life of the Job the verb makes:
/// ok | exit1 | exit3 (Complete over a pod that exited 3) | deadline | imagepull | admission | unsched | never |
/// norecord | nook (both mounts trimmed, exit 0, no closing line) | swapuid |
/// swaplate | vanish. STUB_CREATE: ok | exists | refused | nouid | lost (the
/// server makes the Job and the answer never arrives). STUB_DARK=all fails
/// every read; STUB_DARK=after fails every read once the Job is made;
/// STUB_DARK=readback fails every read once a create was SENT, whatever
/// it answered. STUB_DELETE=noop answers and removes nothing.
/// STUB_LOGS=fail refuses the log.
const STUB: &str = r#"#!/bin/sh
case "$*" in
'-n docker image inspect '*|'-n docker create '*|'-n docker rm '*) exit 0 ;;
'-n docker container inspect '*) exit 1 ;;
esac
S="$STUB_STATE"
while [ $# -gt 0 ]; do
    case "$1" in --kubeconfig=/kc) shift; break ;; esac
    shift
done
printf '%s\n' "$*" >> "$STUB_ARGV"
scenario="${STUB_SCENARIO:-ok}"
mine=""
[ -f "$S/mine" ] && mine=$(cat "$S/mine")
dark() { echo "Unable to connect to the server: dial tcp 10.20.0.31:6443: i/o timeout" >&2; exit 1; }
record_ok() {
    printf 'node-trim: before /sys/block/nvme0n1: discards=0 sectors=0\n'
    printf 'node-trim: before /sys/block/nvme1n1: discards=699 sectors=4096\n'
    printf 'node-trim: /trim/gate: 442701234176 bytes trimmed\n'
    printf 'node-trim: /trim/ephemeral: 5368709120 bytes trimmed\n'
    printf 'node-trim: after /sys/block/nvme0n1: discards=41 sectors=10485760\n'
    printf 'node-trim: after /sys/block/nvme1n1: discards=3120 sectors=864650848\n'
    printf 'node-trim: OK\n'
}
record_red() {
    printf 'node-trim: before /sys/block/nvme1n1: discards=699 sectors=4096\n'
    printf 'node-trim: RED fstrim /trim/gate failed: fstrim: /trim/gate: FITRIM: Operation not permitted\n'
    printf 'node-trim: /trim/ephemeral: 5368709120 bytes trimmed\n'
    printf 'node-trim: after /sys/block/nvme1n1: discards=699 sectors=4096\n'
    printf 'node-trim: RED\n'
}
record() {
    case "$scenario" in
    exit1) record_red ;;
    norecord) printf 'node-trim: before /sys/block/nvme1n1: discards=699 sectors=4096\n' ;;
    nook) record_ok | grep -v '^node-trim: OK$' ;;
    *) record_ok ;;
    esac
}
polls=0
[ -f "$S/polls" ] && polls=$(cat "$S/polls")
finished() { [ "$polls" -gt "${STUB_FINISH_AFTER:-2}" ]; }
pod_item() { # $1 = status JSON
    jq -n --arg n "$mine-x7k2p" --argjson st "$1" \
        '{metadata: {name: $n, creationTimestamp: "2026-10-08T01:00:01Z"}, spec: {nodeName: "w-1"}, status: $st}'
}
case "$1" in
get)
    [ "${STUB_DARK:-}" = all ] && dark
    [ "${STUB_DARK:-}" = after ] && [ -n "$mine" ] && dark
    [ "${STUB_DARK:-}" = readback ] && [ -f "$S/sent" ] && dark
    case "$2" in
    validatingadmissionpolicybinding)
        [ -f "$S/binding.json" ] && cat "$S/binding.json"; exit 0 ;;
    validatingadmissionpolicy)
        [ "$3" = node-maintenance-holds-one-workload ] || { echo "stub: get policy $3" >&2; exit 2; }
        [ -f "$S/policy.json" ] && cat "$S/policy.json"; exit 0 ;;
    cronjob)
        [ "$3" = boss-node-trim ] || { echo "stub: get cronjob $3" >&2; exit 2; }
        [ -f "$S/cronjob.json" ] && cat "$S/cronjob.json"; exit 0 ;;
    jobs)
        jq -s '{kind: "List", items: .}' "$S"/jobs/*.json 2>/dev/null || echo '{"kind":"List","items":[]}'
        exit 0 ;;
    job)
        f="$S/jobs/$3.json"
        [ -f "$f" ] || exit 0
        [ "$3" = "$mine" ] || { cat "$f"; exit 0; }
        polls=$((polls + 1)); echo "$polls" > "$S/polls"
        [ "$scenario" = vanish ] && [ "$polls" -gt 1 ] && exit 0
        uid=$(jq -r .metadata.uid "$f")
        [ "$scenario" = swapuid ] && [ "$polls" -gt 1 ] && uid=uid-of-another-job
        [ "$scenario" = swaplate ] && [ "$polls" -gt $((${STUB_FINISH_AFTER:-2} + 1)) ] && uid=uid-of-another-job
        cond='[]'
        if finished; then
            case "$scenario" in
            ok|norecord|nook|swaplate|exit3) cond='[{"type":"Complete","status":"True"}]' ;;
            exit1) cond='[{"type":"Failed","status":"True","reason":"BackoffLimitExceeded","message":"Job has reached the specified backoff limit"}]' ;;
            deadline) cond='[{"type":"Failed","status":"True","reason":"DeadlineExceeded","message":"Job was active longer than specified deadline"}]' ;;
            esac
        fi
        jq --arg u "$uid" --argjson c "$cond" '.metadata.uid = $u | .status.conditions = $c' "$f"
        exit 0 ;;
    pods)
        case "$*" in
        *"-l job-name=$mine "*)
            [ -n "$mine" ] || { echo "stub: pods of no job" >&2; exit 2; }
            case "$scenario" in
            admission) items='[]' ;;
            imagepull) items="[$(pod_item '{"phase":"Pending","containerStatuses":[{"name":"trim","state":{"waiting":{"reason":"ImagePullBackOff","message":"Back-off pulling image: manifest unknown"}}}]}')]" ;;
            unsched) items="[$(pod_item '{"phase":"Pending","conditions":[{"type":"PodScheduled","status":"False","reason":"Unschedulable","message":"0/4 nodes are available: 1 node(s) were unschedulable"}]}')]" ;;
            never) items="[$(pod_item '{"phase":"Running","containerStatuses":[{"name":"trim","state":{"running":{"startedAt":"2026-10-08T01:00:03Z"}}}]}')]" ;;
            deadline)
                if finished; then items='[]'
                else items="[$(pod_item '{"phase":"Running","containerStatuses":[{"name":"trim","state":{"running":{"startedAt":"2026-10-08T01:00:03Z"}}}]}')]"; fi ;;
            exit1|exit3)
                ec=1; [ "$scenario" = exit3 ] && ec=3
                if finished; then items="[$(pod_item "$(jq -n --arg m "$(record)" --argjson ec "$ec" '{phase:"Failed",containerStatuses:[{name:"trim",state:{terminated:{exitCode:$ec,reason:"Error",message:$m}}}]}')")]"
                else items="[$(pod_item '{"phase":"Running","containerStatuses":[{"name":"trim","state":{"running":{}}}]}')]"; fi ;;
            *)
                if finished; then items="[$(pod_item "$(jq -n --arg m "$(record)" '{phase:"Succeeded",containerStatuses:[{name:"trim",state:{terminated:{exitCode:0,reason:"Completed",message:$m}}}]}')")]"
                else items="[$(pod_item '{"phase":"Running","containerStatuses":[{"name":"trim","state":{"running":{}}}]}')]"; fi ;;
            esac
            printf '{"kind":"List","items":%s}\n' "$items"; exit 0 ;;
        *" -l "*) echo "stub: pods by another selector: $*" >&2; exit 2 ;;
        *) printf '{"kind":"List","items":%s}\n' "$(cat "$S/pods.json")"; exit 0 ;;
        esac ;;
    events)
        if [ "$scenario" = admission ]; then
            echo '{"kind":"List","items":[{"type":"Warning","reason":"FailedCreate","message":"Error creating: pods \"x\" is forbidden: ValidatingAdmissionPolicy node-maintenance-holds-one-workload denied request: the trim container image is pinned by digest"}]}'
        else
            echo '{"kind":"List","items":[]}'
        fi
        exit 0 ;;
    esac ;;
create)
    [ "$2" = job ] || { echo "stub: create $2" >&2; exit 2; }
    name="$3"
    case "$*" in
    *" --from=cronjob/boss-node-trim -n boss-node-maintenance "*) ;;
    *) echo "stub: a create that is not from the one CronJob in the one namespace: $*" >&2; exit 2 ;;
    esac
    : > "$S/sent"
    case "${STUB_CREATE:-ok}" in
    lost)
        # The API server committed the Job and the answer never came
        # back: the object is there, and this caller holds no uid for it.
        jq -n --arg n "$name" '{kind: "Job", metadata: {name: $n, uid: "uid-of-the-lost-create", creationTimestamp: "2026-10-08T01:00:00Z"}, status: {active: 1}}' > "$S/jobs/$name.json"
        echo "Unable to connect to the server: net/http: request canceled (Client.Timeout exceeded while awaiting headers)" >&2; exit 1 ;;
    exists)
        # Somebody else's Job holds the very name: it is there to be
        # read, and to be deleted by a run that believed it its own.
        jq -n --arg n "$name" '{kind: "Job", metadata: {name: $n, uid: "uid-somebody-elses"}, status: {conditions: [{type: "Complete", status: "True"}]}}' > "$S/jobs/$name.json"
        echo "Error from server (AlreadyExists): jobs.batch \"$name\" already exists" >&2; exit 1 ;;
    refused) echo "Error from server (Forbidden): jobs.batch \"$name\" is forbidden: ValidatingAdmissionPolicy 'node-maintenance-holds-one-workload' with binding 'node-maintenance-holds-one-workload' denied request: the trim container image is pinned by digest" >&2; exit 1 ;;
    esac
    jq -n --arg n "$name" '{kind: "Job", metadata: {name: $n, uid: "uid-made-by-this-run", creationTimestamp: "2026-10-08T01:00:00Z"}, status: {}}' > "$S/jobs/$name.json"
    echo "$name" > "$S/mine"
    if [ "${STUB_CREATE:-ok}" = nouid ]; then echo "job.batch/$name created"; else cat "$S/jobs/$name.json"; fi
    exit 0 ;;
logs)
    [ "${STUB_LOGS:-}" = fail ] && { echo "Error from server (Forbidden): pods \"$2\" is forbidden: cannot get resource \"pods/log\"" >&2; exit 1; }
    [ "$2" = "$mine-x7k2p" ] || { echo "stub: logs of $2" >&2; exit 2; }
    case "$scenario" in imagepull|unsched)
        echo "Error from server (BadRequest): container \"trim\" in pod \"$2\" is waiting to start" >&2; exit 1 ;;
    esac
    record; exit 0 ;;
delete)
    [ "$2" = job ] || { echo "stub: delete $2" >&2; exit 2; }
    [ "${STUB_DELETE:-ok}" = noop ] || rm -f "$S/jobs/$3.json"
    echo "job.batch \"$3\" deleted"; exit 0 ;;
esac
echo "stub: unexpected kubectl call: $*" >&2
exit 2
"#;

fn manifest() -> String {
    std::fs::read_to_string(repo_root().join(MANIFEST)).expect("the trim manifest")
}

/// The one value of `<key>: ` on a line that is not a comment.
fn declared(key: &str) -> String {
    let lead = format!("{key}: ");
    let found: Vec<String> = manifest()
        .lines()
        .map(str::trim_start)
        .filter(|l| !l.starts_with('#'))
        .filter_map(|l| l.strip_prefix(&lead).map(str::to_string))
        .collect();
    assert_eq!(
        found.len(),
        1,
        "{MANIFEST} declares `{key}` {} time(s)",
        found.len()
    );
    found[0].clone()
}

/// The trim script as the block scalar under `args:` yields it — read
/// here by the block's own indent, independently of the awk in the
/// script under test.
fn declared_script() -> String {
    let text = manifest();
    let lines: Vec<&str> = text.lines().collect();
    let at = lines
        .iter()
        .position(|l| l.trim() == "args:")
        .expect("an args: line");
    assert_eq!(lines[at + 1].trim(), "- |", "args opens a block scalar");
    let indent = lines[at + 2].len() - lines[at + 2].trim_start().len();
    let mut out = String::new();
    for l in &lines[at + 2..] {
        if l.len() - l.trim_start().len() < indent {
            break;
        }
        out.push_str(&l[indent..]);
        out.push('\n');
    }
    assert!(out.contains("fstrim -v"), "the block is the trim script");
    out
}

fn cronjob() -> Value {
    json!({
        "apiVersion": "batch/v1", "kind": "CronJob",
        "metadata": {"name": CRONJOB, "namespace": NS, "uid": "uid-cronjob"},
        "spec": {"schedule": "40 12 * * *", "jobTemplate": {"spec": {
            "backoffLimit": 0,
            "activeDeadlineSeconds": declared("activeDeadlineSeconds").parse::<u64>().unwrap(),
            "template": {"spec": {"containers": [{
                "name": "trim", "image": declared("image"),
                "command": ["/bin/sh", "-c"], "args": [declared_script()]
            }]}}
        }}}
    })
}

fn policy() -> Value {
    json!({"kind": "ValidatingAdmissionPolicy",
           "metadata": {"name": "node-maintenance-holds-one-workload"},
           "spec": {"failurePolicy": "Fail"}})
}

fn binding() -> Value {
    json!({"kind": "ValidatingAdmissionPolicyBinding",
           "metadata": {"name": "node-maintenance-holds-one-workload"},
           "spec": {"policyName": "node-maintenance-holds-one-workload",
                    "validationActions": ["Deny", "Audit"]}})
}

fn other_job(name: &str, conditions: Value, active: u64) -> Value {
    json!({"kind": "Job", "metadata": {"name": name, "uid": format!("uid-{name}"),
           "creationTimestamp": "2026-10-07T12:40:00Z"},
           "status": {"active": active, "conditions": conditions}})
}

fn complete() -> Value {
    json!([{"type": "Complete", "status": "True"}])
}

struct Fixture {
    dir: PathBuf,
}

impl Fixture {
    /// A converged cluster with no trim in flight.
    fn new(case: &str) -> Self {
        let dir = scratch_dir(&format!("trim-build-node-{case}"));
        let f = Fixture { dir };
        std::fs::create_dir_all(f.dir.join("bin")).unwrap();
        write_exec(&f.dir.join("bin/sudo"), STUB);
        write_exec(&f.dir.join("bin/sleep"), "#!/bin/sh\nexit 0\n");
        std::fs::create_dir_all(f.dir.join("state/jobs")).unwrap();
        f.state("binding.json", &binding());
        f.state("policy.json", &policy());
        f.state("cronjob.json", &cronjob());
        f.state("pods.json", &json!([]));
        f
    }

    fn state(&self, name: &str, v: &Value) {
        write_file(&self.dir.join("state").join(name), &v.to_string());
    }

    fn job(&self, v: &Value) {
        let name = v["metadata"]["name"].as_str().unwrap();
        self.state(&format!("jobs/{name}.json"), v);
    }

    fn command(&self, args: &[&str], env: &[(&str, &str)]) -> Command {
        let path = format!(
            "{}:{}",
            self.dir.join("bin").display(),
            std::env::var("PATH").unwrap_or_default()
        );
        let mut c = Command::new("bash");
        c.arg(repo_root().join(SCRIPT))
            .args(args)
            .env("PATH", path)
            .env_remove("HOME")
            .env_remove("BOSS_JOBS_URL")
            .env("BOSS_SOR_ENV", self.dir.join("absent-sor.env"))
            .env("BOSS_FORGE_REGISTRY_HOST", "reg.test")
            .env("BOSS_OPS_DIR", self.dir.join("boss-ops"))
            .env("STUB_ARGV", self.dir.join("argv"))
            .env("STUB_STATE", self.dir.join("state"));
        for (k, v) in env {
            c.env(k, v);
        }
        c
    }

    fn run(&self, env: &[(&str, &str)]) -> Output {
        self.command(&[], env).output().expect("the script runs")
    }

    /// Every door call, the door's prefix already cut off by the stub.
    fn calls(&self) -> Vec<String> {
        std::fs::read_to_string(self.dir.join("argv"))
            .unwrap_or_default()
            .lines()
            .map(str::to_string)
            .collect()
    }

    fn calls_of(&self, verb: &str) -> Vec<String> {
        let lead = format!("{verb} ");
        self.calls()
            .into_iter()
            .filter(|c| c.starts_with(&lead))
            .collect()
    }

    /// The Jobs the stand-in still holds, by name.
    fn jobs(&self) -> Vec<String> {
        let mut names: Vec<String> = std::fs::read_dir(self.dir.join("state/jobs"))
            .unwrap()
            .map(|e| {
                e.unwrap()
                    .file_name()
                    .to_string_lossy()
                    .trim_end_matches(".json")
                    .to_string()
            })
            .collect();
        names.sort();
        names
    }
}

fn text(o: &Output) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&o.stdout),
        String::from_utf8_lossy(&o.stderr)
    )
}

fn code(o: &Output) -> i32 {
    o.status.code().expect("an exit code")
}

fn verb() -> Value {
    serde_json::from_str(&std::fs::read_to_string(repo_root().join(VERB)).unwrap()).unwrap()
}

/// Does any line of `out` match the verb's declared effect, in the
/// runner's own engine (jq `test`, line by line)?
fn shows_effect(out: &str) -> bool {
    let re = verb()["effect"].as_str().unwrap().to_string();
    let o = Command::new("jq")
        .args([
            "-Rn",
            "--arg",
            "re",
            &re,
            "[inputs | select(test($re))] | length",
        ])
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .spawn()
        .and_then(|mut c| {
            use std::io::Write;
            c.stdin.take().unwrap().write_all(out.as_bytes())?;
            c.wait_with_output()
        })
        .expect("jq judges the effect");
    assert!(o.status.success(), "jq could not judge {re:?}");
    String::from_utf8_lossy(&o.stdout).trim() != "0"
}

/// A refusal: exit 78, the reason named, and not one write behind it.
fn assert_refused(f: &Fixture, o: &Output, naming: &str) {
    let t = text(o);
    assert_eq!(code(o), 78, "expected a refusal naming {naming:?}: {t}");
    assert!(t.contains("trim-build-node: REFUSED — "), "{t}");
    assert!(
        t.contains(naming),
        "the refusal does not name {naming:?}: {t}"
    );
    assert!(t.contains("Nothing was created."), "{t}");
    assert_no_write(f, &t);
    assert!(!shows_effect(&t), "a refusal showed the effect: {t}");
}

fn assert_no_write(f: &Fixture, t: &str) {
    assert!(
        f.calls().iter().all(|c| c.starts_with("get ")),
        "something other than a read went through the door: {:?}\n{t}",
        f.calls()
    );
}

/// A run that made a Job and FAILED: exit 1, the reason named, no effect
/// line, and the Job it made removed by name.
fn assert_failed_and_removed(f: &Fixture, o: &Output, naming: &[&str]) {
    let t = text(o);
    assert_eq!(code(o), 1, "expected FAILED: {t}");
    assert!(t.contains("trim-build-node: FAILED — "), "{t}");
    for n in naming {
        assert!(t.contains(n), "the failure does not name {n:?}: {t}");
    }
    assert!(!shows_effect(&t), "a failed trim showed the effect: {t}");
    assert!(!t.contains("trim-build-node: OK"), "{t}");
    let made = f.calls_of("create");
    assert_eq!(made.len(), 1, "{t}");
    assert_eq!(f.calls_of("delete").len(), 1, "its own Job is removed: {t}");
    assert!(
        f.jobs().is_empty(),
        "the made Job is still there: {:?}",
        f.jobs()
    );
}

#[test]
fn a_healthy_run_creates_the_declared_job_reports_its_record_and_removes_it() {
    let f = Fixture::new("healthy");
    let o = f.run(&[]);
    let t = text(&o);
    assert_eq!(code(&o), 0, "{t}");

    // ONE create and ONE delete, each exactly this — nothing a caller
    // or a packet could have put there.
    let creates = f.calls_of("create");
    assert_eq!(creates.len(), 1, "{t}");
    let words: Vec<&str> = creates[0].split(' ').collect();
    let name = words[2];
    assert!(
        name.len() == "boss-node-trim-manual-".len() + 14
            && name.starts_with("boss-node-trim-manual-")
            && name["boss-node-trim-manual-".len()..]
                .chars()
                .all(|c| c.is_ascii_digit()),
        "the Job's name is not the fixed prefix and a UTC second: {name}"
    );
    assert_eq!(
        creates[0],
        format!("create job {name} --from=cronjob/{CRONJOB} -n {NS} -o json --request-timeout=30s")
    );
    assert_eq!(
        f.calls_of("delete"),
        vec![format!(
            "delete job {name} -n {NS} --wait=true --timeout=60s --request-timeout=70s"
        )]
    );
    for c in f.calls() {
        let v = c.split(' ').next().unwrap();
        assert!(
            ["get", "create", "delete", "logs"].contains(&v),
            "an unexpected kubectl verb went through the door: {c}"
        );
    }

    // The trim's own record is on the packet: both mounts, the counters
    // before and after, its closing line.
    for line in [
        "    node-trim: before /sys/block/nvme1n1: discards=699 sectors=4096",
        "    node-trim: /trim/gate: 442701234176 bytes trimmed",
        "    node-trim: /trim/ephemeral: 5368709120 bytes trimmed",
        "    node-trim: after /sys/block/nvme1n1: discards=3120 sectors=864650848",
        "    node-trim: OK",
    ] {
        assert!(t.contains(line), "the record lacks {line:?}: {t}");
    }
    assert!(t.contains(&format!("the log of {name}-x7k2p")), "{t}");
    assert!(shows_effect(&t), "no line matches the declared effect: {t}");
    assert!(
        t.contains(&format!(
            "with 2 discard-counter line(s) before and 2 after; Job {name} removed (read back: NotFound)"
        )),
        "{t}"
    );
    assert!(f.jobs().is_empty(), "{:?}", f.jobs());
}

#[test]
fn any_argument_is_refused_before_any_door_opens() {
    for args in [
        vec!["w-1"],
        vec!["--node=cp-1"],
        vec!["/var"],
        vec!["kube-system", "busybox"],
        vec![""],
    ] {
        let f = Fixture::new("argument");
        let o = f.command(&args, &[]).output().unwrap();
        let t = text(&o);
        assert_eq!(code(&o), 78, "{args:?} was not refused: {t}");
        assert!(t.contains("takes no argument"), "{t}");
        assert!(
            f.calls().is_empty(),
            "{args:?}: a door was opened for a run with an argument: {:?}",
            f.calls()
        );
    }
}

#[test]
fn the_verb_takes_nothing_from_a_packet_and_asks_no_passkey() {
    let v = verb();
    assert_eq!(v["argv"], json!([SCRIPT]), "no placeholder, no flag");
    assert_eq!(v["params"], json!([]), "no packet-supplied input at all");
    assert_eq!(v["hosts"], json!(["forge"]));
    assert!(
        v.get("requires_approval").is_none() && v.get("plan_verb").is_none(),
        "the verb is passkey-free by David's decision of 2026-10-07; a change to that is its own car"
    );
    let about = v["about"].as_str().unwrap();
    for must in [
        "MUTATING",
        "David",
        "node-maintenance-holds-one-workload",
        "NO PASSKEY",
        // Finding 8: the authorisation is David's OWN release of the
        // held car, and the about says an agent's release is not it.
        "authorised only by his own release of its held trust-boundary car",
        "a release run by an agent session is not that authorisation",
    ] {
        assert!(
            about.contains(must),
            "the verb's about does not say {must:?}"
        );
    }

    // THE WAIT AND THE RUNNER'S KILL ARE HELD APART: the script waits
    // the manifest's own deadline plus its margin, and the verb's
    // timeout leaves room after that for the door, the log and the
    // removal's read-back.
    let deadline: u64 = declared("activeDeadlineSeconds").parse().unwrap();
    let script = std::fs::read_to_string(repo_root().join(SCRIPT)).unwrap();
    let constant = |name: &str| -> u64 {
        script
            .lines()
            .find_map(|l| l.strip_prefix(&format!("{name}=")))
            .unwrap_or_else(|| panic!("{SCRIPT} declares no {name}"))
            .parse()
            .unwrap()
    };
    let timeout = v["timeout"].as_u64().expect("a declared timeout");
    assert!(
        timeout >= deadline + constant("MARGIN_S") + 300,
        "the verb's timeout ({timeout}s) would kill a trim the script is still entitled to wait \
         for ({deadline}s deadline + {}s margin, and 300s for the door and the removal)",
        constant("MARGIN_S")
    );
    assert!(constant("START_GRACE_S") < deadline);
}

#[test]
fn the_script_carries_no_pod_spec_and_writes_through_one_create_and_one_delete() {
    let script = std::fs::read_to_string(repo_root().join(SCRIPT)).unwrap();
    let code: Vec<&str> = script
        .lines()
        .filter(|l| !l.trim_start().starts_with('#'))
        .collect();
    let body = code.join("\n");
    for word in [
        "hostPath",
        "privileged",
        "SYS_ADMIN",
        "securityContext",
        "nodeSelector",
        "--image",
        "--overrides",
        "apply",
        "kind: ",
        "fstrim",
    ] {
        assert!(
            !body.contains(word),
            "{SCRIPT} carries {word:?} in its code: the pod is the CronJob's template, never \
             something this script spells"
        );
    }
    // Every kubectl verb the script hands the door, read off its code.
    let mut verbs: Vec<&str> = code
        .iter()
        .flat_map(|l| l.match_indices("$K ").map(move |(i, _)| &l[i + 3..]))
        .map(|rest| rest.split(' ').next().unwrap())
        .collect();
    verbs.sort();
    assert_eq!(
        verbs,
        vec!["create", "delete", "get", "logs"],
        "the script's writes are one create and one delete; its reads all go through kget"
    );
    assert!(body.contains("\"--from=cronjob/$CRONJOB\""));
    // The delete names the Job this run made — never a selector.
    let delete = code.iter().find(|l| l.contains("$K delete ")).unwrap();
    assert!(
        delete.contains("delete job \"$NAME\" -n \"$NS\"")
            && !delete.contains(" -l")
            && !delete.contains("--all")
            && !delete.contains("--selector"),
        "{delete}"
    );
    // The literals are the manifest's own.
    let m = manifest();
    assert!(body.contains(&format!("\nNS={NS}\n")) && m.contains(&format!("  namespace: {NS}\n")));
    assert!(
        body.contains(&format!("\nCRONJOB={CRONJOB}\n"))
            && m.contains(&format!("  name: {CRONJOB}\n"))
    );
    assert!(body.contains("\nMOUNTS=\"/trim/gate /trim/ephemeral\"\n"));
    for mount in ["/trim/gate", "/trim/ephemeral"] {
        assert!(m.contains(&format!("mountPath: {mount}\n")), "{mount}");
    }
}

#[test]
fn a_trim_in_flight_is_refused_and_nothing_is_created() {
    // The CronJob's own run, active.
    let f = Fixture::new("active-scheduled");
    f.job(&other_job("boss-node-trim-29330680", json!([]), 1));
    assert_refused(&f, &f.run(&[]), "Job boss-node-trim-29330680");
    assert_eq!(f.jobs(), vec!["boss-node-trim-29330680"]);

    // Another manual one, made a moment ago: no pod yet, active 0.
    let f = Fixture::new("active-just-made");
    f.job(
        &json!({"kind": "Job", "metadata": {"name": "boss-node-trim-manual-20261008005959",
                  "uid": "u", "creationTimestamp": "2026-10-08T00:59:59Z"}, "status": {}}),
    );
    assert_refused(&f, &f.run(&[]), "already in flight");

    // A pod there that is not terminal, with no Job behind it.
    let f = Fixture::new("active-pod");
    f.state(
        "pods.json",
        &json!([{"metadata": {"name": "a-hand-made-trim"}, "status": {"phase": "Pending"}}]),
    );
    assert_refused(&f, &f.run(&[]), "pod a-hand-made-trim (Pending)");

    // FINISHED history is not a trim in flight: the CronJob keeps three
    // of each kind, and the verb must still run beside them — and must
    // leave every one of them where it was.
    let f = Fixture::new("history");
    f.job(&other_job("boss-node-trim-29330680", complete(), 0));
    f.job(&other_job(
        "boss-node-trim-29329240",
        json!([{"type": "Failed", "status": "True", "reason": "DeadlineExceeded"}]),
        0,
    ));
    f.job(&other_job(
        "boss-node-trim-manual-20261006010203",
        complete(),
        0,
    ));
    f.state(
        "pods.json",
        &json!([{"metadata": {"name": "boss-node-trim-29330680-abcde"}, "status": {"phase": "Succeeded"}}]),
    );
    let o = f.run(&[]);
    assert_eq!(code(&o), 0, "{}", text(&o));
    assert_eq!(
        f.jobs(),
        vec![
            "boss-node-trim-29329240",
            "boss-node-trim-29330680",
            "boss-node-trim-manual-20261006010203"
        ],
        "a Job this run did not make was touched"
    );
    assert_eq!(f.calls_of("delete").len(), 1);
}

#[test]
fn an_unconverged_namespace_is_refused() {
    let f = Fixture::new("no-cronjob");
    std::fs::remove_file(f.dir.join("state/cronjob.json")).unwrap();
    assert_refused(&f, &f.run(&[]), "CronJob boss-node-trim does not exist");

    let f = Fixture::new("no-binding");
    std::fs::remove_file(f.dir.join("state/binding.json")).unwrap();
    assert_refused(
        &f,
        &f.run(&[]),
        "ValidatingAdmissionPolicyBinding node-maintenance-holds-one-workload does not exist",
    );

    let f = Fixture::new("binding-audits-only");
    f.state(
        "binding.json",
        &json!({"spec": {"validationActions": ["Audit"]}}),
    );
    assert_refused(&f, &f.run(&[]), "does not Deny");
}

#[test]
fn a_cronjob_that_is_not_the_one_the_tree_declares_is_refused() {
    let edit = |case: &str, pointer: &str, value: Value, naming: &str| {
        let f = Fixture::new(case);
        let mut c = cronjob();
        *c.pointer_mut(pointer).unwrap() = value;
        f.state("cronjob.json", &c);
        assert_refused(&f, &f.run(&[]), naming);
        assert!(text(&f.run(&[])).contains("drifted"));
    };
    let c0 = "/spec/jobTemplate/spec/template/spec/containers/0";
    // Another digest: the tree moved and the converge has not, or the
    // reverse.
    let moved = declared("image").replace("@sha256:c", "@sha256:d");
    assert_ne!(moved, declared("image"));
    edit("drift-digest", &format!("{c0}/image"), json!(moved), &moved);
    // The same digest by a tag-only reference.
    edit(
        "drift-tag",
        &format!("{c0}/image"),
        json!(declared("image").split('@').next().unwrap()),
        "the manifest declares",
    );
    // One byte of script.
    edit(
        "drift-script",
        &format!("{c0}/args/0"),
        json!(declared_script().replace("fstrim -v", "fstrim -a")),
        "byte for byte",
    );
    edit(
        "drift-script-newline",
        &format!("{c0}/args/0"),
        json!(declared_script().trim_end()),
        "byte for byte",
    );
    edit(
        "drift-deadline",
        "/spec/jobTemplate/spec/activeDeadlineSeconds",
        json!(86400),
        "activeDeadlineSeconds is '86400'",
    );
    // A second container beside the trim.
    let f = Fixture::new("drift-second-container");
    let mut c = cronjob();
    let cs = c
        .pointer_mut("/spec/jobTemplate/spec/template/spec/containers")
        .unwrap()
        .as_array_mut()
        .unwrap();
    cs.push(json!({"name": "rider", "image": "busybox"}));
    f.state("cronjob.json", &c);
    assert_refused(&f, &f.run(&[]), "exactly one container");
}

#[test]
fn a_job_that_fails_is_failed_with_the_clusters_reason() {
    // The trim itself exited 1: its RED lines are on the packet.
    let f = Fixture::new("fail-exit");
    let o = f.run(&[("STUB_SCENARIO", "exit1")]);
    assert_failed_and_removed(
        &f,
        &o,
        &[
            "BackoffLimitExceeded",
            "(pod exit code 1)",
            "    node-trim: RED fstrim /trim/gate failed",
        ],
    );

    // The Job's own deadline.
    let f = Fixture::new("fail-deadline");
    let o = f.run(&[("STUB_SCENARIO", "deadline")]);
    assert_failed_and_removed(
        &f,
        &o,
        &[
            "DeadlineExceeded",
            "Job was active longer than specified deadline",
        ],
    );

    // An image that cannot be pulled never fails a Job by itself: the
    // verb fails it at the start grace, not at the deadline.
    let f = Fixture::new("fail-image");
    let o = f.run(&[("STUB_SCENARIO", "imagepull")]);
    assert_failed_and_removed(
        &f,
        &o,
        &[
            "had not started after 180s",
            "ImagePullBackOff",
            "manifest unknown",
        ],
    );
    assert!(
        f.calls_of("get")
            .iter()
            .filter(|c| c.starts_with("get job "))
            .count()
            < 30,
        "an unpullable image was waited on past the start grace"
    );

    // The admission policy refused the POD: the Job stands with no pod,
    // and the reason is the controller's FailedCreate event.
    let f = Fixture::new("fail-admission-pod");
    let o = f.run(&[("STUB_SCENARIO", "admission")]);
    assert_failed_and_removed(
        &f,
        &o,
        &[
            "no pod was created",
            "FailedCreate",
            "node-maintenance-holds-one-workload denied request",
        ],
    );

    // The build node is cordoned or down.
    let f = Fixture::new("fail-unschedulable");
    let o = f.run(&[("STUB_SCENARIO", "unsched")]);
    assert_failed_and_removed(&f, &o, &["Unschedulable", "1 node(s) were unschedulable"]);

    // A Job that says Complete over a pod that exited 3, with a record
    // that reads clean: the exit code is judged on its own.
    let f = Fixture::new("fail-complete-over-a-bad-exit");
    let o = f.run(&[("STUB_SCENARIO", "exit3")]);
    assert_failed_and_removed(
        &f,
        &o,
        &["the Job says Complete but its pod's exit code reads '3'"],
    );

    // Complete, exit 0 — and a record that shows no trim.
    let f = Fixture::new("fail-no-record");
    let o = f.run(&[("STUB_SCENARIO", "norecord")]);
    assert_failed_and_removed(
        &f,
        &o,
        &[
            "does not show the trim",
            "/trim/gate /trim/ephemeral",
            "closing OK line absent",
        ],
    );
}

#[test]
fn a_create_the_api_server_refuses_makes_nothing_and_removes_nothing() {
    // The admission policy refused the JOB.
    let f = Fixture::new("create-refused");
    let o = f.run(&[("STUB_CREATE", "refused")]);
    let t = text(&o);
    assert_eq!(code(&o), 1, "{t}");
    assert!(
        t.contains("FAILED — the API server did not create Job")
            && t.contains("denied request: the trim container image is pinned by digest"),
        "{t}"
    );
    // "made no Job" is said only behind a read that shows it: the name
    // is read back AFTER the create, and answers nothing.
    assert!(
        t.contains("read back: no Job of that name exists")
            && t.contains("this run made no Job and removes none"),
        "{t}"
    );
    let calls = f.calls();
    let sent = calls.iter().position(|c| c.starts_with("create ")).unwrap();
    let name = calls[sent].split(' ').nth(2).unwrap();
    assert!(
        calls[sent + 1..]
            .iter()
            .any(|c| c.starts_with(&format!("get job {name} -n {NS} "))),
        "the failed create was not read back: {calls:?}"
    );
    assert!(f.calls_of("delete").is_empty(), "{t}");
    assert!(!shows_effect(&t));

    // A Job of that very name already exists: it is somebody else's.
    let f = Fixture::new("create-exists");
    f.job(&other_job("boss-node-trim-29330680", complete(), 0));
    let o = f.run(&[("STUB_CREATE", "exists")]);
    let t = text(&o);
    assert_eq!(code(&o), 1, "{t}");
    assert!(
        t.contains("AlreadyExists")
            && t.contains("IS STANDING")
            && t.contains("uid uid-somebody-elses")
            && t.contains("NOT removed by this run"),
        "{t}"
    );
    assert!(!t.contains("made no Job"), "{t}");
    assert!(
        f.calls_of("delete").is_empty(),
        "a Job this run did not make was deleted: {t}"
    );
    assert_eq!(
        f.jobs().len(),
        2,
        "the Job that held the name is gone: {:?}",
        f.jobs()
    );

    // A create that answers success without the object: no uid, so no
    // delete by name alone.
    let f = Fixture::new("create-no-uid");
    let o = f.run(&[("STUB_CREATE", "nouid")]);
    let t = text(&o);
    assert_eq!(code(&o), 1, "{t}");
    assert!(
        t.contains("gave no uid") && t.contains("NOT removed by this run"),
        "{t}"
    );
    assert!(f.calls_of("delete").is_empty(), "{t}");
}

/// Review 3e750cce, finding 1. `kubectl create` exits non-zero for a
/// refusal AND for an answer that never arrived (its request timeout, a
/// dropped connection) after the API server committed the Job. The old
/// script said "this run made no Job" for both, without a read.
#[test]
fn a_create_whose_answer_is_lost_is_read_back_before_anything_is_said() {
    // The server made the Job; the answer was lost.
    let f = Fixture::new("create-lost");
    let o = f.run(&[("STUB_CREATE", "lost")]);
    let t = text(&o);
    assert_eq!(code(&o), 1, "{t}");
    let name = f.jobs().pop().expect("the lost create's Job stands");
    assert!(
        t.contains("trim-build-node: FAILED — ")
            && t.contains("Client.Timeout exceeded")
            && t.contains(&format!("a Job named {name} IS STANDING"))
            && t.contains("uid uid-of-the-lost-create")
            && t.contains("created 2026-10-08T01:00:00Z")
            && t.contains("NOT removed by this run")
            && t.contains(&format!("kubectl -n {NS} get job {name}")),
        "{t}"
    );
    assert!(
        !t.contains("made no Job") && !t.contains("did not create"),
        "a Job that stands was reported as not made: {t}"
    );
    // Not provably this run's (no uid came back), so it is not deleted.
    assert!(f.calls_of("delete").is_empty(), "{t}");
    assert_eq!(f.jobs(), vec![name]);
    assert!(
        !shows_effect(&t) && !t.contains("trim-build-node: OK"),
        "{t}"
    );

    // The create failed and the read-back cannot be made: whether a Job
    // stands is UNKNOWN — never "made no Job", never FAILED.
    for create in ["refused", "lost"] {
        let f = Fixture::new(&format!("create-{create}-readback-dark"));
        let o = f.run(&[("STUB_CREATE", create), ("STUB_DARK", "readback")]);
        let t = text(&o);
        assert_eq!(code(&o), 4, "{t}");
        assert!(
            t.contains("trim-build-node: CANNOT ANSWER — ")
                && t.contains("could not be read back")
                && t.contains("i/o timeout")
                && t.contains("UNKNOWN")
                && t.contains(&format!("kubectl -n {NS} get job boss-node-trim-manual-")),
            "{t}"
        );
        assert!(
            !t.contains("made no Job") && !t.contains("trim-build-node: FAILED"),
            "{t}"
        );
        assert!(f.calls_of("delete").is_empty(), "{t}");
        assert!(!shows_effect(&t), "{t}");
    }
}

/// Finding 2. `.spec.suspend: true` is the one switch a human has in
/// the cluster to stop the trim, and `create job --from` a suspended
/// CronJob makes a running Job all the same.
#[test]
fn a_suspended_cronjob_is_the_stop_switch_and_is_refused() {
    let f = Fixture::new("suspended");
    let mut c = cronjob();
    c["spec"]["suspend"] = json!(true);
    f.state("cronjob.json", &c);
    let o = f.run(&[]);
    assert_refused(&f, &o, "CronJob boss-node-trim is SUSPENDED");
    assert!(text(&o).contains("stop switch"), "{}", text(&o));

    // `suspend: false`, written out, is not a stop.
    let f = Fixture::new("suspend-false");
    let mut c = cronjob();
    c["spec"]["suspend"] = json!(false);
    f.state("cronjob.json", &c);
    let o = f.run(&[]);
    assert_eq!(code(&o), 0, "{}", text(&o));
}

/// Finding 3(a). A binding by the right name that binds another policy,
/// or is narrowed to other resources, or whose policy is gone or fails
/// open, bounds nothing — and used to pass as "binding … denies".
#[test]
fn a_binding_that_does_not_bind_the_declared_policy_to_everything_is_refused() {
    let edit = |case: &str, pointer: &str, value: Value, naming: &str| {
        let f = Fixture::new(case);
        let mut b = binding();
        b["spec"][pointer] = value;
        f.state("binding.json", &b);
        assert_refused(&f, &f.run(&[]), naming);
    };
    edit(
        "binding-other-policy",
        "policyName",
        json!("some-other-policy"),
        "names policy 'some-other-policy'",
    );
    edit(
        "binding-narrowed",
        "matchResources",
        json!({"namespaceSelector": {"matchLabels": {"kubernetes.io/metadata.name": "elsewhere"}}}),
        "matchResources",
    );
    edit(
        "binding-param",
        "paramRef",
        json!({"name": "x", "parameterNotFoundAction": "Allow"}),
        "paramRef",
    );

    let f = Fixture::new("policy-absent");
    std::fs::remove_file(f.dir.join("state/policy.json")).unwrap();
    assert_refused(
        &f,
        &f.run(&[]),
        "ValidatingAdmissionPolicy node-maintenance-holds-one-workload does not exist",
    );

    let f = Fixture::new("policy-fails-open");
    let mut p = policy();
    p["spec"]["failurePolicy"] = json!("Ignore");
    f.state("policy.json", &p);
    assert_refused(&f, &f.run(&[]), "failurePolicy is 'Ignore'");
}

/// Finding 3(b). What the admission policy does NOT bound is how many of
/// the admitted pod a Job asks for; the script compares those to the
/// checkout too. A value the manifest leaves unset is one.
#[test]
fn a_cronjob_that_asks_for_more_than_one_trim_pod_is_refused() {
    let js = "/spec/jobTemplate/spec";
    for (case, key, value, naming) in [
        (
            "drift-parallelism",
            "parallelism",
            json!(5),
            "parallelism is '5'",
        ),
        (
            "drift-completions",
            "completions",
            json!(3),
            "completions is '3'",
        ),
        (
            "drift-backoff",
            "backoffLimit",
            json!(6),
            "backoffLimit is '6'",
        ),
    ] {
        let f = Fixture::new(case);
        let mut c = cronjob();
        c.pointer_mut(js).unwrap()[key] = value;
        f.state("cronjob.json", &c);
        let o = f.run(&[]);
        assert_refused(&f, &o, naming);
        assert!(text(&o).contains("drifted"), "{}", text(&o));
    }
    // An unset backoffLimit is the server's default of six, not zero.
    let f = Fixture::new("drift-backoff-unset");
    let mut c = cronjob();
    c.pointer_mut(js)
        .unwrap()
        .as_object_mut()
        .unwrap()
        .remove("backoffLimit");
    f.state("cronjob.json", &c);
    assert_refused(&f, &f.run(&[]), "backoffLimit is 'unset'");
    // One, written out, is what unset means.
    let f = Fixture::new("parallelism-one");
    let mut c = cronjob();
    c.pointer_mut(js).unwrap()["parallelism"] = json!(1);
    c.pointer_mut(js).unwrap()["completions"] = json!(1);
    f.state("cronjob.json", &c);
    let o = f.run(&[]);
    assert_eq!(code(&o), 0, "{}", text(&o));
}

/// Finding 3: the line printed before the create names exactly what was
/// read and compared, and says what was not. The old line said "every
/// bound holds" and "is the one this checkout declares" over four
/// compared fields and one word of the binding.
#[test]
fn the_line_before_the_create_names_what_was_compared_and_no_more() {
    let f = Fixture::new("sentence");
    let o = f.run(&[]);
    let t = text(&o);
    assert_eq!(code(&o), 0, "{t}");
    let line = t
        .lines()
        .find(|l| l.contains("Creating Job "))
        .unwrap_or_else(|| panic!("no line announces the create: {t}"));
    let deadline = declared("activeDeadlineSeconds");
    for part in [
        "what this run read holds".to_string(),
        "binding node-maintenance-holds-one-workload names policy node-maintenance-holds-one-workload, lists Deny and carries no matchResources and no paramRef".to_string(),
        "that policy exists with failurePolicy Fail (its rules were not read)".to_string(),
        "CronJob boss-node-trim on the server is not suspended".to_string(),
        format!("one container, image {}", declared("image")),
        "its script byte for byte".to_string(),
        format!("activeDeadlineSeconds {deadline}, backoffLimit 0, parallelism 1, completions 1"),
        "The rest of its pod template was NOT compared by this run".to_string(),
        "no unfinished Job and no non-terminal pod".to_string(),
    ] {
        assert!(line.contains(&part), "the line lacks {part:?}: {line}");
    }
    for overclaim in ["every bound holds", "is the one this checkout declares"] {
        assert!(
            !t.contains(overclaim),
            "the run still says {overclaim:?}, which is more than it read: {t}"
        );
    }
    // A pod template that differs where the script does not look is NOT
    // refused here — the admission policy judges it at the create, and
    // the stand-in has no policy. What the test holds is that such a run
    // never calls the CronJob the one the checkout declares.
    let f = Fixture::new("sentence-pod-level-drift");
    let mut c = cronjob();
    c.pointer_mut("/spec/jobTemplate/spec/template/spec")
        .unwrap()["hostPID"] = json!(true);
    f.state("cronjob.json", &c);
    let t = text(&f.run(&[]));
    assert!(
        t.contains("was NOT compared by this run") && !t.contains("this checkout declares"),
        "{t}"
    );
}

/// Finding 6 (the reviewer's surviving mutant r03): both mounts trimmed
/// and exit 0, but the record does not close on `node-trim: OK`.
#[test]
fn a_record_that_does_not_close_ok_is_failed_although_both_mounts_trimmed() {
    let f = Fixture::new("fail-no-closing-ok");
    let o = f.run(&[("STUB_SCENARIO", "nook")]);
    assert_failed_and_removed(
        &f,
        &o,
        &[
            "    node-trim: /trim/gate: 442701234176 bytes trimmed",
            "    node-trim: /trim/ephemeral: 5368709120 bytes trimmed",
            "does not show the trim",
            "(none missing)",
            "closing OK line absent",
        ],
    );
}

#[test]
fn a_job_that_never_ends_times_out_as_failed_and_is_removed() {
    let f = Fixture::new("timeout");
    let o = f.run(&[("STUB_SCENARIO", "never")]);
    let deadline: u64 = declared("activeDeadlineSeconds").parse().unwrap();
    assert_failed_and_removed(
        &f,
        &o,
        &[
            &format!("neither completed nor failed after {}s", deadline + 120),
            &format!("past its own deadline of {deadline}s"),
            "running since",
        ],
    );
}

#[test]
fn the_name_is_deleted_only_while_it_carries_the_uid_this_run_made() {
    // Another Job took the name while this run waited.
    let f = Fixture::new("uid-swapped");
    let o = f.run(&[("STUB_SCENARIO", "swapuid")]);
    let t = text(&o);
    assert_eq!(code(&o), 1, "{t}");
    assert!(t.contains("removed by something else"), "{t}");
    assert!(
        f.calls_of("delete").is_empty(),
        "a Job with another uid was deleted by name: {t}"
    );
    assert!(!shows_effect(&t));

    // The trim completed, and by the time of the delete the name is
    // another Job's: the read just before the delete is what holds it.
    let f = Fixture::new("uid-swapped-at-the-delete");
    let o = f.run(&[("STUB_SCENARIO", "swaplate")]);
    let t = text(&o);
    assert_eq!(code(&o), 1, "{t}");
    assert!(
        t.contains("is now held by uid uid-of-another-job") && t.contains("left alone"),
        "{t}"
    );
    assert!(
        f.calls_of("delete").is_empty(),
        "a Job with another uid was deleted by name: {t}"
    );
    assert!(!shows_effect(&t), "{t}");

    // The Job vanished mid-wait.
    let f = Fixture::new("vanished");
    let o = f.run(&[("STUB_SCENARIO", "vanish")]);
    let t = text(&o);
    assert_eq!(code(&o), 1, "{t}");
    assert!(t.contains("its outcome is unknown"), "{t}");
    assert!(f.calls_of("delete").is_empty(), "{t}");
}

#[test]
fn a_cluster_that_cannot_be_read_is_neither_failed_nor_ok() {
    // Before anything is made.
    let f = Fixture::new("dark-before");
    let o = f.run(&[("STUB_DARK", "all")]);
    let t = text(&o);
    assert_eq!(code(&o), 4, "{t}");
    assert!(
        t.contains("trim-build-node: CANNOT ANSWER — ")
            && t.contains("i/o timeout")
            && t.contains("nothing was created"),
        "{t}"
    );
    assert!(!t.contains("FAILED"), "{t}");
    assert_no_write(&f, &t);

    // After the Job is made: the outcome is UNKNOWN, and the Job is not
    // claimed removed.
    let f = Fixture::new("dark-after");
    let o = f.run(&[("STUB_DARK", "after")]);
    let t = text(&o);
    assert_eq!(code(&o), 4, "{t}");
    assert!(
        t.contains("CANNOT ANSWER — Job boss-node-trim-manual-")
            && t.contains("UNKNOWN, neither failed nor ok")
            && t.contains("was NOT proven removed"),
        "{t}"
    );
    assert!(!t.contains("trim-build-node: FAILED"), "{t}");
    assert!(
        !shows_effect(&t) && !t.contains("trim-build-node: OK"),
        "{t}"
    );
}

#[test]
fn a_trim_whose_job_cannot_be_removed_is_not_ok() {
    let f = Fixture::new("delete-noop");
    let o = f.run(&[("STUB_DELETE", "noop")]);
    let t = text(&o);
    assert_eq!(code(&o), 1, "{t}");
    assert!(
        t.contains("STILL THERE after the delete")
            && t.contains("the trim itself SUCCEEDED")
            && t.contains("was NOT proven removed"),
        "{t}"
    );
    assert!(!shows_effect(&t), "{t}");
    // Once, not again from the exit trap.
    assert_eq!(f.calls_of("delete").len(), 1, "{t}");
}

#[test]
fn an_unreadable_log_falls_back_to_the_termination_message_and_says_so() {
    let f = Fixture::new("no-log");
    let o = f.run(&[("STUB_LOGS", "fail")]);
    let t = text(&o);
    assert_eq!(code(&o), 0, "{t}");
    assert!(
        t.contains("the termination message of boss-node-trim-manual-")
            && t.contains("its log could not be read")
            && t.contains("    node-trim: /trim/gate: 442701234176 bytes trimmed"),
        "{t}"
    );
    assert!(shows_effect(&t), "{t}");
}

#[test]
fn a_run_killed_mid_wait_still_removes_the_job_it_made() {
    // The runner's `timeout` sends TERM. With the real `sleep` back on
    // PATH the script is inside its first poll's sleep when it lands.
    let f = Fixture::new("killed");
    std::fs::remove_file(f.dir.join("bin/sleep")).unwrap();
    let path = format!(
        "{}:{}",
        f.dir.join("bin").display(),
        std::env::var("PATH").unwrap_or_default()
    );
    let o = Command::new("timeout")
        .arg("3")
        .arg("bash")
        .arg(repo_root().join(SCRIPT))
        .env("PATH", path)
        .env_remove("HOME")
        .env("BOSS_SOR_ENV", f.dir.join("absent-sor.env"))
        .env("BOSS_FORGE_REGISTRY_HOST", "reg.test")
        .env("BOSS_OPS_DIR", f.dir.join("boss-ops"))
        .env("STUB_ARGV", f.dir.join("argv"))
        .env("STUB_STATE", f.dir.join("state"))
        .env("STUB_SCENARIO", "never")
        .output()
        .expect("timeout runs the script");
    let t = text(&o);
    assert_eq!(code(&o), 124, "{t}");
    assert_eq!(f.calls_of("create").len(), 1, "{t}");
    assert!(
        t.contains("ending before Job boss-node-trim-manual-"),
        "{t}"
    );
    assert_eq!(f.calls_of("delete").len(), 1, "{t}");
    assert!(f.jobs().is_empty(), "{:?}", f.jobs());
    assert!(!shows_effect(&t), "{t}");
}
